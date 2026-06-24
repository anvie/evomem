use std::sync::LazyLock;

use regex::Regex;

use crate::model::Intent;

static ENTITY_LEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(who is|who works|who's|where does|what does .{1,40} do|tell me about)\b")
        .unwrap()
});
static TEMPORAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(yesterday|today|tomorrow|tonight|last (week|month|year|night)|this (week|month|year|quarter)|recent(ly)?|\d+ (days?|weeks?|months?) ago|since (19|20)\d\d|in (19|20)\d\d|january|february|march|april|may|june|july|august|september|october|november|december|\d{4}-\d{2}-\d{2})\b",
    )
    .unwrap()
});
static EVENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(meeting|conference|launch(ed)?|demo day|series [a-d]\b|funding round|summit|hackathon|keynote|offsite)\b",
    )
    .unwrap()
});

/// Deterministic intent classification — zero LLM calls. `is_known_entity`
/// lets the caller check TitleCase spans against page titles/aliases without
/// this module touching the database. Misclassification degrades gracefully:
/// the hybrid stack runs regardless.
pub fn classify(query: &str, is_known_entity: impl Fn(&str) -> bool) -> Intent {
    let word_count = query.split_whitespace().count();
    if ENTITY_LEAD.is_match(query) {
        return Intent::Entity;
    }
    if word_count <= 6 {
        for span in titlecase_spans(query) {
            if is_known_entity(&span) {
                return Intent::Entity;
            }
        }
    }
    if TEMPORAL.is_match(query) {
        return Intent::Temporal;
    }
    if EVENT.is_match(query) {
        return Intent::Event;
    }
    Intent::General
}

/// Capitalized word runs ("Acme AI", "Garry Tan") — candidate entity names.
pub fn titlecase_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        let w = word.trim_matches(|c: char| !c.is_alphanumeric());
        if !w.is_empty() && w.chars().next().unwrap().is_uppercase() {
            cur.push(w);
        } else if !cur.is_empty() {
            spans.push(cur.join(" "));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        spans.push(cur.join(" "));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_questions_are_entity() {
        assert_eq!(classify("Who works at Acme?", |_| false), Intent::Entity);
        assert_eq!(classify("who is Alice Chen", |_| false), Intent::Entity);
    }

    #[test]
    fn known_titlecase_span_is_entity() {
        let known = |s: &str| s == "Acme AI";
        assert_eq!(classify("Acme AI funding", known), Intent::Entity);
    }

    #[test]
    fn temporal_queries() {
        assert_eq!(
            classify("what happened last week", |_| false),
            Intent::Temporal
        );
        assert_eq!(
            classify("notes from 2026-05-01", |_| false),
            Intent::Temporal
        );
    }

    #[test]
    fn event_queries() {
        assert_eq!(
            classify("acme series A round details", |_| false),
            Intent::Event
        );
        assert_eq!(
            classify("the conference takeaways", |_| false),
            Intent::Event
        );
    }

    #[test]
    fn general_fallback() {
        assert_eq!(classify("what is retrieval", |_| false), Intent::General);
    }

    #[test]
    fn titlecase_span_extraction() {
        assert_eq!(
            titlecase_spans("met Garry Tan at Acme AI today"),
            vec!["Garry Tan", "Acme AI"]
        );
    }
}
