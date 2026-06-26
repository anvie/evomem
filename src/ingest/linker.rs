//! The self-wiring knowledge graph: pure regex extraction of entity refs from
//! markdown plus heuristic edge-type inference from the surrounding sentence.
//! Zero LLM calls — this is what keeps the graph fresh at near-zero cost.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{EdgeType, LinkDraft};

static MD_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)\s]+)\)").unwrap());
// Markdown's angle-bracket form allows spaces in the target: [x](<my notes/a.md>)
static MD_LINK_ANGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(<([^>]+)>\)").unwrap());
static WIKILINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").unwrap());
// > **edge_type:** ... [anchor](target) — type is case-insensitive, normalized
// to lowercase.
static TYPED_BLOCKQUOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?mi)^>\s*\*\*([a-z][a-z_]*):?\*\*[^\n]*?\[([^\]]*)\]\(<?([^)>\s]+)>?\)").unwrap()
});
/// Hedge/negation words: when one appears in the sentence *before* a relation
/// trigger, the relation is hypothetical or denied — fall back to `mentions`.
static HEDGES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(not|never|considered|considering|consider|might|may|maybe|plans? to|planning|wants? to|hopes? to|almost|declined|won't|wouldn't|didn't|doesn't|hasn't|isn't|wasn't|denies|denied|rumored)\b",
    )
    .unwrap()
});

/// Ordered edge-type inference: more specific relations take precedence over
/// generic ones when a sentence matches several. `mentions` is the default.
static EDGE_RULES: LazyLock<Vec<(EdgeType, Regex)>> = LazyLock::new(|| {
    vec![
        (EdgeType::Founded, Regex::new(r"(?i)\b(co-?founded|founded|founder of|started the company)\b").unwrap()),
        (EdgeType::InvestedIn, Regex::new(r"(?i)\b(invested in|investor in|backed|led .{0,24}(seed|series [a-d]))\b").unwrap()),
        (EdgeType::WorksAt, Regex::new(r"(?i)\b(works? at|working at|joined|employed (at|by)|(ceo|cto|cfo|vp|head of \w+) (of|at)|runs \w+ at)\b").unwrap()),
        (EdgeType::Advises, Regex::new(r"(?i)\b(advis(es|or|ing)|mentors?)\b").unwrap()),
        (EdgeType::Attended, Regex::new(r"(?i)\b(attended|spoke at|presented at|went to|met\b.{0,60}\b(at|during))\b").unwrap()),
    ]
});

