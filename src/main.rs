use clap::{CommandFactory, Parser};
use paperbridge::cli::{
    Cli, CollectionAction, Command, ConfigAction, ItemAction, LibraryAction, PaperAction,
    PapersAction, PaperseedAction, PaperseedCorpusAction, PaperseedExportFormat,
    PaperseedSeedAction, SnippetTarget,
};
use paperbridge::config::Config;
use paperbridge::external::SearchOptions;
use paperbridge::models::{
    CollectionUpdateRequest, CollectionWriteRequest, DeleteCollectionRequest, DeleteItemRequest,
    ItemUpdateRequest, ItemWriteRequest, ListCollectionsQuery, PaperSource, SearchCacheMode,
    SearchDetail, SearchItemsQuery,
};
use paperbridge::server::PaperbridgeServer;
use paperbridge::service::{
    OpenPaperRequest, PaperbridgeService, PaperseedMirrorConfig, PrepareItemForVoxRequest,
    PrepareSearchResultForVoxRequest,
};
use paperbridge::zotero_api::build_backend;
use rmcp::ServiceExt;
use serde::Serialize;
use std::io::{self, Write};
use std::process::ExitCode;
use tracing::warn;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output = OutputFormat::new(cli.json);

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            print_runtime_error(&error, output);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> paperbridge::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| paperbridge::ZoteroMcpError::Config(e.to_string()))?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> paperbridge::Result<()> {
    let output = OutputFormat::new(cli.json);

    if let Some(Command::Completions { shell }) = &cli.command {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(*shell, &mut cmd, name, &mut io::stdout());
        return Ok(());
    }

    if let Some(Command::Config { action }) = &cli.command {
        match action {
            ConfigAction::Path => {
                if output.is_json() {
                    print_output(
                        &serde_json::json!({
                            "config_path": Config::config_path().display().to_string()
                        }),
                        output,
                    )?;
                } else {
                    println!("{}", Config::config_path().display());
                }
                return Ok(());
            }
            ConfigAction::Snippet {
                target,
                binary_path,
            } => {
                print_client_snippet(*target, binary_path.as_deref());
                return Ok(());
            }
            ConfigAction::Init { force, interactive } => {
                handle_config_init(*force, *interactive).await?;
                return Ok(());
            }
            ConfigAction::Get { key, show_secret } => {
                handle_config_get(key.as_deref(), *show_secret, output)?;
                return Ok(());
            }
            ConfigAction::Set { key, value } => {
                handle_config_set(key, value)?;
                return Ok(());
            }
            ConfigAction::ResolveUserId {
                login,
                api_key,
                api_base,
            } => {
                handle_config_resolve_user_id(
                    login,
                    api_key.as_deref(),
                    api_base.as_deref(),
                    output,
                )
                .await?;
                return Ok(());
            }
            ConfigAction::Doctor { verbose, setup } => {
                handle_config_doctor(output, *verbose, *setup)?;
                return Ok(());
            }
            ConfigAction::Validate => {}
        }
    }

    let config = Config::load()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Some(Command::Config {
            action: ConfigAction::Validate,
        }) => {
            if output.is_json() {
                print_output(
                    &serde_json::json!({
                        "valid": true,
                        "config": safe_config_value(&config)?,
                    }),
                    output,
                )?;
            } else {
                println!("Config valid.\n\n{}", config.display_safe());
            }
        }
        Some(Command::Skill) => {
            println!("{}", paperbridge::server::SKILL_MD);
        }
        Some(Command::Update) => {
            paperbridge::update::run_update().await?;
        }
        Some(Command::Status) => handle_status(config, output).await?,
        Some(Command::Library { action }) => match action {
            LibraryAction::Query {
                q,
                qmode,
                item_type,
                tag,
                limit,
                offset,
            } => {
                handle_library_query(config, q, qmode, item_type, tag, limit, offset, output)
                    .await?
            }
            LibraryAction::Collections {
                top_only,
                limit,
                offset,
            } => handle_library_collections(config, top_only, limit, offset, output).await?,
            LibraryAction::Read {
                item_key,
                attachment_key,
                max_chars_per_chunk,
            } => {
                handle_library_read(
                    config,
                    item_key,
                    attachment_key,
                    max_chars_per_chunk,
                    output,
                )
                .await?
            }
            LibraryAction::ReadSearch {
                q,
                qmode,
                item_type,
                tag,
                result_index,
                search_limit,
                max_chars_per_chunk,
            } => {
                handle_library_read_search(
                    config,
                    q,
                    qmode,
                    item_type,
                    tag,
                    result_index,
                    search_limit,
                    max_chars_per_chunk,
                    output,
                )
                .await?
            }
        },
        Some(Command::Item { action }) => match action {
            ItemAction::Validate { file, online } => {
                handle_item_validate(config, file, online, output).await?
            }
            ItemAction::Create { file } => handle_item_create(config, file, output).await?,
            ItemAction::Update { file } => handle_item_update(config, file, output).await?,
            ItemAction::Delete { file } => handle_item_delete(config, file, output).await?,
        },
        Some(Command::Collection { action }) => match action {
            CollectionAction::Create {
                name,
                parent_collection,
            } => handle_collection_create(config, name, parent_collection, output).await?,
            CollectionAction::Update { file } => {
                handle_collection_update(config, file, output).await?
            }
            CollectionAction::Delete { file } => {
                handle_collection_delete(config, file, output).await?
            }
        },
        Some(Command::Papers { action }) => match action {
            PapersAction::Search {
                q,
                positional_query,
                per_source,
                sources,
                timeout_ms,
                cache,
                offset,
                limit,
                detail,
                abstract_max_chars,
            } => {
                handle_papers_search(
                    config,
                    PapersSearchArgs {
                        q: normalize_papers_query(q, positional_query)?,
                        per_source,
                        sources,
                        timeout_ms,
                        cache,
                        offset,
                        limit,
                        detail,
                        abstract_max_chars,
                    },
                    output,
                )
                .await?
            }
            PapersAction::Open {
                hit_id,
                doi,
                arxiv_id,
                item_key,
                paper_id,
                attachment_key,
                url,
                want,
                max_chars,
                selector,
            } => {
                handle_papers_open(
                    config,
                    hit_id,
                    doi,
                    arxiv_id,
                    item_key,
                    paper_id,
                    attachment_key,
                    url,
                    want,
                    max_chars,
                    selector,
                    output,
                )
                .await?
            }
            PapersAction::ResolveDoi { doi } => {
                handle_papers_resolve_doi(config, doi, output).await?
            }
            PapersAction::Structure { key, attachment } => {
                handle_paper_structure(config, key, attachment, output).await?
            }
            PapersAction::Query {
                key,
                selector,
                attachment,
            } => handle_paper_query(config, key, selector, attachment, output).await?,
            PapersAction::Skill { key, attachment } => {
                handle_paper_skill(config, key, attachment).await?
            }
        },

        Some(Command::Paper { action }) => {
            warn!(
                "'paper' is deprecated; use 'paperbridge papers structure' or 'paperbridge papers query' instead"
            );
            match action {
                PaperAction::Structure { key, attachment } => {
                    handle_paper_structure(config, key, attachment, output).await?
                }
                PaperAction::Query {
                    key,
                    selector,
                    attachment,
                } => handle_paper_query(config, key, selector, attachment, output).await?,
            }
        }

        Some(Command::Paperseed { action }) => handle_paperseed(config, action, output).await?,

        // ---------- Hidden legacy aliases (delegate + deprecation warning) ----------
        Some(Command::Query {
            q,
            qmode,
            item_type,
            tag,
            limit,
            start,
        }) => {
            warn!("'query' is deprecated; use 'paperbridge library query' instead");
            handle_library_query(config, q, qmode, item_type, tag, limit, start, output).await?;
        }
        Some(Command::Collections {
            top_only,
            limit,
            start,
        }) => {
            warn!("'collections' is deprecated; use 'paperbridge library collections' instead");
            handle_library_collections(config, top_only, limit, start, output).await?;
        }
        Some(Command::Read {
            item_key,
            attachment_key,
            max_chars_per_chunk,
        }) => {
            warn!("'read' is deprecated; use 'paperbridge library read' instead");
            handle_library_read(
                config,
                item_key,
                attachment_key,
                max_chars_per_chunk,
                output,
            )
            .await?;
        }
        Some(Command::ReadSearch {
            q,
            qmode,
            item_type,
            tag,
            result_index,
            search_limit,
            max_chars_per_chunk,
        }) => {
            warn!("'read-search' is deprecated; use 'paperbridge library read-search' instead");
            handle_library_read_search(
                config,
                q,
                qmode,
                item_type,
                tag,
                result_index,
                search_limit,
                max_chars_per_chunk,
                output,
            )
            .await?;
        }
        Some(Command::CreateCollection {
            name,
            parent_collection,
        }) => {
            warn!("'create-collection' is deprecated; use 'paperbridge collection create' instead");
            handle_collection_create(config, name, parent_collection, output).await?;
        }
        Some(Command::UpdateCollection { file }) => {
            warn!("'update-collection' is deprecated; use 'paperbridge collection update' instead");
            handle_collection_update(config, file, output).await?;
        }
        Some(Command::DeleteCollection { file }) => {
            warn!("'delete-collection' is deprecated; use 'paperbridge collection delete' instead");
            handle_collection_delete(config, file, output).await?;
        }
        Some(Command::ValidateItem { file, online }) => {
            warn!("'validate-item' is deprecated; use 'paperbridge item validate' instead");
            handle_item_validate(config, file, online, output).await?;
        }
        Some(Command::CreateItem { file }) => {
            warn!("'create-item' is deprecated; use 'paperbridge item create' instead");
            handle_item_create(config, file, output).await?;
        }
        Some(Command::UpdateItem { file }) => {
            warn!("'update-item' is deprecated; use 'paperbridge item update' instead");
            handle_item_update(config, file, output).await?;
        }
        Some(Command::DeleteItem { file }) => {
            warn!("'delete-item' is deprecated; use 'paperbridge item delete' instead");
            handle_item_delete(config, file, output).await?;
        }
        Some(Command::BackendInfo) => {
            warn!("'backend-info' is deprecated; use 'paperbridge status' instead");
            handle_status(config, output).await?;
        }
        Some(Command::SearchPapers {
            q,
            query,
            limit,
            sources,
            timeout_ms,
            cache,
        }) => {
            warn!("'search-papers' is deprecated; use 'paperbridge papers search' instead");
            handle_papers_search(
                config,
                PapersSearchArgs {
                    q: normalize_papers_query(q, query)?,
                    per_source: limit,
                    sources,
                    timeout_ms,
                    cache,
                    offset: None,
                    limit: None,
                    detail: None,
                    abstract_max_chars: None,
                },
                output,
            )
            .await?;
        }
        Some(Command::ResolveDoi { doi }) => {
            warn!("'resolve-doi' is deprecated; use 'paperbridge papers resolve-doi' instead");
            handle_papers_resolve_doi(config, doi, output).await?;
        }

        Some(Command::Serve) | None => {
            run_stdio(config).await?;
        }
        Some(Command::Config {
            action: ConfigAction::Path,
        }) => unreachable!("path command handled before config load"),
        Some(Command::Config {
            action: ConfigAction::Snippet { .. },
        }) => unreachable!("snippet command handled before config load"),
        Some(Command::Config {
            action: ConfigAction::Init { .. },
        }) => unreachable!("init command handled before config load"),
        Some(Command::Config {
            action: ConfigAction::Get { .. },
        }) => unreachable!("get command handled before config load"),
        Some(Command::Config {
            action: ConfigAction::Set { .. },
        }) => unreachable!("set command handled before config load"),
        Some(Command::Config {
            action: ConfigAction::ResolveUserId { .. },
        }) => unreachable!("resolve-user-id command handled before config load"),
        Some(Command::Config {
            action: ConfigAction::Doctor { .. },
        }) => unreachable!("doctor command handled before config load"),
        Some(Command::Completions { .. }) => {
            unreachable!("completions command handled before config load")
        }
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    config_path: String,
    config_exists: bool,
    status: DoctorStatus,
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    id: &'static str,
    level: DoctorLevel,
    message: String,
    next: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
enum DoctorLevel {
    Info,
    Warning,
    Error,
}

fn handle_config_doctor(
    output: OutputFormat,
    verbose: bool,
    setup: bool,
) -> paperbridge::Result<()> {
    let path = Config::config_path();
    let mut config_exists = path.exists();
    let mut raw = if config_exists {
        Some(std::fs::read_to_string(&path).map_err(|e| {
            paperbridge::ZoteroMcpError::Config(format!(
                "Failed to read config at {}: {e}",
                path.display()
            ))
        })?)
    } else {
        None
    };
    let mut config = Config::load_file_or_default()?;
    if setup {
        run_doctor_setup(&mut config, raw.as_deref().unwrap_or_default())?;
        config.write_to_file()?;
        raw = Some(toml::to_string_pretty(&config)?);
        config_exists = true;
        if output.is_json() {
            eprintln!("Updated {}", path.display());
        } else {
            println!("Updated {}", path.display());
        }
    }
    let mut checks = Vec::new();

    if !config_exists {
        checks.push(DoctorCheck {
            id: "config.missing",
            level: DoctorLevel::Warning,
            message: "No config file exists; Paperbridge is using compiled defaults.".to_string(),
            next: vec!["paperbridge config init".to_string()],
        });
    }

    if let Err(error) = config.validate() {
        checks.push(DoctorCheck {
            id: "config.validate",
            level: DoctorLevel::Error,
            message: error.to_string(),
            next: vec![
                "paperbridge config init --interactive".to_string(),
                "paperbridge config doctor".to_string(),
            ],
        });
    } else {
        checks.push(DoctorCheck {
            id: "config.validate",
            level: DoctorLevel::Info,
            message: "Core Paperbridge config validates.".to_string(),
            next: vec![],
        });
    }

    let raw = raw.as_deref().unwrap_or_default();
    for key in [
        "paperseed_enabled",
        "paperseed_auto_download",
        "paperseed_yams_enabled",
    ] {
        if config_exists && !toml_mentions_key(raw, key) {
            checks.push(DoctorCheck {
                id: "paperseed.config-drift",
                level: DoctorLevel::Warning,
                message: format!(
                    "Config does not mention `{key}`; this may be an older config created before Paperseed integration."
                ),
                next: vec![
                    format!("paperbridge config set {key} <value>"),
                    "paperbridge config get".to_string(),
                ],
            });
        }
    }

    let yams_health = paperseed::yams::yams_health("yams");
    let yams_ready = config.paperseed_yams_enabled && yams_health.ready();
    if config_exists && !toml_mentions_key(raw, "paperseed_corpus_root") && !yams_ready {
        checks.push(DoctorCheck {
            id: "paperseed.config-drift",
            level: DoctorLevel::Warning,
            message: "Config does not mention `paperseed_corpus_root`; set it when not using a ready YAMS daemon.".to_string(),
            next: vec![
                "paperbridge config doctor --setup".to_string(),
                "paperbridge config set paperseed_corpus_root <path>".to_string(),
            ],
        });
    }

    if config.paperseed_enabled {
        checks.push(DoctorCheck {
            id: "paperseed.enabled",
            level: DoctorLevel::Info,
            message: "Paperseed mirroring is enabled for Paperbridge paper search results."
                .to_string(),
            next: vec!["paperbridge paperseed corpus status".to_string()],
        });
    } else {
        checks.push(DoctorCheck {
            id: "paperseed.disabled",
            level: DoctorLevel::Warning,
            message: "Paperseed mirroring is disabled; open-access paper results will not be cached automatically.".to_string(),
            next: vec!["paperbridge config set paperseed_enabled true".to_string()],
        });
    }

    if config.paperseed_enabled && !config.paperseed_auto_download {
        checks.push(DoctorCheck {
            id: "paperseed.auto-download-disabled",
            level: DoctorLevel::Warning,
            message: "Paperseed is enabled but automatic OA PDF download is disabled.".to_string(),
            next: vec!["paperbridge config set paperseed_auto_download true".to_string()],
        });
    }

    let corpus_root = config
        .paperseed_corpus_root
        .clone()
        .unwrap_or_else(|| paperseed::app::default_corpus_root().display().to_string());
    checks.push(DoctorCheck {
        id: "paperseed.corpus-root",
        level: DoctorLevel::Info,
        message: format!("Paperseed corpus root resolves to `{corpus_root}`."),
        next: vec!["paperbridge paperseed corpus status".to_string()],
    });

    checks.push(DoctorCheck {
        id: "paperseed.yams",
        level: DoctorLevel::Info,
        message: if !config.paperseed_yams_enabled {
            "Experimental YAMS integration is disabled; Paperseed will use the local corpus only."
                .to_string()
        } else if yams_health.ready() {
            "Experimental YAMS integration is enabled and the YAMS daemon is running; Paperseed will try YAMS retrieval first with local fallback.".to_string()
        } else if yams_health.binary_available {
            "Experimental YAMS integration is enabled, but the YAMS daemon is not running; Paperseed will use the local corpus fallback.".to_string()
        } else {
            "Experimental YAMS integration is enabled, but `yams` was not detected; Paperseed will use the local corpus fallback.".to_string()
        },
        next: vec![
            "yams daemon start".to_string(),
            "paperbridge config doctor --setup".to_string(),
        ],
    });

    let status = if checks.iter().any(|check| check.level == DoctorLevel::Error) {
        DoctorStatus::Error
    } else if checks
        .iter()
        .any(|check| check.level == DoctorLevel::Warning)
    {
        DoctorStatus::Warning
    } else {
        DoctorStatus::Ok
    };

    let report = DoctorReport {
        config_path: path.display().to_string(),
        config_exists,
        status,
        checks,
    };

    if output.is_json() {
        print_output(&report, output)?;
    } else {
        print_doctor_report(&report, verbose);
    }
    Ok(())
}

fn run_doctor_setup(config: &mut Config, raw: &str) -> paperbridge::Result<()> {
    if !toml_mentions_key(raw, "paperseed_enabled") {
        config.paperseed_enabled =
            prompt_bool("Enable Paperseed OA caching?", config.paperseed_enabled)?;
    }
    if !toml_mentions_key(raw, "paperseed_auto_download") {
        config.paperseed_auto_download = prompt_bool(
            "Auto-download open-access PDFs into Paperseed?",
            config.paperseed_auto_download,
        )?;
    }
    if !toml_mentions_key(raw, "paperseed_yams_enabled") {
        config.paperseed_yams_enabled = prompt_bool(
            "Enable experimental YAMS indexing/search when the daemon is running?",
            config.paperseed_yams_enabled,
        )?;
    }
    let yams_ready = config.paperseed_yams_enabled && paperseed::yams::yams_health("yams").ready();
    if !toml_mentions_key(raw, "paperseed_corpus_root") && !yams_ready {
        let current = config
            .paperseed_corpus_root
            .clone()
            .unwrap_or_else(|| paperseed::app::default_corpus_root().display().to_string());
        let value = prompt_string("Paperseed corpus root", &current)?;
        config.paperseed_corpus_root = Some(value);
    } else if config.paperseed_yams_enabled && yams_ready && config.paperseed_corpus_root.is_none()
    {
        println!("YAMS daemon detected; using YAMS retrieval with local fallback defaults.");
    }
    Ok(())
}

fn prompt_bool(prompt: &str, default: bool) -> paperbridge::Result<bool> {
    let suffix = if default { "Y/n" } else { "y/N" };
    let value = prompt_string(&format!("{prompt} [{suffix}]"), "")?;
    match value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "y" | "yes" | "true" | "1" | "on" => Ok(true),
        "n" | "no" | "false" | "0" | "off" => Ok(false),
        other => Err(paperbridge::ZoteroMcpError::InvalidInput(format!(
            "Expected yes/no for `{prompt}`, got `{other}`"
        ))),
    }
}

