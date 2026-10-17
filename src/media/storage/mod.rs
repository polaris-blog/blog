//! Storage abstraction — the media service never touches the filesystem
//! directly.
//!
//! ```text
//! MediaService → StorageSet ── active (reads + writes)  ── Local / S3
//!                        └── mirrors (write-through)    ── Local / S3
//! ```
//!
//! Multiple named storages can be configured at once (`[[media.storages]]`):
//! one *active* storage serves reads and receives writes, and optional
//! *mirrors* receive a write-through copy of every object (CDN warm-up,
//! cloud backup). Storage keys are provider-relative
//! (`2026/08/8c7d2f91.webp`), so switching the active storage — or adding a
//! mirror — never rewrites database rows.
//!
//! Providers: `local` (zero dependencies) and `s3` (any S3-compatible
//! service: AWS S3, Cloudflare R2, MinIO, Backblaze B2…). SigV4 signing is
//! implemented in-tree over the existing blocking HTTP client, so the
//! static musl build keeps its small, dependency-free footprint.
//!
//! Security: keys are validated segment-by-segment (no `..`, no absolute
//! paths, no Windows reserved separators) before any provider sees them.

pub mod local;
pub mod s3;

use axum::body::Body;
use tokio_util::io::ReaderStream;

use crate::config::{MediaConfig, StorageEntry};
use crate::error::{AppError, AppResult};

pub use local::LocalStorage;
pub use s3::S3Storage;

/// A stored object opened for streaming responses.
pub struct StoredObject {
    pub size: u64,
    pub body: Body,
}

/// Validate a provider-relative storage key. Rejects traversal, absolute
/// keys, control characters and Windows reserved names — the rules are
/// provider-independent so a key that is safe for local disk is also safe
/// as an object-store key.
pub fn validate_key(key: &str) -> AppResult<()> {
    if key.is_empty() {
        return Err(AppError::BadRequest("empty storage key".into()));
    }
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
    }
    Ok(())
}

/// Runtime-selected storage provider. Enum dispatch (like `CacheBackend`
/// and `SearchProvider`) keeps calls monomorphic with no boxed futures.
#[derive(Debug)]
pub enum Storage {
    Local(LocalStorage),
    S3(S3Storage),
}

impl Storage {
    /// Build one configured storage entry. Fails at startup (never silently
    /// falls back) when a provider is selected that this build lacks or a
    /// provider's configuration is incomplete.
    pub fn build_entry(entry: &StorageEntry) -> AppResult<Self> {
        let kind = entry.kind();
        match kind {
            "local" => {
                let root = std::path::PathBuf::from(&entry.dir);
                std::fs::create_dir_all(&root)?;
                // Staging dir for streamed uploads (same filesystem → O(1) rename).
                std::fs::create_dir_all(root.join(".tmp"))?;
                Ok(Self::Local(LocalStorage::new(root)))
            }
            "s3" => Ok(Self::S3(S3Storage::new(entry)?)),
            _ => Err(AppError::BadRequest(format!(
                "unknown storage provider '{}' for storage '{}' (expected \"local\" or \"s3\")",
                entry.r#type, entry.id
            ))),
        }
    }

