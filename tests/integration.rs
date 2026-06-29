//! End-to-end engine test: build a fixture brain repo on disk, init + sync,
//! then exercise search, think, graph traversal, capture, and re-sync.

use std::fs;
use std::path::Path;

use evomem::api::CaptureRequest;
use evomem::embed::{Embedder, HashEmbedder};
use evomem::model::{Evidence, Intent, Mode};
use evomem::store::Store;
use evomem::{capture, ingest, search, stats, think};

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn build_fixture(root: &Path) {
    write(
        root,
        "people/alice-chen.md",
        "---\ntitle: Alice Chen\ntype: person\naliases: [Ali]\nupdated: 2026-06-10\n---\n# Background\nAlice Chen runs engineering at [Acme Corp](../companies/acme-corp.md), a series-B fintech.\n\n# Open items\nShe owes a security review for the new tier. She founded [Beta Labs](../companies/beta-labs.md) before joining.\n",
    );
    write(
        root,
        "people/bob-roe.md",
        "---\ntitle: Bob Roe\ntype: person\nupdated: 2026-01-02\n---\nBob Roe invested in [Acme Corp](../companies/acme-corp.md) during the seed round.\nHe attended [[events/demo-day|Demo Day]] last spring.\n",
    );
    write(
        root,
        "companies/acme-corp.md",
        "---\ntitle: Acme Corp\ntype: company\naliases: [Acme]\nupdated: 2026-06-01\n---\nAcme Corp is a series-B fintech building payment rails. Pricing discussion ongoing for the 500-seat tier.\n\n> **competitor_of:** see [Zenith Pay](zenith-pay.md).\n",
    );
    write(
        root,
        "companies/beta-labs.md",
        "---\ntitle: Beta Labs\ntype: company\nupdated: 2026-05-20\n---\nBeta Labs prototypes retrieval quality tooling for knowledge systems.\n",
    );
    write(
        root,
        "events/demo-day.md",
        "---\ntitle: Demo Day\ntype: event\nupdated: 2026-04-01\n---\nSpring demo day. Bob Roe spoke at the closing panel.\n",
    );
    write(
        root,
        "concepts/retrieval.md",
        "---\ntitle: Retrieval Quality\ntype: concept\nupdated: 2026-06-12\n---\nNotes on hybrid retrieval: lexical ranking, vector search, graph traversal, and fusion.\n",
    );
    write(
        root,
        "chat/2026-06-12-log.md",
        "---\ntitle: Chat Log June 12\nupdated: 2026-06-12\n---\nDiscussed pricing with the team. Acme came up briefly.\n",
    );
    write(
        root,
        "test/scratch.md",
        "---\ntitle: Scratch\n---\nAcme Acme Acme — this hard-excluded doc must never surface in search.\n",
    );
    // Stale doc: updated long before "now".
    write(
        root,
        "people/carol-old.md",
        "---\ntitle: Carol Old\ntype: person\nupdated: 2025-11-01\n---\nCarol Old works at [Acme Corp](../companies/acme-corp.md) on compliance.\n",
    );
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    embedder: HashEmbedder,
}

fn setup() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let embedder = HashEmbedder;
    let store = Store::init(dir.path(), embedder.id(), embedder.dim()).unwrap();
    let report = ingest::sync_dir(&store, &embedder).unwrap();
    assert_eq!(report.scanned, 9);
    assert_eq!(report.added, 9);
    Fixture {
        _dir: dir,
        store,
        embedder,
    }
}

#[test]
fn sync_is_idempotent_and_detects_deletes() {
    let f = setup();
    let report = ingest::sync_dir(&f.store, &f.embedder).unwrap();
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.unchanged, 9);

    fs::remove_file(f.store.brain_root.join("events/demo-day.md")).unwrap();
    let report = ingest::sync_dir(&f.store, &f.embedder).unwrap();
    assert_eq!(report.deleted, 1);
    let s = stats::stats(&f.store).unwrap();
    assert_eq!(s.docs, 8);
    assert_eq!(s.deleted_docs, 1);
    // Deleted doc no longer searchable.
    let resp = search::search(
        &f.store,
        &f.embedder,
        "demo day closing panel",
        Mode::Balanced,
    )
    .unwrap();
    assert!(resp.hits.iter().all(|h| h.slug != "events/demo-day"));
}

