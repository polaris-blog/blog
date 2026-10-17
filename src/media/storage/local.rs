//! Local filesystem storage provider (`data/media/`).
//!
//! Keys are provider-relative (`2026/08/8c7d2f91.webp`) and validated
//! segment-by-segment before use: a hostile key can never escape the root.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::validate_key;
use crate::error::AppResult;

#[derive(Debug)]
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Root directory of this provider (the media data dir).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate a provider-relative key and resolve it under the root.
    /// Rejects traversal, absolute keys and control characters.
    pub(crate) fn resolve(&self, key: &str) -> AppResult<PathBuf> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }
}

pub(crate) fn temporary_sibling(path: &Path) -> PathBuf {
    path.with_file_name(format!(".{}.part", crate::utils::cookies::random_token(16)))
}

pub(crate) async fn commit_temporary(tmp: &Path, path: &Path) -> AppResult<()> {
    if let Err(error) = tokio::fs::rename(tmp, path).await {
        tokio::fs::remove_file(tmp).await.ok();
        return Err(error.into());
    }
    Ok(())
}

pub(crate) async fn copy_atomic(src: &Path, dst: &Path) -> AppResult<()> {
    let tmp = temporary_sibling(dst);
    if let Err(error) = tokio::fs::copy(src, &tmp).await {
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(error.into());
    }
    commit_temporary(&tmp, dst).await
}

/// Stream a file through SHA-256 (shared integrity path).
pub(crate) async fn hash_file(path: &Path) -> AppResult<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
