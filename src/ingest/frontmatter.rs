use crate::error::{EvoError, Result};
use crate::model::Frontmatter;

/// Split a markdown document into (frontmatter yaml, body). Frontmatter must
/// start on the very first line with `---`; `---` later in the body is never
/// treated as frontmatter. Handles CRLF.
pub fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let rest = match content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))
    {
        Some(r) => r,
        None => return (None, content),
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(end) = rest.find(marker) {
            return (Some(&rest[..end]), &rest[end + marker.len()..]);
        }
    }
    // Frontmatter closed at EOF without trailing newline.
    for marker in ["\n---", "\r\n---"] {
        if let Some(stripped) = rest.strip_suffix(marker) {
            return (Some(stripped), "");
        }
    }
    (None, content)
}

/// Parse a document into ([`Frontmatter`], body). YAML values that are dates
/// (`2026-06-13`) arrive as strings via serde_yaml's lenient string coercion;
/// a missing or empty frontmatter yields defaults.
pub fn parse(path: &str, content: &str) -> Result<(Frontmatter, String)> {
    let (yaml, body) = split_frontmatter(content);
    let fm = match yaml {
        Some(y) if !y.trim().is_empty() => {
            serde_yaml::from_str(y).map_err(|e| EvoError::Frontmatter {
                path: path.to_string(),
                message: e.to_string(),
            })?
        }
        _ => Frontmatter::default(),
    };
    Ok((fm, body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter() {
        let (fm, body) = parse("x.md", "# Hello\n\nWorld").unwrap();
        assert!(fm.title.is_none());
        assert_eq!(body, "# Hello\n\nWorld");
    }

    #[test]
    fn full_frontmatter() {
        let doc = "---\ntitle: Alice Chen\ntype: person\naliases: [Ali, A. Chen]\ntags:\n  - vip\ncreated: 2026-01-05\n---\nBody here.";
        let (fm, body) = parse("x.md", doc).unwrap();
        assert_eq!(fm.title.as_deref(), Some("Alice Chen"));
        assert_eq!(fm.page_type.as_deref(), Some("person"));
        assert_eq!(fm.aliases, vec!["Ali", "A. Chen"]);
        assert_eq!(fm.tags, vec!["vip"]);
        assert_eq!(fm.created.as_deref(), Some("2026-01-05"));
        assert_eq!(body, "Body here.");
    }

    #[test]
    fn crlf_frontmatter() {
        let doc = "---\r\ntitle: T\r\n---\r\nbody";
        let (fm, body) = parse("x.md", doc).unwrap();
        assert_eq!(fm.title.as_deref(), Some("T"));
        assert_eq!(body, "body");
    }

    #[test]
    fn hr_in_body_is_not_frontmatter() {
        let doc = "Intro\n\n---\n\nMore text";
        let (yaml, body) = split_frontmatter(doc);
        assert!(yaml.is_none());
        assert_eq!(body, doc);
    }

    #[test]
    fn bad_yaml_is_an_error_with_path() {
        let doc = "---\ntitle: [unclosed\n---\nbody";
        let err = parse("notes/x.md", doc).unwrap_err();
        assert!(err.to_string().contains("notes/x.md"));
    }

    #[test]
    fn scalar_where_list_expected_is_lenient() {
        let doc = "---\ntitle: T\naliases: Ali\ntags: solo\n---\nbody";
        let (fm, _) = parse("x.md", doc).unwrap();
        assert_eq!(fm.aliases, vec!["Ali"]);
        assert_eq!(fm.tags, vec!["solo"]);
    }

    #[test]
    fn non_string_scalars_coerce_instead_of_failing() {
        let doc = "---\ntitle: 123\ntype: 7\ncreated: 2026\ntags: [1, real-tag]\n---\nbody";
        let (fm, _) = parse("x.md", doc).unwrap();
        assert_eq!(fm.title.as_deref(), Some("123"));
        assert_eq!(fm.page_type.as_deref(), Some("7"));
        assert_eq!(fm.created.as_deref(), Some("2026"));
        assert_eq!(fm.tags, vec!["1", "real-tag"]);
    }

    #[test]
    fn null_and_empty_values_default() {
        let doc = "---\ntitle:\naliases:\ntags: []\n---\nbody";
        let (fm, _) = parse("x.md", doc).unwrap();
        assert!(fm.title.is_none());
        assert!(fm.aliases.is_empty());
        assert!(fm.tags.is_empty());
    }
}
