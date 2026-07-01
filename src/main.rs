use std::path::Path;

use anyhow::Context;
use clap::Parser;

use evomem::api::{CaptureRequest, GraphResponse, SearchResponse, StatsResponse, ThinkResponse};
use evomem::cli::{Cli, Command, ContradictionAction};
use evomem::client::RemoteClient;
use evomem::contradiction::{self, DetectReport};
use evomem::embed::HashEmbedder;
use evomem::error::EvoError;
use evomem::hygiene::ConsolidateReport;
use evomem::ingest::SyncReport;
use evomem::store::contradictions::Contradiction;
use evomem::store::Store;
use evomem::validate::ValidateReport;
use evomem::{capture, config, hygiene, ingest, search, stats, think, validate};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let embedder = HashEmbedder;

    // CLI → server mode: any data command goes over HTTP when --server is set.
    if let Some(base) = &cli.server {
        let client = RemoteClient::new(base);
        match &cli.command {
            Command::Init
            | Command::Serve { .. }
            | Command::Validate { .. }
            | Command::Consolidate { .. }
            | Command::RecallBump { .. }
            | Command::Contradiction { .. } => {
                anyhow::bail!(
                    "`init`, `serve`, `validate`, `consolidate`, `recall-bump`, and `contradiction` run locally; drop --server for them"
                )
            }
            Command::Sync => {
                let report = client.sync()?;
                emit(cli.json, &report, render_sync)?;
            }
            Command::Capture { text, title } => {
                let resp = client.capture(&CaptureRequest {
                    text: text.clone(),
                    title: title.clone(),
                })?;
                println!("captured -> {} ({})", resp.slug, resp.path);
            }
            Command::Search { query, mode, limit } => {
                let resp = client.search(query, *mode, *limit)?;
                emit(cli.json, &resp, render_search)?;
            }
            Command::Think { query, mode } => {
                let resp = client.think(query, *mode)?;
                emit(cli.json, &resp, render_think)?;
            }
            Command::GraphQuery { start, edge, hops } => {
                let resp = client.graph(start, edge.as_deref(), *hops)?;
                emit(cli.json, &resp, render_graph)?;
            }
            Command::Doc { slug } => {
                let doc = client.doc(slug)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&doc)?);
                } else {
                    println!("# {} ({})\n\n{}", doc.title, doc.slug, doc.content);
                }
            }
            Command::Stats => {
                let resp = client.stats()?;
                emit(cli.json, &resp, render_stats)?;
            }
        }
        return Ok(());
    }

    // Standalone mode: direct local database access.
    let knowledge_root = Path::new(&cli.knowledge);
    match &cli.command {
        Command::Init => {
            use evomem::embed::Embedder;
            let store = Store::init(knowledge_root, embedder.id(), embedder.dim())?;
            if Store::ensure_gitignore(&store.brain_root)? {
                println!("added .evomem.db* to .gitignore");
            }
            println!(
                "initialized knowledge at {} (db: {})",
                store.brain_root.display(),
                store.brain_root.join(config::DB_FILE).display()
            );
        }
        Command::Sync => {
            let store = open(knowledge_root, &embedder)?;
            let report = ingest::sync_dir(&store, &embedder).context("sync failed")?;
            emit(cli.json, &report, render_sync)?;
        }
        Command::Capture { text, title } => {
            let store = open(knowledge_root, &embedder)?;
            let resp = capture::capture(
                &store,
                &embedder,
                &CaptureRequest {
                    text: text.clone(),
                    title: title.clone(),
                },
                chrono::Utc::now(),
            )?;
            println!("captured -> {} ({})", resp.slug, resp.path);
        }
        Command::Search { query, mode, limit } => {
            let store = open(knowledge_root, &embedder)?;
            let mut resp = search::search(&store, &embedder, query, *mode)?;
            if let Some(l) = limit {
                resp.hits.truncate(*l);
            }
            emit(cli.json, &resp, render_search)?;
        }
        Command::Think { query, mode } => {
            let store = open(knowledge_root, &embedder)?;
            let resp = think::think(&store, &embedder, query, *mode, chrono::Utc::now())?;
            emit(cli.json, &resp, render_think)?;
        }
        Command::GraphQuery { start, edge, hops } => {
            let store = open(knowledge_root, &embedder)?;
            let doc = store
                .resolve_doc(start)?
                .ok_or_else(|| EvoError::DocNotFound(start.clone()))?;
            let edges = search::graph::traverse(&store, doc.id, edge.as_deref(), *hops)?;
            let resp = GraphResponse {
                start: doc.slug,
                edges,
                cached: false,
            };
            emit(cli.json, &resp, render_graph)?;
        }
        Command::Doc { slug } => {
            let store = open(knowledge_root, &embedder)?;
            let doc = store
                .resolve_doc(slug)?
                .ok_or_else(|| EvoError::DocNotFound(slug.clone()))?;
            let content =
                std::fs::read_to_string(store.brain_root.join(format!("{}.md", doc.slug)))
                    .unwrap_or_default();
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "slug": doc.slug, "title": doc.title, "type": doc.doc_type,
                        "tags": doc.tags, "updated_at": doc.updated_at, "content": content,
                        "recall_count": doc.recall_count,
                        "last_recalled_at": doc.last_recalled_at,
                    }))?
                );
            } else {
                println!("# {} ({})\n\n{}", doc.title, doc.slug, content);
            }
        }
        Command::Validate { path, since, all } => {
            let store = open(knowledge_root, &embedder)?;
            let report = validate::run(&store, path.as_deref(), since.as_deref(), *all)
                .context("validate failed")?;
            emit(cli.json, &report, render_validate)?;
        }
        Command::Consolidate {
            threshold,
            dry_run,
            source_dir,
        } => {
            let store = open(knowledge_root, &embedder)?;
            let report =
                hygiene::consolidate(&store, *threshold, *dry_run, source_dir.as_deref())?;
            emit(cli.json, &report, render_consolidate)?;
        }
        Command::RecallBump { slugs } => {
            let store = open(knowledge_root, &embedder)?;
            let now = chrono::Utc::now().to_rfc3339();
            let bumped = store.bump_recall(slugs, &now)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "requested": slugs.len(), "bumped": bumped, "at": now,
                    }))?
                );
            } else {
                println!("recall-bump: {bumped}/{} doc(s) updated", slugs.len());
            }
        }
        Command::Contradiction { action } => {
            let store = open(knowledge_root, &embedder)?;
            handle_contradiction(&store, action, cli.json)?;
        }
        Command::Stats => {
            let store = open(knowledge_root, &embedder)?;
            let resp = stats::stats(&store)?;
            emit(cli.json, &resp, render_stats)?;
        }
        Command::Serve { host, port } => {
            let store = open(knowledge_root, &embedder)?;
            evomem::server::serve(store, Box::new(HashEmbedder), host, *port)?;
        }
    }
    Ok(())
}