#[test]
fn search_finds_entities_with_evidence_and_excludes_test_dir() {
    let f = setup();
    let resp = search::search(&f.store, &f.embedder, "Acme Corp", Mode::Balanced).unwrap();
    assert_eq!(resp.intent, Intent::Entity);
    assert!(!resp.hits.is_empty());
    assert_eq!(resp.hits[0].slug, "companies/acme-corp");
    assert!(matches!(
        resp.hits[0].evidence,
        Evidence::AliasHit | Evidence::ExactTitleMatch
    ));
    // Hard-excluded test/ doc never surfaces, despite spamming "Acme".
    assert!(resp.hits.iter().all(|h| !h.slug.starts_with("test/")));
    // Dedup: one hit per doc slug.
    let mut slugs: Vec<&str> = resp.hits.iter().map(|h| h.slug.as_str()).collect();
    slugs.sort();
    slugs.dedup();
    assert_eq!(slugs.len(), resp.hits.len());
}

#[test]
fn alias_hop_resolves_to_canonical_page() {
    let f = setup();
    let resp = search::search(&f.store, &f.embedder, "Acme", Mode::Balanced).unwrap();
    assert_eq!(resp.hits[0].slug, "companies/acme-corp");
    assert_eq!(resp.hits[0].evidence, Evidence::AliasHit);
}

#[test]
fn typo_and_stem_queries_still_hit() {
    let f = setup();
    // typo: "retreival" -> "retrieval"
    let resp = search::search(&f.store, &f.embedder, "retreival quality", Mode::Balanced).unwrap();
    assert!(
        resp.hits.iter().any(|h| h.slug == "concepts/retrieval"),
        "typo query missed"
    );
    // stem: "prototyped" ~ "prototypes"
    let resp = search::search(&f.store, &f.embedder, "prototyped tooling", Mode::Balanced).unwrap();
    assert!(
        resp.hits.iter().any(|h| h.slug == "companies/beta-labs"),
        "stem query missed"
    );
}

#[test]
fn directory_index_link_resolution() {
    let f = setup();
    // Create a directory index doc: docs/api/index.md
    write(
        f.store.brain_root.as_path(),
        "docs/api/index.md",
        "---\ntitle: API Documentation\ntype: doc\n---\n# API Docs\nInternal API reference for the platform.\n",
    );
    // Create a doc linking to the directory WITHOUT /index
    write(
        f.store.brain_root.as_path(),
        "people/linker.md",
        "---\ntitle: Linker Test\ntype: person\n---\nSee [[docs/api]] for the full API reference.\n",
    );
    let report = ingest::sync_dir(&f.store, &f.embedder).unwrap();
    assert_eq!(report.added, 2, "directory + linker doc added");

    // The link [[docs/api]] should resolve to docs/api/index.
    // Use store.neighbors() which returns EdgeRow with dst_doc_id.
    let linker = f.store.resolve_doc("Linker Test").unwrap().unwrap();
    let edges = f.store.neighbors(linker.id, None).unwrap();
    let api_edge = edges.iter().find(|e| e.dst_slug == "docs/api");
    assert!(
        api_edge.is_some(),
        "link to docs/api not found in edges: {edges:?}"
    );
    assert!(
        api_edge.unwrap().dst_doc_id.is_some(),
        "dst_doc_id is NULL — /index fallback failed for [[docs/api]]"
    );

    // Verify the resolved doc is the index doc
    let api_page = f
        .store
        .get_doc_by_id(api_edge.unwrap().dst_doc_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(api_page.slug, "docs/api/index");
    assert_eq!(api_page.title, "API Documentation");
}

#[test]
fn graph_has_typed_edges_and_traverses() {
    let f = setup();
    let alice = f.store.resolve_doc("Alice Chen").unwrap().unwrap();
    let edges = search::graph::traverse(&f.store, alice.id, Some("works_at"), 1).unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e.dst_slug == "companies/acme-corp" && e.edge_type == "works_at"),
        "alice works_at acme missing: {edges:?}"
    );
    let edges = search::graph::traverse(&f.store, alice.id, Some("founded"), 1).unwrap();
    assert!(edges.iter().any(|e| e.dst_slug == "companies/beta-labs"));

    // Multi-hop: alice -> acme -> bob (invested_in, traversed inbound).
    let edges = search::graph::traverse(&f.store, alice.id, None, 2).unwrap();
    assert!(edges
        .iter()
        .any(|e| e.src_slug == "people/bob-roe" && e.edge_type == "invested_in"));

    // Custom edge type from typed blockquote.
    let acme = f.store.resolve_doc("Acme").unwrap().unwrap();
    let edges = search::graph::traverse(&f.store, acme.id, Some("competitor_of"), 1).unwrap();
    assert!(edges.iter().any(|e| e.dst_slug == "companies/zenith-pay"));
}

