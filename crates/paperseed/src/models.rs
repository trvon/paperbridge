use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperMetadata {
    pub id: String,
    pub title: String,
    pub doi: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub venue: Option<String>,
    pub license: License,
    pub source_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPaper {
    pub metadata: PaperMetadata,
    pub file: StoredFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    pub hash: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mime: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum License {
    Cc0,
    CcBy,
    CcBySa,
    PublicDomain,
    OpenGovernment,
    UserOwnedPrivate,
    Unknown,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusAction {
    StorePrivate,
    Download,
    CacheOpenAccess,
    SeedRedistribute,
}
