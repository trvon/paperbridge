use crate::error::Result;
use crate::models::LocalPaper;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusDb {
    pub papers: Vec<IndexedPaper>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedPaper {
    pub paper: LocalPaper,
    pub full_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yams_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryHit {
    pub id: String,
    pub title: String,
    pub score: usize,
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
        // This is resilient to non-atomic writes that left duplicate data.
        let mut de = serde_json::Deserializer::from_str(&raw);
        Ok(Self::deserialize(&mut de).unwrap_or_else(|_| Self::default()))
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

    pub fn get(&self, id_or_hash: &str) -> Option<&IndexedPaper> {
        self.papers.iter().find(|entry| {
            entry.paper.metadata.id == id_or_hash || entry.paper.file.hash.starts_with(id_or_hash)
        })
    }
}
