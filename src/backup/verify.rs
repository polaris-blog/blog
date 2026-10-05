//! Backup integrity verification.
//!
//! Streams every entry named in the manifest out of the archive, recomputes
//! its SHA-256 and compares against the manifest. Nothing is ever buffered:
//! one entry at a time, 64 KiB at a time. Structural zip limits (entry
//! count, declared uncompressed size) are enforced *before* verification so
//! a hostile archive cannot waste time or disk.

use std::io::Read;

use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

use super::manifest::{BackupManifest, MANIFEST_ENTRY};

/// Archive-level limits (from `[backup]` configuration).
#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    pub max_files: usize,
    pub max_uncompressed_bytes: u64,
}

impl ArchiveLimits {
    pub fn from_backup_cfg(cfg: &crate::config::BackupConfig) -> Self {
        Self {
            max_files: cfg.max_files,
            max_uncompressed_bytes: cfg.max_uncompressed_bytes(),
        }
    }
}

/// Result of verifying one archive.
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub manifest_ok: bool,
    pub files_checked: usize,
    pub bytes_checked: u64,
    /// Entries named in the manifest but missing from the archive.
    pub missing: Vec<String>,
    /// Entries whose recomputed SHA-256 does not match the manifest.
    pub mismatched: Vec<String>,
    /// Archive entries not covered by the manifest (excluding the manifest
    /// itself) — unexpected content.
    pub unlisted: Vec<String>,
    pub errors: Vec<String>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.manifest_ok
            && self.missing.is_empty()
            && self.mismatched.is_empty()
            && self.unlisted.is_empty()
            && self.errors.is_empty()
    }

    pub fn summary(&self) -> String {
        use crate::i18n::tr;
        if self.ok() {
            let size = crate::services::media::human_size(self.bytes_checked as i64);
            tr(
                "verify.report.ok",
                &[("files", &self.files_checked.to_string()), ("size", &size)],
            )
        } else {
            tr(
                "verify.report.failed",
                &[
                    ("missing", &self.missing.len().to_string()),
                    ("corrupted", &self.mismatched.len().to_string()),
                    ("unlisted", &self.unlisted.len().to_string()),
                    ("errors", &self.errors.len().to_string()),
                ],
            )
        }
    }
}