fn prompt_string(prompt: &str, default: &str) -> paperbridge::Result<String> {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    io::stdout()
        .flush()
        .map_err(|e| paperbridge::ZoteroMcpError::Config(e.to_string()))?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| paperbridge::ZoteroMcpError::Config(e.to_string()))?;
    let input = input.trim();
    if input.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(input.to_string())
    }
}

fn toml_mentions_key(raw: &str, key: &str) -> bool {
    raw.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with('#')
            && line.starts_with(key)
            && line[key.len()..].trim_start().starts_with('=')
    })
}

fn print_doctor_report(report: &DoctorReport, verbose: bool) {
    let warnings = report
        .checks
        .iter()
        .filter(|check| check.level == DoctorLevel::Warning)
        .count();
    let errors = report
        .checks
        .iter()
        .filter(|check| check.level == DoctorLevel::Error)
        .count();
    println!(
        "Paperbridge doctor: {:?} ({} issue{})",
        report.status,
        warnings + errors,
        if warnings + errors == 1 { "" } else { "s" }
    );

    let config_drift: Vec<&DoctorCheck> = report
        .checks
        .iter()
        .filter(|check| check.id == "paperseed.config-drift")
        .collect();
    if !config_drift.is_empty() {
        let missing = config_drift
            .iter()
            .filter_map(|check| check.message.split('`').nth(1))
            .collect::<Vec<_>>()
            .join(", ");
        println!("- Missing Paperseed config: {missing}");
        println!("  Run: paperbridge config doctor --setup");
    }

    for check in report
        .checks
        .iter()
        .filter(|check| check.id != "paperseed.config-drift")
    {
        match check.level {
            DoctorLevel::Error => {
                println!("- Error: {}", check.message);
                if let Some(cmd) = check.next.first() {
                    println!("  Run: {cmd}");
                }
            }
            DoctorLevel::Warning => {
                println!("- {}", check.message);
                if let Some(cmd) = check.next.first() {
                    println!("  Run: {cmd}");
                }
            }
            DoctorLevel::Info if verbose => {
                println!("- {}", check.message);
            }
            DoctorLevel::Info => {}
        }
    }

    if !verbose {
        println!("Advanced: paperbridge config doctor --verbose | --json");
    }
}

