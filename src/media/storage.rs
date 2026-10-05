//! Storage abstraction — the media service never touches the filesystem
//! directly.
//!
//! ```text
//! MediaService → Storage (enum dispatch, monomorphic calls)
//!                  └── LocalStorage   data/media/YYYY/MM/<uuid>.<ext>
//! ```
//!
//! S3-compatible providers (S3 / Cloudflare R2 / MinIO) are designed as an
//! optional build feature: selecting `provider = "s3"` fails fast at startup
//! in builds without it, rather than silently falling back to local disk.
//! Local storage keys are provider-relative (`2026/08/8c7d2f91.webp`), so a
//! later migration to object storage never rewrites database rows.
//!
//! Security: keys are validated segment-by-segment (no `..`, no absolute
//! paths, no Windows reserved separators) and resolved strictly under the
//! storage root — path traversal from a hostile URL cannot escape it.

use std::path::{Path, PathBuf};

use axum::body::Body;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use crate::config::MediaStorageConfig;
use crate::error::{AppError, AppResult};

/// A stored object opened for streaming responses.
pub struct StoredObject {
    pub size: u64,
    pub body: Body,
}

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
    fn resolve(&self, key: &str) -> AppResult<PathBuf> {
        let mut path = self.root.clone();
        for seg in key.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                return Err(AppError::BadRequest("invalid storage key".into()));
            }
            let stem = seg.split('.').next().unwrap_or("").to_ascii_uppercase();
            let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'));
            if seg.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
                || seg.ends_with(['.', ' '])
                || reserved
                || seg.bytes().any(|b| b < 0x20 || b == 0x7f)
            {
                return Err(AppError::BadRequest("invalid storage key".into()));
            }
            path.push(seg);
        }
        Ok(path)
    }
}

fn temporary_sibling(path: &Path) -> PathBuf {
    path.with_file_name(format!(".{}.part", crate::utils::cookies::random_token(16)))
}

async fn commit_temporary(tmp: &Path, path: &Path) -> AppResult<()> {
    if let Err(error) = tokio::fs::rename(tmp, path).await {
        tokio::fs::remove_file(tmp).await.ok();
        return Err(error.into());
    }
    Ok(())
}

async fn copy_atomic(src: &Path, dst: &Path) -> AppResult<()> {
    let tmp = temporary_sibling(dst);
    if let Err(error) = tokio::fs::copy(src, &tmp).await {
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(error.into());
    }
    commit_temporary(&tmp, dst).await
}

/// Runtime-selected storage provider. Enum dispatch (like `CacheBackend`
/// and `SearchProvider`) keeps calls monomorphic with no boxed futures.
#[derive(Debug)]
pub enum Storage {
    Local(LocalStorage),
}

impl Storage {
    /// Build the configured provider. Fails at startup (never silently
    /// falls back) when a provider is selected that this build lacks.
    pub fn build(cfg: &MediaStorageConfig) -> AppResult<Self> {
        let provider = cfg.provider.trim().to_ascii_lowercase();
        match provider.as_str() {
            "" | "local" => {
                let root = PathBuf::from(&cfg.dir);
                std::fs::create_dir_all(&root)?;
                // Staging dir for streamed uploads (same filesystem → O(1) rename).
                std::fs::create_dir_all(root.join(".tmp"))?;
                Ok(Self::Local(LocalStorage::new(root)))
            }
            "s3" | "r2" | "minio" => Err(AppError::Internal(anyhow::anyhow!(
                "storage provider '{provider}' requires a build with object-storage \
                 support; this build ships local storage only — use provider = \"local\" \
                 or build with the s3 feature"
            ))),
            other => Err(AppError::BadRequest(format!(
                "unknown storage provider '{other}' (expected \"local\")"
            ))),
        }
    }