fn open(brain: &Path, embedder: &HashEmbedder) -> Result<Store, EvoError> {
    use evomem::embed::Embedder;
    Store::open(brain, embedder.id())
}

fn emit<T: serde::Serialize>(json: bool, value: &T, render: impl Fn(&T)) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        render(value);
    }
    Ok(())
}

fn render_sync(r: &SyncReport) {
    println!(
        "synced: {} scanned | {} added | {} updated | {} unchanged | {} deleted | {} renamed | {} links resolved",
        r.scanned, r.added, r.updated, r.unchanged, r.deleted, r.renamed, r.links_resolved
    );
    if !r.errors.is_empty() {
        eprintln!("{} file(s) skipped:", r.errors.len());
        for issue in &r.errors {
            eprintln!("  ! {}: {}", issue.path, issue.message);
        }
    }
}

fn render_validate(r: &ValidateReport) {
    println!(
        "validated {} KB file(s): {} valid, {} invalid",
        r.checked, r.valid, r.invalid
    );
    for issue in &r.issues {
        eprintln!("  ✗ {} — {}", issue.path, issue.message);
    }
}

fn render_search(r: &SearchResponse) {
    let cached = if r.cached { " (cached)" } else { "" };
    println!(
        "intent: {:?} | mode: {} | {} hits{}",
        r.intent,
        r.mode,
        r.hits.len(),
        cached
    );
    for h in &r.hits {
        let heading = if h.heading_path.is_empty() {
            String::new()
        } else {
            format!(" § {}", h.heading_path)
        };
        println!("\n{:>2}. {} [{}]{}", h.rank, h.title, h.slug, heading);
        println!(
            "    score {:.4} | {:?} | {}",
            h.score, h.evidence, h.doc_type
        );
        println!("    {}", h.snippet.replace('\n', " "));
    }
}