// ---------- Shared handlers (used by canonical + legacy dispatch arms) ----------

async fn handle_status(config: Config, output: OutputFormat) -> paperbridge::Result<()> {
    let update_check_enabled = config.update_check_enabled;
    let service = build_service(config)?;
    print_output(&service.backend_info(), output)?;
    if update_check_enabled {
        let info = paperbridge::update::check_for_update().await;
        paperbridge::update::print_nag(info.as_ref());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_library_query(
    config: Config,
    q: Option<String>,
    qmode: Option<String>,
    item_type: Option<String>,
    tag: Option<String>,
    limit: Option<u32>,
    start: Option<u32>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let results = service
        .search_items_page(SearchItemsQuery {
            q,
            qmode,
            item_type,
            tag,
            limit: limit.unwrap_or(10),
            start: start.unwrap_or(0),
        })
        .await?;
    print_output(&results, output)
}

async fn handle_library_collections(
    config: Config,
    top_only: bool,
    limit: Option<u32>,
    start: Option<u32>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let results = service
        .list_collections_page(ListCollectionsQuery {
            top_only,
            limit: limit.unwrap_or(10),
            start: start.unwrap_or(0),
        })
        .await?;
    print_output(&results, output)
}

async fn handle_library_read(
    config: Config,
    item_key: String,
    attachment_key: Option<String>,
    max_chars_per_chunk: Option<usize>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload = service
        .prepare_item_for_vox(PrepareItemForVoxRequest {
            item_key,
            attachment_key,
            max_chars_per_chunk,
        })
        .await?;
    print_output(&payload, output)
}

#[allow(clippy::too_many_arguments)]
async fn handle_library_read_search(
    config: Config,
    q: String,
    qmode: Option<String>,
    item_type: Option<String>,
    tag: Option<String>,
    result_index: Option<usize>,
    search_limit: Option<u32>,
    max_chars_per_chunk: Option<usize>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload = service
        .prepare_search_result_for_vox(PrepareSearchResultForVoxRequest {
            q,
            qmode,
            item_type,
            tag,
            result_index,
            search_limit,
            max_chars_per_chunk,
        })
        .await?;
    print_output(&payload, output)
}

async fn handle_item_validate(
    config: Config,
    file: String,
    online: bool,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload: ItemWriteRequest = read_json_file(&file)?;
    let report = if online {
        service.validate_item_online(&payload).await?
    } else {
        service.validate_item_request(&payload)
    };
    print_output(&report, output)
}

async fn handle_item_create(
    config: Config,
    file: String,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload: ItemWriteRequest = read_json_file(&file)?;
    let created = service.create_item(payload).await?;
    print_output(&created, output)
}

async fn handle_item_update(
    config: Config,
    file: String,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload: ItemUpdateRequest = read_json_file(&file)?;
    let updated = service.update_item(payload).await?;
    print_output(&updated, output)
}

async fn handle_item_delete(
    config: Config,
    file: String,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload: DeleteItemRequest = read_json_file(&file)?;
    service.delete_item(payload).await?;
    print_output(&serde_json::json!({"deleted": true}), output)
}

async fn handle_collection_create(
    config: Config,
    name: String,
    parent_collection: Option<String>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload = service
        .create_collection(CollectionWriteRequest {
            name,
            parent_collection,
        })
        .await?;
    print_output(&payload, output)
}

async fn handle_collection_update(
    config: Config,
    file: String,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload: CollectionUpdateRequest = read_json_file(&file)?;
    let updated = service.update_collection(payload).await?;
    print_output(&updated, output)
}

async fn handle_collection_delete(
    config: Config,
    file: String,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload: DeleteCollectionRequest = read_json_file(&file)?;
    service.delete_collection(payload).await?;
    print_output(&serde_json::json!({"deleted": true}), output)
}

struct PapersSearchArgs {
    q: String,
    per_source: Option<u32>,
    sources: Option<Vec<PaperSource>>,
    timeout_ms: Option<u64>,
    cache: Option<SearchCacheMode>,
    offset: Option<u32>,
    limit: Option<u32>,
    detail: Option<SearchDetail>,
    abstract_max_chars: Option<usize>,
}

async fn handle_papers_search(
    config: Config,
    args: PapersSearchArgs,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let opts = SearchOptions {
        query: args.q,
        limit_per_source: args.per_source.unwrap_or(10),
        sources: args.sources,
        timeout_ms: args.timeout_ms.unwrap_or(8000),
        offset: args.offset.unwrap_or(0),
        limit: args
            .limit
            .unwrap_or(paperbridge::external::DEFAULT_PAGE_LIMIT),
        cache_mode: args.cache.unwrap_or(SearchCacheMode::Auto),
        detail: args.detail.unwrap_or(SearchDetail::Compact),
        abstract_max_chars: args.abstract_max_chars,
    };
    let result = service.search_papers(opts).await?;
    print_output(&result, output)
}

#[allow(clippy::too_many_arguments)]
async fn handle_papers_open(
    config: Config,
    hit_id: Option<String>,
    doi: Option<String>,
    arxiv_id: Option<String>,
    item_key: Option<String>,
    paper_id: Option<String>,
    attachment_key: Option<String>,
    url: Option<String>,
    want: Option<Vec<String>>,
    max_chars: Option<usize>,
    selector: Option<String>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let result = service
        .open_paper(OpenPaperRequest {
            hit_id,
            doi,
            arxiv_id,
            item_key,
            paper_id,
            attachment_key,
            url,
            want: want.unwrap_or_else(|| vec!["metadata".into()]),
            max_chars,
            selector,
            max_chars_per_chunk: None,
        })
        .await?;
    print_output(&result, output)
}

fn normalize_papers_query(
    q: Option<String>,
    positional_query: Option<String>,
) -> paperbridge::Result<String> {
    let query = q.or(positional_query).ok_or_else(|| {
        paperbridge::ZoteroMcpError::InvalidInput(
            "Missing search query. Use `paperbridge papers search -q \"...\"` or `paperbridge papers search \"...\"`.".to_string(),
        )
    })?;
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(paperbridge::ZoteroMcpError::InvalidInput(
            "Search query must not be empty.".to_string(),
        ));
    }
    Ok(query)
}

async fn handle_papers_resolve_doi(
    config: Config,
    doi: String,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let work = service.resolve_doi(&doi).await?;
    print_output(&work, output)
}

async fn handle_paper_structure(
    config: Config,
    key: String,
    attachment: Option<String>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let structure = service
        .get_paper_structure(&key, attachment.as_deref())
        .await?;
    print_output(&structure, output)
}

async fn handle_paper_query(
    config: Config,
    key: String,
    selector: String,
    attachment: Option<String>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let value = service
        .query_paper(&key, &selector, attachment.as_deref())
        .await?;
    print_output(&value, output)
}

async fn handle_paper_skill(
    config: Config,
    key: String,
    attachment: Option<String>,
) -> paperbridge::Result<()> {
    let service = build_service(config)?;
    let payload = service
        .prepare_paper_for_skill(&key, attachment.as_deref())
        .await?;
    // Print the SKILL.md scaffold directly so it can be piped into a file.
    println!("{}", payload.markdown);
    Ok(())
}

async fn handle_paperseed(
    config: Config,
    action: PaperseedAction,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let api = build_paperseed_api(&config);
    match action {
        PaperseedAction::Corpus { action } => match action {
            PaperseedCorpusAction::Status => {
                print_output(&api.corpus_status()?, output)?;
            }
            PaperseedCorpusAction::Import {
                file,
                title,
                license,
            } => {
                let paper = api.import_local_file(file, title, license)?;
                print_output(&paper, output)?;
            }
            PaperseedCorpusAction::Ingest {
                metadata,
                file,
                license,
            } => {
                let raw = std::fs::read_to_string(&metadata).map_err(|e| {
                    paperbridge::ZoteroMcpError::Config(format!(
                        "Failed to read metadata file {metadata}: {e}"
                    ))
                })?;
                let metadata = paperseed::sources::metadata_from_paperbridge_json(&raw)?;
                let paper = api.ingest_with_metadata(file, metadata, license)?;
                print_output(&paper, output)?;
            }
            PaperseedCorpusAction::Query { q } => {
                print_output(&api.query_corpus(&q)?, output)?;
            }
            PaperseedCorpusAction::Export { format } => {
                let db = api.corpus_status()?;
                match (format, output) {
                    (None | Some(PaperseedExportFormat::Json), OutputFormat::Json) => {
                        print_json(&db)?
                    }
                    (None | Some(PaperseedExportFormat::Bibtex), OutputFormat::Human) => {
                        println!("{}", paperseed::app::export_bibtex(&db));
                    }
                    (Some(PaperseedExportFormat::Json), OutputFormat::Human) => {
                        return Err(paperbridge::ZoteroMcpError::InvalidInput(
                            "JSON corpus export requires --json. Try: paperbridge paperseed corpus export --json"
                                .to_string(),
                        ));
                    }
                    (Some(PaperseedExportFormat::Bibtex), OutputFormat::Json) => {
                        return Err(paperbridge::ZoteroMcpError::InvalidInput(
                            "--format bibtex conflicts with --json. Remove one of those flags."
                                .to_string(),
                        ));
                    }
                }
            }
        },
        PaperseedAction::Seed { action } => match action {
            PaperseedSeedAction::Check { paper_id } => {
                let reason = paperseed::app::seed_check(api.paths(), &paper_id)
                    .map_err(paperbridge::paperseed_api::map_error)?;
                print_output(
                    &serde_json::json!({
                        "paper_id": paper_id,
                        "allowed": true,
                        "reason": reason,
                    }),
                    output,
                )?;
            }
            PaperseedSeedAction::Create { paper_id } => {
                print_output(&api.create_seed_manifest(&paper_id)?, output)?;
            }
        },
    }
    Ok(())
}

fn build_paperseed_api(config: &Config) -> paperbridge::paperseed_api::PaperseedApi {
    let yams = if config.paperseed_yams_enabled {
        paperseed::yams::YamsConfig::auto_detect()
    } else {
        paperseed::yams::YamsConfig::disabled()
    };
    match &config.paperseed_corpus_root {
        Some(root) => paperbridge::paperseed_api::PaperseedApi::with_yams(
            root.clone(),
            config.unpaywall_email.clone(),
            yams,
        ),
        None => paperbridge::paperseed_api::PaperseedApi::default_with_yams(
            config.unpaywall_email.clone(),
            yams,
        ),
    }
}

fn read_json_file<T: serde::de::DeserializeOwned>(file: &str) -> paperbridge::Result<T> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}")))
}