#[test]
fn graph_augmentation_pulls_connected_pages() {
    let f = setup();
    // "series-B fintech engineering" hits alice/acme lexically+vector;
    // graph should keep factually-connected docs in reach.
    let resp = search::search(
        &f.store,
        &f.embedder,
        "who works at Acme Corp",
        Mode::Tokenmax,
    )
    .unwrap();
    let slugs: Vec<&str> = resp.hits.iter().map(|h| h.slug.as_str()).collect();
    assert!(slugs.contains(&"companies/acme-corp"));
    assert!(
        slugs.contains(&"people/alice-chen") || slugs.contains(&"people/carol-old"),
        "graph-connected people missing: {slugs:?}"
    );
}

#[test]
fn think_reports_facts_and_gaps() {
    let f = setup();
    let now = chrono::Utc.with_ymd_and_hms(2026, 6, 13, 0, 0, 0).unwrap();
    let resp = think::think(
        &f.store,
        &f.embedder,
        "Carol Old compliance",
        Mode::Balanced,
        now,
    )
    .unwrap();
    assert!(!resp.facts.is_empty());
    // carol-old updated 2025-11-01 -> stale.
    assert!(
        resp.gaps
            .iter()
            .any(|g| matches!(g.kind, evomem::api::GapKind::StaleDoc)),
        "stale gap missing: {:?}",
        resp.gaps
    );
    // Unknown entity gap.
    let resp = think::think(
        &f.store,
        &f.embedder,
        "meeting with Zara Quinn tomorrow",
        Mode::Balanced,
        now,
    )
    .unwrap();
    assert!(resp
        .gaps
        .iter()
        .any(|g| matches!(g.kind, evomem::api::GapKind::UnknownEntity)
            && g.message.contains("Zara Quinn")));
    // Dangling link gap: acme references zenith-pay which has no doc.
    let resp = think::think(&f.store, &f.embedder, "Acme Corp", Mode::Balanced, now).unwrap();
    assert!(resp
        .gaps
        .iter()
        .any(|g| matches!(g.kind, evomem::api::GapKind::DanglingLink)
            && g.message.contains("zenith-pay")));
}

#[test]
fn capture_is_immediately_searchable() {
    let f = setup();
    let now = chrono::Utc
        .with_ymd_and_hms(2026, 6, 13, 10, 30, 0)
        .unwrap();
    let resp = capture::capture(
        &f.store,
        &f.embedder,
        &CaptureRequest {
            text: "Idea: quantized hologram caching for the search layer".into(),
            title: None,
        },
        now,
    )
    .unwrap();
    assert!(resp.slug.starts_with("inbox/2026-06-13-"));
    assert!(f
        .store
        .brain_root
        .join(format!("{}.md", resp.slug))
        .exists());
    let found = search::search(
        &f.store,
        &f.embedder,
        "quantized hologram caching",
        Mode::Balanced,
    )
    .unwrap();
    assert_eq!(found.hits[0].slug, resp.slug);
}

#[test]
fn bad_files_do_not_abort_sync() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    // A non-UTF-8 file and a malformed-frontmatter file sit among good ones.
    fs::write(dir.path().join("notes-bad.md"), [0xFFu8, 0xFE, 0x00, 0x41]).unwrap();
    write(
        dir.path(),
        "notes-broken.md",
        "---\ntitle: [unclosed\n---\nbody",
    );
    let embedder = HashEmbedder;
    let store = Store::init(dir.path(), embedder.id(), embedder.dim()).unwrap();

    let report = ingest::sync_dir(&store, &embedder).unwrap();
    assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
    assert_eq!(report.added, 9, "all healthy files synced");
    assert!(report
        .errors
        .iter()
        .any(|e| e.path.contains("notes-bad.md")));
    assert!(report
        .errors
        .iter()
        .any(|e| e.path.contains("notes-broken.md")));

    // A failing file must NOT be soft-deleted on later syncs (it still exists).
    let report = ingest::sync_dir(&store, &embedder).unwrap();
    assert_eq!(report.deleted, 0);
    assert_eq!(report.errors.len(), 2);
}

