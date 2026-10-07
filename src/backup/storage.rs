//! Backup storage abstraction.
//!
//! ```text
//! BackupService → BackupStorage (enum dispatch)
//!                   └── LocalBackupStorage   <backup-dir>/<name>.zip
//! ```
//!
//! S3-compatible providers (S3 / R2 / MinIO) follow the same pattern as the
//! media `Storage` layer: selecting one in a build without object-storage
//! support fails fast at startup instead of silently falling back. The
//! service only ever touches this abstraction — adding a provider means
//! adding a variant here, never touching backup logic.
//!
//! All names handed to this layer go through [`valid_backup_name`]: plain
//! file names, no separators, no traversal, `.zip` suffix only.

use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

/// The only shape a backup file name may have. Blocks traversal, separators
/// and anything unexpected (`backup_id` values are stamped by us, uploaded
/// archives are stored under their sanitized id).
pub fn valid_backup_name(name: &str) -> bool {
    if !name.ends_with(".zip") || name.len() > 200 {
        return false;
    }
    let stem = &name[..name.len() - 4];
    !stem.is_empty()
        && stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !stem.contains("..")
}

#[derive(Debug)]
pub struct LocalBackupStorage {
    dir: PathBuf,
    tmp_dir: PathBuf,
}

#[derive(Debug)]
pub enum BackupStorage {
    Local(LocalBackupStorage),
}

impl BackupStorage {
    /// Build the configured provider. Local roots are created eagerly so a
    /// first backup never fails on a missing directory.
    pub fn build(dir: &str, tmp_dir: &str) -> AppResult<Self> {
        let dir = PathBuf::from(dir);
        let tmp_dir = PathBuf::from(tmp_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("cannot create backup dir: {e}")))?;
        std::fs::create_dir_all(&tmp_dir).map_err(|e| {
            AppError::Internal(anyhow::anyhow!("cannot create backup tmp dir: {e}"))
        })?;
        Ok(Self::Local(LocalBackupStorage { dir, tmp_dir }))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
        }
    }

    /// Staging path for a new archive (inside the provider's tmp area so
    /// the final `commit` is a same-filesystem rename).
    pub fn staging_path(&self, id: &str) -> AppResult<PathBuf> {
        if !valid_backup_name(&format!("{id}.zip")) {
            return Err(AppError::BadRequest("invalid backup id".into()));
        }
        match self {
            Self::Local(l) => Ok(l.tmp_dir.join(format!("{id}.zip.part"))),
        }
    }

    /// Staging path for an uploaded archive (streamed by the HTTP/CLI layer).
    pub fn upload_staging_path(&self) -> AppResult<PathBuf> {
        match self {
            Self::Local(l) => Ok(l.tmp_dir.join(format!(
                "upload-{}.zip",
                crate::utils::cookies::random_token(16)
            ))),
        }
    }

    /// Atomically move a finished staging archive into the backup listing.
    pub async fn commit(&self, staging: &Path, id: &str) -> AppResult<PathBuf> {
        let name = format!("{id}.zip");
        if !valid_backup_name(&name) {
            return Err(AppError::BadRequest("invalid backup id".into()));
        }
        let dest = self.path_of(&name)?;
        if dest.exists() {
            return Err(AppError::Conflict(format!(
                "backup '{name}' already exists"
            )));
        }
        // Staging and the backup store may sit on different filesystems.
        if let Err(e) = tokio::fs::rename(&staging, &dest).await {
            if e.kind() != std::io::ErrorKind::CrossesDevices && e.raw_os_error() != Some(18) {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "cannot commit backup '{name}': {e}"
                )));
            }
            tokio::fs::copy(&staging, &dest).await.map_err(|e| {
                AppError::Internal(anyhow::anyhow!("cannot commit backup '{name}': {e}"))
            })?;
            tokio::fs::remove_file(&staging).await.ok();
        }
        Ok(dest)
    }

    /// Final path of a stored backup (name pre-validated).
    pub fn path_of(&self, name: &str) -> AppResult<PathBuf> {
        if !valid_backup_name(name) {
            return Err(AppError::BadRequest("invalid backup file name".into()));
        }
        match self {
            Self::Local(l) => Ok(l.dir.join(name)),
        }
    }

    /// Sorted list of stored backup file names (oldest first by name; the
    /// service re-sorts by manifest time).
    pub async fn list(&self) -> AppResult<Vec<String>> {
        match self {
            Self::Local(l) => {
                let mut out = Vec::new();
                let mut rd = tokio::fs::read_dir(&l.dir).await?;
                while let Some(entry) = rd.next_entry().await? {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if entry.file_type().await?.is_file() && valid_backup_name(&name) {
                        out.push(name);
                    }
                }
                out.sort();
                Ok(out)
            }
        }
    }

    pub async fn delete(&self, name: &str) -> AppResult<bool> {
        let path = self.path_of(name)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AppError::Internal(anyhow::anyhow!(
                "cannot delete backup '{name}': {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_name_validation() {
        assert!(valid_backup_name(
            "polaris-20260905-091530-a1b2c3d4-full.zip"
        ));
        assert!(valid_backup_name("pre-restore-20260905.zip"));
        assert!(!valid_backup_name("x"));
        assert!(!valid_backup_name("x.rar"));
        assert!(!valid_backup_name("../evil.zip"));
        assert!(!valid_backup_name("a/b.zip"));
        assert!(!valid_backup_name("a\\b.zip"));
        assert!(!valid_backup_name("with space.zip"));
        assert!(!valid_backup_name("..zip"));
    }

    #[tokio::test]
    async fn local_storage_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("b");
        let store = BackupStorage::build(
            root.join("backups").to_str().unwrap(),
            root.join("tmp").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(store.name(), "local");

        let staging = store.staging_path("20260905-000000-deadbeef").unwrap();
        tokio::fs::write(&staging, b"payload").await.unwrap();
        let dest = store
            .commit(&staging, "20260905-000000-deadbeef")
            .await
            .unwrap();
        assert!(dest.exists());
        assert_eq!(
            store.list().await.unwrap(),
            vec!["20260905-000000-deadbeef.zip".to_string()]
        );

        assert!(store.delete("20260905-000000-deadbeef.zip").await.unwrap());
        assert!(!store.delete("20260905-000000-deadbeef.zip").await.unwrap());
        assert!(store.list().await.unwrap().is_empty());

        // Hostile names never resolve to paths.
        assert!(store.path_of("../evil.zip").is_err());
    }
}
