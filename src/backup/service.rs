//! Backup service — the only entry point business code touches.
//!
//! ```text
//! create:  Db (portable SQL) ─┐
//!          media Storage ─────┼─→ streaming zip → tmp file → atomic rename
//!          themes/ plugins/ ──┘        (SHA-256 per entry, manifest last)
//!
//! verify:  manifest → streamed per-entry SHA-256 re-check
//! restore: manifest → verify → pre-restore snapshot → staged files →
//!          single-transaction DB swap → migrate → caches → search rebuild
//! ```
//!
//! The service depends only on abstractions: the portable `Db` layer (SQL
//! is written with `?` and translated per dialect), the media `Storage`
//! provider enum, and the [`BackupStorage`] provider. Nothing here knows
//! which database or storage backend is actually running.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::backup::format::{self, DumpWriter, EXCLUDED_SETTINGS, TABLES};
use crate::backup::manifest::{BackupManifest, DATABASE_ENTRY, MANIFEST_ENTRY, is_safe_entry_name};
use crate::backup::storage::{BackupStorage, valid_backup_name};
use crate::backup::verify::{ArchiveLimits, verify_archive};
use crate::config::BackupConfig;
use crate::db::{Db, Dialect};
use crate::error::{AppError, AppResult};
use crate::media::Storage;
use crate::utils::{cookies, time};
use sqlx::Row;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Kinds & summaries
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupKind {
    Full,
    Database,
    Media,
}

impl BackupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Database => "database",
            Self::Media => "media",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Self::Full),
            "database" => Some(Self::Database),
            "media" => Some(Self::Media),
            _ => None,
        }
    }

    fn includes(self) -> crate::backup::manifest::BackupIncludes {
        crate::backup::manifest::BackupIncludes {
            database: self != Self::Media,
            media: self != Self::Database,
            themes: self == Self::Full,
            plugins: self == Self::Full,
        }
    }
}

/// One entry of the backup listing (or a broken archive).
#[derive(Clone, Debug)]
pub struct BackupSummary {
    pub name: String,
    pub backup_id: String,
    pub kind: String,
    pub created_at: i64,
    pub created_by: String,
    pub polaris_version: String,
    pub dialect: String,
    pub includes: crate::backup::manifest::BackupIncludes,
    pub counts: crate::backup::manifest::BackupCounts,
    /// Size of the `.zip` file on disk.
    pub zip_bytes: u64,
    /// `false` when the manifest is missing or invalid (the archive cannot
    /// be verified or restored).
    pub ok: bool,
    pub error: Option<String>,
}