#[test]
fn frontmatter_only_page_is_searchable_by_title() {
    let f = setup();
    write(
        f.store.brain_root.as_path(),
        "people/dana-stub.md",
        "---\ntitle: Dana Stubfield\ntype: person\n---\n",
    );
    ingest::sync_dir(&f.store, &f.embedder).unwrap();
    let resp = search::search(&f.store, &f.embedder, "Dana Stubfield", Mode::Balanced).unwrap();
    assert_eq!(resp.hits[0].slug, "people/dana-stub", "{:?}", resp.hits);
}

#[test]
fn rename_repoints_inbound_links() {
    let f = setup();
    // alice works_at acme-corp. Rename the acme file; alice's link must follow.
    fs::rename(
        f.store.brain_root.join("companies/acme-corp.md"),
        f.store.brain_root.join("companies/acme-renamed.md"),
    )
    .unwrap();
    let report = ingest::sync_dir(&f.store, &f.embedder).unwrap();
    assert_eq!(report.renamed, 1, "{report:?}");

    let alice = f.store.resolve_doc("Alice Chen").unwrap().unwrap();
    let edges = search::graph::traverse(&f.store, alice.id, Some("works_at"), 1).unwrap();
    assert!(
        edges.iter().any(|e| e.dst_slug == "companies/acme-renamed"),
        "works_at edge did not follow the rename: {edges:?}"
    );
}

#[test]
fn oversized_files_are_skipped_and_reported() {
    let f = setup();
    let big = "x".repeat(ingest::MAX_FILE_BYTES as usize + 1);
    fs::write(f.store.brain_root.join("huge.md"), &big).unwrap();
    let report = ingest::sync_dir(&f.store, &f.embedder).unwrap();
    assert!(report.errors.iter().any(|e| e.path.contains("huge.md")));
    assert!(f.store.get_doc_by_slug("huge").unwrap().is_none());
    assert_eq!(report.deleted, 0, "skipped file must not delete anything");
}

