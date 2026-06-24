use crate::model::ChunkDraft;

/// Target chunk size in characters; sections larger than MAX are re-split on
/// paragraph boundaries, paragraphs are greedily packed up to TARGET.
const TARGET: usize = 1200;
const MAX: usize = 2000;

/// Split a markdown body into chunks along heading sections, tracking the
/// heading path ("Career > YC era"). Oversized sections are re-split on
/// blank-line paragraph boundaries. Fenced code blocks are never split and
/// their `#` lines are not treated as headings.
pub fn chunk(body: &str) -> Vec<ChunkDraft> {
    let mut sections: Vec<(Vec<String>, String)> = Vec::new(); // (heading stack, text)
    let mut stack: Vec<(usize, String)> = Vec::new(); // (level, heading)
    let mut current = String::new();
    let mut in_fence = false;

    let flush = |sections: &mut Vec<(Vec<String>, String)>,
                 stack: &[(usize, String)],
                 current: &mut String| {
        let text = current.trim();
        if !text.is_empty() {
            let path: Vec<String> = stack.iter().map(|(_, h)| h.clone()).collect();
            sections.push((path, text.to_string()));
        }
        current.clear();
    };

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            current.push_str(line);
            current.push('\n');
            continue;
        }
        if !in_fence {
            if let Some(heading) = parse_heading(line) {
                flush(&mut sections, &stack, &mut current);
                while stack.last().is_some_and(|(l, _)| *l >= heading.0) {
                    stack.pop();
                }
                stack.push(heading);
                continue;
            }
        }
        current.push_str(line);
        current.push('\n');
    }
    flush(&mut sections, &stack, &mut current);

    let mut out = Vec::new();
    for (path, text) in sections {
        let heading_path = path.join(" > ");
        if text.len() <= MAX {
            out.push(ChunkDraft { heading_path, text });
        } else {
            for piece in split_paragraph_groups(&text) {
                out.push(ChunkDraft {
                    heading_path: heading_path.clone(),
                    text: piece,
                });
            }
        }
    }
    out
}

fn parse_heading(line: &str) -> Option<(usize, String)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    let title = rest.trim();
    if title.is_empty() {
        None
    } else {
        Some((hashes, title.to_string()))
    }
}

/// Greedily pack blank-line-separated paragraphs into ~TARGET-char pieces.
/// A single paragraph longer than MAX is kept whole (never split mid-fence).
fn split_paragraph_groups(text: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if line.trim().is_empty() && !in_fence {
            if !cur.trim().is_empty() {
                paragraphs.push(cur.trim_end().to_string());
            }
            cur.clear();
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.trim().is_empty() {
        paragraphs.push(cur.trim_end().to_string());
    }

    let mut out = Vec::new();
    let mut piece = String::new();
    for p in paragraphs {
        if !piece.is_empty() && piece.len() + p.len() > TARGET {
            out.push(piece.trim_end().to_string());
            piece = String::new();
        }
        piece.push_str(&p);
        piece.push_str("\n\n");
    }
    if !piece.trim().is_empty() {
        out.push(piece.trim_end().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_headings_with_path() {
        let body = "intro text\n\n# Career\nworked places\n\n## YC era\npartner things\n\n# Hobbies\nclimbing";
        let chunks = chunk(body);
        let paths: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
        assert_eq!(paths, vec!["", "Career", "Career > YC era", "Hobbies"]);
        assert_eq!(chunks[2].text, "partner things");
    }

    #[test]
    fn heading_stack_pops_correctly() {
        let body = "# A\n\n## B\nb text\n\n## C\nc text";
        let chunks = chunk(body);
        let paths: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
        assert_eq!(paths, vec!["A > B", "A > C"]);
    }

    #[test]
    fn code_fences_are_not_headings_and_not_split() {
        let body = "# Code\n```\n# not a heading\nline\n```\nafter";
        let chunks = chunk(body);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("# not a heading"));
        assert!(chunks[0].text.contains("after"));
    }

    #[test]
    fn oversized_section_splits_on_paragraphs() {
        let para = "word ".repeat(150); // ~750 chars
        let body = format!("# Big\n{para}\n\n{para}\n\n{para}\n\n{para}");
        let chunks = chunk(&body);
        assert!(chunks.len() >= 2, "expected split, got {}", chunks.len());
        assert!(chunks.iter().all(|c| c.heading_path == "Big"));
        assert!(chunks.iter().all(|c| c.text.len() <= super::MAX + 800));
    }

    #[test]
    fn empty_body_yields_no_chunks() {
        assert!(chunk("").is_empty());
        assert!(chunk("\n\n  \n").is_empty());
    }

    #[test]
    fn hash_without_space_is_not_heading() {
        let chunks = chunk("#hashtag is not a heading\ntext");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, "");
    }
}