fn build_service(config: Config) -> paperbridge::Result<PaperbridgeService> {
    let keys = paperbridge::external::PaperSearchKeys {
        hf_token: config.hf_token.clone(),
        s2_api_key: config.semantic_scholar_api_key.clone(),
        core_api_key: config.core_api_key.clone(),
        ads_api_token: config.ads_api_token.clone(),
        ncbi_api_key: config.ncbi_api_key.clone(),
        scholarapi_key: config.scholarapi_key.clone(),
        unpaywall_email: config.unpaywall_email.clone(),
    };
    let unpaywall_email = config.unpaywall_email.clone();
    let paper_config = paperbridge::service::PaperConfig {
        grobid_url: config.grobid_url.clone(),
        grobid_auto_spawn: config.grobid_auto_spawn,
        grobid_image: config.grobid_image.clone(),
        grobid_timeout_secs: config.grobid_timeout_secs,
    };
    let paperseed_enabled = config.paperseed_enabled;
    let paperseed_config = PaperseedMirrorConfig {
        corpus_root: config.paperseed_corpus_root.clone(),
        unpaywall_email: config.unpaywall_email.clone(),
        auto_download: config.paperseed_auto_download,
        yams_enabled: config.paperseed_yams_enabled,
    };
    let paper_search = paperbridge::external::PaperSearch::with_keys_struct(keys);
    let backend = build_backend(config)?;
    let service = PaperbridgeService::with_paper_search(backend, paper_search)
        .with_unpaywall(unpaywall_email)
        .with_paper_config(paper_config);
    Ok(if paperseed_enabled {
        service.with_paperseed(paperseed_config)
    } else {
        service
    })
}