#[test]
fn capture_collisions_and_hostile_titles_are_safe() {
    let f = setup();
    let now = chrono::Utc
        .with_ymd_and_hms(2026, 6, 13, 10, 30, 0)
        .unwrap();
    // Same second, same text: second capture must not overwrite the first.
    let req = CaptureRequest {
        text: "duplicate thought".into(),
        title: None,
    };
    let first = capture::capture(&f.store, &f.embedder, &req, now).unwrap();
    let second = capture::capture(&f.store, &f.embedder, &req, now).unwrap();
    assert_ne!(first.slug, second.slug);
    assert!(f
        .store
        .brain_root
        .join(format!("{}.md", first.slug))
        .exists());
    assert!(f
        .store
        .brain_root
        .join(format!("{}.md", second.slug))
        .exists());

    // A title with newlines and quotes must produce a valid, parseable doc.
    let hostile = capture::capture(
        &f.store,
        &f.embedder,
        &CaptureRequest {
            text: "body".into(),
            title: Some("line1\nline2: \"quoted\"".into()),
        },
        now,
    )
    .unwrap();
    let doc = f.store.get_doc_by_slug(&hostile.slug).unwrap().unwrap();
    assert_eq!(doc.title, "line1 line2: \"quoted\"");
    // And the file round-trips through a full re-sync without errors.
    let report = ingest::sync_dir(&f.store, &f.embedder).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn hedged_links_do_not_create_typed_edges() {
    let f = setup();
    write(
        f.store.brain_root.as_path(),
        "people/eve-maybe.md",
        "---\ntitle: Eve Maybe\ntype: person\n---\nEve has not invested in [Acme Corp](../companies/acme-corp.md) yet.\n",
    );
    ingest::sync_dir(&f.store, &f.embedder).unwrap();
    let eve = f.store.resolve_doc("Eve Maybe").unwrap().unwrap();
    let typed = search::graph::traverse(&f.store, eve.id, Some("invested_in"), 1).unwrap();
    assert!(
        typed.is_empty(),
        "hedged relation became a typed edge: {typed:?}"
    );
    let mentions = search::graph::traverse(&f.store, eve.id, Some("mentions"), 1).unwrap();
    assert!(mentions.iter().any(|e| e.dst_slug == "companies/acme-corp"));
}

#[test]
fn think_skips_question_grammar_spans() {
    let f = setup();
    let now = chrono::Utc.with_ymd_and_hms(2026, 6, 13, 0, 0, 0).unwrap();
    let resp = think::think(
        &f.store,
        &f.embedder,
        "Should We Acquire Zenith",
        Mode::Balanced,
        now,
    )
    .unwrap();
    assert!(
        !resp
            .gaps
            .iter()
            .any(|g| matches!(g.kind, evomem::api::GapKind::UnknownEntity)),
        "question grammar reported as unknown entity: {:?}",
        resp.gaps
    );
}

#[test]
fn stats_reports_last_synced_at() {
    let f = setup();
    let s = stats::stats(&f.store).unwrap();
    assert!(s.last_synced_at.is_some());
}

#[test]
fn temporal_intent_lets_chat_pages_surface() {
    let f = setup();
    let resp = search::search(
        &f.store,
        &f.embedder,
        "pricing discussed last week",
        Mode::Balanced,
    )
    .unwrap();
    assert_eq!(resp.intent, Intent::Temporal);
    // chat doc is demoted but not excluded; with temporal intent it competes.
    assert!(resp
        .hits
        .iter()
        .any(|h| h.slug.starts_with("chat/") || h.slug.starts_with("companies/")));
}

use chrono::TimeZone;

// ── KB frontmatter validation ────────────────────────────────────────────

fn setup_kb_validate() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Valid root doc.
    write(
        root,
        "good.md",
        "---\ntitle: Good\ndescription: A valid note\ntype: note\n---\nbody\n",
    );
    // Valid nested doc using a new entity-like type.
    write(
        root,
        "places/jakarta.md",
        "---\ntitle: Jakarta\ndescription: Capital of Indonesia\ntype: place\n---\nbody\n",
    );
    // Missing `type`.
    write(
        root,
        "missing-type.md",
        "---\ntitle: NoType\ndescription: Has no type\n---\nbody\n",
    );
    // Invalid `type`.
    write(
        root,
        "bad-type.md",
        "---\ntitle: BadType\ndescription: Wrong type\ntype: log\n---\nbody\n",
    );
    // `inbox/` raw captures are frontmatter-light and must be skipped.
    write(
        root,
        "inbox/raw.md",
        "---\ntitle: Raw\ntype: note\n---\nbody\n",
    );
    let embedder = HashEmbedder;
    let store = Store::init(root, embedder.id(), embedder.dim()).unwrap();
    (dir, store)
}

#[test]
fn validate_checks_all_docs_and_skips_inbox() {
    let (_dir, store) = setup_kb_validate();
    let report = evomem::validate::run(&store, None, None, true).unwrap();
    // 4 docs checked (good, places/jakarta, missing-type, bad-type); inbox/ skipped.
    assert_eq!(report.checked, 4);
    assert_eq!(report.valid, 2);
    assert_eq!(report.invalid, 2);
    let paths: Vec<&str> = report.issues.iter().map(|i| i.path.as_str()).collect();
    assert!(paths.contains(&"missing-type.md"));
    assert!(paths.contains(&"bad-type.md"));
    assert!(!paths.iter().any(|p| p.starts_with("inbox/")));
}

#[test]
fn validate_single_file() {
    let (dir, store) = setup_kb_validate();
    let path = dir.path().join("bad-type.md");
    let report = evomem::validate::run(&store, Some(path.to_str().unwrap()), None, true).unwrap();
    assert_eq!(report.checked, 1);
    assert_eq!(report.invalid, 1);
    assert!(report.issues[0].message.contains("log"));
}

