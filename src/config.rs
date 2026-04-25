use crate::error::{Result, ZoteroMcpError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const CONFIG_ENV_VAR: &str = "PAPERBRIDGE_CONFIG";
const LEGACY_CONFIG_ENV_VAR: &str = "ZOTERO_MCP_CONFIG";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BackendModeConfig {
    Cloud,
    Local,
    Hybrid,
}

impl BackendModeConfig {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Local => "local",
            Self::Hybrid => "hybrid",
        }
    }
}

impl std::str::FromStr for BackendModeConfig {
    type Err = ZoteroMcpError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cloud" => Ok(Self::Cloud),
            "local" => Ok(Self::Local),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(ZoteroMcpError::InvalidInput(format!(
                "Invalid backend_mode '{other}'. Valid values: cloud, local, hybrid"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LibraryType {
    User,
    Group,
}

impl LibraryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Group => "group",
        }
    }
}

impl std::str::FromStr for LibraryType {
    type Err = ZoteroMcpError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "group" => Ok(Self::Group),
            other => Err(ZoteroMcpError::InvalidInput(format!(
                "Invalid library_type '{other}'. Valid values: user, group"
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct Config {
    pub backend_mode: BackendModeConfig,
    pub cloud_api_base: String,
    pub local_api_base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub library_type: LibraryType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<u64>,
    pub timeout_secs: u64,
    pub log_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hf_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_scholar_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ads_api_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ncbi_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpaywall_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grobid_url: Option<String>,
    pub grobid_timeout_secs: u64,
    pub grobid_auto_spawn: bool,
    pub grobid_image: String,
    pub update_check_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend_mode: BackendModeConfig::Cloud,
            cloud_api_base: "https://api.zotero.org".to_string(),
            local_api_base: "http://127.0.0.1:23119/api".to_string(),
            api_key: None,
            library_type: LibraryType::User,
            user_id: None,
            group_id: None,
            timeout_secs: 20,
            log_level: "info".to_string(),
            hf_token: None,
            semantic_scholar_api_key: None,
            core_api_key: None,
            ads_api_token: None,
            ncbi_api_key: None,
            unpaywall_email: None,
            grobid_url: None,
            grobid_timeout_secs: 120,
            grobid_auto_spawn: false,
            grobid_image: "lfoppiano/grobid:0.8.1".to_string(),
            update_check_enabled: true,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct PartialConfig {
    backend_mode: Option<BackendModeConfig>,
    api_base: Option<String>,
    cloud_api_base: Option<String>,
    local_api_base: Option<String>,
    api_key: Option<String>,
    library_type: Option<LibraryType>,
    user_id: Option<u64>,
    group_id: Option<u64>,
    timeout_secs: Option<u64>,
    log_level: Option<String>,
    hf_token: Option<String>,
    semantic_scholar_api_key: Option<String>,
    core_api_key: Option<String>,
    ads_api_token: Option<String>,
    ncbi_api_key: Option<String>,
    unpaywall_email: Option<String>,
    grobid_url: Option<String>,
    grobid_timeout_secs: Option<u64>,
    grobid_auto_spawn: Option<bool>,
    grobid_image: Option<String>,
    update_check_enabled: Option<bool>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let mut cfg = Self::load_file_or_default()?;

        cfg.apply_env_overrides(std::env::vars())?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load_file_or_default() -> Result<Self> {
        let mut cfg = Self::default();
        let path = Self::config_path();
        if path.exists() {
            let text = fs::read_to_string(&path)
                .map_err(|e| ZoteroMcpError::Config(format!("Failed to read {path:?}: {e}")))?;
            let partial: PartialConfig = toml::from_str(&text)?;
            cfg.apply_partial(partial);
        }
        Ok(cfg)
    }

    pub fn config_path() -> PathBuf {
        if let Ok(path) = std::env::var(CONFIG_ENV_VAR)
            && !path.trim().is_empty()
        {
            return PathBuf::from(path);
        }
        if let Ok(path) = std::env::var(LEGACY_CONFIG_ENV_VAR)
            && !path.trim().is_empty()
        {
            return PathBuf::from(path);
        }

        match dirs::config_dir() {
            Some(base) => base.join("paperbridge").join("config.toml"),
            None => PathBuf::from("config.toml"),
        }
    }

    pub fn library_prefix(&self) -> Result<String> {
        match self.library_type {
            LibraryType::User => {
                let id = self
                    .user_id
                    .ok_or_else(|| ZoteroMcpError::MissingConfig("user_id".to_string()))?;
                Ok(format!("/users/{id}"))
            }
            LibraryType::Group => {
                let id = self
                    .group_id
                    .ok_or_else(|| ZoteroMcpError::MissingConfig("group_id".to_string()))?;
                Ok(format!("/groups/{id}"))
            }
        }
    }

    pub fn active_read_api_base(&self) -> &str {
        match self.backend_mode {
            BackendModeConfig::Cloud => &self.cloud_api_base,
            BackendModeConfig::Local | BackendModeConfig::Hybrid => &self.local_api_base,
        }
    }

    pub fn active_write_api_base(&self) -> &str {
        match self.backend_mode {
            BackendModeConfig::Cloud | BackendModeConfig::Hybrid => &self.cloud_api_base,
            BackendModeConfig::Local => &self.local_api_base,
        }
    }

    pub fn active_cloud_api_base(&self) -> &str {
        &self.cloud_api_base
    }

    pub fn display_safe(&self) -> String {
        let mask = |opt: &Option<String>| {
            if opt.is_some() { "<set>" } else { "<unset>" }
        };
        let plain = |opt: &Option<String>| opt.clone().unwrap_or_else(|| "<unset>".to_string());
        format!(
            "backend_mode = \"{}\"\ncloud_api_base = \"{}\"\nlocal_api_base = \"{}\"\napi_key = {}\nlibrary_type = \"{}\"\nuser_id = {}\ngroup_id = {}\ntimeout_secs = {}\nlog_level = \"{}\"\nhf_token = {}\nsemantic_scholar_api_key = {}\ncore_api_key = {}\nads_api_token = {}\nncbi_api_key = {}\nunpaywall_email = {}\ngrobid_url = {}\ngrobid_timeout_secs = {}\ngrobid_auto_spawn = {}\ngrobid_image = \"{}\"\nupdate_check_enabled = {}",
            self.backend_mode.as_str(),
            self.cloud_api_base,
            self.local_api_base,
            mask(&self.api_key),
            self.library_type.as_str(),
            self.user_id
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<unset>".to_string()),
            self.group_id
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<unset>".to_string()),
            self.timeout_secs,
            self.log_level,
            mask(&self.hf_token),
            mask(&self.semantic_scholar_api_key),
            mask(&self.core_api_key),
            mask(&self.ads_api_token),
            mask(&self.ncbi_api_key),
            plain(&self.unpaywall_email),
            plain(&self.grobid_url),
            self.grobid_timeout_secs,
            self.grobid_auto_spawn,
            self.grobid_image,
            self.update_check_enabled,
        )
    }

    pub fn write_to_file(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                ZoteroMcpError::Config(format!("Failed to create config dir {parent:?}: {e}"))
            })?;
        }
        let toml = toml::to_string_pretty(self)?;
        fs::write(&path, toml)
            .map_err(|e| ZoteroMcpError::Config(format!("Failed to write {path:?}: {e}")))?;
        Ok(())
    }

    pub fn init_file(force: bool) -> Result<PathBuf> {
        let path = Self::config_path();
        if path.exists() && !force {
            return Err(ZoteroMcpError::Config(format!(
                "Config already exists at {} (use --force to overwrite)",
                path.display()
            )));
        }

        let cfg = Self::default();
        cfg.write_to_file()?;
        Ok(path)
    }

    pub fn get_value(&self, key: &str) -> Option<String> {
        match key {
            "backend_mode" => Some(self.backend_mode.as_str().to_string()),
            "api_base" | "cloud_api_base" => Some(self.cloud_api_base.clone()),
            "local_api_base" => Some(self.local_api_base.clone()),
            "api_key" => Some(
                self.api_key
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "library_type" => Some(self.library_type.as_str().to_string()),
            "user_id" => Some(
                self.user_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "group_id" => Some(
                self.group_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "timeout_secs" => Some(self.timeout_secs.to_string()),
            "log_level" => Some(self.log_level.clone()),
            "hf_token" => Some(
                self.hf_token
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "semantic_scholar_api_key" => Some(
                self.semantic_scholar_api_key
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "core_api_key" => Some(
                self.core_api_key
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "ads_api_token" => Some(
                self.ads_api_token
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "ncbi_api_key" => Some(
                self.ncbi_api_key
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "unpaywall_email" => Some(
                self.unpaywall_email
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "grobid_url" => Some(
                self.grobid_url
                    .clone()
                    .unwrap_or_else(|| "<unset>".to_string()),
            ),
            "grobid_timeout_secs" => Some(self.grobid_timeout_secs.to_string()),
            "grobid_auto_spawn" => Some(self.grobid_auto_spawn.to_string()),
            "grobid_image" => Some(self.grobid_image.clone()),
            "update_check_enabled" => Some(self.update_check_enabled.to_string()),
            _ => None,
        }
    }

    pub fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        let v = value.trim();
        match key {
            "backend_mode" => {
                self.backend_mode = v.parse::<BackendModeConfig>()?;
            }
            "api_base" | "cloud_api_base" => {
                if v.is_empty() {
                    return Err(ZoteroMcpError::InvalidInput(
                        "cloud_api_base cannot be empty".to_string(),
                    ));
                }
                self.cloud_api_base = v.to_string();
            }
            "local_api_base" => {
                if v.is_empty() {
                    return Err(ZoteroMcpError::InvalidInput(
                        "local_api_base cannot be empty".to_string(),
                    ));
                }
                self.local_api_base = v.to_string();
            }
            "api_key" => {
                self.api_key = optional_string(v);
            }
            "library_type" => {
                self.library_type = v.parse::<LibraryType>()?;
            }
            "user_id" => {
                self.user_id = optional_u64(key, v)?;
            }
            "group_id" => {
                self.group_id = optional_u64(key, v)?;
            }
            "timeout_secs" => {
                self.timeout_secs = v.parse::<u64>().map_err(|_| {
                    ZoteroMcpError::InvalidInput(format!(
                        "timeout_secs must be an unsigned integer, got '{v}'"
                    ))
                })?;
            }
            "log_level" => {
                if v.is_empty() {
                    return Err(ZoteroMcpError::InvalidInput(
                        "log_level cannot be empty".to_string(),
                    ));
                }
                self.log_level = v.to_string();
            }
            "hf_token" => {
                self.hf_token = optional_string(v);
            }
            "semantic_scholar_api_key" => {
                self.semantic_scholar_api_key = optional_string(v);
            }
            "core_api_key" => {
                self.core_api_key = optional_string(v);
            }
            "ads_api_token" => {
                self.ads_api_token = optional_string(v);
            }
            "ncbi_api_key" => {
                self.ncbi_api_key = optional_string(v);
            }
            "unpaywall_email" => {
                self.unpaywall_email = optional_string(v);
            }
            "grobid_url" => {
                self.grobid_url = optional_string(v);
            }
            "grobid_timeout_secs" => {
                self.grobid_timeout_secs = v.parse::<u64>().map_err(|_| {
                    ZoteroMcpError::InvalidInput(format!(
                        "grobid_timeout_secs must be an unsigned integer, got '{v}'"
                    ))
                })?;
            }
            "grobid_auto_spawn" => {
                self.grobid_auto_spawn = match v.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    other => {
                        return Err(ZoteroMcpError::InvalidInput(format!(
                            "grobid_auto_spawn must be a boolean, got '{other}'"
                        )));
                    }
                };
            }
            "grobid_image" => {
                if v.is_empty() {
                    return Err(ZoteroMcpError::InvalidInput(
                        "grobid_image cannot be empty".to_string(),
                    ));
                }
                self.grobid_image = v.to_string();
            }
            "update_check_enabled" => {
                self.update_check_enabled = match v.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    other => {
                        return Err(ZoteroMcpError::InvalidInput(format!(
                            "update_check_enabled must be a boolean, got '{other}'"
                        )));
                    }
                };
            }
            _ => {
                return Err(ZoteroMcpError::InvalidInput(format!(
                    "Unknown config key '{key}'. Valid keys: backend_mode, cloud_api_base, local_api_base, api_base, api_key, library_type, user_id, group_id, timeout_secs, log_level, hf_token, semantic_scholar_api_key, core_api_key, ads_api_token, ncbi_api_key, unpaywall_email, grobid_url, grobid_timeout_secs, grobid_auto_spawn, grobid_image, update_check_enabled"
                )));
            }
        }
        Ok(())
    }

    fn apply_partial(&mut self, partial: PartialConfig) {
        if let Some(v) = partial.backend_mode {
            self.backend_mode = v;
        }
        if let Some(v) = partial.api_base {
            self.cloud_api_base = v;
        }
        if let Some(v) = partial.cloud_api_base {
            self.cloud_api_base = v;
        }
        if let Some(v) = partial.local_api_base {
            self.local_api_base = v;
        }
        if let Some(v) = partial.api_key {
            self.api_key = Some(v);
        }
        if let Some(v) = partial.library_type {
            self.library_type = v;
        }
        if let Some(v) = partial.user_id {
            self.user_id = Some(v);
        }
        if let Some(v) = partial.group_id {
            self.group_id = Some(v);
        }
        if let Some(v) = partial.timeout_secs {
            self.timeout_secs = v;
        }
        if let Some(v) = partial.log_level {
            self.log_level = v;
        }
        if let Some(v) = partial.hf_token {
            self.hf_token = Some(v);
        }
        if let Some(v) = partial.semantic_scholar_api_key {
            self.semantic_scholar_api_key = Some(v);
        }
        if let Some(v) = partial.core_api_key {
            self.core_api_key = Some(v);
        }
        if let Some(v) = partial.ads_api_token {
            self.ads_api_token = Some(v);
        }
        if let Some(v) = partial.ncbi_api_key {
            self.ncbi_api_key = Some(v);
        }
        if let Some(v) = partial.unpaywall_email {
            self.unpaywall_email = Some(v);
        }
        if let Some(v) = partial.grobid_url {
            self.grobid_url = Some(v);
        }
        if let Some(v) = partial.grobid_timeout_secs {
            self.grobid_timeout_secs = v;
        }
        if let Some(v) = partial.grobid_auto_spawn {
            self.grobid_auto_spawn = v;
        }
        if let Some(v) = partial.grobid_image {
            self.grobid_image = v;
        }
        if let Some(v) = partial.update_check_enabled {
            self.update_check_enabled = v;
        }
    }

    pub(crate) fn apply_env_overrides<I, K, V>(&mut self, pairs: I) -> Result<()>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        for (key, value) in pairs {
            let key = key.as_ref();
            let value = value.as_ref();
            match key {
                "PAPERBRIDGE_BACKEND_MODE" => {
                    self.backend_mode = value.parse::<BackendModeConfig>()?;
                }
                "PAPERBRIDGE_API_BASE" | "ZOTERO_MCP_API_BASE" | "PAPERBRIDGE_CLOUD_API_BASE" => {
                    self.cloud_api_base = value.to_string()
                }
                "PAPERBRIDGE_LOCAL_API_BASE" => self.local_api_base = value.to_string(),
                "PAPERBRIDGE_API_KEY" | "ZOTERO_MCP_API_KEY" => {
                    if value.trim().is_empty() {
                        self.api_key = None;
                    } else {
                        self.api_key = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_LIBRARY_TYPE" | "ZOTERO_MCP_LIBRARY_TYPE" => {
                    self.library_type = match value.trim().to_ascii_lowercase().as_str() {
                        "user" => LibraryType::User,
                        "group" => LibraryType::Group,
                        other => {
                            return Err(ZoteroMcpError::Config(format!(
                                "Invalid PAPERBRIDGE_LIBRARY_TYPE '{other}'"
                            )));
                        }
                    };
                }
                "PAPERBRIDGE_USER_ID" | "ZOTERO_MCP_USER_ID" => {
                    self.user_id = Some(parse_u64_env(key, value)?);
                }
                "PAPERBRIDGE_GROUP_ID" | "ZOTERO_MCP_GROUP_ID" => {
                    self.group_id = Some(parse_u64_env(key, value)?);
                }
                "PAPERBRIDGE_TIMEOUT_SECS" | "ZOTERO_MCP_TIMEOUT_SECS" => {
                    self.timeout_secs = parse_u64_env(key, value)?;
                }
                "PAPERBRIDGE_LOG_LEVEL" | "ZOTERO_MCP_LOG_LEVEL" => {
                    self.log_level = value.to_string()
                }
                "PAPERBRIDGE_HF_TOKEN" | "HF_TOKEN" => {
                    if value.trim().is_empty() {
                        self.hf_token = None;
                    } else {
                        self.hf_token = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_SEMANTIC_SCHOLAR_API_KEY" | "SEMANTIC_SCHOLAR_API_KEY" => {
                    if value.trim().is_empty() {
                        self.semantic_scholar_api_key = None;
                    } else {
                        self.semantic_scholar_api_key = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_CORE_API_KEY" | "CORE_API_KEY" => {
                    if value.trim().is_empty() {
                        self.core_api_key = None;
                    } else {
                        self.core_api_key = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_ADS_API_TOKEN" | "ADS_API_TOKEN" => {
                    if value.trim().is_empty() {
                        self.ads_api_token = None;
                    } else {
                        self.ads_api_token = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_NCBI_API_KEY" | "NCBI_API_KEY" => {
                    if value.trim().is_empty() {
                        self.ncbi_api_key = None;
                    } else {
                        self.ncbi_api_key = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_UNPAYWALL_EMAIL" | "UNPAYWALL_EMAIL" => {
                    if value.trim().is_empty() {
                        self.unpaywall_email = None;
                    } else {
                        self.unpaywall_email = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_GROBID_URL" | "GROBID_URL" => {
                    if value.trim().is_empty() {
                        self.grobid_url = None;
                    } else {
                        self.grobid_url = Some(value.to_string());
                    }
                }
                "PAPERBRIDGE_GROBID_TIMEOUT_SECS" => {
                    self.grobid_timeout_secs = parse_u64_env(key, value)?;
                }
                "PAPERBRIDGE_GROBID_AUTO_SPAWN" => {
                    self.grobid_auto_spawn = match value.trim().to_ascii_lowercase().as_str() {
                        "true" | "1" | "yes" | "on" => true,
                        "false" | "0" | "no" | "off" => false,
                        other => {
                            return Err(ZoteroMcpError::Config(format!(
                                "Invalid PAPERBRIDGE_GROBID_AUTO_SPAWN '{other}'"
                            )));
                        }
                    };
                }
                "PAPERBRIDGE_GROBID_IMAGE" if !value.trim().is_empty() => {
                    self.grobid_image = value.to_string();
                }
                "PAPERBRIDGE_UPDATE_CHECK" => {
                    self.update_check_enabled = match value.trim().to_ascii_lowercase().as_str() {
                        "true" | "1" | "yes" | "on" => true,
                        "false" | "0" | "no" | "off" => false,
                        other => {
                            return Err(ZoteroMcpError::Config(format!(
                                "Invalid PAPERBRIDGE_UPDATE_CHECK '{other}'"
                            )));
                        }
                    };
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.timeout_secs == 0 {
            return Err(ZoteroMcpError::Config(
                "timeout_secs must be greater than zero".to_string(),
            ));
        }

        if self.cloud_api_base.trim().is_empty() {
            return Err(ZoteroMcpError::Config(
                "cloud_api_base must not be empty".to_string(),
            ));
        }

        if self.local_api_base.trim().is_empty() {
            return Err(ZoteroMcpError::Config(
                "local_api_base must not be empty".to_string(),
            ));
        }

        match self.library_type {
            LibraryType::User if self.user_id.is_none() => {
                return Err(ZoteroMcpError::MissingConfig(
                    "user_id is required when library_type=user.\nTry:\n  paperbridge config init --interactive\n  paperbridge config resolve-user-id --login <your-username>".to_string(),
                ));
            }
            LibraryType::Group if self.group_id.is_none() => {
                return Err(ZoteroMcpError::MissingConfig(
                    "group_id is required when library_type=group.\nTry:\n  paperbridge config set group_id <numeric-group-id>".to_string(),
                ));
            }
            _ => {}
        }

        Ok(())
    }
}

fn parse_u64_env(key: &str, raw: &str) -> Result<u64> {
    raw.parse::<u64>().map_err(|_| {
        ZoteroMcpError::Config(format!("{key} must be an unsigned integer, got '{raw}'"))
    })
}

fn optional_string(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    if raw.is_empty() || matches!(lower.as_str(), "unset" | "none" | "null") {
        None
    } else {
        Some(raw.to_string())
    }
}

fn optional_u64(key: &str, raw: &str) -> Result<Option<u64>> {
    let lower = raw.to_ascii_lowercase();
    if raw.is_empty() || matches!(lower.as_str(), "unset" | "none" | "null") {
        return Ok(None);
    }

    let parsed = raw.parse::<u64>().map_err(|_| {
        ZoteroMcpError::InvalidInput(format!(
            "{key} must be an unsigned integer or unset/none/null, got '{raw}'"
        ))
    })?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_prefix_for_user() {
        let cfg = Config {
            user_id: Some(42),
            ..Config::default()
        };
        assert_eq!(cfg.library_prefix().unwrap(), "/users/42");
    }

    #[test]
    fn library_prefix_requires_group_id_for_group_library() {
        let cfg = Config {
            library_type: LibraryType::Group,
            user_id: Some(42),
            group_id: None,
            ..Config::default()
        };
        assert!(cfg.library_prefix().is_err());
    }

    #[test]
    fn env_overrides_apply() {
        let mut cfg = Config {
            user_id: Some(1),
            ..Config::default()
        };
        cfg.apply_env_overrides([
            ("PAPERBRIDGE_BACKEND_MODE", "hybrid"),
            ("PAPERBRIDGE_LIBRARY_TYPE", "group"),
            ("PAPERBRIDGE_GROUP_ID", "777"),
            ("PAPERBRIDGE_TIMEOUT_SECS", "60"),
        ])
        .unwrap();

        assert_eq!(cfg.backend_mode, BackendModeConfig::Hybrid);
        assert_eq!(cfg.library_type, LibraryType::Group);
        assert_eq!(cfg.group_id, Some(777));
        assert_eq!(cfg.timeout_secs, 60);
    }

    #[test]
    fn invalid_env_type_fails() {
        let mut cfg = Config {
            user_id: Some(1),
            ..Config::default()
        };
        let err = cfg
            .apply_env_overrides([("PAPERBRIDGE_LIBRARY_TYPE", "team")])
            .unwrap_err();
        assert!(err.to_string().contains("PAPERBRIDGE_LIBRARY_TYPE"));
    }

    #[test]
    fn legacy_env_prefix_is_still_supported() {
        let mut cfg = Config {
            user_id: Some(1),
            ..Config::default()
        };
        cfg.apply_env_overrides([
            ("ZOTERO_MCP_LIBRARY_TYPE", "group"),
            ("ZOTERO_MCP_GROUP_ID", "9"),
        ])
        .unwrap();
        assert_eq!(cfg.library_type, LibraryType::Group);
        assert_eq!(cfg.group_id, Some(9));
    }

    #[test]
    fn validation_requires_user_id_for_user_library() {
        let cfg = Config::default();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("user_id"));
    }

    #[test]
    fn partial_config_merges_without_overwriting_unset_values() {
        let mut cfg = Config {
            user_id: Some(10),
            ..Config::default()
        };
        cfg.apply_partial(PartialConfig {
            api_base: Some("https://example.com".to_string()),
            timeout_secs: Some(15),
            ..PartialConfig::default()
        });

        assert_eq!(cfg.cloud_api_base, "https://example.com");
        assert_eq!(cfg.timeout_secs, 15);
        assert_eq!(cfg.user_id, Some(10));
    }

    #[test]
    fn get_value_returns_expected_keys() {
        let cfg = Config {
            api_key: Some("secret".to_string()),
            user_id: Some(12),
            ..Config::default()
        };
        assert_eq!(cfg.get_value("backend_mode").as_deref(), Some("cloud"));
        assert_eq!(cfg.get_value("library_type").as_deref(), Some("user"));
        assert_eq!(cfg.get_value("user_id").as_deref(), Some("12"));
        assert_eq!(cfg.get_value("api_key").as_deref(), Some("secret"));
        assert!(cfg.get_value("unknown").is_none());
    }

    #[test]
    fn active_api_bases_follow_backend_mode() {
        let mut cfg = Config {
            cloud_api_base: "https://api.zotero.org".to_string(),
            local_api_base: "http://127.0.0.1:23119/api".to_string(),
            user_id: Some(1),
            ..Config::default()
        };

        cfg.backend_mode = BackendModeConfig::Cloud;
        assert_eq!(cfg.active_read_api_base(), "https://api.zotero.org");
        assert_eq!(cfg.active_write_api_base(), "https://api.zotero.org");

        cfg.backend_mode = BackendModeConfig::Local;
        assert_eq!(cfg.active_read_api_base(), "http://127.0.0.1:23119/api");
        assert_eq!(cfg.active_write_api_base(), "http://127.0.0.1:23119/api");

        cfg.backend_mode = BackendModeConfig::Hybrid;
        assert_eq!(cfg.active_read_api_base(), "http://127.0.0.1:23119/api");
        assert_eq!(cfg.active_write_api_base(), "https://api.zotero.org");
    }

    #[test]
    fn set_value_supports_unset_for_optional_fields() {
        let mut cfg = Config {
            api_key: Some("secret".to_string()),
            user_id: Some(12),
            group_id: Some(99),
            ..Config::default()
        };

        cfg.set_value("api_key", "unset").unwrap();
        cfg.set_value("user_id", "none").unwrap();
        cfg.set_value("group_id", "null").unwrap();

        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.user_id, None);
        assert_eq!(cfg.group_id, None);
    }
}