async fn run_stdio(config: Config) -> paperbridge::Result<()> {
    eprintln!("paperbridge ready (stdio)");
    let service = build_service(config)?;
    let server = PaperbridgeServer::new(service);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| paperbridge::ZoteroMcpError::Http(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| paperbridge::ZoteroMcpError::Http(e.to_string()))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Serialize, Eq, PartialEq)]
struct CliErrorEnvelope {
    error: &'static str,
    reason: String,
    #[serde(rename = "try")]
    suggestions: Vec<String>,
}

fn print_runtime_error(error: &paperbridge::ZoteroMcpError, output: OutputFormat) {
    if output.is_json() {
        let envelope = cli_error_envelope(error);
        match serde_json::to_string_pretty(&envelope) {
            Ok(json) => eprintln!("{json}"),
            Err(_) => eprintln!(
                "{}",
                serde_json::json!({
                    "error": "serialization_error",
                    "reason": "Failed to render the structured runtime error.",
                    "try": [],
                })
            ),
        }
    } else {
        eprintln!("Error: {error}");
    }
}

fn cli_error_envelope(error: &paperbridge::ZoteroMcpError) -> CliErrorEnvelope {
    use paperbridge::ZoteroMcpError;

    let (code, reason, suggestions): (&'static str, String, Vec<String>) = match error {
        ZoteroMcpError::Config(reason) => (
            "configuration_error",
            reason.clone(),
            commands(&["paperbridge config doctor", "paperbridge config validate"]),
        ),
        ZoteroMcpError::MissingConfig(key) => (
            "missing_configuration",
            format!("Required configuration key '{key}' is not set."),
            vec![
                format!("paperbridge config set {key} <value>"),
                "paperbridge config init --interactive".to_string(),
            ],
        ),
        ZoteroMcpError::InvalidInput(reason) => (
            "invalid_input",
            reason.clone(),
            invalid_input_suggestions(reason),
        ),
        ZoteroMcpError::Http(reason) => (
            "http_error",
            reason.clone(),
            commands(&["paperbridge status", "paperbridge config doctor"]),
        ),
        ZoteroMcpError::Api { status, message } => (
            "zotero_api_error",
            format!("Zotero API returned HTTP {status}: {message}"),
            commands(api_error_suggestions(*status)),
        ),
        ZoteroMcpError::Serde(reason) => (
            "serialization_error",
            reason.clone(),
            commands(&["paperbridge config doctor", "paperbridge --help"]),
        ),
    };

    CliErrorEnvelope {
        error: code,
        reason: redact_sensitive_values(&reason),
        suggestions,
    }
}

fn commands(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn invalid_input_suggestions(reason: &str) -> Vec<String> {
    if reason.contains("Unknown config key") {
        commands(&["paperbridge config get", "paperbridge config --help"])
    } else if reason.contains("JSON corpus export requires --json") {
        commands(&["paperbridge paperseed corpus export --json"])
    } else if reason.contains("--format bibtex conflicts with --json") {
        commands(&[
            "paperbridge paperseed corpus export --format bibtex",
            "paperbridge paperseed corpus export --json",
        ])
    } else if reason.contains("search query") || reason.contains("Search query") {
        commands(&["paperbridge papers search --help"])
    } else {
        commands(&["paperbridge --help"])
    }
}

fn api_error_suggestions(status: u16) -> &'static [&'static str] {
    match status {
        401 | 403 => &[
            "paperbridge config set api_key <your-key>",
            "paperbridge config validate",
        ],
        404 => &[
            "paperbridge papers search --help",
            "paperbridge library query --help",
        ],
        412 => &[
            "paperbridge library query --help",
            "paperbridge item update --help",
        ],
        429 => &["paperbridge status"],
        _ => &["paperbridge status", "paperbridge config doctor"],
    }
}

fn redact_sensitive_values(reason: &str) -> String {
    const SECRET_ENV_KEYS: &[&str] = &[
        "PAPERBRIDGE_API_KEY",
        "PAPERBRIDGE_HF_TOKEN",
        "PAPERBRIDGE_SEMANTIC_SCHOLAR_API_KEY",
        "PAPERBRIDGE_CORE_API_KEY",
        "PAPERBRIDGE_ADS_API_TOKEN",
        "PAPERBRIDGE_NCBI_API_KEY",
        "PAPERBRIDGE_SCHOLARAPI_KEY",
        "ZOTERO_MCP_API_KEY",
        "HF_TOKEN",
        "SEMANTIC_SCHOLAR_API_KEY",
        "CORE_API_KEY",
        "ADS_API_TOKEN",
        "NCBI_API_KEY",
        "SCHOLARAPI_KEY",
    ];

    let mut redacted = reason.to_string();
    for key in SECRET_ENV_KEYS {
        if let Ok(secret) = std::env::var(key)
            && !secret.is_empty()
        {
            redacted = redacted.replace(&secret, "<redacted>");
        }
    }
    redacted
}