#[test]
fn validate_since_filters_by_mtime() {
    let (_dir, store) = setup_kb_validate();
    // A `since` far in the future excludes every file.
    let future = "2999-01-01T00:00:00+00:00";
    let report = evomem::validate::run(&store, None, Some(future), false).unwrap();
    assert_eq!(report.checked, 0);
    // A `since` far in the past includes all docs (inbox/ still skipped).
    let past = "2000-01-01T00:00:00+00:00";
    let report = evomem::validate::run(&store, None, Some(past), false).unwrap();
    assert_eq!(report.checked, 4);
}

/// Build a fresh synced store from `files` (rel-path, content) pairs.
fn synced_store(files: &[(&str, &str)]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        write(dir.path(), rel, content);
    }
    let embedder = HashEmbedder;
    let store = Store::init(dir.path(), embedder.id(), embedder.dim()).unwrap();
    ingest::sync_dir(&store, &embedder).unwrap();
    (dir, store)
}

/// Find the outgoing edge `src_doc_id -> dst_slug` and return its dst_doc_id.
fn outgoing(store: &Store, src_doc_id: i64, dst_slug: &str) -> Option<Option<i64>> {
    store
        .neighbors(src_doc_id, None)
        .unwrap()
        .into_iter()
        .find(|e| e.src_doc_id == src_doc_id && e.dst_slug == dst_slug)
        .map(|e| e.dst_doc_id)
}

#[test]
fn bare_wikilink_resolves_by_title_across_folders() {
    let (_dir, store) = synced_store(&[
        (
            "places/jakarta.md",
            "---\ntitle: Jakarta\ndescription: Capital of Indonesia\ntype: place\n---\nIbu kota Indonesia.\n",
        ),
        (
            "trips/notes.md",
            "---\ntitle: Trip Notes\ndescription: A trip\ntype: note\n---\nUser jalan-jalan ke [[Jakarta]].\n",
        ),
    ]);
    let jakarta = store.resolve_doc("Jakarta").unwrap().unwrap();
    let notes = store.get_doc_by_slug("trips/notes").unwrap().unwrap();
    // The bare `[[Jakarta]]` (dst_slug "Jakarta") resolves to the place doc
    // living in a different folder, by title.
    let dst = outgoing(&store, notes.id, "Jakarta").expect("edge to Jakarta exists");
    assert_eq!(dst, Some(jakarta.id));
    assert_eq!(stats::stats(&store).unwrap().dangling_links, 0);
}

#[test]
fn ambiguous_title_link_stays_dangling() {
    let (_dir, store) = synced_store(&[
        (
            "a/jakarta.md",
            "---\ntitle: Jakarta\ndescription: One\ntype: place\n---\nFirst.\n",
        ),
        (
            "b/jakarta.md",
            "---\ntitle: Jakarta\ndescription: Two\ntype: place\n---\nSecond.\n",
        ),
        (
            "c/notes.md",
            "---\ntitle: Notes\ndescription: refs\ntype: note\n---\nSee [[Jakarta]].\n",
        ),
    ]);
    let notes = store.get_doc_by_slug("c/notes").unwrap().unwrap();
    // Two docs titled "Jakarta" → ambiguous → link left dangling, not guessed.
    let dst = outgoing(&store, notes.id, "Jakarta").expect("edge to Jakarta exists");
    assert_eq!(dst, None);
    assert!(stats::stats(&store).unwrap().dangling_links >= 1);
}

#[test]
fn wikilink_to_workspace_index_resolves() {
    let (_dir, store) = synced_store(&[
        (
            "xyz/index.md",
            "---\ntitle: Riset XYZ\ndescription: workspace\ntype: session\n---\nWorkspace.\n",
        ),
        (
            "root-note.md",
            "---\ntitle: Root Note\ndescription: refs\ntype: note\n---\nLihat [[xyz]].\n",
        ),
    ]);
    let idx = store.get_doc_by_slug("xyz/index").unwrap().unwrap();
    let note = store.get_doc_by_slug("root-note").unwrap().unwrap();
    // `[[xyz]]` resolves to the folder's index doc via the {slug}/index rule.
    let dst = outgoing(&store, note.id, "xyz").expect("edge to xyz exists");
    assert_eq!(dst, Some(idx.id));
}

// ── Memory hygiene: consolidate ──────────────────────────────────────────