/// Extract typed entity references from a markdown body. `doc_dir` is the
/// slug-directory of the containing doc, used to resolve relative targets.
pub fn extract_links(body: &str, doc_dir: &str) -> Vec<LinkDraft> {
    let mut out: Vec<LinkDraft> = Vec::new();
    let scrubbed = strip_code_blocks(body);

    // Typed blockquotes are explicit user statements: several relations to the
    // same target are allowed (dedup by (slug, type)), and any blockquoted
    // slug suppresses the inferred extractors below (explicit beats guessed).
    let mut explicit: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for cap in TYPED_BLOCKQUOTE.captures_iter(&scrubbed) {
        let (raw_type, anchor, target) = (&cap[1], &cap[2], &cap[3]);
        // Blockquote targets use markdown-link syntax (explicit/relative paths).
        if let Some(slug) = normalize_slug(doc_dir, target, true) {
            let edge = EdgeType::parse(&raw_type.to_lowercase());
            if explicit.insert((slug.clone(), edge.as_str().to_string())) {
                out.push(LinkDraft {
                    dst_slug: slug,
                    edge_type: edge,
                    anchor_text: anchor.to_string(),
                });
            }
        }
    }
    // Inferred edges: dedup by destination slug, first writer wins.
    let mut seen: std::collections::HashSet<String> =
        explicit.into_iter().map(|(slug, _)| slug).collect();

    let inferred = |anchor: &str,
                    target: &str,
                    at: usize,
                    prefix_bare: bool,
                    out: &mut Vec<LinkDraft>,
                    seen: &mut std::collections::HashSet<String>| {
        if let Some(slug) = normalize_slug(doc_dir, target, prefix_bare) {
            let edge = infer_edge_type(context_around(&scrubbed, at));
            if seen.insert(slug.clone()) {
                out.push(LinkDraft {
                    dst_slug: slug,
                    edge_type: edge,
                    anchor_text: anchor.to_string(),
                });
            }
        }
    };

    // Markdown links are real paths: a bare `[x](note)` is doc-dir-relative.
    for cap in MD_LINK_ANGLE.captures_iter(&scrubbed) {
        let start = cap.get(0).unwrap().start();
        if start > 0 && scrubbed.as_bytes()[start - 1] == b'!' {
            continue; // image
        }
        inferred(&cap[1], &cap[2], start, true, &mut out, &mut seen);
    }

    for cap in MD_LINK.captures_iter(&scrubbed) {
        let start = cap.get(0).unwrap().start();
        if start > 0 && scrubbed.as_bytes()[start - 1] == b'!' {
            continue; // image
        }
        inferred(&cap[1], &cap[2], start, true, &mut out, &mut seen);
    }

    // Wiki-links are Obsidian-style name references: a bare `[[Jakarta]]` is a
    // GLOBAL name (resolved by title/alias/basename anywhere), never doc-dir-
    // prefixed. Multi-segment `[[a/b]]` stays root-relative.
    for cap in WIKILINK.captures_iter(&scrubbed) {
        let target = cap[1].trim();
        let anchor = cap.get(2).map(|m| m.as_str()).unwrap_or(target);
        let start = cap.get(0).unwrap().start();
        inferred(anchor, target, start, false, &mut out, &mut seen);
    }

    out
}

/// Infer the edge type from the sentence containing the link. A rule only
/// fires if its trigger is not preceded by a hedge/negation word *in the same
/// clause* ("never works at", "has not invested in") — hedged relations fall
/// through to weaker rules and ultimately to `mentions`. The lookback is
/// clause-scoped so a hedge in an earlier clause ("considered founding it,
/// then invested in X") doesn't block a genuine later relation.
pub fn infer_edge_type(context: &str) -> EdgeType {
    for (edge, re) in EDGE_RULES.iter() {
        if let Some(m) = re.find(context) {
            let clause_start = context[..m.start()]
                .rfind([',', ';', ':', '.'])
                .map(|i| i + 1)
                .unwrap_or(0);
            if HEDGES.is_match(&context[clause_start..m.start()]) {
                continue;
            }
            return edge.clone();
        }
    }
    EdgeType::Mentions
}

/// The containing sentence (split on .!?\n), capped at ±150 chars around `at`.
fn context_around(text: &str, at: usize) -> &str {
    let bytes = text.as_bytes();
    let is_boundary = |b: u8| matches!(b, b'.' | b'!' | b'?' | b'\n');
    let mut start = at;
    while start > 0 && !is_boundary(bytes[start - 1]) && at - start < 150 {
        start -= 1;
    }
    let mut end = at;
    while end < bytes.len() && !is_boundary(bytes[end]) && end - at < 150 {
        end += 1;
    }
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    while !text.is_char_boundary(end) {
        end += 1;
    }
    &text[start..end]
}

