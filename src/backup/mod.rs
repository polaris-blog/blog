//! Backup & restore subsystem.
//!
//! ```text
//! Admin UI / CLI → BackupService → Db (portable SQL) + media Storage +
//!                                  BackupStorage (archive provider)
//! ```
//!
//! Archive layout (versioned via `manifest.toml`):
//! ```text
//! polaris-<stamp>-<id>-<kind>.zip
//! ├── manifest.toml      # format version, versions, counts, SHA-256 per entry
//! ├── database.dump      # NDJSON portable dump (all core tables)
//! ├── media/<key>        # storage objects (same keys as the media provider)
//! ├── themes/<id>/…      # full backups only
//! └── plugins/<id>/…     # full backups only
//! ```
//!
//! Design rules:
//! - Everything streams — no archive entry, table or media object is ever
//!   fully buffered in memory.
//! - `security.secret` never leaves the instance: it is excluded from the
//!   dump and preserved across restores, so AES-GCM-encrypted configuration
//!   values stay readable and the acting admin's session stays valid.
//! - Derived state (search index, caches) is rebuilt, not backed up.
//! - Restore verifies first, snapshots the current database second, and
//!   only then swaps — any failure leaves the previous state intact.

pub mod format;
pub mod manifest;
pub mod restore;
pub mod scheduler;
pub mod service;
pub mod storage;
pub mod verify;

pub use service::{BackupKind, BackupService, BackupSummary};
