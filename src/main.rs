use clap::Parser;
use paperbridge::cli::{Cli, Command, ConfigAction, SnippetTarget};
use paperbridge::config::Config;
use paperbridge::models::{
    CollectionUpdateRequest, CollectionWriteRequest, DeleteCollectionRequest, DeleteItemRequest,
    ItemUpdateRequest, ItemWriteRequest, ListCollectionsQuery, SearchItemsQuery,
};
use paperbridge::server::PaperbridgeServer;
use paperbridge::service::{
    PaperbridgeService, PrepareItemForVoxRequest, PrepareSearchResultForVoxRequest,
};
use paperbridge::zotero_api::build_backend;
use rmcp::ServiceExt;
use serde::Serialize;
use std::io::{self, Write};

fn main() -> paperbridge::Result<()> {
    let cli = Cli::parse();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| paperbridge::ZoteroMcpError::Config(e.to_string()))?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> paperbridge::Result<()> {
    if let Some(Command::Config { action }) = &cli.command {
        match action {
            ConfigAction::Path => {
                println!("{}", Config::config_path().display());
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
            ConfigAction::Get { key } => {
                handle_config_get(key.as_deref())?;
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
                handle_config_resolve_user_id(login, api_key.as_deref(), api_base.as_deref())
                    .await?;
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
            println!("Config valid.\n\n{}", config.display_safe());
        }
        Some(Command::Query {
            q,
            qmode,
            item_type,
            tag,
            limit,
            start,
        }) => {
            let service = build_service(config)?;
            let results = service
                .search_items(SearchItemsQuery {
                    q,
                    qmode,
                    item_type,
                    tag,
                    limit: limit.unwrap_or(25),
                    start: start.unwrap_or(0),
                })
                .await?;
            print_json(&results)?;
        }
        Some(Command::Collections {
            top_only,
            limit,
            start,
        }) => {
            let service = build_service(config)?;
            let results = service
                .list_collections(ListCollectionsQuery {
                    top_only,
                    limit: limit.unwrap_or(50),
                    start: start.unwrap_or(0),
                })
                .await?;
            print_json(&results)?;
        }
        Some(Command::Read {
            item_key,
            attachment_key,
            max_chars_per_chunk,
        }) => {
            let service = build_service(config)?;
            let payload = service
                .prepare_item_for_vox(PrepareItemForVoxRequest {
                    item_key,
                    attachment_key,
                    max_chars_per_chunk,
                })
                .await?;
            print_json(&payload)?;
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
            print_json(&payload)?;
        }
        Some(Command::CreateCollection {
            name,
            parent_collection,
        }) => {
            let service = build_service(config)?;
            let payload = service
                .create_collection(CollectionWriteRequest {
                    name,
                    parent_collection,
                })
                .await?;
            print_json(&payload)?;
        }
        Some(Command::ResolveDoi { doi }) => {
            let service = build_service(config)?;
            let work = service.resolve_doi(&doi).await?;
            print_json(&work)?;
        }
        Some(Command::ValidateItem { file, online }) => {
            let service = build_service(config)?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}"))
            })?;
            let payload: ItemWriteRequest = serde_json::from_str(&text).map_err(|e| {
                paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}"))
            })?;
            let report = if online {
                service.validate_item_online(&payload).await?
            } else {
                service.validate_item_request(&payload)
            };
            print_json(&report)?;
        }
        Some(Command::CreateItem { file }) => {
            let service = build_service(config)?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}"))
            })?;
            let payload: ItemWriteRequest = serde_json::from_str(&text).map_err(|e| {
                paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}"))
            })?;
            let created = service.create_item(payload).await?;
            print_json(&created)?;
        }
        Some(Command::UpdateCollection { file }) => {
            let service = build_service(config)?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}"))
            })?;
            let payload: CollectionUpdateRequest = serde_json::from_str(&text).map_err(|e| {
                paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}"))
            })?;
            let updated = service.update_collection(payload).await?;
            print_json(&updated)?;
        }
        Some(Command::UpdateItem { file }) => {
            let service = build_service(config)?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}"))
            })?;
            let payload: ItemUpdateRequest = serde_json::from_str(&text).map_err(|e| {
                paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}"))
            })?;
            let updated = service.update_item(payload).await?;
            print_json(&updated)?;
        }
        Some(Command::DeleteCollection { file }) => {
            let service = build_service(config)?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}"))
            })?;
            let payload: DeleteCollectionRequest = serde_json::from_str(&text).map_err(|e| {
                paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}"))
            })?;
            service.delete_collection(payload).await?;
            print_json(&serde_json::json!({"deleted": true}))?;
        }
        Some(Command::DeleteItem { file }) => {
            let service = build_service(config)?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                paperbridge::ZoteroMcpError::Config(format!("Failed to read {file}: {e}"))
            })?;
            let payload: DeleteItemRequest = serde_json::from_str(&text).map_err(|e| {
                paperbridge::ZoteroMcpError::Serde(format!("Invalid JSON in {file}: {e}"))
            })?;
            service.delete_item(payload).await?;
            print_json(&serde_json::json!({"deleted": true}))?;
        }
        Some(Command::BackendInfo) => {
            let service = build_service(config)?;
            print_json(&service.backend_info())?;
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
    }

    Ok(())
}

fn build_service(config: Config) -> paperbridge::Result<PaperbridgeService> {
    let backend = build_backend(config)?;
    Ok(PaperbridgeService::new(backend))
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

fn print_json<T: Serialize>(value: &T) -> paperbridge::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| paperbridge::ZoteroMcpError::Serde(e.to_string()))?;
    println!("{json}");
    Ok(())
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
                    "search": [bin, "query", "--q", "<query>", "--limit", "5"],
                    "collections": [bin, "collections", "--top-only"],
                    "read_item": [bin, "read", "--item-key", "<item-key>", "--max-chars-per-chunk", "1200"],
                    "read_search_result": [bin, "read-search", "--q", "<query>", "--result-index", "0", "--max-chars-per-chunk", "1200"]
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

    cfg.write_to_file()?;
    println!("Initialized config at {}", path.display());
    match cfg.validate() {
        Ok(()) => println!("Config is valid."),
        Err(e) => eprintln!("Config saved, but validation currently fails: {e}"),
    }
    Ok(())
}

fn handle_config_get(key: Option<&str>) -> paperbridge::Result<()> {
    let cfg = Config::load_file_or_default()?;
    if let Some(key) = key {
        let value = cfg.get_value(key).ok_or_else(|| {
            paperbridge::ZoteroMcpError::InvalidInput(format!(
                "Unknown config key '{key}'. Valid keys: backend_mode, cloud_api_base, local_api_base, api_base, api_key, library_type, user_id, group_id, timeout_secs, log_level"
            ))
        })?;
        println!("{value}");
        return Ok(());
    }

    println!("{}", cfg.display_safe());
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
) -> paperbridge::Result<()> {
    let cfg = Config::load_file_or_default()?;
    let api_base = api_base_override.unwrap_or(cfg.active_cloud_api_base());
    let api_key = api_key_override.or(cfg.api_key.as_deref());

    let user_id = resolve_user_id_from_login_id(login, api_base, api_key).await?;
    println!("{user_id}");
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
    let response = reqwest::Client::new()
        .get(url)
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
    let response = reqwest::Client::new()
        .get(format!("{base}/keys/{api_key}"))
        .header("Zotero-API-Version", "3")
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
}