impl OutputFormat {
    fn new(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }

    fn is_json(self) -> bool {
        self == Self::Json
    }
}

fn print_output<T: Serialize>(value: &T, output: OutputFormat) -> paperbridge::Result<()> {
    if output.is_json() {
        return print_json(value);
    }

    let value = serde_json::to_value(value)
        .map_err(|e| paperbridge::ZoteroMcpError::Serde(e.to_string()))?;
    print!("{}", render_human(&value));
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> paperbridge::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| paperbridge::ZoteroMcpError::Serde(e.to_string()))?;
    println!("{json}");
    Ok(())
}

fn render_human(value: &serde_json::Value) -> String {
    let mut output = String::new();
    render_human_value(value, 0, &mut output);
    output
}

fn render_human_value(value: &serde_json::Value, indent: usize, output: &mut String) {
    match value {
        serde_json::Value::Object(fields) => {
            if fields.is_empty() {
                push_indented_line(output, indent, "(none)");
                return;
            }
            for (key, value) in fields {
                if is_inline_value(value) {
                    push_indented_line(output, indent, &format!("{key}: {}", inline_value(value)));
                } else if is_empty_collection(value) {
                    push_indented_line(output, indent, &format!("{key}: (none)"));
                } else {
                    push_indented_line(output, indent, &format!("{key}:"));
                    render_human_value(value, indent + 2, output);
                }
            }
        }
        serde_json::Value::Array(values) => {
            if values.is_empty() {
                push_indented_line(output, indent, "(none)");
                return;
            }
            for value in values {
                if is_inline_value(value) {
                    push_indented_line(output, indent, &format!("- {}", inline_value(value)));
                } else {
                    push_indented_line(output, indent, "-");
                    render_human_value(value, indent + 2, output);
                }
            }
        }
        serde_json::Value::String(text) if text.contains('\n') => {
            for line in text.lines() {
                push_indented_line(output, indent, line);
            }
        }
        _ => push_indented_line(output, indent, &inline_value(value)),
    }
}

fn is_inline_value(value: &serde_json::Value) -> bool {
    !matches!(
        value,
        serde_json::Value::Array(_) | serde_json::Value::Object(_)
    ) && !matches!(value, serde_json::Value::String(text) if text.contains('\n'))
}

fn is_empty_collection(value: &serde_json::Value) -> bool {
    matches!(value, serde_json::Value::Array(values) if values.is_empty())
        || matches!(value, serde_json::Value::Object(fields) if fields.is_empty())
}

fn inline_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "(none)".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) if value.is_empty() => "(empty)".to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            unreachable!("collections are rendered recursively")
        }
    }
}

fn push_indented_line(output: &mut String, indent: usize, line: &str) {
    output.push_str(&" ".repeat(indent));
    output.push_str(line);
    output.push('\n');
}

fn print_client_snippet(target: SnippetTarget, binary_path: Option<&str>) {
    let bin = binary_path.unwrap_or("paperbridge");
    let snippet = match target {
        SnippetTarget::Opencode => serde_json::json!({
            "mcp": {
                "paperbridge": {
                    "type": "local",
                    "command": [bin, "serve"],
                    "environment": {
                        "PAPERBRIDGE_LIBRARY_TYPE": "user",
                        "PAPERBRIDGE_USER_ID": "<your-user-id>",
                        "PAPERBRIDGE_API_KEY": "<optional-api-key>"
                    }
                }
            }
        }),
        SnippetTarget::Claude => serde_json::json!({
            "mcpServers": {
                "paperbridge": {
                    "command": bin,
                    "args": ["serve"],
                    "env": {
                        "PAPERBRIDGE_LIBRARY_TYPE": "user",
                        "PAPERBRIDGE_USER_ID": "<your-user-id>",
                        "PAPERBRIDGE_API_KEY": "<optional-api-key>"
                    }
                }
            }
        }),
        SnippetTarget::Pi => serde_json::json!({
            "paperbridge": {
                "commands": {
                    "search": [bin, "library", "query", "-q", "<query>", "--limit", "5"],
                    "collections": [bin, "library", "collections", "--top-only"],
                    "read_item": [bin, "library", "read", "--item-key", "<item-key>", "--max-chars-per-chunk", "1200"],
                    "read_search_result": [bin, "library", "read-search", "-q", "<query>", "--result-index", "0", "--max-chars-per-chunk", "1200"]
                }
            }
        }),
    };

    match serde_json::to_string_pretty(&snippet) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("Failed to render snippet JSON: {e}"),
    }
}

