use crate::models::PaperSource;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "paperbridge",
    version,
    about = "Paperbridge MCP + CLI for Zotero"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run MCP server over stdio transport
    Serve,

    /// Generate shell completion script to stdout
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Show active backend mode, capabilities, and config health
    Status,

    /// Config helpers: init, get, set, validate, resolve-user-id, snippet
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Read local Zotero library: query, collections, read, read-search
    Library {
        #[command(subcommand)]
        action: LibraryAction,
    },

    /// Write Zotero items: create, update, delete, validate
    Item {
        #[command(subcommand)]
        action: ItemAction,
    },

    /// Write Zotero collections: create, update, delete
    Collection {
        #[command(subcommand)]
        action: CollectionAction,
    },

    /// Search external paper indexes and resolve DOIs
    Papers {
        #[command(subcommand)]
        action: PapersAction,
    },

    /// Retrieve and query structured paper content (sections, references, figures)
    Paper {
        #[command(subcommand)]
        action: PaperAction,
    },

    /// Print the agent operating guide (same content served over MCP as the `paperbridge_skill` prompt)
    Skill,

    // ---------- Hidden legacy aliases (removal targeted for 0.4.0) ----------
    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge status' instead."
    )]
    BackendInfo,

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge library query' instead."
    )]
    Query {
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        qmode: Option<String>,
        #[arg(long)]
        item_type: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        start: Option<u32>,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge library collections' instead."
    )]
    Collections {
        #[arg(long)]
        top_only: bool,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        start: Option<u32>,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge library read' instead."
    )]
    Read {
        #[arg(long)]
        item_key: String,
        #[arg(long)]
        attachment_key: Option<String>,
        #[arg(long)]
        max_chars_per_chunk: Option<usize>,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge library read-search' instead."
    )]
    ReadSearch {
        #[arg(long)]
        q: String,
        #[arg(long)]
        qmode: Option<String>,
        #[arg(long)]
        item_type: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        result_index: Option<usize>,
        #[arg(long)]
        search_limit: Option<u32>,
        #[arg(long)]
        max_chars_per_chunk: Option<usize>,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge collection create' instead."
    )]
    CreateCollection {
        #[arg(long)]
        name: String,
        #[arg(long)]
        parent_collection: Option<String>,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge papers resolve-doi' instead."
    )]
    ResolveDoi {
        #[arg(long)]
        doi: String,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge item validate' instead."
    )]
    ValidateItem {
        #[arg(long)]
        file: String,
        #[arg(long)]
        online: bool,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge item create' instead."
    )]
    CreateItem {
        #[arg(long)]
        file: String,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge collection update' instead."
    )]
    UpdateCollection {
        #[arg(long)]
        file: String,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge item update' instead."
    )]
    UpdateItem {
        #[arg(long)]
        file: String,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge collection delete' instead."
    )]
    DeleteCollection {
        #[arg(long)]
        file: String,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge item delete' instead."
    )]
    DeleteItem {
        #[arg(long)]
        file: String,
    },

    #[command(
        hide = true,
        after_help = "Deprecated. Use 'paperbridge papers search' instead."
    )]
    SearchPapers {
        #[arg(long)]
        q: String,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, value_enum, value_delimiter = ',')]
        sources: Option<Vec<PaperSource>>,
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
}

// ---------- Canonical domain subcommands ----------

