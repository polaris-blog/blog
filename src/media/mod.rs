//! Media management — a core capability of Polaris, not a standalone DAM.
//!
//! ```text
//!                    Media API (HTTP)
//!                          │
//!                          ↓
//!                     MediaService
//!           ┌──────────────┼──────────────┐
//!           ↓              ↓              ↓
//!       Validator      Metadata       Permission (RBAC)
//!           └──────────────┼──────────────┘
//!                          ↓
//!                      Storage API
//!                          ↓
//!                  Local / (S3 / R2 / MinIO optional)
//! ```
//!
//! Principles (see the project design doc):
//! - **Metadata in the database, bytes in storage** — never BLOBs in SQL.
//! - **Streaming first** — uploads stream through SHA-256 into a staging
//!   file; only images are ever buffered (for processing), and they are
//!   size-capped.
//! - **Content hash** — SHA-256 of the stored bytes drives dedup, ETags and
//!   integrity verification.
//! - **URL ↔ key decoupling** — public URLs use `/media/{uuid}.{ext}`;
//!   storage keys use `YYYY/MM/{uuid}.{ext}`. Moving to a CDN or object
//!   storage never rewrites database rows.
//! - **Privacy by default** — EXIF is stripped from JPEGs, sensitive fields
//!   never reach visitors, SVGs are sanitized with an allow-list.

pub mod image;
pub mod storage;
pub mod validate;

use std::path::Path;

use crate::config::MediaConfig;
use crate::error::AppResult;
use crate::utils::time;

pub use storage::{Storage, StorageInfo, StorageSet};

/// The media subsystem's runtime handle: the configured storages (active +
/// mirrors) plus the media configuration. Business logic lives in
/// `crate::services::media` (it needs `db`/`cache`/`search` access and
/// composes this service with them).
pub struct MediaService {
    storages: StorageSet,
    cfg: MediaConfig,
    pub(crate) image_workers: std::sync::Arc<tokio::sync::Semaphore>,
}

impl MediaService {
    /// Build the storage set; fails fast at startup when the configuration
    /// selects a provider this build does not ship or the topology is
    /// invalid (unknown active/mirror id, duplicate ids, …).
    pub fn build(cfg: &MediaConfig) -> AppResult<Self> {
        let storages = StorageSet::build(cfg)?;
        Ok(Self {
            storages,
            cfg: cfg.clone(),
            image_workers: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }

    /// The active storage — reads, staging, backups and maintenance flow
    /// through it.
    pub fn storage(&self) -> &Storage {
        self.storages.get()
    }

    /// Status of every configured storage (admin/CLI display).
    pub fn storage_infos(&self) -> &[StorageInfo] {
        self.storages.infos()
    }

    pub fn active_storage_id(&self) -> &str {
        self.storages.active_id()
    }

    /// Mirror storages receiving write-through copies (maintenance/inspection).
    pub fn mirror_storages(&self) -> &[Storage] {
        self.storages.mirrors()
    }

    // -- write-through operations (active + mirrors) ------------------------

    pub async fn put_object(&self, key: &str, data: &[u8]) -> AppResult<()> {
        self.storages.put(key, data).await
    }

    pub async fn put_staged_object(&self, key: &str, staged: &Path) -> AppResult<()> {
        self.storages.put_staged(key, staged).await
    }

    pub async fn copy_object(&self, from: &str, to: &str) -> AppResult<()> {
        self.storages.copy(from, to).await
    }

    pub async fn delete_object(&self, key: &str) -> AppResult<()> {
        self.storages.delete(key).await
    }

    pub fn config(&self) -> &MediaConfig {
        &self.cfg
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// Storage key for an original: `2026/08/{uuid}.{ext}`.
    pub fn storage_key(&self, uuid: &str, ext: &str) -> String {
        let d = time::breakdown(time::now());
        format!("{:04}/{:02}/{}.{}", d.year, d.month, uuid, ext)
    }

    /// Public URL path for an original (CDN prefix applied by the caller
    /// when configured).
    pub fn url_path(&self, uuid: &str, ext: &str) -> String {
        format!("/media/{}.{}", uuid, ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_key_uses_date_buckets() {
        let svc = MediaService::build(&MediaConfig::default()).unwrap();
        let key = svc.storage_key("abc123", "webp");
        assert!(key.ends_with("/abc123.webp"), "{key}");
        assert!(key.len() == "0000/00/abc123.webp".len(), "{key}");
    }

    #[test]
    fn legacy_config_folds_into_one_local_storage() {
        let cfg = MediaConfig::default();
        assert_eq!(cfg.storage.normalized().storages.len(), 1);
        assert_eq!(cfg.storage.normalized().storages[0].kind(), "local");
        assert_eq!(cfg.storage.normalized().storages[0].dir, "data/media");
    }

    #[tokio::test]
    async fn writes_flow_through_active_storage() {
        let dir = tempfile::tempdir().unwrap();
        let svc = MediaService::build(&MediaConfig::default()).unwrap();
        svc.put_object("2026/10/abc.webp", b"bytes").await.unwrap();
        assert_eq!(
            svc.storage()
                .read("2026/10/abc.webp")
                .await
                .unwrap()
                .unwrap(),
            b"bytes"
        );
        svc.delete_object("2026/10/abc.webp").await.unwrap();
        assert!(
            svc.storage()
                .read("2026/10/abc.webp")
                .await
                .unwrap()
                .is_none()
        );
        let _ = dir;
    }
}