impl BackupSummary {
    fn broken(name: String, zip_bytes: u64, error: String) -> Self {
        Self {
            name,
            backup_id: String::new(),
            kind: String::new(),
            created_at: 0,
            created_by: String::new(),
            polaris_version: String::new(),
            dialect: String::new(),
            includes: crate::backup::manifest::BackupIncludes {
                database: false,
                media: false,
                themes: false,
                plugins: false,
            },
            counts: crate::backup::manifest::BackupCounts {
                files: 0,
                db_tables: 0,
                db_rows: 0,
                media_files: 0,
                theme_files: 0,
                plugin_files: 0,
            },
            zip_bytes,
            ok: false,
            error: Some(error),
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming helpers
// ---------------------------------------------------------------------------

const PUMP_CHUNK: usize = 64 * 1024;

/// A `Write` wrapper that hashes and counts everything passing through, so
/// every zip entry gets its SHA-256 for free (no second pass over data).
pub(crate) struct HashWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> HashWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    pub(crate) fn hex(self) -> (String, u64) {
        (format!("{:x}", self.hasher.finalize()), self.bytes)
    }
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.bytes += buf.len() as u64;
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Async → sync pump: bounded-chunk reads from an async source written into
/// a sync sink (the zip entry). Nothing is buffered beyond one chunk.
/// Generic over the sink so the future stays `Send` (spawnable).
pub(crate) async fn pump<R: tokio::io::AsyncRead + Unpin, W: std::io::Write>(
    src: &mut R,
    dst: &mut W,
) -> std::io::Result<u64> {
    let mut buf = vec![0u8; PUMP_CHUNK];
    let mut total = 0u64;
    loop {
        let n = tokio::io::AsyncReadExt::read(src, &mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
    Ok(total)
}

fn io_err(what: &str, e: std::io::Error) -> AppError {
    AppError::Internal(anyhow::anyhow!("{what}: {e}"))
}

fn stamp(ts: i64) -> String {
    let d = time::breakdown(ts);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        d.year, d.month, d.day, d.hour, d.minute, d.second
    )
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct BackupService<'a> {
    pub(crate) db: &'a Db,
    pub(crate) dialect: Dialect,
    pub(crate) cfg: &'a BackupConfig,
    pub(crate) media: &'a Storage,
    pub(crate) themes_dir: PathBuf,
    pub(crate) plugins_dir: PathBuf,
    pub(crate) storage: BackupStorage,
}

impl<'a> BackupService<'a> {
    pub fn new(db: &'a Db, cfg: &'a crate::config::Config, media: &'a Storage) -> AppResult<Self> {
        Ok(Self {
            db,
            dialect: db.dialect(),
            cfg: &cfg.backup,
            media,
            themes_dir: PathBuf::from(&cfg.theme.dir),
            plugins_dir: PathBuf::from(&cfg.plugin.dir),
            storage: BackupStorage::build(&cfg.backup.dir, &cfg.backup.tmp_dir)?,
        })
    }

    pub fn limits(&self) -> ArchiveLimits {
        ArchiveLimits::from_backup_cfg(self.cfg)
    }

    /// Provider name (admin UI display).
    pub fn storage_name(&self) -> &'static str {
        self.storage.name()
    }

    /// Active database dialect.
    pub fn dialect_name(&self) -> &'static str {
        self.dialect.name()
    }

    /// Staging path for an uploaded archive (streamed by the HTTP layer).
    pub fn upload_staging_path(&self) -> AppResult<PathBuf> {
        self.storage.upload_staging_path()
    }

    /// Commit an uploaded + verified archive into the listing under its own
    /// backup id.
    pub async fn commit_uploaded(
        &self,
        staged: &Path,
        manifest: &BackupManifest,
    ) -> AppResult<PathBuf> {
        let stem = format!("polaris-{}-{}", manifest.backup_id, manifest.kind);
        self.storage.commit(staged, &stem).await
    }

    // -- create --------------------------------------------------------------

    /// Create a backup archive. The archive is built in the staging area and
    /// atomically renamed into place — an interrupted run leaves at most a
    /// stray `.part` file, never a half-written backup.
    pub async fn create(&self, kind: BackupKind, actor: &str) -> AppResult<BackupSummary> {
        let now = time::now();
        let backup_id = format!("{}-{}", stamp(now), cookies::random_token(4));
        let stem = format!("polaris-{}-{}", backup_id, kind.as_str());
        let staging = self.storage.staging_path(&stem)?;

        let result = self
            .create_into(&staging, &backup_id, kind, actor, now)
            .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&staging).await;
            return Err(e);
        }
        let counts = result.unwrap();
        self.storage.commit(&staging, &stem).await?;
        let path = self.storage.path_of(&format!("{stem}.zip"))?;
        let zip_bytes = tokio::fs::metadata(&path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        tracing::info!(
            backup = %stem,
            kind = kind.as_str(),
            by = actor,
            size = zip_bytes,
            "backup created"
        );
        Ok(BackupSummary {
            name: format!("{stem}.zip"),
            backup_id,
            kind: kind.as_str().to_string(),
            created_at: now,
            created_by: actor.to_string(),
            polaris_version: APP_VERSION.to_string(),
            dialect: self.dialect.name().to_string(),
            includes: kind.includes(),
            counts,
            zip_bytes,
            ok: true,
            error: None,
        })
    }

    async fn create_into(
        &self,
        staging: &Path,
        backup_id: &str,
        kind: BackupKind,
        actor: &str,
        now: i64,
    ) -> AppResult<crate::backup::manifest::BackupCounts> {
        let file = std::fs::File::create(staging)
            .map_err(|e| io_err("cannot create staging archive", e))?;
        let mut zip = ZipWriter::new(file);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let mut checksums: BTreeMap<String, String> = BTreeMap::new();
        let mut counts = crate::backup::manifest::BackupCounts {
            files: 0,
            db_tables: 0,
            db_rows: 0,
            media_files: 0,
            theme_files: 0,
            plugin_files: 0,
        };
        let mut sizes = crate::backup::manifest::BackupSizes::default();

        // 1. Database dump.
        if kind != BackupKind::Media {
            zip.start_file(DATABASE_ENTRY, opts)
                .map_err(|e| AppError::BadRequest(format!("cannot write archive: {e}")))?;
            let mut hw = HashWriter::new(&mut zip);
            let (tables, rows) = self.dump_database(&mut hw).await?;
            let (hash, bytes) = hw.hex();
            checksums.insert(DATABASE_ENTRY.to_string(), hash);
            sizes.database = bytes;
            sizes.total += bytes;
            counts.db_tables = tables;
            counts.db_rows = rows;
        }

        // 2. Media objects (streamed from the storage provider).
        if kind != BackupKind::Database {
            let keys = self.media.list("").await?;
            for key in keys {
                let entry = format!("media/{key}");
                if !is_safe_entry_name(&entry) {
                    tracing::warn!(key = %key, "skipping media object with unsafe key");
                    continue;
                }
                let mut reader = match self.media.open_reader(&key).await? {
                    Some(r) => r,
                    None => {
                        tracing::warn!(key = %key, "media object vanished during backup — skipped");
                        continue;
                    }
                };
                zip.start_file(entry.clone(), opts)
                    .map_err(|e| AppError::BadRequest(format!("cannot write archive: {e}")))?;
                let mut hw = HashWriter::new(&mut zip);
                let n = pump(&mut reader, &mut hw)
                    .await
                    .map_err(|e| io_err("media streaming failed", e))?;
                let (hash, _) = hw.hex();
                checksums.insert(entry, hash);
                sizes.media += n;
                sizes.total += n;
                counts.media_files += 1;
            }
        }

        // 3. Theme / plugin directories (full backups only).
        if kind == BackupKind::Full {
            for (part, root) in [("themes", &self.themes_dir), ("plugins", &self.plugins_dir)] {
                for rel in walk_files(root).await {
                    let entry = format!("{part}/{rel}");
                    if !is_safe_entry_name(&entry) {
                        tracing::warn!(entry = %entry, "skipping file with unsafe path");
                        continue;
                    }
                    let path = root.join(&rel);
                    let mut reader = match tokio::fs::File::open(&path).await {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    zip.start_file(entry.clone(), opts)
                        .map_err(|e| AppError::BadRequest(format!("cannot write archive: {e}")))?;
                    let mut hw = HashWriter::new(&mut zip);
                    let n = pump(&mut reader, &mut hw)
                        .await
                        .map_err(|e| io_err("file streaming failed", e))?;
                    let (hash, _) = hw.hex();
                    checksums.insert(entry, hash);
                    match part {
                        "themes" => {
                            sizes.themes += n;
                            counts.theme_files += 1;
                        }
                        _ => {
                            sizes.plugins += n;
                            counts.plugin_files += 1;
                        }
                    }
                    sizes.total += n;
                }
            }
        }

        // 4. Manifest last (it covers every other entry).
        counts.files = checksums.len();
        let manifest = BackupManifest {
            format_version: crate::backup::manifest::FORMAT_VERSION,
            polaris_version: APP_VERSION.to_string(),
            created_at: now,
            dialect: self.dialect.name().to_string(),
            kind: kind.as_str().to_string(),
            backup_id: backup_id.to_string(),
            created_by: actor.to_string(),
            includes: kind.includes(),
            counts: counts.clone(),
            sizes: sizes.clone(),
            checksums,
        };
        zip.start_file(MANIFEST_ENTRY, opts)
            .map_err(|e| AppError::BadRequest(format!("cannot write archive: {e}")))?;
        let raw = manifest.to_toml()?;
        zip.write_all(raw.as_bytes())
            .map_err(|e| io_err("cannot write manifest", e))?;
        zip.finish()
            .map_err(|e| AppError::BadRequest(format!("cannot finalize archive: {e}")))?;

        Ok(counts)
    }

    /// Stream the whole database into `sink` as a versioned NDJSON dump.
    /// Rows travel in bounded keyset-pagination batches — a table of any
    /// size never sits in memory. Hashing/byte-counting is the caller's
    /// `HashWriter`'s job (one pass, no re-wrap).
    async fn dump_database<W: Write>(&self, sink: &mut W) -> AppResult<(usize, i64)> {
        let mut writer = DumpWriter::new(sink, self.dialect, APP_VERSION)?;

        for spec in TABLES {
            writer.note_table();

            match spec.paginate {
                Some(pk) => {
                    let cols = spec.cols.join(", ");
                    let mut last = 0i64;
                    loop {
                        let sql = format!(
                            "SELECT {cols} FROM {} WHERE {pk} > ? ORDER BY {pk} LIMIT {}",
                            spec.name,
                            format::CHUNK_ROWS
                        );
                        let rows = self.db.fetch_all(&sql, &[crate::db::Bind::I(last)]).await?;
                        if rows.is_empty() {
                            break;
                        }
                        let mut vals = Vec::with_capacity(rows.len());
                        for r in &rows {
                            vals.push(format::row_values(r, spec.cols).map_err(AppError::Db)?);
                        }
                        last = rows
                            .last()
                            .unwrap()
                            .try_get::<i64, _>(pk)
                            .map_err(AppError::Db)?;
                        if spec.name == "settings" {
                            vals.retain(|v| {
                                v.first()
                                    .and_then(|x| x.as_str())
                                    .map(|name| !EXCLUDED_SETTINGS.contains(&name))
                                    .unwrap_or(true)
                            });
                        }
                        if !vals.is_empty() {
                            writer.write_chunk(spec.name, spec.cols, &vals)?;
                        }
                        if rows.len() < format::CHUNK_ROWS {
                            break;
                        }
                    }
                }
                None => {
                    // Composite-PK tables: small and bounded by content size.
                    let cols = spec.cols.join(", ");
                    let rows = self
                        .db
                        .fetch_all(&format!("SELECT {cols} FROM {}", spec.name), &[])
                        .await?;
                    for group in rows.chunks(format::CHUNK_ROWS) {
                        let mut vals = Vec::with_capacity(group.len());
                        for r in group {
                            vals.push(format::row_values(r, spec.cols).map_err(AppError::Db)?);
                        }
                        if spec.name == "settings" {
                            vals.retain(|v| {
                                v.first()
                                    .and_then(|x| x.as_str())
                                    .map(|name| !EXCLUDED_SETTINGS.contains(&name))
                                    .unwrap_or(true)
                            });
                        }
                        if !vals.is_empty() {
                            writer.write_chunk(spec.name, spec.cols, &vals)?;
                        }
                    }
                }
            }
        }

        let (t, rows) = writer.finish()?;
        Ok((t, rows))
    }

    // -- read ----------------------------------------------------------------

    /// Read + validate the manifest of a stored archive.
    pub fn read_manifest(&self, path: &Path) -> AppResult<BackupManifest> {
        read_manifest_from(path)
    }

    /// On-disk path of a stored backup (name validated).
    pub fn storage_path_of(&self, name: &str) -> AppResult<PathBuf> {
        self.storage.path_of(name)
    }

    /// List stored backups (newest first; broken archives last).
    pub async fn list(&self) -> AppResult<Vec<BackupSummary>> {
        let mut out = Vec::new();
        for name in self.storage.list().await? {
            let path = self.storage.path_of(&name)?;
            let zip_bytes = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            match self.read_manifest(&path) {
                Ok(m) => out.push(BackupSummary {
                    name,
                    backup_id: m.backup_id,
                    kind: m.kind,
                    created_at: m.created_at,
                    created_by: m.created_by,
                    polaris_version: m.polaris_version,
                    dialect: m.dialect,
                    includes: m.includes,
                    counts: m.counts,
                    zip_bytes,
                    ok: true,
                    error: None,
                }),
                Err(e) => out.push(BackupSummary::broken(name, zip_bytes, e.message())),
            }
        }
        out.sort_by(|a, b| {
            b.ok.cmp(&a.ok)
                .then(b.created_at.cmp(&a.created_at))
                .then(a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// Verify one archive (structural limits + every entry's SHA-256).
    pub fn verify_file(&self, path: &Path) -> AppResult<crate::backup::verify::VerifyReport> {
        let manifest = self.read_manifest(path)?;
        verify_archive(path, &manifest, &self.limits())
    }

    // -- mutate --------------------------------------------------------------

    pub async fn delete(&self, name: &str) -> AppResult<bool> {
        self.storage.delete(name).await
    }

    /// Retention: keep the newest `keep` scheduled backups, delete the rest.
    /// Manual and pre-restore snapshots are never pruned here.
    pub async fn apply_retention(&self, keep: usize) -> AppResult<usize> {
        if keep == 0 {
            return Ok(0);
        }
        let mut scheduled: Vec<BackupSummary> = self
            .list()
            .await?
            .into_iter()
            .filter(|b| b.ok && b.created_by == "scheduler")
            .collect();
        scheduled.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.name.cmp(&b.name)));
        let mut deleted = 0;
        for old in scheduled.into_iter().skip(keep) {
            if self.storage.delete(&old.name).await? {
                tracing::info!(backup = %old.name, "retention: pruned old scheduled backup");
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// Remove staging leftovers older than a day (interrupted runs).
    pub async fn purge_stale_staging(&self) -> AppResult<usize> {
        let cutoff = time::now() - 86_400;
        let mut removed = 0;
        let mut rd = match tokio::fs::read_dir(&self.cfg.tmp_dir).await {
            Ok(rd) => rd,
            Err(_) => return Ok(0),
        };
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| io_err("cannot read tmp dir", e))?
        {
            let path = entry.path();
            if !entry
                .file_type()
                .await
                .map(|t| t.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            if let Ok(meta) = entry.metadata().await {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if modified < cutoff && tokio::fs::remove_file(&path).await.is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Restore an archive (see [`crate::backup::restore`]).
    pub async fn restore(
        &self,
        app: &crate::state::App,
        path: &Path,
        opts: crate::backup::restore::RestoreOptions,
    ) -> AppResult<crate::backup::restore::RestoreReport> {
        crate::backup::restore::run(self, app, path, opts).await
    }
}

/// Read + validate a manifest from an archive on disk.
pub fn read_manifest_from(path: &Path) -> AppResult<BackupManifest> {
    let file = std::fs::File::open(path)
        .map_err(|e| AppError::BadRequest(format!("cannot open backup: {e}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::BadRequest(format!("invalid backup archive: {e}")))?;
    let mut entry = archive.by_name(MANIFEST_ENTRY).map_err(|_| {
        AppError::BadRequest("backup has no manifest.toml — not a Polaris backup".into())
    })?;
    if entry.size() > 256 * 1024 {
        return Err(AppError::BadRequest("backup manifest is too large".into()));
    }
    let mut raw = String::new();
    entry
        .read_to_string(&mut raw)
        .map_err(|_| AppError::BadRequest("backup manifest is not valid UTF-8".into()))?;
    BackupManifest::parse(&raw)
}

/// Recursively list files under `dir` as forward-slash relative paths.
async fn walk_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((current, rel)) = stack.pop() {
        let Ok(mut rd) = tokio::fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            match entry.file_type().await {
                Ok(t) if t.is_dir() => stack.push((entry.path(), child_rel)),
                Ok(t) if t.is_file() => out.push(child_rel),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Validate a backup file name supplied by a user (download/delete/restore).
pub fn sanitize_requested_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if !valid_backup_name(name) {
        return Err(AppError::BadRequest("invalid backup file name".into()));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_format() {
        // 2026-08-23 09:41:36 UTC
        let ts = 1_787_478_096;
        assert_eq!(stamp(ts), "20260823-094136");
    }

    #[test]
    fn hash_writer_hashes_and_counts() {
        let mut buf: Vec<u8> = Vec::new();
        let mut hw = HashWriter::new(&mut buf);
        hw.write_all(b"hello ").unwrap();
        hw.write_all(b"world").unwrap();
        let (hex, bytes) = hw.hex();
        assert_eq!(bytes, 11);
        assert_eq!(hex, {
            let mut h = Sha256::new();
            h.update(b"hello world");
            format!("{:x}", h.finalize())
        });
    }

    #[test]
    fn requested_names_are_sanitized() {
        assert_eq!(
            sanitize_requested_name("polaris-20260905-091530-x-full.zip").unwrap(),
            "polaris-20260905-091530-x-full.zip"
        );
        assert!(sanitize_requested_name("../secret.zip").is_err());
        assert!(sanitize_requested_name("a.txt").is_err());
    }
}
