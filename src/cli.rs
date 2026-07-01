use clap::{Parser, Subcommand};

use crate::model::Mode;

#[derive(Parser)]
#[command(
    name = "evomem",
    version,
    about = "Knowledge infrastructure for AI agents: markdown knowledge, hybrid retrieval, self-wiring knowledge graph"
)]
pub struct Cli {
    /// Knowledge root directory (default: current dir, or $EVOMEM_ROOT)
    #[arg(
        long,
        alias = "brain",
        global = true,
        env = "EVOMEM_ROOT",
        default_value = "."
    )]
    pub knowledge: String,

    /// Run against a remote evomem server instead of the local database
    #[arg(long, global = true, env = "EVOMEM_SERVER")]
    pub server: Option<String>,

    /// Emit machine-readable JSON instead of human output
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a knowledge store (creates the database in the knowledge root directory)
    Init,
    /// Sync markdown files into the database (disk is the source of truth)
    Sync,
    /// Capture a quick thought into inbox/ and index it immediately
    Capture {
        /// The text to capture
        text: String,
        /// Optional title (derived from the first line if omitted)
        #[arg(long)]
        title: Option<String>,
    },
    /// Raw hybrid retrieval: ranked results with evidence tags
    Search {
        query: String,
        #[arg(long, default_value = "balanced")]
        mode: Mode,
        /// Cap the number of results
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Knowledge synthesis: composed facts with citations + gap analysis
    Think {
        query: String,
        #[arg(long, default_value = "balanced")]
        mode: Mode,
    },
    /// Traverse typed edges from a doc (multi-hop)
    GraphQuery {
        /// Start doc: slug, title, or alias
        start: String,
        /// Filter by edge type (founded|invested_in|works_at|advises|attended|mentions|custom)
        #[arg(long)]
        edge: Option<String>,
        #[arg(long, default_value_t = 2)]
        hops: usize,
    },
    /// Show a doc's metadata and content
    Doc { slug: String },
    /// Validate doc frontmatter (title, description, type ∈ note|session|group|person|place|venue|event|organization|company|product|contact)
    Validate {
        /// Validate a single markdown file (overrides the recency filter)
        path: Option<String>,
        /// Only validate files modified at/after this RFC3339 timestamp
        #[arg(long)]
        since: Option<String>,
        /// Validate all KB files, ignoring the recency filter
        #[arg(long)]
        all: bool,
    },
    /// Fold near-duplicate docs into their newest version (memory hygiene).
    /// Older near-duplicates are marked superseded and drop out of retrieval,
    /// but remain on disk and in the database for history.
    Consolidate {
        /// Minimum Jaccard token overlap (0.0–1.0) to treat two same-type docs
        /// as duplicates
        #[arg(long, default_value_t = 0.85)]
        threshold: f64,
        /// Preview the merges without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Restrict folding to docs under one source_dir (top-level folder, e.g.
        /// `memory`). Folding always requires the same type AND source_dir; this
        /// further limits the whole pass to a single layer so an automated caller
        /// can consolidate only volatile captures (memory/) and leave
        /// hand-authored notes and entities untouched. Omit to scan every layer.
        #[arg(long)]
        source_dir: Option<String>,
    },
    /// Track conflicting facts: flag, resolve, list, or auto-detect contradictions
    Contradiction {
        #[command(subcommand)]
        action: ContradictionAction,
    },
    /// Bump the recall counter for one or more docs (by slug): records that a
    /// doc was actually surfaced into an agent's recall context. Feeds the Auto
    /// Dream prune phase. Runtime state — never reset by `sync`.
    RecallBump {
        /// Slugs of the docs that were recalled (one or more)
        #[arg(required = true)]
        slugs: Vec<String>,
    },
    /// Knowledge store statistics
    Stats,
    /// Run as a standalone REST API server
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 7700)]
        port: u16,
    },
}

#[derive(Subcommand)]
pub enum ContradictionAction {
    /// Flag two items (by slug) as contradicting each other
    Flag {
        /// First item slug
        a: String,
        /// Second item slug
        b: String,
        /// Relation the conflict is about (optional)
        #[arg(long)]
        edge: Option<String>,
        /// Human description of the conflict
        #[arg(long)]
        desc: Option<String>,
    },
    /// Mark a contradiction (by id) resolved
    Resolve {
        /// Contradiction id (from `contradiction list`)
        id: i64,
        /// How it was resolved
        #[arg(long)]
        resolution: Option<String>,
        /// Who resolved it
        #[arg(long)]
        by: Option<String>,
    },
    /// List contradictions
    List {
        /// Only show open (unresolved) ones
        #[arg(long)]
        open: bool,
    },
    /// Auto-detect conflicts from functional typed edges (same subject +
    /// relation pointing at two different targets)
    Detect,
}

impl clap::builder::ValueParserFactory for Mode {
    type Parser = clap::builder::ValueParser;
    fn value_parser() -> Self::Parser {
        clap::builder::ValueParser::new(|s: &str| s.parse::<Mode>())
    }
}