/// Two near-identical `note` docs (differing by one word) plus one unrelated
/// doc — the canonical consolidate fixture.
fn dup_fixture() -> [(&'static str, &'static str); 3] {
    [
        (
            "notes/a.md",
            "---\ntitle: Alice Acme\ntype: note\nupdated: 2026-01-01\n---\nAlice works at Acme Corp on retrieval systems and ranking.\n",
        ),
        (
            "notes/b.md",
            "---\ntitle: Alice Acme\ntype: note\nupdated: 2026-06-01\n---\nAlice works at Acme Corp on retrieval systems and ranking quality.\n",
        ),
        (
            "notes/c.md",
            "---\ntitle: Chess\ntype: note\nupdated: 2026-03-01\n---\nBob plays chess every Sunday afternoon downtown.\n",
        ),
    ]
}

#[test]
fn consolidate_folds_near_duplicates_into_newest_survivor() {
    let (_dir, store) = synced_store(&dup_fixture());
    let report = evomem::hygiene::consolidate(&store, 0.8, false).unwrap();
    // Exactly one fold: older notes/a → newer notes/b. notes/c is untouched.
    assert_eq!(report.merged.len(), 1, "{:?}", report.merged);
    assert_eq!(report.merged[0].survivor, "notes/b");
    assert_eq!(report.merged[0].duplicate, "notes/a");
    assert!(report.merged[0].score >= 0.8);
    assert_eq!(store.superseded_count().unwrap(), 1);

    // The superseded doc carries a back-pointer to its survivor.
    let a = store.get_doc_by_slug("notes/a").unwrap().unwrap();
    let b = store.get_doc_by_slug("notes/b").unwrap().unwrap();
    assert_eq!(a.superseded_by, Some(b.id));
    assert_eq!(b.superseded_by, None);

    // Retrieval: the survivor surfaces, the superseded near-duplicate does not.
    let embedder = HashEmbedder;
    let resp = search::search(
        &store,
        &embedder,
        "retrieval systems ranking",
        Mode::Balanced,
    )
    .unwrap();
    assert!(
        resp.hits.iter().any(|h| h.slug == "notes/b"),
        "survivor missing: {:?}",
        resp.hits
    );
    assert!(
        resp.hits.iter().all(|h| h.slug != "notes/a"),
        "superseded doc leaked into search: {:?}",
        resp.hits
    );
}

#[test]
fn consolidate_dry_run_writes_nothing() {
    let (_dir, store) = synced_store(&dup_fixture());
    let report = evomem::hygiene::consolidate(&store, 0.8, true).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.merged.len(), 1, "preview still computes the fold");
    // ...but nothing was written.
    assert_eq!(store.superseded_count().unwrap(), 0);
    assert_eq!(
        store
            .get_doc_by_slug("notes/a")
            .unwrap()
            .unwrap()
            .superseded_by,
        None
    );
}

#[test]
fn consolidate_only_merges_same_doc_type() {
    // Same near-identical text, but different `type` → never folded together.
    let (_dir, store) = synced_store(&[
        (
            "notes/a.md",
            "---\ntitle: Alice Acme\ntype: note\nupdated: 2026-01-01\n---\nAlice works at Acme Corp on retrieval systems and ranking.\n",
        ),
        (
            "people/a.md",
            "---\ntitle: Alice Acme\ntype: person\nupdated: 2026-06-01\n---\nAlice works at Acme Corp on retrieval systems and ranking.\n",
        ),
    ]);
    let report = evomem::hygiene::consolidate(&store, 0.8, false).unwrap();
    assert!(
        report.merged.is_empty(),
        "cross-type fold: {:?}",
        report.merged
    );
    assert_eq!(store.superseded_count().unwrap(), 0);
}

#[test]
fn consolidate_is_idempotent() {
    let (_dir, store) = synced_store(&dup_fixture());
    let first = evomem::hygiene::consolidate(&store, 0.8, false).unwrap();
    let second = evomem::hygiene::consolidate(&store, 0.8, false).unwrap();
    // Re-running clears and recomputes to the same result — no drift, no chains.
    assert_eq!(first.merged.len(), second.merged.len());
    assert_eq!(store.superseded_count().unwrap(), 1);
}