    /// Provider name (admin/CLI status display).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
            Self::S3(_) => "s3",
        }
    }

    /// One-line configuration summary for admin/CLI display.
    pub fn summary(&self) -> String {
        match self {
            Self::Local(l) => l.root().display().to_string(),
            Self::S3(s) => format!("{} @ {}", s.bucket(), s.endpoint()),
        }
    }

    /// Root path when the provider is filesystem-backed (CLI maintenance).
    pub fn local_root(&self) -> Option<&std::path::Path> {
        match self {
            Self::Local(l) => Some(l.root()),
            Self::S3(_) => None,
        }
    }

    /// Reserve a staging file for a streamed upload. The caller writes into
    /// it and hands it to [`Storage::put_staged`]. For local storage the
    /// staging area shares the filesystem with the final location, so
    /// committing is a rename; object storage stages in the system temp dir
    /// and streams the file up on commit.
    pub fn staging_file(&self) -> AppResult<std::path::PathBuf> {
        match self {
            Self::Local(l) => {
                let dir = l.root().join(".tmp");
                std::fs::create_dir_all(&dir)?;
                let name = crate::utils::cookies::random_token(16);
                Ok(dir.join(name))
            }
            Self::S3(_) => {
                let dir = std::env::temp_dir().join("polaris-staging");
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
                let tmp = local::temporary_sibling(&path);
                if let Err(error) = tokio::fs::write(&tmp, data).await {
                    tokio::fs::remove_file(&tmp).await.ok();
                    return Err(error.into());
                }
                local::commit_temporary(&tmp, &path).await
            }
            Self::S3(s) => s.put(key, data).await,
        }
    }

    /// Commit a staged upload under `key` (streaming path for large files).
    pub async fn put_staged(&self, key: &str, staged: &std::path::Path) -> AppResult<()> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                if tokio::fs::rename(staged, &path).await.is_err() {
                    // Cross-device uploads still become visible atomically.
                    local::copy_atomic(staged, &path).await?;
                    tokio::fs::remove_file(staged).await.ok();
                }
                Ok(())
            }
            Self::S3(s) => s.put_staged(key, staged).await,
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
            Self::S3(s) => s.read(key).await,
        }
    }

    /// Open an object as a raw async reader (backup streaming). The reader
    /// is boxed so provider-specific types stay behind this abstraction —
    /// callers stream through it in bounded chunks and never hold a whole
    /// object in memory. Object storage streams through the HTTP body.
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
            Self::S3(s) => s.open_reader(key).await,
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
            Self::S3(s) => s.open(key).await,
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
            Self::S3(s) => s.open_range(key, start, end).await,
        }
    }

    /// Stream an object through SHA-256 (integrity verification).
    pub async fn hash_of(&self, key: &str) -> AppResult<Option<String>> {
        match self {
            Self::Local(l) => {
                let path = l.resolve(key)?;
                match tokio::fs::File::open(&path).await {
                    Ok(_) => Ok(Some(local::hash_file(&path).await?)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Self::S3(s) => s.hash_of(key).await,
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
            Self::S3(s) => s.size_of(key).await,
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
                local::copy_atomic(&src, &dst).await?;
                Ok(())
            }
            Self::S3(s) => s.copy(from, to).await,
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
            Self::S3(s) => s.delete(key).await,
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
            Self::S3(s) => s.list(prefix).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-storage set: active storage + write-through mirrors
// ---------------------------------------------------------------------------

/// Status summary of one configured storage (admin/CLI display).
#[derive(Clone, Debug)]
pub struct StorageInfo {
    pub id: String,
    pub kind: String,
    pub summary: String,
    pub active: bool,
    pub mirror: bool,
}

/// The configured storages: one *active* (reads + writes) and zero or more
/// *mirrors* (write-through copies of every write).
#[derive(Debug)]
pub struct StorageSet {
    active_id: String,
    active: Storage,
    mirrors: Vec<Storage>,
    infos: Vec<StorageInfo>,
}

impl StorageSet {
    /// Build every configured storage and validate the topology: exactly
    /// one active id, mirror ids that exist and differ from the active one.
    pub fn build(cfg: &MediaConfig) -> AppResult<Self> {
        let storage_cfg = cfg.storage.normalized();
        if storage_cfg.storages.is_empty() {
            return Err(AppError::BadRequest(
                "no storages configured — expected [[media.storages]] or legacy [media.storage] provider/dir"
                    .into(),
            ));
        }
        let mut by_id = std::collections::HashMap::new();
        for entry in &storage_cfg.storages {
            if entry.id.is_empty() {
                return Err(AppError::BadRequest(
                    "every [[media.storages]] entry needs an `id`".into(),
                ));
            }
            if by_id.insert(entry.id.clone(), entry).is_some() {
                return Err(AppError::BadRequest(format!(
                    "duplicate storage id '{}'",
                    entry.id
                )));
            }
        }
        let active_id = storage_cfg.active.clone();
        let Some(active_entry) = by_id.get(&active_id) else {
            return Err(AppError::BadRequest(format!(
                "active storage '{active_id}' is not configured in [[media.storages]]"
            )));
        };
        let active = Storage::build_entry(active_entry)?;

        let mut mirror_ids = Vec::new();
        for id in &storage_cfg.mirror {
            if *id == active_id {
                return Err(AppError::BadRequest(
                    "a storage cannot mirror itself (mirror id equals the active id)".into(),
                ));
            }
            if !by_id.contains_key(id) {
                return Err(AppError::BadRequest(format!(
                    "mirror storage '{id}' is not configured in [[media.storages]]"
                )));
            }
            mirror_ids.push(id.clone());
        }
        let mut mirrors = Vec::new();
        for id in &mirror_ids {
            mirrors.push(Storage::build_entry(by_id[id])?);
        }

        let infos = storage_cfg
            .storages
            .iter()
            .map(|e| StorageInfo {
                id: e.id.clone(),
                kind: e.kind().to_string(),
                summary: Storage::build_entry(e)
                    .map(|s| s.summary())
                    .unwrap_or_else(|_| "unavailable".into()),
                active: e.id == active_id,
                mirror: mirror_ids.contains(&e.id),
            })
            .collect();

        Ok(Self {
            active_id,
            active,
            mirrors,
            infos,
        })
    }

    /// The active storage — every read and every backup/restore flows
    /// through it.
    pub fn get(&self) -> &Storage {
        &self.active
    }

    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    pub fn mirrors(&self) -> &[Storage] {
        &self.mirrors
    }

    pub fn infos(&self) -> &[StorageInfo] {
        &self.infos
    }

    /// Write an object to the active storage, then write it through to
    /// every mirror. A mirror failure is logged, not propagated: the
    /// primary write succeeded and the visitor's upload must not fail
    /// because a redundant copy could not be placed.
    pub async fn put(&self, key: &str, data: &[u8]) -> AppResult<()> {
        self.active.put(key, data).await?;
        for m in &self.mirrors {
            if let Err(e) = m.put(key, data).await {
                tracing::warn!(storage = m.name(), key, error = %e, "mirror write failed");
            }
        }
        Ok(())
    }

    pub async fn put_staged(&self, key: &str, staged: &std::path::Path) -> AppResult<()> {
        self.active.put_staged(key, staged).await?;
        for m in &self.mirrors {
            if let Err(e) = m.put_staged(key, staged).await {
                tracing::warn!(storage = m.name(), key, error = %e, "mirror write failed");
            }
        }
        Ok(())
    }

    pub async fn copy(&self, from: &str, to: &str) -> AppResult<()> {
        self.active.copy(from, to).await?;
        for m in &self.mirrors {
            if let Err(e) = m.copy(from, to).await {
                tracing::warn!(storage = m.name(), from, to, error = %e, "mirror copy failed");
            }
        }
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> AppResult<()> {
        self.active.delete(key).await?;
        for m in &self.mirrors {
            if let Err(e) = m.delete(key).await {
                tracing::warn!(storage = m.name(), key, error = %e, "mirror delete failed");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MediaConfig, MediaStorageConfig};

    fn cfg(dir: &str) -> MediaConfig {
        MediaConfig {
            storage: MediaStorageConfig {
                provider: "local".into(),
                dir: dir.into(),
                ..MediaStorageConfig::default()
            },
            ..MediaConfig::default()
        }
    }

    fn multi_cfg(dir_a: &str, dir_b: &str) -> MediaConfig {
        let mut c = cfg(dir_a);
        c.storage.active = "local".into();
        c.storage.mirror = vec!["backup".into()];
        c.storage.storages = vec![
            StorageEntry {
                id: "local".into(),
                r#type: "local".into(),
                dir: dir_a.into(),
                ..StorageEntry::default()
            },
            StorageEntry {
                id: "backup".into(),
                r#type: "local".into(),
                dir: dir_b.into(),
                ..StorageEntry::default()
            },
        ];
        c
    }

    #[tokio::test]
    async fn put_read_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let set = StorageSet::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        assert_eq!(set.active_id(), "local");
        set.put("2026/08/abc.webp", b"hello").await.unwrap();
        assert_eq!(
            set.get().read("2026/08/abc.webp").await.unwrap().unwrap(),
            b"hello"
        );
        assert_eq!(
            set.get().size_of("2026/08/abc.webp").await.unwrap(),
            Some(5)
        );
        assert!(set.get().exists("2026/08/abc.webp").await.unwrap());
        set.delete("2026/08/abc.webp").await.unwrap();
        assert!(!set.get().exists("2026/08/abc.webp").await.unwrap());
    }

    #[tokio::test]
    async fn traversal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let set = StorageSet::build(&cfg(dir.path().to_str().unwrap())).unwrap();
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
                set.get().read(bad).await.is_err(),
                "key must be rejected: {bad}"
            );
        }
    }

    #[tokio::test]
    async fn list_skips_tmp_and_lists_keys() {
        let dir = tempfile::tempdir().unwrap();
        let set = StorageSet::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        set.put("2026/08/a.webp", b"a").await.unwrap();
        set.put("2026/09/b.jpg", b"b").await.unwrap();
        let keys = set.get().list("").await.unwrap();
        assert_eq!(
            keys,
            vec!["2026/08/a.webp".to_string(), "2026/09/b.jpg".to_string()]
        );
        let aug = set.get().list("2026/08").await.unwrap();
        assert_eq!(aug, vec!["2026/08/a.webp".to_string()]);
    }

    #[tokio::test]
    async fn staged_uploads_commit_by_rename() {
        let dir = tempfile::tempdir().unwrap();
        let set = StorageSet::build(&cfg(dir.path().to_str().unwrap())).unwrap();
        let staged = set.get().staging_file().unwrap();
        std::fs::write(&staged, b"streamed").unwrap();
        set.put_staged("2026/10/x.bin", &staged).await.unwrap();
        assert!(!staged.exists());
        assert_eq!(
            set.get().read("2026/10/x.bin").await.unwrap().unwrap(),
            b"streamed"
        );
    }

    #[tokio::test]
    async fn mirror_receives_write_through_copies() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let set = StorageSet::build(&multi_cfg(
            a.path().to_str().unwrap(),
            b.path().to_str().unwrap(),
        ))
        .unwrap();
        assert_eq!(set.mirrors().len(), 1);
        assert_eq!(set.infos().len(), 2);
        assert!(set.infos()[0].active);
        assert!(set.infos()[1].mirror);

        // Writes land on the active storage *and* the mirror.
        set.put("2026/08/a.webp", b"payload").await.unwrap();
        assert_eq!(
            set.get().read("2026/08/a.webp").await.unwrap().unwrap(),
            b"payload"
        );
        assert_eq!(
            set.mirrors()[0]
                .read("2026/08/a.webp")
                .await
                .unwrap()
                .unwrap(),
            b"payload"
        );

        // Deletes propagate to the mirror as well.
        set.delete("2026/08/a.webp").await.unwrap();
        assert!(!set.mirrors()[0].exists("2026/08/a.webp").await.unwrap());
    }

    #[tokio::test]
    async fn invalid_topologies_are_rejected() {
        let mut c = multi_cfg("/tmp/polaris-a", "/tmp/polaris-b");
        // Unknown active id.
        c.storage.active = "nope".into();
        assert!(StorageSet::build(&c).is_err());
        // Mirror that is also the active storage.
        let mut c2 = multi_cfg("/tmp/polaris-a", "/tmp/polaris-b");
        c2.storage.mirror = vec!["local".into()];
        assert!(StorageSet::build(&c2).is_err());
        // Mirror id that does not exist.
        let mut c3 = multi_cfg("/tmp/polaris-a", "/tmp/polaris-b");
        c3.storage.mirror = vec!["ghost".into()];
        assert!(StorageSet::build(&c3).is_err());
    }
}