fn render_think(r: &ThinkResponse) {
    let cached = if r.cached { " (cached)" } else { "" };
    println!("intent: {:?} | mode: {}{}\n", r.intent, r.mode, cached);
    if r.facts.is_empty() {
        println!("No relevant knowledge found.");
    } else {
        println!("What is known:");
        for f in &r.facts {
            let heading = if f.heading_path.is_empty() {
                String::new()
            } else {
                format!(" § {}", f.heading_path)
            };
            let trust = match f.confidence {
                Some(c) => format!(" · confidence {c:.2}"),
                None => String::new(),
            };
            println!(
                "  • {} — {} [{}{}] ({:?}{})",
                f.title, f.lead, f.slug, heading, f.evidence, trust
            );
        }
    }
    if !r.gaps.is_empty() {
        println!("\nHeads up — gaps:");
        for g in &r.gaps {
            println!("  ! {:?}: {}", g.kind, g.message);
        }
    }
}

fn render_graph(r: &GraphResponse) {
    let cached = if r.cached { " (cached)" } else { "" };
    println!("graph from {}{}:", r.start, cached);
    if r.edges.is_empty() {
        println!("  (no typed edges)");
    }
    for e in &r.edges {
        println!(
            "  [hop {}] {} ── {} ──> {}",
            e.hop, e.src_slug, e.edge_type, e.dst_slug
        );
    }
}

fn render_consolidate(r: &ConsolidateReport) {
    let note = if r.dry_run {
        " (dry run — nothing written)"
    } else {
        ""
    };
    println!(
        "consolidate: {} scanned | {} merged | threshold {:.2}{}",
        r.scanned,
        r.merged.len(),
        r.threshold,
        note
    );
    for m in &r.merged {
        println!(
            "  {} ← {} (jaccard {:.2})",
            m.survivor, m.duplicate, m.score
        );
    }
}

fn handle_contradiction(
    store: &Store,
    action: &ContradictionAction,
    json: bool,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    match action {
        ContradictionAction::Flag { a, b, edge, desc } => {
            let id = store.flag_contradiction(
                a,
                b,
                edge.as_deref(),
                desc.as_deref().unwrap_or(""),
                &now,
            )?;
            let c = store
                .get_contradiction(id)?
                .ok_or_else(|| EvoError::Other("contradiction vanished after flag".into()))?;
            emit(json, &c, |c| {
                println!(
                    "flagged contradiction #{}: \"{}\" vs \"{}\"",
                    c.id, c.item_a, c.item_b
                )
            })?;
        }
        ContradictionAction::Resolve { id, resolution, by } => {
            let ok =
                store.resolve_contradiction(*id, resolution.as_deref(), by.as_deref(), &now)?;
            if !ok {
                anyhow::bail!("no contradiction with id {id}");
            }
            println!("resolved contradiction #{id}");
        }
        ContradictionAction::List { open } => {
            let list = store.list_contradictions(*open)?;
            emit(json, &list, |l| render_contradiction_list(l))?;
        }
        ContradictionAction::Detect => {
            let report = contradiction::detect_contradictions(store, &now)?;
            emit(json, &report, render_detect)?;
        }
    }
    Ok(())
}

fn render_contradiction_list(list: &[Contradiction]) {
    if list.is_empty() {
        println!("no contradictions");
        return;
    }
    for c in list {
        let edge = if c.edge_type.is_empty() {
            String::new()
        } else {
            format!(" [{}]", c.edge_type)
        };
        println!(
            "#{} {} : \"{}\" vs \"{}\"{}",
            c.id, c.status, c.item_a, c.item_b, edge
        );
        if !c.description.is_empty() {
            println!("    {}", c.description);
        }
        if let Some(r) = &c.resolution {
            println!("    resolution: {r}");
        }
    }
}

fn render_detect(r: &DetectReport) {
    println!(
        "detect: {} conflict(s) seen, {} newly flagged",
        r.conflicts.len(),
        r.flagged
    );
    for c in &r.conflicts {
        let tag = if c.new { "NEW" } else { "exists" };
        println!(
            "  [{}] {} {} → {} vs {}",
            tag, c.subject, c.edge_type, c.item_a, c.item_b
        );
    }
}

fn render_stats(s: &StatsResponse) {
    println!(
        "docs: {} live, {} superseded, {} deleted | chunks: {} | vocabulary: {} words | links: {} ({} dangling) | contradictions: {} open",
        s.docs, s.superseded_docs, s.deleted_docs, s.chunks, s.indexed_words, s.links, s.dangling_links, s.open_contradictions
    );
    if !s.links_by_type.is_empty() {
        println!("edges by type:");
        for (t, n) in &s.links_by_type {
            println!("  {t}: {n}");
        }
    }
    if !s.docs_by_source.is_empty() {
        println!("docs by source:");
        for (d, n) in &s.docs_by_source {
            println!("  {}: {}", if d.is_empty() { "(root)" } else { d }, n);
        }
    }
}