#[derive(Debug, Subcommand)]
pub enum LibraryAction {
    /// Search items in the local Zotero library and print JSON
    Query {
        /// Quick search query
        #[arg(long)]
        q: Option<String>,
        /// Query mode (e.g. titleCreatorYear, everything)
        #[arg(long)]
        qmode: Option<String>,
        /// Item type filter (e.g. journalArticle)
        #[arg(long)]
        item_type: Option<String>,
        /// Tag filter
        #[arg(long)]
        tag: Option<String>,
        /// Result limit (1-100, default 25)
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination start index
        #[arg(long)]
        start: Option<u32>,
    },
    /// List Zotero collections and print JSON
    Collections {
        /// If true, list only top-level collections
        #[arg(long)]
        top_only: bool,
        /// Result limit (1-100, default 50)
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination start index
        #[arg(long)]
        start: Option<u32>,
    },
    /// Prepare one item for read-aloud and print Vox-ready JSON
    Read {
        /// Zotero item key
        #[arg(long)]
        item_key: String,
        /// Optional specific attachment key
        #[arg(long)]
        attachment_key: Option<String>,
        /// Max chars per chunk (default 1200)
        #[arg(long)]
        max_chars_per_chunk: Option<usize>,
    },
    /// Search the library then prepare the selected result for read-aloud
    ReadSearch {
        /// Search query
        #[arg(long)]
        q: String,
        /// Query mode (e.g. titleCreatorYear, everything)
        #[arg(long)]
        qmode: Option<String>,
        /// Item type filter
        #[arg(long)]
        item_type: Option<String>,
        /// Tag filter
        #[arg(long)]
        tag: Option<String>,
        /// 0-based search result index (default 0)
        #[arg(long)]
        result_index: Option<usize>,
        /// Number of search results to inspect (default 5)
        #[arg(long)]
        search_limit: Option<u32>,
        /// Max chars per chunk (default 1200)
        #[arg(long)]
        max_chars_per_chunk: Option<usize>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ItemAction {
    /// Validate an item payload JSON file (optionally cross-check DOI with Crossref)
    Validate {
        /// Path to JSON file matching ItemWriteRequest
        #[arg(long)]
        file: String,
        /// Also validate DOI against Crossref (slower, requires network)
        #[arg(long)]
        online: bool,
    },
    /// Create an item from a JSON payload file and print JSON
    Create {
        /// Path to JSON file matching ItemWriteRequest
        #[arg(long)]
        file: String,
    },
    /// Update an item from a JSON payload file (requires key + version) and print JSON
    Update {
        /// Path to JSON file matching ItemUpdateRequest
        #[arg(long)]
        file: String,
    },
    /// Delete an item from a JSON payload file (requires key + version)
    Delete {
        /// Path to JSON file matching DeleteItemRequest
        #[arg(long)]
        file: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CollectionAction {
    /// Create a collection and print JSON
    Create {
        /// Collection name
        #[arg(long)]
        name: String,
        /// Optional parent collection key
        #[arg(long)]
        parent_collection: Option<String>,
    },
    /// Update a collection from a JSON payload file and print JSON
    Update {
        /// Path to JSON file matching CollectionUpdateRequest
        #[arg(long)]
        file: String,
    },
    /// Delete a collection from a JSON payload file
    Delete {
        /// Path to JSON file matching DeleteCollectionRequest
        #[arg(long)]
        file: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PapersAction {
    /// Search external paper indexes (arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed, HuggingFace Papers, Semantic Scholar, CORE, NASA ADS)
    Search {
        /// Free-text search query
        #[arg(long)]
        q: String,
        /// Max hits per source (default 10)
        #[arg(long)]
        limit: Option<u32>,
        /// Subset of sources (comma-separated); default is all enabled
        #[arg(long, value_enum, value_delimiter = ',')]
        sources: Option<Vec<PaperSource>>,
        /// Per-source timeout in milliseconds (default 8000)
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Resolve a DOI via Crossref and print structured metadata
    ResolveDoi {
        /// DOI to resolve (e.g. 10.1038/nature12373)
        #[arg(long)]
        doi: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PaperAction {
    /// Fetch the full PaperStructure JSON for a Zotero item
    Structure {
        /// Zotero item key
        #[arg(long)]
        key: String,
        /// Optional attachment key override
        #[arg(long)]
        attachment: Option<String>,
    },
    /// Evaluate a dotted-path selector against a paper's structure
    Query {
        /// Zotero item key
        #[arg(long)]
        key: String,
        /// Dotted-path selector (e.g. "metadata.title", "sections[0].heading")
        #[arg(long)]
        selector: String,
        /// Optional attachment key override
        #[arg(long)]
        attachment: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Eq, PartialEq)]
pub enum SnippetTarget {
    Opencode,
    Claude,
    Pi,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print resolved config file path
    Path,
    /// Initialize config file (writes defaults unless --interactive)
    Init {
        /// Overwrite existing config file
        #[arg(long)]
        force: bool,
        /// Prompt for key settings interactively
        #[arg(long)]
        interactive: bool,
    },
    /// Validate loaded config and print safe values
    Validate,
    /// Get one config key or print all safe values
    Get {
        /// Optional config key
        key: Option<String>,
        /// Print sensitive values (api_key, hf_token, semantic_scholar_api_key) verbatim instead of redacting
        #[arg(long)]
        show_secret: bool,
    },
    /// Set one config key/value in config.toml
    Set {
        /// Config key to update
        key: String,
        /// Config value to write
        value: String,
    },
    /// Resolve username/login to numeric Zotero user ID
    ResolveUserId {
        /// Username or numeric user ID
        #[arg(long)]
        login: String,
        /// Optional API key override (otherwise uses config value)
        #[arg(long)]
        api_key: Option<String>,
        /// Optional API base override (otherwise uses config/default)
        #[arg(long)]
        api_base: Option<String>,
    },
    /// Print client configuration snippet
    Snippet {
        /// Target client for snippet output
        #[arg(long, value_enum)]
        target: SnippetTarget,
        /// Optional absolute binary path override
        #[arg(long)]
        binary_path: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_serve() {
        let cli = Cli::try_parse_from(["paperbridge", "serve"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Serve)));
    }

    #[test]
    fn parse_default_command_none() {
        let cli = Cli::try_parse_from(["paperbridge"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn parse_config_path() {
        let cli = Cli::try_parse_from(["paperbridge", "config", "path"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Path
            })
        ));
    }

    #[test]
    fn parse_canonical_library_query() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "library",
            "query",
            "--q",
            "vision transformers",
            "--limit",
            "10",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Library {
                action: LibraryAction::Query {
                    limit: Some(10),
                    ..
                }
            })
        ));
    }

    #[test]
    fn parse_canonical_item_create() {
        let cli =
            Cli::try_parse_from(["paperbridge", "item", "create", "--file", "item.json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Item {
                action: ItemAction::Create { .. }
            })
        ));
    }

    #[test]
    fn parse_canonical_papers_search_with_value_enum() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "papers",
            "search",
            "--q",
            "attention",
            "--sources",
            "arxiv,crossref",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Papers {
                action: PapersAction::Search { sources, .. },
            }) => {
                let s = sources.expect("sources parsed");
                assert_eq!(s, vec![PaperSource::Arxiv, PaperSource::Crossref]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_canonical_papers_search_rejects_unknown_source_at_parse_time() {
        let err = Cli::try_parse_from([
            "paperbridge",
            "papers",
            "search",
            "--q",
            "x",
            "--sources",
            "bogus",
        ])
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("invalid value"));
    }

    #[test]
    fn parse_canonical_status() {
        let cli = Cli::try_parse_from(["paperbridge", "status"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Status)));
    }

    #[test]
    fn parse_legacy_query_still_works() {
        let cli = Cli::try_parse_from(["paperbridge", "query", "--q", "foo"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Query { .. })));
    }

    #[test]
    fn parse_legacy_backend_info_still_works() {
        let cli = Cli::try_parse_from(["paperbridge", "backend-info"]).unwrap();
        assert!(matches!(cli.command, Some(Command::BackendInfo)));
    }

    #[test]
    fn parse_legacy_search_papers_still_works_with_value_enum() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "search-papers",
            "--q",
            "x",
            "--sources",
            "arxiv,hf,s2",
        ])
        .unwrap();
        match cli.command {
            Some(Command::SearchPapers { sources, .. }) => {
                let s = sources.expect("sources parsed");
                assert_eq!(
                    s,
                    vec![
                        PaperSource::Arxiv,
                        PaperSource::HuggingFace,
                        PaperSource::SemanticScholar
                    ]
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_config_snippet() {
        let cli =
            Cli::try_parse_from(["paperbridge", "config", "snippet", "--target", "pi"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Snippet {
                    target: SnippetTarget::Pi,
                    binary_path: None
                }
            })
        ));
    }

    #[test]
    fn parse_config_init_interactive() {
        let cli =
            Cli::try_parse_from(["paperbridge", "config", "init", "--interactive", "--force"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Init {
                    force: true,
                    interactive: true
                }
            })
        ));
    }

    #[test]
    fn parse_config_set() {
        let cli =
            Cli::try_parse_from(["paperbridge", "config", "set", "library_type", "group"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Set { .. }
            })
        ));
    }

    #[test]
    fn parse_config_get() {
        let cli = Cli::try_parse_from(["paperbridge", "config", "get", "timeout_secs"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Get { key: Some(_), .. }
            })
        ));
    }

    #[test]
    fn parse_config_resolve_user_id() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "config",
            "resolve-user-id",
            "--login",
            "nottrevon",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::ResolveUserId { .. }
            })
        ));
    }
}