/// Open an archive and run the structural + per-entry integrity checks.
/// `manifest` must already be parsed (callers use
/// [`super::service::BackupService::read_manifest`]).
pub fn verify_archive(
    path: &std::path::Path,
    manifest: &BackupManifest,
    limits: &ArchiveLimits,
) -> AppResult<VerifyReport> {
    let mut report = VerifyReport {
        manifest_ok: true,
        ..Default::default()
    };

    // Structural checks before touching any entry content.
    let file = std::fs::File::open(path)
        .map_err(|e| AppError::BadRequest(format!("cannot open backup: {e}")))?;
    let file_len = file
        .metadata()
        .map(|m| m.len())
        .map_err(|e| AppError::BadRequest(format!("cannot stat backup: {e}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::BadRequest(format!("invalid backup archive: {e}")))?;
    if archive.len() > limits.max_files {
        return Err(AppError::BadRequest(format!(
            "backup has {} entries (limit {})",
            archive.len(),
            limits.max_files
        )));
    }

    let mut declared_total: u64 = 0;
    let mut listed: std::collections::BTreeSet<String> =
        manifest.checksums.keys().cloned().collect();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| AppError::BadRequest(format!("cannot read backup entry #{i}: {e}")))?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        declared_total = declared_total.saturating_add(entry.size());
        if name == MANIFEST_ENTRY {
            continue;
        }
        if !listed.remove(&name) {
            report.unlisted.push(name);
        }
    }
    if declared_total > limits.max_uncompressed_bytes {
        return Err(AppError::BadRequest(format!(
            "backup expands to {} bytes (limit {}) — refusing to process",
            declared_total, limits.max_uncompressed_bytes
        )));
    }
    if file_len == 0 {
        report.errors.push("archive file is empty".into());
    }

    // Missing entries (declared but absent).
    report.missing = listed.into_iter().collect();

    // Content checks, one streamed entry at a time. The total actually read
    // is hard-capped: declared sizes can lie, so hashing a hostile archive
    // must not decompress unbounded data either.
    let mut actual_total: u64 = 0;
    for (name, expected) in &manifest.checksums {
        let mut entry = match archive.by_name(name) {
            Ok(e) => e,
            Err(_) => continue, // already reported as missing
        };
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut read: u64 = 0;
        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|e| AppError::BadRequest(format!("cannot read entry '{name}': {e}")))?;
            if n == 0 {
                break;
            }
            read += n as u64;
            actual_total += n as u64;
            if actual_total > limits.max_uncompressed_bytes {
                return Err(AppError::BadRequest(
                    "backup content expands beyond the size limit — refusing to process".into(),
                ));
            }
            hasher.update(&buf[..n]);
        }
        // Declared size must match too (truncated entries hash differently,
        // but the explicit check makes the report actionable).
        if read != entry.size() {
            // by_name gives the declared size; reading stopped at EOF. When
            // the stream is shorter than declared the archive is corrupt.
            report.errors.push(format!(
                "entry '{name}' is truncated ({} of {} bytes)",
                read,
                entry.size()
            ));
            continue;
        }
        let actual = format!("{:x}", hasher.finalize());
        if &actual != expected {
            report.mismatched.push(name.clone());
        } else {
            report.files_checked += 1;
            report.bytes_checked += read;
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::backup::manifest::DATABASE_ENTRY;
    use crate::backup::manifest::FORMAT_VERSION;

    fn write_zip(path: &Path, entries: &[(&str, Vec<u8>)]) -> Vec<(String, String)> {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut sums = Vec::new();
        for (name, data) in entries {
            zip.start_file(name.to_string(), opts).unwrap();
            let mut hasher = Sha256::new();
            hasher.update(data);
            sums.push((name.to_string(), format!("{:x}", hasher.finalize())));
            std::io::Write::write_all(&mut zip, data).unwrap();
        }
        zip.finish().unwrap();
        sums
    }

    fn manifest_with(sums: Vec<(String, String)>) -> BackupManifest {
        let mut checksums = std::collections::BTreeMap::new();
        for (n, s) in &sums {
            checksums.insert(n.clone(), s.clone());
        }
        BackupManifest {
            format_version: FORMAT_VERSION,
            polaris_version: "0.1.0".into(),
            created_at: 1,
            dialect: "sqlite".into(),
            kind: "database".into(),
            backup_id: "t".into(),
            created_by: "test".into(),
            includes: crate::backup::manifest::BackupIncludes {
                database: true,
                media: false,
                themes: false,
                plugins: false,
            },
            counts: crate::backup::manifest::BackupCounts {
                files: checksums.len(),
                db_tables: 1,
                db_rows: 0,
                media_files: 0,
                theme_files: 0,
                plugin_files: 0,
            },
            sizes: Default::default(),
            checksums,
        }
    }

    #[test]
    fn verify_detects_corruption_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.zip");
        let sums = write_zip(&path, &[(DATABASE_ENTRY, b"hello dump".to_vec())]);
        let m = manifest_with(sums.clone());
        let limits = ArchiveLimits {
            max_files: 100,
            max_uncompressed_bytes: 1 << 20,
        };

        let r = verify_archive(&path, &m, &limits).unwrap();
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.files_checked, 1);

        // Manifest claims an entry the archive does not have.
        let mut m2 = manifest_with(sums.clone());
        m2.checksums.insert("media/gone.png".into(), "0".repeat(64));
        m2.counts.files += 1;
        let r = verify_archive(&path, &m2, &limits).unwrap();
        assert!(!r.ok());
        assert_eq!(r.missing, vec!["media/gone.png".to_string()]);

        // Corrupted content: checksum in manifest no longer matches.
        let mut m3 = manifest_with(sums);
        if let Some(s) = m3.checksums.get_mut(DATABASE_ENTRY) {
            *s = "f".repeat(64);
        }
        let r = verify_archive(&path, &m3, &limits).unwrap();
        assert!(!r.ok());
        assert_eq!(r.mismatched, vec![DATABASE_ENTRY.to_string()]);
    }

    #[test]
    fn verify_rejects_zip_bombs_structurally() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.zip");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file(DATABASE_ENTRY, opts).unwrap();
        std::io::Write::write_all(&mut zip, &vec![0u8; 4 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();
        let sums = vec![(DATABASE_ENTRY.to_string(), "f".repeat(64))];
        let m = manifest_with(sums);
        let limits = ArchiveLimits {
            max_files: 100,
            max_uncompressed_bytes: 1 << 20,
        };
        let err = verify_archive(&path, &m, &limits).unwrap_err();
        assert!(err.to_string().contains("limit"), "{err}");
    }
}
