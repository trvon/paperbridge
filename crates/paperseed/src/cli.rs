use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "paperseed",
    version,
    about = "Paperseed local corpus and seeding for Paperbridge"
)]
pub struct Cli {
    /// Emit machine-readable JSON for supported commands
    #[arg(long, global = true)]
    pub json: bool,

    /// Corpus root directory; defaults to ./.paperseed
    #[arg(long, global = true)]
    pub corpus_root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Manage the local paper corpus: status, import, ingest, query, export
    Corpus {
        #[command(subcommand)]
        action: CorpusAction,
    },

    /// Manage license-gated seed manifests
    Seed {
        #[command(subcommand)]
        action: SeedAction,
    },

    /// Show corpus status and policy mode
    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperseed corpus status' instead."
    )]
    Status,

    /// Import a PDF or text file the user already has rights to store locally
    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperseed corpus import' instead."
    )]
    Import {
        path: PathBuf,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        license: Option<String>,
    },

    /// Ingest Paperbridge/Zotero-style JSON metadata plus an authorized local file
    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperseed corpus ingest' instead."
    )]
    Ingest {
        #[arg(long)]
        metadata: PathBuf,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        license: Option<String>,
    },

    /// Search the local full-text corpus
    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperseed corpus query' instead."
    )]
    Query {
        #[arg(short = 'q', long)]
        q: String,
    },

    /// Export the local corpus as JSON or BibTeX
    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperseed corpus export' instead."
    )]
    Export {
        #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
        format: ExportFormat,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CorpusAction {
    /// Show corpus status and policy mode
    Status,

    /// Import a PDF or text file the user already has rights to store locally
    Import {
        /// File path to import
        path: PathBuf,

        /// Optional title override
        #[arg(long)]
        title: Option<String>,

        /// Optional license, e.g. cc-by, cc0, public-domain, user-owned-private
        #[arg(long)]
        license: Option<String>,
    },

    /// Ingest Paperbridge/Zotero-style JSON metadata plus an authorized local file
    Ingest {
        /// Paperbridge/Zotero-style JSON file
        #[arg(long)]
        metadata: PathBuf,

        /// Authorized local PDF/text file to store
        #[arg(long)]
        file: PathBuf,

        /// License override, e.g. cc-by, cc0, public-domain, user-owned-private
        #[arg(long)]
        license: Option<String>,
    },

    /// Search the local full-text corpus
    Query {
        /// Local full-text query
        #[arg(short = 'q', long)]
        q: String,
    },

    /// Export the local corpus as JSON or BibTeX
    Export {
        /// Export format
        #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
        format: ExportFormat,
    },

    /// Rebuild the BM25F search index from corpus.json
    Reindex,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SeedAction {
    /// Check whether a corpus paper may be seeded
    Check {
        /// Local paper id or content hash
        #[arg(long)]
        paper_id: String,
    },

    /// Create a license-gated seed manifest for a corpus paper
    Create {
        /// Local paper id or content hash
        #[arg(long)]
        paper_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Json,
    Bibtex,
}
