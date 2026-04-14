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
    /// Query Zotero items and print JSON
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
    /// List collections and print JSON
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
    /// Search then prepare selected result for read-aloud
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
    /// Create a collection and print JSON
    CreateCollection {
        /// Collection name
        #[arg(long)]
        name: String,
        /// Optional parent collection key
        #[arg(long)]
        parent_collection: Option<String>,
    },
    /// Resolve a DOI via Crossref and print structured metadata
    ResolveDoi {
        /// DOI to resolve (e.g. 10.1038/nature12373)
        #[arg(long)]
        doi: String,
    },
    /// Validate an item payload JSON file
    ValidateItem {
        /// Path to JSON file matching ItemWriteRequest
        #[arg(long)]
        file: String,
        /// Also validate DOI against Crossref (slower, requires network)
        #[arg(long)]
        online: bool,
    },
    /// Create an item from a JSON payload file and print JSON
    CreateItem {
        /// Path to JSON file matching ItemWriteRequest
        #[arg(long)]
        file: String,
    },
    /// Update a collection from a JSON payload file and print JSON
    UpdateCollection {
        /// Path to JSON file matching CollectionUpdateRequest
        #[arg(long)]
        file: String,
    },
    /// Update an item from a JSON payload file and print JSON
    UpdateItem {
        /// Path to JSON file matching ItemUpdateRequest
        #[arg(long)]
        file: String,
    },
    /// Delete a collection from a JSON payload file
    DeleteCollection {
        /// Path to JSON file matching DeleteCollectionRequest
        #[arg(long)]
        file: String,
    },
    /// Delete an item from a JSON payload file
    DeleteItem {
        /// Path to JSON file matching DeleteItemRequest
        #[arg(long)]
        file: String,
    },
    /// Show active backend mode and capabilities
    BackendInfo,
    /// Search external paper sources (arXiv, HuggingFace, Semantic Scholar, Crossref)
    SearchPapers {
        /// Free-text search query
        #[arg(long)]
        q: String,
        /// Max hits per source (default 10)
        #[arg(long)]
        limit: Option<u32>,
        /// Comma-separated subset: arxiv,hugging_face,semantic_scholar,crossref
        #[arg(long)]
        sources: Option<String>,
        /// Per-source timeout in milliseconds (default 8000)
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Config helper commands
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Generate shell completion script to stdout
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: clap_complete::Shell,
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
    fn parse_query_command() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "query",
            "--q",
            "vision transformers",
            "--limit",
            "10",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Query {
                q: Some(_),
                limit: Some(10),
                ..
            })
        ));
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

    #[test]
    fn parse_collections_command() {
        let cli = Cli::try_parse_from(["paperbridge", "collections", "--top-only"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Collections { top_only: true, .. })
        ));
    }

    #[test]
    fn parse_read_search_command() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "read-search",
            "--q",
            "graph learning",
            "--result-index",
            "0",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ReadSearch {
                result_index: Some(0),
                ..
            })
        ));
    }

    #[test]
    fn parse_create_collection_command() {
        let cli = Cli::try_parse_from(["paperbridge", "create-collection", "--name", "P4 Papers"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::CreateCollection { .. })
        ));
    }

    #[test]
    fn parse_resolve_doi_command() {
        let cli =
            Cli::try_parse_from(["paperbridge", "resolve-doi", "--doi", "10.1038/nature12373"])
                .unwrap();
        assert!(matches!(cli.command, Some(Command::ResolveDoi { .. })));
    }

    #[test]
    fn parse_validate_item_command() {
        let cli =
            Cli::try_parse_from(["paperbridge", "validate-item", "--file", "item.json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ValidateItem { online: false, .. })
        ));
    }

    #[test]
    fn parse_validate_item_online_command() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "validate-item",
            "--file",
            "item.json",
            "--online",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ValidateItem { online: true, .. })
        ));
    }

    #[test]
    fn parse_create_item_command() {
        let cli =
            Cli::try_parse_from(["paperbridge", "create-item", "--file", "item.json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::CreateItem { .. })));
    }

    #[test]
    fn parse_update_collection_command() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "update-collection",
            "--file",
            "collection.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::UpdateCollection { .. })
        ));
    }

    #[test]
    fn parse_update_item_command() {
        let cli =
            Cli::try_parse_from(["paperbridge", "update-item", "--file", "item.json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::UpdateItem { .. })));
    }

    #[test]
    fn parse_delete_collection_command() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "delete-collection",
            "--file",
            "collection.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::DeleteCollection { .. })
        ));
    }

    #[test]
    fn parse_delete_item_command() {
        let cli =
            Cli::try_parse_from(["paperbridge", "delete-item", "--file", "item.json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::DeleteItem { .. })));
    }

    #[test]
    fn parse_backend_info_command() {
        let cli = Cli::try_parse_from(["paperbridge", "backend-info"]).unwrap();
        assert!(matches!(cli.command, Some(Command::BackendInfo)));
    }

    #[test]
    fn parse_search_papers_command() {
        let cli = Cli::try_parse_from([
            "paperbridge",
            "search-papers",
            "--q",
            "vision transformers",
            "--limit",
            "5",
            "--sources",
            "arxiv,crossref",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::SearchPapers {
                limit: Some(5),
                sources: Some(_),
                ..
            })
        ));
    }
}