    /// Provider name (admin/CLI status display).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
        }
    }

    /// Root path when the provider is filesystem-backed (CLI maintenance).
    pub fn local_root(&self) -> Option<&Path> {
        match self {
            Self::Local(l) => Some(l.root()),
        }
    }

    /// Reserve a staging file for a streamed upload. The caller writes into
    /// it and hands it to [`Storage::put_staged`]. For local storage the
    /// staging area shares the filesystem with the final location, so
    /// committing is a rename.
    pub fn staging_file(&self) -> AppResult<PathBuf> {
        match self {
            Self::Local(l) => {
                let dir = l.root().join(".tmp");
                std::fs::create_dir_all(&dir)?;
                let name = crate::utils::cookies::random_token(16);
                Ok(dir.join(name))
            }
        }
    }

    pub async fn put(&self, key: &str, data: &[u8]) -> AppResult<()> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                // Write to a sibling temp file, then rename: readers never
                // observe a partially written object.
                let tmp = temporary_sibling(&path);
                if let Err(error) = tokio::fs::write(&tmp, data).await {
                    tokio::fs::remove_file(&tmp).await.ok();
                    return Err(error.into());
                }
                commit_temporary(&tmp, &path).await
            }
        }
    }

    /// Commit a staged upload under `key` (streaming path for large files).
    pub async fn put_staged(&self, key: &str, staged: &Path) -> AppResult<()> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                if tokio::fs::rename(staged, &path).await.is_err() {
                    // Cross-device uploads still become visible atomically.
                    copy_atomic(staged, &path).await?;
                    tokio::fs::remove_file(staged).await.ok();
                }
                Ok(())
            }
        }
    }

    /// Read a whole object into memory (metadata reads, small image
    /// processing, thumbnail re-generation — never for serving).
    pub async fn read(&self, key: &str) -> AppResult<Option<Vec<u8>>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::read(&path).await {
                    Ok(v) => Ok(Some(v)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Open an object as a raw async reader (backup streaming). The reader
    /// is boxed so provider-specific types stay behind this abstraction —
    /// callers stream through it in bounded chunks and never hold a whole
    /// object in memory.
    pub async fn open_reader(
        &self,
        key: &str,
    ) -> AppResult<Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::File::open(&path).await {
                    Ok(f) => Ok(Some(Box::new(f))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Open an object for a streaming HTTP response (large files are never
    /// buffered into RAM).
    pub async fn open(&self, key: &str) -> AppResult<Option<StoredObject>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::File::open(&path).await {
                    Ok(file) => {
                        let size = file.metadata().await?.len();
                        let stream = ReaderStream::with_capacity(file, 64 * 1024);
                        Ok(Some(StoredObject {
                            size,
                            body: Body::from_stream(stream),
                        }))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Open the byte range `[start, end)` of an object for a `206 Partial
    /// Content` response (seekable playback for video/audio). `end` is
    /// clamped to the object size.
    pub async fn open_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
    ) -> AppResult<Option<StoredObject>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::File::open(&path).await {
                    Ok(mut file) => {
                        let size = file.metadata().await?.len();
                        if start >= size || end <= start {
                            return Ok(None);
                        }
                        let len = end.min(size) - start;
                        tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(start))
                            .await?;
                        // Take stops exactly at the range end — the stream
                        // never spills past it.
                        let reader = tokio::io::AsyncReadExt::take(file, len);
                        let stream = ReaderStream::with_capacity(reader, 64 * 1024);
                        Ok(Some(StoredObject {
                            size: len,
                            body: Body::from_stream(stream),
                        }))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Stream an object through SHA-256 (integrity verification).
    pub async fn hash_of(&self, key: &str) -> AppResult<Option<String>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::File::open(&path).await {
                    Ok(mut file) => {
                        let mut hasher = Sha256::new();
                        let mut buf = vec![0u8; 64 * 1024];
                        loop {
                            let n = file.read(&mut buf).await?;
                            if n == 0 {
                                break;
                            }
                            hasher.update(&buf[..n]);
                        }
                        Ok(Some(format!("{:x}", hasher.finalize())))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// Object size in bytes, if present.
    pub async fn size_of(&self, key: &str) -> AppResult<Option<u64>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::metadata(&path).await {
                    Ok(m) if m.is_file() => Ok(Some(m.len())),
                    Ok(_) => Ok(None),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    pub async fn exists(&self, key: &str) -> AppResult<bool> {
        Ok(self.size_of(key).await?.is_some())
    }

    /// Copy an object (media "duplicate" operation). Storage keys are
    /// provider-relative; copying never re-reads through the service.
    pub async fn copy(&self, from: &str, to: &str) -> AppResult<()> {
        match self {
            Self::Local(l) => {
                let src = l.resolve(from)?;
                let dst = l.resolve(to)?;
                if let Some(parent) = dst.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                copy_atomic(&src, &dst).await?;
                Ok(())
            }
        }
    }

    pub async fn delete(&self, key: &str) -> AppResult<()> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    /// List all object keys under a prefix (orphan detection / cleanup).
    /// Skips the `.tmp` staging area. Keys use `/` separators. An empty
    /// prefix lists the entire storage tree.
    pub async fn list(&self, prefix: &str) -> AppResult<Vec<String>> {
        match self {
            Self::Local(l) => {
                let mut out = Vec::new();
                let start = if prefix.is_empty() {
                    l.root().to_path_buf()
                } else {
                    l.resolve(prefix)?
                };
                if !start.is_dir() {
                    return Ok(out);
                }
                let mut queue = vec![(start, prefix.to_string())];
                while let Some((dir, rel)) = queue.pop() {
                    let mut entries = tokio::fs::read_dir(&dir).await?;
                    while let Some(entry) = entries.next_entry().await? {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') {
                            continue;
                        }
                        let child_rel = if rel.is_empty() {
                            name.clone()
                        } else {
                            format!("{rel}/{name}")
                        };
                        let ft = entry.file_type().await?;
                        if ft.is_dir() {
                            queue.push((entry.path(), child_rel));
                        } else {
                            out.push(child_rel);
                        }
                    }
                }
                out.sort();
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &str) -> MediaStorageConfig {
        MediaStorageConfig {
            provider: "local".into(),
            dir: dir.into(),
        }
    }

    #[tokio::test]
    async fn put_read_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        storage.put("2026/08/abc.webp", b"hello").await.unwrap();
        assert_eq!(
            storage.read("2026/08/abc.webp").await.unwrap().unwrap(),
            b"hello"
        );
        assert_eq!(storage.size_of("2026/08/abc.webp").await.unwrap(), Some(5));
        assert!(storage.exists("2026/08/abc.webp").await.unwrap());
        storage.delete("2026/08/abc.webp").await.unwrap();
        assert!(!storage.exists("2026/08/abc.webp").await.unwrap());
    }

    #[tokio::test]
    async fn traversal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        for bad in [
            "../etc/passwd",
            "a/../../b",
            "/absolute",
            "a\\b",
            "a/\u{1}b",
            "..",
            "C:escape",
            "a/C:/escape",
            "a/file:secret",
            "a/NUL.txt",
            "a/COM1",
            "a/trailing.",
            "a/trailing ",
        ] {
            assert!(
                storage.read(bad).await.is_err(),
                "key must be rejected: {bad}"
            );
        }
    }

    #[tokio::test]
    async fn list_skips_tmp_and_lists_keys() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        storage.put("2026/08/a.webp", b"a").await.unwrap();
        storage.put("2026/09/b.jpg", b"b").await.unwrap();
        let keys = storage.list("").await.unwrap();
        assert_eq!(
            keys,
            vec!["2026/08/a.webp".to_string(), "2026/09/b.jpg".to_string()]
        );
        let aug = storage.list("2026/08").await.unwrap();
        assert_eq!(aug, vec!["2026/08/a.webp".to_string()]);
    }

    #[tokio::test]
    async fn staged_uploads_commit_by_rename() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        let staged = storage.staging_file().unwrap();
        std::fs::write(&staged, b"streamed").unwrap();
        storage.put_staged("2026/10/x.bin", &staged).await.unwrap();
        assert!(!staged.exists());
        assert_eq!(
            storage.read("2026/10/x.bin").await.unwrap().unwrap(),
            b"streamed"
        );
    }

    #[tokio::test]
    async fn concurrent_writes_and_failed_replacements_preserve_objects() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        let a = vec![1; 256 * 1024];
        let b = vec![2; 256 * 1024];
        let (left, right) = tokio::join!(storage.put("same.jpg", &a), storage.put("same.png", &b));
        left.unwrap();
        right.unwrap();
        assert_eq!(storage.read("same.jpg").await.unwrap().unwrap(), a);
        assert_eq!(storage.read("same.png").await.unwrap().unwrap(), b);
        storage.put("same.jpg", b"replacement").await.unwrap();
        assert_eq!(
            storage.read("same.jpg").await.unwrap().unwrap(),
            b"replacement"
        );
        assert!(
            storage
                .put_staged("same.jpg", &dir.path().join("missing"))
                .await
                .is_err()
        );
        assert_eq!(
            storage.read("same.jpg").await.unwrap().unwrap(),
            b"replacement"
        );
        assert!(
            storage
                .open_range("same.jpg", 5, 2)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .list("")
                .await
                .unwrap()
                .iter()
                .all(|k| !k.ends_with(".part"))
        );
    }

    #[tokio::test]
    async fn s3_provider_fails_fast_when_unsupported() {
        let err = Storage::build(&MediaStorageConfig {
            provider: "s3".into(),
            dir: "unused".into(),
        })
        .unwrap_err();
        // `message()` masks internals on purpose; the Display chain keeps it.
        assert!(err.to_string().contains("object-storage"), "{}", err);
        assert!(matches!(err, AppError::Internal(_)));
    }
}
