# Evomem

**Knowledge infrastructure for AI agents: markdown memory repo, hybrid retrieval, self-wiring knowledge graph.**

Evomem is a CLI tool and embeddable library that turns a directory of markdown files into a queryable "knowledge" inspired by gbrain [gbrain](https://github.com/garrytan/gbrain) and [Obsidian](https://obsidian.md/), combining lexical search, hash-based vector embeddings, and typed knowledge graphs with zero LLM dependency at query time. It gives AI agents persistent, structured memory without the cost, latency, or unpredictability of calling an LLM for every retrieval.

The design is minimal by choice. Your knowledge is just markdown files in a git repo — easy to edit, diff, backup, and version. Write your notes, capture thoughts with `evomem capture`, and the system handles indexing, ranking, and graph traversal automatically.

Because knowledge is power, and power need knowledge.


## How it works

```
markdown files/  ──[sync]──►  SQLite store  ──[search/think]──►  ranked results
     │                              │
     ├── YAML frontmatter           ├── lexical index (words)
     ├── heading structure          ├── hash vector index
     ├── typed links                └── typed edge graph
     └── body text
```

- **Disk is the source of truth.** Edit your markdown files, run `evomem sync` to update the database.
- **No LLM required** for retrieval. Everything — intent classification, embedding, ranking — is deterministic.
- **Self-wiring graph.** Typed edges (founded, works_at, advises, invests_in, mentions, custom) are extracted from markdown blockquotes and auto-resolved across pages.
- **EvoRank.** Deterministic scoring: authority prior (in-degree), recency boost, reciprocal-rank fusion of lexical + vector signals.

## Installation

```bash
cargo install evomem
```

Or build from source:

```bash
git clone <repo-url>
cd evomem
make build
```

## Quick start

```bash
# Initialize a evomem in the current directory
evomem init

# Write some markdown files...
echo '# Hello World' > hello.md

# Sync them into the database
evomem sync

# Search
evomem search "hello"

# Capture a quick thought (creates a timestamped file in inbox/)
evomem capture "Interesting idea about neural networks"

# Think — synthesize facts with gap analysis
evomem think "what do I know about rust"

# Traverse the knowledge graph
evomem graph-query some-page --hops 3

# Show page content
evomem page hello
```

## CLI commands

| Command | Description |
|---------|-------------|
| `init` | Initialize a evomem (creates the database in the evomem directory) |
| `sync` | Sync markdown files into the database (disk is the source of truth) |
| `capture <text>` | Capture a quick thought into `inbox/` and index it immediately |
| `search <query>` | Raw hybrid retrieval: ranked results with evidence tags |
| `think <query>` | Knowledge synthesis: composed facts with citations + gap analysis |
| `graph-query <start>` | Traverse typed edges from a page (multi-hop) |
| `page <slug>` | Show a page's metadata and content |
| `stats` | Knowledge store statistics |
| `serve` | Run as a standalone REST API server |

### Global flags

| Flag | Description |
|------|-------------|
| `--knowledge <dir>` | Knowledge root directory (default: `.`, env: `EVOMEM_ROOT`) |
| `--server <url>` | Run against a remote evomem server instead of the local database (env: `EVOMEM_SERVER`) |
| `--json` | Emit machine-readable JSON instead of human output |

### Retrieval modes

| Mode | Description |
|------|-------------|
| `conservative` | Fewer results, higher precision |
| `balanced` (default) | Balanced precision/recall |
| `tokenmax` | Maximum recall |

## Markdown features

### Frontmatter

```yaml
---
title: My Page
type: person
aliases: [nickname, handle]
tags: [rust, systems]
created: 2026-01-05
updated: 2026-06-20
---
```

All fields are optional and parsed leniently — a stray scalar where a list is expected won't reject the file.

### Typed links

Create typed edges between pages with blockquote syntax:

```markdown
> founded: Acme Corp
> works_at: Acme Corp
> advises: Some Startup
```

Edge types: `founded`, `invested_in`, `works_at`, `advises`, `attended`, `mentions`, or any custom type.

## Server mode

Run as a REST API:

```bash
evomem serve --host 127.0.0.1 --port 7700
```

Then use `--server` from another machine:

```bash
evomem --server http://host:7700 search "query"
```

## Architecture

### Retrieval pipeline

1. **Intent classification** — deterministic: entity, temporal, event, or general
2. **Lexical search** — bucket-sort ranking over word-indexed postings
3. **Vector search** — BLAKE3-based hash embedding (512-dim, deterministic)
4. **Reciprocal-rank fusion** — merges lexical + vector candidates
5. **Graph augmentation** — re-ranks via authority prior (in-degree)
6. **EvoRank scoring** — final rank with evidence tags

### Bucket-sort ranking

Lexical ranking uses bucket-sort: results are sorted not by a single BM25 score, but by a cascade of tie-breaking rules. Each rule acts as a bucket — if two chunks tie on the first rule, the second decides, and so on.

The rules in order:

1. **Word count** — chunks matching more query words win outright.
2. **IDF weight** — among equal word counts, matching rarer (more discriminative) query words wins.
3. **Typo cost** — fewer/cheaper corrections win: exact (0) < stem match (1) < one typo (2) < two typos (4).
4. **Proximity** — matched words closer together win; reversed query-order pairs pay a penalty.
5. **Attribute** — earlier attribute wins (title > heading > body), then earlier word position.
6. **Exactness** — exact word matches beat prefix, stem, or typo-derived matches.

This approach gives deterministic, explainable ranking — every result position is traceable to the rule that decided it.

### Search evidence tags

| Tag | Meaning |
|-----|---------|
| `alias_hit` | Matched an alias from frontmatter |
| `exact_title_match` | Title matched exactly |
| `keyword_exact` | Keyword matched in body text |
| `high_vector_match` | Cosine similarity > 0.45 |
| `graph_adjacent` | Adjacent in the knowledge graph |
| `weak_semantic` | Weak vector/semantic match |

## Build

```bash
make build        # native release build
make check        # cargo check
make test         # cargo test
make clippy       # cargo clippy (deny warnings)
make fmt          # cargo fmt --check
make fmt-fix      # cargo fmt

# Cross-compile (requires cargo-zigbuild)
make build-linux      # x86_64-unknown-linux-musl (fully static)
make build-linux-gnu  # x86_64-unknown-linux-gnu
```

## License

MIT