/// Normalize a link target to a knowledge slug: skip external URLs and anchors,
/// strip `.md` and `#fragment`, resolve `./`/`../` against the doc dir,
/// normalize `\` to `/`. Returns None for targets that aren't knowledge docs.
///
/// `prefix_bare` controls how a single-segment bare name (no `/`) is treated:
/// `true` (markdown links) resolves it against `doc_dir`; `false` (Obsidian
/// wiki-links) keeps it as a bare global name for title/alias/basename lookup.
/// Explicitly relative targets (`./`, `../`) are always doc-dir-relative.
pub fn normalize_slug(doc_dir: &str, target: &str, prefix_bare: bool) -> Option<String> {
    let mut t = target.trim();
    // Angle-bracket targets: [x](<my notes/a.md>)
    if let Some(inner) = t.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        t = inner.trim();
    }
    let decoded = percent_decode(t);
    let t = decoded.trim();
    if t.is_empty() || t.starts_with('#') || t.contains("://") || t.starts_with("mailto:") {
        return None;
    }
    let t = t.replace('\\', "/");
    let t = t.split('#').next().unwrap_or("");
    let t = t.strip_suffix(".md").unwrap_or(t);
    if t.is_empty() {
        return None;
    }

    let mut parts: Vec<&str> = Vec::new();
    let absolute = t.starts_with('/');
    // `./x` and `../x` always resolve against the doc's directory. A bare name
    // (no `/`) is doc-dir-relative only when `prefix_bare` is set; otherwise it
    // stays a global name. Multi-segment targets are knowledge-root-relative.
    let bare = !t.contains('/');
    if !absolute && (t.starts_with("./") || t.starts_with("../") || (bare && prefix_bare)) {
        parts.extend(doc_dir.split('/').filter(|s| !s.is_empty()));
    }
    for seg in t.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Decode %XX escapes ([x](my%20notes/a.md)); invalid sequences pass through.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replace fenced code blocks and inline code with spaces (preserving byte
/// offsets) so links inside code are never extracted.
fn strip_code_blocks(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            out.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
            continue;
        }
        if in_fence {
            out.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
        } else {
            // Blank inline code spans.
            let mut in_tick = false;
            for c in line.chars() {
                if c == '`' {
                    in_tick = !in_tick;
                    out.push(' ');
                } else if in_tick && c != '\n' {
                    out.push(' ');
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_markdown_links_with_edge_inference() {
        let body = "Alice works at [Acme Corp](companies/acme). She founded [Beta](companies/beta) in 2020.";
        let links = extract_links(body, "people");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].dst_slug, "companies/acme");
        assert_eq!(links[0].edge_type, EdgeType::WorksAt);
        assert_eq!(links[1].dst_slug, "companies/beta");
        assert_eq!(links[1].edge_type, EdgeType::Founded);
    }

    #[test]
    fn extracts_wikilinks() {
        let body = "Met [[people/bob|Bob]] at the conference. He attended [[events/demo-day]].";
        let links = extract_links(body, "daily");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].dst_slug, "people/bob");
        assert_eq!(links[0].edge_type, EdgeType::Attended); // "met at"
        assert_eq!(links[1].dst_slug, "events/demo-day");
        assert_eq!(links[1].edge_type, EdgeType::Attended);
    }

    #[test]
    fn typed_blockquote_wins_with_custom_type() {
        let body = "> **acquired:** see [Acme](companies/acme) for details.";
        let links = extract_links(body, "");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].edge_type, EdgeType::Custom("acquired".into()));
    }

    #[test]
    fn edge_precedence_specific_over_generic() {
        // Sentence matches both works_at ("joined") and founded — founded wins.
        assert_eq!(
            infer_edge_type("she founded the startup and joined the board"),
            EdgeType::Founded
        );
        assert_eq!(
            infer_edge_type("he invested in the round and advises them"),
            EdgeType::InvestedIn
        );
        assert_eq!(infer_edge_type("just some text"), EdgeType::Mentions);
    }

    #[test]
    fn external_urls_anchors_and_images_are_skipped() {
        let body = "See [site](https://example.com), [sec](#heading), ![img](pics/x.png), [ok](notes/real).";
        let links = extract_links(body, "");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].dst_slug, "notes/real");
    }

    #[test]
    fn code_blocks_are_ignored() {
        let body = "```\n[fake](inside/code)\n```\nand `[inline](in/code)` but [real](notes/real)";
        let links = extract_links(body, "");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].dst_slug, "notes/real");
    }

    #[test]
    fn slug_normalization() {
        assert_eq!(
            normalize_slug("people", "../companies/acme.md", true),
            Some("companies/acme".into())
        );
        assert_eq!(normalize_slug("a/b", "./c.md", true), Some("a/b/c".into()));
        assert_eq!(
            normalize_slug("", "wiki/people/garry-tan", true),
            Some("wiki/people/garry-tan".into())
        );
        assert_eq!(normalize_slug("x", "/abs/path.md", true), Some("abs/path".into()));
        // With prefix_bare, a bare name is doc-dir-relative (markdown links).
        assert_eq!(normalize_slug("x", "doc.md#section", true), Some("x/doc".into()));
        assert_eq!(normalize_slug("x", "https://a.b/c", true), None);
        assert_eq!(normalize_slug("x", "#anchor", true), None);
        assert_eq!(normalize_slug("", "../escapes", true), None);
    }

    #[test]
    fn bare_wikilink_target_is_global_not_dir_prefixed() {
        // prefix_bare=false: a bare name stays global (no doc_dir prefix).
        assert_eq!(normalize_slug("xyz", "Jakarta", false), Some("Jakarta".into()));
        assert_eq!(normalize_slug("a/b/c", "Bob", false), Some("Bob".into()));
        // Multi-segment wiki-links remain root-relative regardless of prefix_bare.
        assert_eq!(normalize_slug("xyz", "people/bob", false), Some("people/bob".into()));
        // Explicitly relative wiki-links still resolve against doc_dir.
        assert_eq!(normalize_slug("xyz", "./sibling", false), Some("xyz/sibling".into()));
    }

    #[test]
    fn bare_wikilink_in_body_emits_global_slug() {
        let body = "User jalan ke [[Jakarta]] makan di [[Ayam Bakar Taliwang]].";
        let links = extract_links(body, "trips/jakarta-2026");
        let slugs: Vec<&str> = links.iter().map(|l| l.dst_slug.as_str()).collect();
        assert!(slugs.contains(&"Jakarta"), "{slugs:?}");
        assert!(slugs.contains(&"Ayam Bakar Taliwang"), "{slugs:?}");
    }

    #[test]
    fn duplicate_links_dedupe() {
        let body = "[A](x/a) and again [A](x/a).";
        assert_eq!(extract_links(body, "").len(), 1);
    }

    #[test]
    fn hedged_relations_downgrade_to_mentions() {
        assert_eq!(
            infer_edge_type("Bob has not invested in the startup"),
            EdgeType::Mentions
        );
        assert_eq!(
            infer_edge_type("Alice never works at the firm"),
            EdgeType::Mentions
        );
        assert_eq!(
            infer_edge_type("she might be advising them"),
            EdgeType::Mentions
        );
        // The relation itself still fires when unhedged…
        assert_eq!(
            infer_edge_type("Bob invested in the startup"),
            EdgeType::InvestedIn
        );
        // …and a hedge on one rule lets a later, unhedged rule win.
        assert_eq!(
            infer_edge_type("considered founding the firm, then invested in it"),
            EdgeType::InvestedIn
        );
        // Hedge AFTER the trigger does not block it.
        assert_eq!(
            infer_edge_type("invested in the firm, though he may regret it"),
            EdgeType::InvestedIn
        );
    }

    #[test]
    fn uppercase_typed_blockquote_normalizes() {
        let body = "> **Works_At:** see [Acme](companies/acme).";
        let links = extract_links(body, "");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].edge_type, EdgeType::WorksAt);
    }

    #[test]
    fn multiple_explicit_relations_to_same_target() {
        let body = "> **founded:** see [Beta](companies/beta).\n> **advises:** see [Beta](companies/beta).\nShe founded [Beta](companies/beta) long ago.";
        let links = extract_links(body, "");
        // Both explicit edges survive; the inferred one is suppressed.
        assert_eq!(links.len(), 2);
        let types: Vec<&str> = links.iter().map(|l| l.edge_type.as_str()).collect();
        assert!(types.contains(&"founded") && types.contains(&"advises"));
    }

    #[test]
    fn targets_with_spaces_and_percent_encoding() {
        let body = "See [A](<my notes/alpha.md>) and [B](my%20notes/beta.md).";
        let links = extract_links(body, "");
        let slugs: Vec<&str> = links.iter().map(|l| l.dst_slug.as_str()).collect();
        assert!(slugs.contains(&"my notes/alpha"), "{slugs:?}");
        assert!(slugs.contains(&"my notes/beta"), "{slugs:?}");
    }

    #[test]
    fn percent_decode_handles_invalid_sequences() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
    }
}
