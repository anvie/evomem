//! `capture` — write a quick thought into the knowledge repo (markdown is the
//! source of truth) and sync just that file so it's immediately searchable.

use chrono::{DateTime, Utc};

use crate::api::{CaptureRequest, CaptureResponse};
use crate::embed::Embedder;
use crate::error::Result;
use crate::ingest;
use crate::store::Store;

pub fn capture(
    store: &Store,
    embedder: &dyn Embedder,
    req: &CaptureRequest,
    now: DateTime<Utc>,
) -> Result<CaptureResponse> {
    let title = sanitize_title(&req.title.clone().unwrap_or_else(|| derive_title(&req.text)));
    let file_slug = slugify(&title);
    let stamp = now.format("%Y-%m-%d-%H%M%S");
    let base_slug = format!("inbox/{stamp}-{file_slug}");

    // Same-second captures with the same derived title must not overwrite
    // each other — probe for a free path with a numeric suffix.
    let (slug, abs_path) = {
        let mut slug = base_slug.clone();
        let mut path = store.brain_root.join(format!("{slug}.md"));
        let mut n = 1;
        while path.exists() && n < 100 {
            n += 1;
            slug = format!("{base_slug}-{n}");
            path = store.brain_root.join(format!("{slug}.md"));
        }
        (slug, path)
    };

    let content = format!(
        "---\ntitle: {title}\ntype: note\ncreated: {created}\ntags: [captured]\n---\n\n{body}\n",
        title = yaml_quote(&title),
        created = now.format("%Y-%m-%dT%H:%M:%SZ"),
        body = req.text.trim()
    );

    if let Some(parent) = abs_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&abs_path, &content)?;

    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    ingest::sync_one(
        store,
        embedder,
        &slug,
        &content,
        &hash,
        &now.to_rfc3339(),
        &abs_path,
    )?;
    store.resolve_dangling_links()?;

    Ok(CaptureResponse {
        slug,
        path: abs_path.display().to_string(),
    })
}

fn derive_title(text: &str) -> String {
    let first_line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Captured note");
    let words: Vec<&str> = first_line.split_whitespace().take(8).collect();
    let mut t = words.join(" ");
    if t.len() > 60 {
        t.truncate(t.char_indices().take_while(|(i, _)| *i < 60).count());
    }
    if t.is_empty() {
        t = "Captured note".to_string();
    }
    t
}

/// Strip control characters (newlines included) so a user-supplied title can
/// never break the generated YAML frontmatter.
fn sanitize_title(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "Captured note".to_string()
    } else {
        collapsed
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "note".to_string()
    } else {
        trimmed
    }
}

/// Always double-quote: unquoted YAML scalars have too many sharp edges
/// (`:`, `#`, leading `[`, `yes/no`, …) to allowlist.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  ---  "), "note");
    }

    #[test]
    fn derive_title_takes_first_words() {
        assert_eq!(
            derive_title("idea: build a rust brain\nmore"),
            "idea: build a rust brain"
        );
    }

    #[test]
    fn sanitize_strips_newlines_and_controls() {
        assert_eq!(sanitize_title("line1\nline2\ttabbed"), "line1 line2 tabbed");
        assert_eq!(sanitize_title("\n\t "), "Captured note");
    }

    #[test]
    fn yaml_quoting_survives_hostile_titles() {
        assert_eq!(yaml_quote("plain"), "\"plain\"");
        assert_eq!(
            yaml_quote("with \"quotes\" and \\slash"),
            "\"with \\\"quotes\\\" and \\\\slash\""
        );
        // Round-trip through the frontmatter parser.
        let doc = format!("---\ntitle: {}\n---\nbody", yaml_quote("a: b #c \"d\""));
        let (fm, _) = crate::ingest::frontmatter::parse("x.md", &doc).unwrap();
        assert_eq!(fm.title.as_deref(), Some("a: b #c \"d\""));
    }
}





