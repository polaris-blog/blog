//! Versioned backup manifest.
//!
//! Every archive carries a `manifest.toml` entry that fully describes the
//! backup: who made it, from which Polaris/database version, what it
//! contains, how many files there are and the SHA-256 of every entry.
//! Restore refuses archives whose manifest is missing, malformed, written
//! by a newer Polaris, or whose declared integrity does not hold.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Current backup format version. Bump when the layout changes and keep the
/// reader able to accept every older version it can still understand.
pub const FORMAT_VERSION: i64 = 1;

/// Oldest format version this build can restore.
pub const MIN_FORMAT_VERSION: i64 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupIncludes {
    pub database: bool,
    pub media: bool,
    pub themes: bool,
    pub plugins: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupCounts {
    /// Total zip entries covered by `checksums` (excluding the manifest).
    pub files: usize,
    pub db_tables: usize,
    pub db_rows: i64,
    pub media_files: usize,
    pub theme_files: usize,
    pub plugin_files: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BackupSizes {
    /// Uncompressed byte size of each top-level part.
    pub database: u64,
    pub media: u64,
    pub themes: u64,
    pub plugins: u64,
    /// Sum of all parts (the write budget used while creating the archive).
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BackupManifest {
    pub format_version: i64,
    pub polaris_version: String,
    /// Unix epoch seconds.
    pub created_at: i64,
    /// Database dialect the dump was taken from: sqlite | mysql | postgres.
    pub dialect: String,
    /// `full` | `database` | `media` — the requested backup kind.
    pub kind: String,
    /// Stable id, e.g. `20260905-091530-a1b2c3d4`.
    pub backup_id: String,
    /// Who triggered the backup (username / "cli" / "scheduler" / "pre-restore").
    pub created_by: String,
    pub includes: BackupIncludes,
    pub counts: BackupCounts,
    pub sizes: BackupSizes,
    /// Zip entry name → SHA-256 (hex) of the uncompressed entry content.
    /// The manifest itself is excluded (it would self-reference).
    #[serde(default)]
    pub checksums: BTreeMap<String, String>,
}

impl BackupManifest {
    /// Serialize to TOML (the `manifest.toml` entry).
    pub fn to_toml(&self) -> AppResult<String> {
        toml::to_string_pretty(self).map_err(|e| {
            AppError::Internal(anyhow::anyhow!("cannot serialize backup manifest: {e}"))
        })
    }

    /// Parse and structurally validate a `manifest.toml`.
    pub fn parse(raw: &str) -> AppResult<Self> {
        let m: BackupManifest = toml::from_str(raw)
            .map_err(|e| AppError::BadRequest(format!("invalid backup manifest: {e}")))?;
        m.validate()?;
        Ok(m)
    }

    /// Structural validation (everything that does not need instance state).
    pub fn validate(&self) -> AppResult<()> {
        if self.format_version < MIN_FORMAT_VERSION || self.format_version > FORMAT_VERSION {
            return Err(AppError::BadRequest(format!(
                "backup format version {} is not supported (this build understands {MIN_FORMAT_VERSION}..={FORMAT_VERSION})",
                self.format_version
            )));
        }
        if self.polaris_version.is_empty() {
            return Err(AppError::BadRequest(
                "manifest is missing polaris_version".into(),
            ));
        }
        if !matches!(self.dialect.as_str(), "sqlite" | "mysql" | "postgres") {
            return Err(AppError::BadRequest(format!(
                "unknown database dialect '{}' in manifest",
                self.dialect
            )));
        }
        if !matches!(self.kind.as_str(), "full" | "database" | "media") {
            return Err(AppError::BadRequest(format!(
                "unknown backup kind '{}' in manifest",
                self.kind
            )));
        }
        if self.created_at <= 0 {
            return Err(AppError::BadRequest("manifest has no created_at".into()));
        }
        if !self.includes.database
            && !self.includes.media
            && !self.includes.themes
            && !self.includes.plugins
        {
            return Err(AppError::BadRequest(
                "manifest includes nothing (empty backup)".into(),
            ));
        }
        if self.includes.database && self.counts.db_tables == 0 {
            return Err(AppError::BadRequest(
                "manifest claims a database but lists no tables".into(),
            ));
        }
        if self.checksums.is_empty() {
            return Err(AppError::BadRequest(
                "manifest carries no checksums — integrity cannot be verified".into(),
            ));
        }
        if self.counts.files != self.checksums.len() {
            return Err(AppError::BadRequest(format!(
                "manifest file count ({}) does not match its checksum list ({})",
                self.counts.files,
                self.checksums.len()
            )));
        }
        for name in self.checksums.keys() {
            if !is_safe_entry_name(name) {
                return Err(AppError::BadRequest(format!(
                    "manifest references an unsafe entry name: '{name}'"
                )));
            }
        }
        Ok(())
    }

    /// Cross-version compatibility. Restoring a backup written by a *newer*
    /// Polaris may feed rows the current schema cannot hold — blocked unless
    /// the operator forces it (CLI only). Older versions are fine: the
    /// migration runner brings the schema up after the restore.
    pub fn compatibility_error(&self, current_version: &str) -> Option<String> {
        let Ok(current) = semver::Version::parse(current_version) else {
            return Some("current Polaris version is not valid semver".into());
        };
        let Ok(backup) = semver::Version::parse(&self.polaris_version) else {
            return Some(format!(
                "backup declares a non-semver Polaris version '{}'",
                self.polaris_version
            ));
        };
        if backup > current {
            return Some(format!(
                "backup was created by Polaris {} which is newer than this build ({}) — \
                 upgrade Polaris before restoring",
                backup, current
            ));
        }
        None
    }

    /// True when the entry name belongs to the given top-level part
    /// (`database.dump`, `media/…`, `themes/…`, `plugins/…`).
    pub fn part_of(&self, entry: &str) -> &'static str {
        if entry == DATABASE_ENTRY {
            "database"
        } else if entry.starts_with("media/") {
            "media"
        } else if entry.starts_with("themes/") {
            "themes"
        } else if entry.starts_with("plugins/") {
            "plugins"
        } else {
            ""
        }
    }
}

/// The database dump entry name inside the archive.
pub const DATABASE_ENTRY: &str = "database.dump";
/// The manifest entry name inside the archive.
pub const MANIFEST_ENTRY: &str = "manifest.toml";

/// Entry names are plain relative paths under the archive root. Anything
/// else (absolute, traversal, backslash, control bytes) is rejected before
/// it can ever reach the filesystem.
pub fn is_safe_entry_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 512 {
        return false;
    }
    if name.contains('\\') || name.contains('\0') || name.starts_with('/') {
        return false;
    }
    if name.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return false;
    }
    name.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BackupManifest {
        BackupManifest {
            format_version: FORMAT_VERSION,
            polaris_version: "0.1.0".into(),
            created_at: 1_000,
            dialect: "sqlite".into(),
            kind: "full".into(),
            backup_id: "20260905-091530-a1b2c3d4".into(),
            created_by: "admin".into(),
            includes: BackupIncludes {
                database: true,
                media: true,
                themes: true,
                plugins: false,
            },
            counts: BackupCounts {
                files: 2,
                db_tables: 14,
                db_rows: 10,
                media_files: 1,
                theme_files: 1,
                plugin_files: 0,
            },
            sizes: BackupSizes {
                database: 100,
                media: 50,
                themes: 10,
                plugins: 0,
                total: 160,
            },
            checksums: BTreeMap::from([
                (DATABASE_ENTRY.to_string(), "a".repeat(64)),
                ("media/2026/09/x.png".to_string(), "b".repeat(64)),
            ]),
        }
    }

    #[test]
    fn toml_roundtrip() {
        let m = sample();
        let raw = m.to_toml().unwrap();
        let parsed = BackupManifest::parse(&raw).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn rejects_unsupported_format_version() {
        let mut m = sample();
        m.format_version = FORMAT_VERSION + 1;
        assert!(m.validate().is_err());
        m.format_version = MIN_FORMAT_VERSION - 1;
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_count_checksum_mismatch_and_bad_names() {
        let mut m = sample();
        m.counts.files = 3;
        assert!(m.validate().is_err());

        let mut m = sample();
        m.checksums.insert("../evil".into(), "c".repeat(64));
        assert!(m.validate().is_err());

        let mut m = sample();
        m.checksums.insert("/abs".into(), "c".repeat(64));
        assert!(m.validate().is_err());
    }

    #[test]
    fn empty_backup_rejected() {
        let mut m = sample();
        m.includes = BackupIncludes {
            database: false,
            media: false,
            themes: false,
            plugins: false,
        };
        assert!(m.validate().is_err());
    }

    #[test]
    fn newer_polaris_version_blocked() {
        let mut m = sample();
        m.polaris_version = "99.0.0".into();
        assert!(m.compatibility_error("0.1.0").is_some());
        m.polaris_version = "0.1.0".into();
        assert!(m.compatibility_error("0.1.0").is_none());
        // Older backups are fine (migrations run after restore).
        m.polaris_version = "0.0.9".into();
        assert!(m.compatibility_error("0.1.0").is_none());
    }

    #[test]
    fn safe_entry_names() {
        assert!(is_safe_entry_name("media/2026/09/x.png"));
        assert!(is_safe_entry_name(DATABASE_ENTRY));
        assert!(!is_safe_entry_name("a/../b"));
        assert!(!is_safe_entry_name("a\\b"));
        assert!(!is_safe_entry_name(""));
        assert!(!is_safe_entry_name("//x"));
    }
}
