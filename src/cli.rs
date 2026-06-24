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
    /// Traverse typed edges from a page (multi-hop)
    GraphQuery {
        /// Start page: slug, title, or alias
        start: String,
        /// Filter by edge type (founded|invested_in|works_at|advises|attended|mentions|custom)
        #[arg(long)]
        edge: Option<String>,
        #[arg(long, default_value_t = 2)]
        hops: usize,
    },
    /// Show a page's metadata and content
    Page { slug: String },
    /// Validate KB frontmatter (title, description, type ∈ note|session|group)
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

impl clap::builder::ValueParserFactory for Mode {
    type Parser = clap::builder::ValueParser;
    fn value_parser() -> Self::Parser {
        clap::builder::ValueParser::new(|s: &str| s.parse::<Mode>())
    }
}
