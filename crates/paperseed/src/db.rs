use crate::error::Result;
use crate::models::LocalPaper;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static CORPUS_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusDb {
    pub papers: Vec<IndexedPaper>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedPaper {
    pub paper: LocalPaper,
    /// Legacy inline text retained for backwards-compatible corpus reads.
    /// New writes externalize this into `full_text_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_text_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yams_hash: Option<String>,
}

impl IndexedPaper {
    pub fn has_full_text(&self) -> bool {
        self.full_text.is_some() || self.full_text_path.is_some()
    }

    pub fn read_full_text(&self) -> Result<Option<String>> {
        if let Some(text) = &self.full_text {
            return Ok(Some(text.clone()));
        }
        self.full_text_path
            .as_ref()
            .map(fs::read_to_string)
            .transpose()
            .map_err(Into::into)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryHit {
    pub id: String,
    pub title: String,
    pub score: f32,
    pub path: PathBuf,
}

impl CorpusDb {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        // Use a streaming deserializer that ignores trailing content.
        // This remains resilient to historical non-atomic writes that left
        // duplicate trailing data, while malformed primary data is quarantined.
        let mut de = serde_json::Deserializer::from_str(&raw);
        match Self::deserialize(&mut de) {
            Ok(db) => Ok(db),
            Err(source) => {
                let quarantine = quarantine_path(path);
                fs::rename(path, &quarantine)?;
                Err(crate::error::PaperseedError::CorruptCorpus {
                    path: path.to_path_buf(),
                    quarantine,
                    source,
                })
            }
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        // Atomic write: write to a temp file first, then rename.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, format!("{raw}\n"))?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn upsert(&mut self, entry: IndexedPaper) {
        if let Some(existing) = self
            .papers
            .iter_mut()
            .find(|indexed| indexed.paper.file.hash == entry.paper.file.hash)
        {
            *existing = entry;
            return;
        }
        self.papers.push(entry);
    }

    pub fn get(&self, id_or_hash: &str) -> Result<Option<&IndexedPaper>> {
        let query = id_or_hash.trim();
        if query.is_empty() {
            return Err(crate::error::PaperseedError::EmptyPaperId);
        }

        let exact_ids = self
            .papers
            .iter()
            .filter(|entry| entry.paper.metadata.id == query)
            .collect::<Vec<_>>();
        if exact_ids.len() == 1 {
            return Ok(exact_ids.first().copied());
        }
        if exact_ids.len() > 1 {
            return Err(ambiguous_id_error(query, &exact_ids));
        }

        if let Some(entry) = self
            .papers
            .iter()
            .find(|entry| entry.paper.file.hash == query)
        {
            return Ok(Some(entry));
        }

        let prefix_matches = self
            .papers
            .iter()
            .filter(|entry| entry.paper.file.hash.starts_with(query))
            .collect::<Vec<_>>();
        match prefix_matches.len() {
            0 => Ok(None),
            1 => Ok(prefix_matches.first().copied()),
            _ => Err(ambiguous_id_error(query, &prefix_matches)),
        }
    }
}

pub(crate) fn with_corpus_write_lock<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = CORPUS_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    operation()
}

fn quarantine_path(path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("corpus.json");
    path.with_file_name(format!(
        "{file_name}.bad.{}-{timestamp}",
        std::process::id()
    ))
}

fn ambiguous_id_error(input: &str, matches: &[&IndexedPaper]) -> crate::error::PaperseedError {
    let mut candidates = matches
        .iter()
        .map(|entry| format!("{} ({})", entry.paper.metadata.id, entry.paper.file.hash))
        .collect::<Vec<_>>();
    candidates.sort();
    crate::error::PaperseedError::AmbiguousPaperId {
        input: input.to_string(),
        candidates: candidates.join(", "),
    }
}
