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

use crate::config::MediaConfig;
use crate::error::AppResult;
use crate::utils::time;

pub use storage::Storage;

/// The media subsystem's runtime handle: the configured storage provider
/// plus the media configuration. Business logic lives in
/// `crate::services::media` (it needs `db`/`cache`/`search` access and
/// composes this service with them).
pub struct MediaService {
    storage: Storage,
    cfg: MediaConfig,
    pub(crate) image_workers: std::sync::Arc<tokio::sync::Semaphore>,
}

impl MediaService {
    /// Build the storage provider; fails fast at startup when the
    /// configuration selects a provider this build does not ship.
    pub fn build(cfg: &MediaConfig) -> AppResult<Self> {
        let storage = Storage::build(&cfg.storage)?;
        Ok(Self {
            storage,
            cfg: cfg.clone(),
            image_workers: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
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
}
