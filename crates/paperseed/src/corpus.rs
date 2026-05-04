use crate::error::{PaperseedError, Result};
use crate::models::{CorpusAction, License, LocalPaper, PaperMetadata};
use crate::policy;
use crate::storage;
use std::path::Path;

pub fn import_local_file(
    path: impl AsRef<Path>,
    title: impl Into<String>,
    license: License,
    mime: impl Into<String>,
) -> Result<LocalPaper> {
    let decision = policy::evaluate(CorpusAction::StorePrivate, license);
    if !decision.allowed {
        return Err(PaperseedError::PolicyBlocked {
            reason: decision.reason.to_string(),
        });
    }

    let file = storage::describe_file(path, mime)?;
    let id = file.hash[..12.min(file.hash.len())].to_string();
    Ok(LocalPaper {
        metadata: PaperMetadata {
            id,
            title: title.into(),
            doi: None,
            authors: Vec::new(),
            year: None,
            venue: None,
            license,
            source_url: None,
        },
        file,
    })
}
