//! KB-frontmatter validation.
//!
//! Enforces the knowledge-base file standard: every KB markdown file (those
//! whose slug lives under `kb/`) must carry frontmatter with non-empty
//! `title`, `description`, and `type`, where `type ∈ {note, session, group}`.
//!
//! Validation never aborts on a bad file — each problem is collected into the
//! report (mirroring `sync`). The command exits 0 even when files are invalid;
//! callers read `issues` from the JSON payload (soft-warn).

use std::path::Path;

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::error::{EvoError, Result};
use crate::ingest::{frontmatter, is_hidden, slug_for};
use crate::model::Frontmatter;
use crate::store::Store;

/// Allowed values for a KB file's `type` frontmatter field.
pub const VALID_TYPES: [&str; 3] = ["note", "session", "group"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateIssue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ValidateReport {
    pub checked: usize,
    pub valid: usize,
    pub invalid: usize,
    pub issues: Vec<ValidateIssue>,
}

/// Validate KB frontmatter.
///
/// - `file`: validate exactly this markdown file (overrides the recency filter).
/// - `since`: only validate files modified at/after this RFC3339 timestamp.
/// - `all`: validate every KB file, ignoring recency.
/// - default (none of the above): validate KB files modified since the last
///   sync (`last_synced_at` meta). Because `validate` is meant to run *before*
///   `sync`, that timestamp still points at the previous ingest, so this is
///   exactly the new/updated set. A fresh store (no meta) validates all.
pub fn run(
    store: &Store,
    file: Option<&str>,
    since: Option<&str>,
    all: bool,
) -> Result<ValidateReport> {
    let mut report = ValidateReport::default();

    // Single-file mode: the caller pointed at a specific file.
    if let Some(f) = file {
        validate_one(Path::new(f), f.to_string(), &mut report);
        return Ok(report);
    }

    let root = store.brain_root.clone();
    let threshold = if all {
        None
    } else if let Some(s) = since {
        Some(parse_rfc3339(s)?)
    } else {
        match store.get_meta("last_synced_at")? {
            Some(s) => parse_rfc3339(&s).ok(),
            None => None, // fresh store → validate all KB files
        }
    };

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name().to_str().unwrap_or("")))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let slug = match slug_for(&root, path) {
            Some(s) => s,
            None => continue,
        };
        // KB scope only: skip entities/, notes/, inbox/, etc.
        if slug.split('/').next().unwrap_or("") != "kb" {
            continue;
        }
        if let Some(thr) = threshold {
            match mtime_utc(path) {
                Some(m) if m >= thr => {}
                _ => continue,
            }
        }
        validate_one(path, format!("{slug}.md"), &mut report);
    }

    Ok(report)
}

fn validate_one(path: &Path, display: String, report: &mut ValidateReport) {
    report.checked += 1;
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            report.invalid += 1;
            report.issues.push(ValidateIssue {
                path: display,
                message: format!("cannot read file: {e}"),
            });
            return;
        }
    };
    let fm = match frontmatter::parse(&display, &content) {
        Ok((fm, _)) => fm,
        Err(e) => {
            report.invalid += 1;
            report.issues.push(ValidateIssue {
                path: display,
                message: format!("malformed frontmatter: {e}"),
            });
            return;
        }
    };
    match check(&fm) {
        Some(msg) => {
            report.invalid += 1;
            report.issues.push(ValidateIssue {
                path: display,
                message: msg,
            });
        }
        None => report.valid += 1,
    }
}

/// Apply the KB frontmatter rule. Returns an error message, or None if valid.
fn check(fm: &Frontmatter) -> Option<String> {
    let mut missing = Vec::new();
    if fm.title.as_deref().unwrap_or("").trim().is_empty() {
        missing.push("title");
    }
    if fm.description.as_deref().unwrap_or("").trim().is_empty() {
        missing.push("description");
    }
    let type_val = fm.page_type.as_deref().unwrap_or("").trim();
    if type_val.is_empty() {
        missing.push("type");
    }
    if !missing.is_empty() {
        return Some(format!("missing required field(s): {}", missing.join(", ")));
    }
    if !VALID_TYPES.contains(&type_val) {
        return Some(format!(
            "invalid type '{}'; must be one of: {}",
            type_val,
            VALID_TYPES.join(", ")
        ));
    }
    None
}

fn mtime_utc(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(mtime.into())
}

fn parse_rfc3339(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| EvoError::Other(format!("invalid timestamp '{s}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(title: Option<&str>, desc: Option<&str>, ty: Option<&str>) -> Frontmatter {
        Frontmatter {
            title: title.map(String::from),
            description: desc.map(String::from),
            page_type: ty.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn valid_passes() {
        assert!(check(&fm(Some("T"), Some("D"), Some("note"))).is_none());
        assert!(check(&fm(Some("T"), Some("D"), Some("session"))).is_none());
        assert!(check(&fm(Some("T"), Some("D"), Some("group"))).is_none());
    }

    #[test]
    fn missing_fields_reported() {
        let msg = check(&fm(None, None, Some("note"))).unwrap();
        assert!(msg.contains("title") && msg.contains("description"));
    }

    #[test]
    fn empty_values_reported() {
        let msg = check(&fm(Some("  "), Some("D"), Some("note"))).unwrap();
        assert!(msg.contains("title"));
    }

    #[test]
    fn invalid_type_reported() {
        let msg = check(&fm(Some("T"), Some("D"), Some("log"))).unwrap();
        assert!(msg.contains("log") && msg.contains("note"));
    }
}
