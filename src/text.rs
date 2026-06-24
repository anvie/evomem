//! Shared tokenizer and bounded edit distance, used by the word index,
//! the lexical ranker, and the hash embedder. One tokenizer everywhere —
//! index-time and query-time must agree.

/// Lowercased word tokens: runs of alphanumeric characters (unicode-aware).
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                cur.push(lc);
            }
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Levenshtein distance between `a` and `b`, bounded by `max`.
/// Returns None if the distance exceeds `max` (early exit).
pub fn bounded_levenshtein(a: &str, b: &str, max: u32) -> Option<u32> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) as u32 > max {
        return None;
    }
    if n == 0 {
        return Some(m as u32);
    }
    if m == 0 {
        return Some(n as u32);
    }
    let mut prev: Vec<u32> = (0..=m as u32).collect();
    let mut cur = vec![0u32; m + 1];
    for i in 1..=n {
        cur[0] = i as u32;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    if prev[m] <= max {
        Some(prev[m])
    } else {
        None
    }
}

/// Meilisearch default typo tolerance: words shorter than 5 chars allow no
/// typos, 5..=8 allow one, 9+ allow two.
pub fn allowed_typos(word: &str) -> u32 {
    match word.chars().count() {
        0..=4 => 0,
        5..=8 => 1,
        _ => 2,
    }
}

/// Light deterministic suffix stemmer. Both index words and query words go
/// through the same function, so it only needs to be consistent, not
/// linguistically perfect — it recovers the morphological matching lost when
/// FTS5's porter tokenizer was dropped ("returns" ~ "returned").
pub fn stem(word: &str) -> String {
    const RULES: [(&str, &str); 8] = [
        ("ies", "y"),
        ("sses", "ss"),
        ("ing", ""),
        ("edly", ""),
        ("ed", ""),
        ("ly", ""),
        ("es", ""),
        ("s", ""),
    ];
    for (suffix, replacement) in RULES {
        if let Some(base) = word.strip_suffix(suffix) {
            if base.chars().count() >= 3 {
                return format!("{base}{replacement}");
            }
        }
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_and_splits_on_punctuation() {
        assert_eq!(
            tokenize("Alice works-at Acme's R&D, since 2024!"),
            vec!["alice", "works", "at", "acme", "s", "r", "d", "since", "2024"]
        );
        assert!(tokenize("  \n--- ").is_empty());
    }

    #[test]
    fn levenshtein_bounded() {
        assert_eq!(bounded_levenshtein("batman", "badman", 2), Some(1));
        assert_eq!(bounded_levenshtein("kitten", "sitting", 3), Some(3));
        assert_eq!(bounded_levenshtein("kitten", "sitting", 2), None);
        assert_eq!(bounded_levenshtein("same", "same", 0), Some(0));
        assert_eq!(bounded_levenshtein("", "abc", 3), Some(3));
        assert_eq!(bounded_levenshtein("abcdef", "x", 2), None);
    }

    #[test]
    fn typo_allowance_follows_some_defaults() {
        assert_eq!(allowed_typos("dark"), 0); // 4 chars
        assert_eq!(allowed_typos("night"), 1); // 5 chars
        assert_eq!(allowed_typos("knowledge"), 2); // 9 chars
    }

    #[test]
    fn stemmer_aligns_morphological_variants() {
        assert_eq!(stem("returns"), stem("returned"));
        assert_eq!(stem("invested"), stem("investing"));
        assert_eq!(stem("companies"), "company");
        assert_eq!(stem("was"), "was"); // too short to strip
        assert_ne!(stem("acme"), stem("act"));
    }
}
