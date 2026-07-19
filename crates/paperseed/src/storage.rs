use crate::error::{PaperseedError, Result};
use crate::models::StoredFile;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static IMPORT_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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

pub fn copy_and_describe_file(
    source: impl AsRef<Path>,
    root: impl AsRef<Path>,
    mime: impl Into<String>,
) -> Result<StoredFile> {
    let source = source.as_ref();
    if !source.is_file() {
        return Err(PaperseedError::NotAFile(source.to_path_buf()));
    }
    let root = root.as_ref();
    fs::create_dir_all(root)?;
    let temp = root.join(format!(
        ".import-{}-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        IMPORT_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut input = fs::File::open(source)?;
    let mut output = fs::File::create(&temp)?;
    let mut hasher = blake3::Hasher::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        size_bytes += read as u64;
    }
    output.sync_all()?;
    drop(output);

    let hash = hasher.finalize().to_hex().to_string();
    let extension = source.extension().and_then(|extension| extension.to_str());
    let destination = content_addressed_path(root, &hash, extension);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if destination.exists() {
        fs::remove_file(&temp)?;
    } else {
        fs::rename(&temp, &destination)?;
    }
    Ok(StoredFile {
        hash,
        path: destination,
        size_bytes,
        mime: mime.into(),
    })
}