async fn handle_config_init(force: bool, interactive: bool) -> paperbridge::Result<()> {
    let path = Config::config_path();

    if !interactive {
        let out = Config::init_file(force)?;
        println!("Initialized config at {}", out.display());
        println!("Edit the file, then run: paperbridge config validate");
        return Ok(());
    }

    if path.exists() && !force {
        return Err(paperbridge::ZoteroMcpError::Config(format!(
            "Config already exists at {} (use --force to overwrite)",
            path.display()
        )));
    }

    let mut cfg = if path.exists() {
        Config::load_file_or_default()?
    } else {
        Config::default()
    };
    let source_default = match cfg.backend_mode {
        paperbridge::config::BackendModeConfig::Cloud => "cloud",
        paperbridge::config::BackendModeConfig::Local => "local",
        paperbridge::config::BackendModeConfig::Hybrid => "hybrid",
    };
    let source = prompt_with_default("Zotero source (cloud/local/hybrid)", source_default)?;
    let source = parse_zotero_source(&source)?;

    match source {
        ZoteroSource::Local => {
            let local_default = cfg.local_api_base.clone();
            cfg.backend_mode = paperbridge::config::BackendModeConfig::Local;
            cfg.local_api_base = prompt_with_default("Local Zotero API base", &local_default)?;
            cfg.set_value("api_key", "unset")?;
            cfg.set_value("library_type", "user")?;
            cfg.set_value("user_id", "0")?;
            cfg.set_value("group_id", "unset")?;
            println!("Configured local mode (library_type=user, user_id=0, api_key=<unset>).",);
        }
        ZoteroSource::Cloud => {
            cfg.backend_mode = paperbridge::config::BackendModeConfig::Cloud;
            cfg.cloud_api_base = prompt_with_default("Zotero API base", &cfg.cloud_api_base)?;

            let api_key_default = if cfg.api_key.is_some() { "<set>" } else { "" };
            let api_key =
                prompt_with_default("API key (optional; enter to keep unset)", api_key_default)?;
            if api_key != "<set>" {
                cfg.set_value("api_key", &api_key)?;
            }

            let library_type =
                prompt_with_default("Library type (user/group)", cfg.library_type.as_str())?;
            cfg.set_value("library_type", &library_type)?;

            match cfg.library_type {
                paperbridge::config::LibraryType::User => {
                    let default_user = cfg.user_id.map(|v| v.to_string()).unwrap_or_default();
                    let login_id = prompt_with_default(
                        "Login ID (username or numeric user ID)",
                        &default_user,
                    )?;
                    let user_id = resolve_user_id_from_login_id(
                        &login_id,
                        &cfg.cloud_api_base,
                        cfg.api_key.as_deref(),
                    )
                    .await?;
                    cfg.set_value("user_id", &user_id.to_string())?;
                    cfg.set_value("group_id", "unset")?;
                }
                paperbridge::config::LibraryType::Group => {
                    let default_group = cfg.group_id.map(|v| v.to_string()).unwrap_or_default();
                    let group_id = prompt_with_default("Group ID", &default_group)?;
                    cfg.set_value("group_id", &group_id)?;
                    cfg.set_value("user_id", "unset")?;
                }
            }
        }
        ZoteroSource::Hybrid => {
            cfg.backend_mode = paperbridge::config::BackendModeConfig::Hybrid;
            cfg.cloud_api_base = prompt_with_default("Cloud Zotero API base", &cfg.cloud_api_base)?;
            cfg.local_api_base = prompt_with_default("Local Zotero API base", &cfg.local_api_base)?;

            let api_key_default = if cfg.api_key.is_some() { "<set>" } else { "" };
            let api_key =
                prompt_with_default("Cloud API key (required for writes)", api_key_default)?;
            if api_key != "<set>" {
                cfg.set_value("api_key", &api_key)?;
            }

            let library_type =
                prompt_with_default("Cloud library type (user/group)", cfg.library_type.as_str())?;
            cfg.set_value("library_type", &library_type)?;

            match cfg.library_type {
                paperbridge::config::LibraryType::User => {
                    let default_user = cfg.user_id.map(|v| v.to_string()).unwrap_or_default();
                    let login_id = prompt_with_default(
                        "Cloud login ID (username or numeric user ID)",
                        &default_user,
                    )?;
                    let user_id = resolve_user_id_from_login_id(
                        &login_id,
                        &cfg.cloud_api_base,
                        cfg.api_key.as_deref(),
                    )
                    .await?;
                    cfg.set_value("user_id", &user_id.to_string())?;
                    cfg.set_value("group_id", "unset")?;
                }
                paperbridge::config::LibraryType::Group => {
                    let default_group = cfg.group_id.map(|v| v.to_string()).unwrap_or_default();
                    let group_id = prompt_with_default("Cloud group ID", &default_group)?;
                    cfg.set_value("group_id", &group_id)?;
                    cfg.set_value("user_id", "unset")?;
                }
            }
        }
    }

    let timeout_default = cfg.timeout_secs.to_string();
    let timeout = prompt_with_default("Timeout seconds", &timeout_default)?;
    cfg.set_value("timeout_secs", &timeout)?;

    cfg.log_level = prompt_with_default("Log level", &cfg.log_level)?;

    println!("\nGROBID provides section-aware paper parsing. It is optional; leave blank to skip.");
    let grobid_default = if cfg.grobid_url.is_some() {
        "<set>"
    } else {
        ""
    };
    let grobid_url = prompt_with_default(
        "GROBID URL (e.g. http://localhost:8070; blank to disable)",
        grobid_default,
    )?;
    if grobid_url != "<set>" {
        let trimmed = grobid_url.trim();
        if trimmed.is_empty() {
            cfg.grobid_url = None;
        } else {
            cfg.grobid_url = Some(trimmed.to_string());
        }
    }

    if cfg.grobid_url.is_none() {
        let auto_default = if cfg.grobid_auto_spawn {
            "true"
        } else {
            "false"
        };
        let auto = prompt_with_default(
            "Auto-spawn local GROBID via docker when needed? (true/false)",
            auto_default,
        )?;
        cfg.set_value("grobid_auto_spawn", auto.trim())?;

        if cfg.grobid_auto_spawn {
            cfg.grobid_image = prompt_with_default("GROBID docker image", &cfg.grobid_image)?;
        }
    }

    cfg.write_to_file()?;
    println!("Initialized config at {}", path.display());
    match cfg.validate() {
        Ok(()) => println!("Config is valid."),
        Err(e) => eprintln!("Config saved, but validation currently fails: {e}"),
    }
    Ok(())
}

const SENSITIVE_CONFIG_KEYS: &[&str] = &[
    "api_key",
    "hf_token",
    "semantic_scholar_api_key",
    "core_api_key",
    "ads_api_token",
    "ncbi_api_key",
    "scholarapi_key",
];

fn safe_config_value(config: &Config) -> paperbridge::Result<serde_json::Value> {
    let mut value = serde_json::to_value(config)
        .map_err(|e| paperbridge::ZoteroMcpError::Serde(e.to_string()))?;
    let fields = value.as_object_mut().ok_or_else(|| {
        paperbridge::ZoteroMcpError::Serde("Config did not serialize to an object".to_string())
    })?;
    for key in SENSITIVE_CONFIG_KEYS {
        let status = if fields.contains_key(*key) {
            "<set>"
        } else {
            "<unset>"
        };
        fields.insert((*key).to_string(), serde_json::Value::String(status.into()));
    }
    Ok(value)
}

fn handle_config_get(
    key: Option<&str>,
    show_secret: bool,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let cfg = Config::load_file_or_default()?;
    if let Some(key) = key {
        let value = cfg.get_value(key).ok_or_else(|| {
            paperbridge::ZoteroMcpError::InvalidInput(format!(
                "Unknown config key '{key}'. Valid keys: backend_mode, cloud_api_base, local_api_base, api_base, api_key, library_type, user_id, group_id, timeout_secs, log_level, hf_token, semantic_scholar_api_key, core_api_key, ads_api_token, ncbi_api_key, scholarapi_key, unpaywall_email, grobid_url, grobid_timeout_secs, grobid_auto_spawn, grobid_image, update_check_enabled, paperseed_enabled, paperseed_auto_download, paperseed_yams_enabled, paperseed_corpus_root"
            ))
        })?;
        if SENSITIVE_CONFIG_KEYS.contains(&key) && !show_secret {
            let value = if value.is_empty() || value == "<unset>" {
                "(unset)".to_string()
            } else {
                format!(
                    "(set, {} chars — pass --show-secret to reveal)",
                    value.len()
                )
            };
            if output.is_json() {
                print_output(
                    &serde_json::json!({"key": key, "value": value, "redacted": true}),
                    output,
                )?;
            } else {
                println!("{value}");
            }
            return Ok(());
        }
        if output.is_json() {
            print_output(
                &serde_json::json!({"key": key, "value": value, "redacted": false}),
                output,
            )?;
        } else {
            println!("{value}");
        }
        return Ok(());
    }

    if output.is_json() {
        print_output(&safe_config_value(&cfg)?, output)?;
    } else {
        println!("{}", cfg.display_safe());
    }
    Ok(())
}

fn handle_config_set(key: &str, value: &str) -> paperbridge::Result<()> {
    let mut cfg = Config::load_file_or_default()?;
    cfg.set_value(key, value)?;
    cfg.write_to_file()?;
    println!("Updated '{}' in {}", key, Config::config_path().display());
    match cfg.validate() {
        Ok(()) => println!("Config is valid."),
        Err(e) => eprintln!("Config saved, but validation currently fails: {e}"),
    }
    Ok(())
}

async fn handle_config_resolve_user_id(
    login: &str,
    api_key_override: Option<&str>,
    api_base_override: Option<&str>,
    output: OutputFormat,
) -> paperbridge::Result<()> {
    let cfg = Config::load_file_or_default()?;
    let api_base = api_base_override.unwrap_or(cfg.active_cloud_api_base());
    let api_key = api_key_override.or(cfg.api_key.as_deref());

    let user_id = resolve_user_id_from_login_id(login, api_base, api_key).await?;
    if output.is_json() {
        print_output(&serde_json::json!({"user_id": user_id}), output)?;
    } else {
        println!("{user_id}");
    }
    Ok(())
}

fn prompt_with_default(prompt: &str, default: &str) -> paperbridge::Result<String> {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    io::stdout()
        .flush()
        .map_err(|e| paperbridge::ZoteroMcpError::Config(format!("Failed to flush stdout: {e}")))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| paperbridge::ZoteroMcpError::Config(format!("Failed to read input: {e}")))?;

    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ZoteroSource {
    Cloud,
    Local,
    Hybrid,
}

fn parse_zotero_source(value: &str) -> paperbridge::Result<ZoteroSource> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cloud" => Ok(ZoteroSource::Cloud),
        "local" => Ok(ZoteroSource::Local),
        "hybrid" => Ok(ZoteroSource::Hybrid),
        other => Err(paperbridge::ZoteroMcpError::InvalidInput(format!(
            "Invalid source '{other}'. Valid values: cloud, local, hybrid"
        ))),
    }
}

