use std::path::Path;

use anyhow::Context;
use clap::Parser;

use evomem::api::{CaptureRequest, GraphResponse, SearchResponse, StatsResponse, ThinkResponse};
use evomem::cli::{Cli, Command};
use evomem::client::RemoteClient;
use evomem::embed::HashEmbedder;
use evomem::error::EvoError;
use evomem::hygiene::ConsolidateReport;
use evomem::ingest::SyncReport;
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
            | Command::Consolidate { .. } => {
                anyhow::bail!(
                    "`init`, `serve`, `validate`, and `consolidate` run locally; drop --server for them"
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
        Command::Consolidate { threshold, dry_run } => {
            let store = open(knowledge_root, &embedder)?;
            let report = hygiene::consolidate(&store, *threshold, *dry_run)?;
            emit(cli.json, &report, render_consolidate)?;
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
            println!(
                "  • {} — {} [{}{}] ({:?})",
                f.title, f.lead, f.slug, heading, f.evidence
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

fn render_stats(s: &StatsResponse) {
    println!(
        "docs: {} live, {} superseded, {} deleted | chunks: {} | vocabulary: {} words | links: {} ({} dangling)",
        s.docs, s.superseded_docs, s.deleted_docs, s.chunks, s.indexed_words, s.links, s.dangling_links
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
