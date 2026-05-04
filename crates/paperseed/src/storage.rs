use crate::error::{PaperseedError, Result};
use crate::models::StoredFile;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn hash_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(PaperseedError::NotAFile(path.to_path_buf()));
    }

    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn content_addressed_path(
    root: impl AsRef<Path>,
    hash: &str,
    extension: Option<&str>,
) -> PathBuf {
    let prefix = &hash[..2.min(hash.len())];
    let suffix = match extension {
        Some(ext) if !ext.is_empty() => format!("{hash}.{}", ext.trim_start_matches('.')),
        _ => hash.to_string(),
    };
    root.as_ref().join(prefix).join(suffix)
}

pub fn describe_file(path: impl AsRef<Path>, mime: impl Into<String>) -> Result<StoredFile> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(PaperseedError::NotAFile(path.to_path_buf()));
    }
    let metadata = fs::metadata(path)?;
    Ok(StoredFile {
        hash: hash_file(path)?,
        path: path.to_path_buf(),
        size_bytes: metadata.len(),
        mime: mime.into(),
    })
}