async fn resolve_user_id_from_login_id(
    login_id: &str,
    api_base: &str,
    api_key: Option<&str>,
) -> paperbridge::Result<u64> {
    let trimmed = login_id.trim();
    if trimmed.is_empty() {
        return Err(paperbridge::ZoteroMcpError::InvalidInput(
            "Login ID cannot be empty".to_string(),
        ));
    }

    if let Ok(user_id) = trimmed.parse::<u64>() {
        return Ok(user_id);
    }

    if let Ok(user_id) = resolve_user_id_from_username_redirect(trimmed).await {
        return Ok(user_id);
    }

    if let Some(key) = api_key
        && let Ok(user_id) = resolve_user_id_from_api_key(api_base, key).await
    {
        return Ok(user_id);
    }

    Err(paperbridge::ZoteroMcpError::InvalidInput(
        "Could not resolve username to numeric user ID. Use your Zotero API key (recommended) or find the numeric user ID on https://www.zotero.org/settings/keys".to_string(),
    ))
}

async fn resolve_user_id_from_username_redirect(username: &str) -> paperbridge::Result<u64> {
    let url = format!("https://www.zotero.org/{username}");
    paperbridge::security::ensure_secure_transport(&url)?;
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| paperbridge::ZoteroMcpError::Http(e.to_string()))?;

    if let Ok(user_id) = parse_user_id_from_profile_url(response.url().as_str()) {
        return Ok(user_id);
    }

    let body = response
        .text()
        .await
        .map_err(|e| paperbridge::ZoteroMcpError::Http(e.to_string()))?;
    if let Some(user_id) = parse_user_id_from_profile_html(&body) {
        return Ok(user_id);
    }

    Err(paperbridge::ZoteroMcpError::InvalidInput(
        "Could not extract user ID from Zotero profile page".to_string(),
    ))
}

async fn resolve_user_id_from_api_key(api_base: &str, api_key: &str) -> paperbridge::Result<u64> {
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/keys/current");
    paperbridge::security::ensure_secure_transport(&url)?;
    let response = reqwest::Client::new()
        .get(&url)
        .header("Zotero-API-Version", "3")
        .header("Zotero-API-Key", api_key)
        .send()
        .await
        .map_err(|e| paperbridge::ZoteroMcpError::Http(e.to_string()))?;

    if !response.status().is_success() {
        return Err(paperbridge::ZoteroMcpError::Http(format!(
            "API key lookup failed with status {}",
            response.status()
        )));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| paperbridge::ZoteroMcpError::Serde(e.to_string()))?;
    parse_user_id_from_key_response(&json)
}

fn parse_user_id_from_profile_url(url: &str) -> paperbridge::Result<u64> {
    let parsed = url::Url::parse(url).map_err(|e| {
        paperbridge::ZoteroMcpError::InvalidInput(format!("Invalid profile URL '{url}': {e}"))
    })?;

    let mut segments = parsed.path_segments().ok_or_else(|| {
        paperbridge::ZoteroMcpError::InvalidInput(format!("Unexpected profile URL '{url}'"))
    })?;

    while let Some(segment) = segments.next() {
        if segment == "users"
            && let Some(next) = segments.next()
            && let Ok(user_id) = next.parse::<u64>()
        {
            return Ok(user_id);
        }
    }

    Err(paperbridge::ZoteroMcpError::InvalidInput(format!(
        "Could not extract numeric user ID from '{}'",
        parsed.path()
    )))
}

fn parse_user_id_from_key_response(value: &serde_json::Value) -> paperbridge::Result<u64> {
    if let Some(user_id) = value.get("userID").and_then(|v| v.as_u64()) {
        return Ok(user_id);
    }

    if let Some(user_id) = value
        .get("userID")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Ok(user_id);
    }

    Err(paperbridge::ZoteroMcpError::InvalidInput(
        "Could not parse userID from Zotero key response".to_string(),
    ))
}

fn parse_user_id_from_profile_html(html: &str) -> Option<u64> {
    let mut remaining = html;
    while let Some(pos) = remaining.find("\"userID\"") {
        let candidate = &remaining[pos + "\"userID\"".len()..];
        let colon = candidate.find(':')?;
        let mut chars = candidate[colon + 1..].chars().peekable();

        while let Some(ch) = chars.peek() {
            if ch.is_whitespace() || *ch == '"' {
                chars.next();
            } else {
                break;
            }
        }

        let mut digits = String::new();
        while let Some(ch) = chars.peek() {
            if ch.is_ascii_digit() {
                digits.push(*ch);
                chars.next();
            } else {
                break;
            }
        }

        if !digits.is_empty()
            && let Ok(user_id) = digits.parse::<u64>()
        {
            return Some(user_id);
        }

        remaining = &candidate[colon + 1..];
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_id_from_profile_url_works() {
        let user_id =
            parse_user_id_from_profile_url("https://www.zotero.org/users/475425/items").unwrap();
        assert_eq!(user_id, 475425);
    }

    #[test]
    fn parse_user_id_from_key_response_supports_number_and_string() {
        let n = serde_json::json!({"userID": 7});
        let s = serde_json::json!({"userID": "7"});
        assert_eq!(parse_user_id_from_key_response(&n).unwrap(), 7);
        assert_eq!(parse_user_id_from_key_response(&s).unwrap(), 7);
    }

    #[test]
    fn parse_zotero_source_accepts_cloud_and_local() {
        assert_eq!(parse_zotero_source("cloud").unwrap(), ZoteroSource::Cloud);
        assert_eq!(parse_zotero_source("local").unwrap(), ZoteroSource::Local);
        assert_eq!(parse_zotero_source("hybrid").unwrap(), ZoteroSource::Hybrid);
        assert!(parse_zotero_source("other").is_err());
    }

    #[test]
    fn parse_user_id_from_profile_html_works() {
        let html = r#"<script>window.__DATA__ = {"userID":7141888,"foo":"bar"};</script>"#;
        assert_eq!(parse_user_id_from_profile_html(html), Some(7141888));
    }

    #[test]
    fn doctor_key_detection_ignores_comments() {
        let raw = r#"
# paperseed_enabled = true
paperseed_auto_download = true
  paperseed_corpus_root = "/tmp/corpus"
"#;
        assert!(!toml_mentions_key(raw, "paperseed_enabled"));
        assert!(toml_mentions_key(raw, "paperseed_auto_download"));
        assert!(toml_mentions_key(raw, "paperseed_corpus_root"));
    }

    #[test]
    fn human_output_renders_nested_values_without_json_syntax() {
        let value = serde_json::json!({
            "query": "attention",
            "hits": [{"title": "Attention Is All You Need", "year": 2017}],
            "diagnostics": {"sources_failed": []},
            "next_offset": null
        });

        let rendered = render_human(&value);
        assert!(rendered.contains("query: attention\n"));
        assert!(rendered.contains("title: Attention Is All You Need\n"));
        assert!(rendered.contains("sources_failed: (none)\n"));
        assert!(rendered.contains("next_offset: (none)\n"));
        assert!(!rendered.contains(['{', '}', '[', ']']));
    }

    #[test]
    fn human_output_preserves_multiline_text() {
        let rendered = render_human(&serde_json::json!({"content": "line one\nline two"}));
        assert_eq!(rendered, "content:\n  line one\n  line two\n");
    }

    #[test]
    fn safe_config_json_redacts_secrets_and_marks_unset_values() {
        let mut config = Config::default();
        config.api_key = Some("super-secret".to_string());

        let value = safe_config_value(&config).unwrap();
        assert_eq!(value["api_key"], "<set>");
        assert_eq!(value["hf_token"], "<unset>");
        assert!(!value.to_string().contains("super-secret"));
    }
}
