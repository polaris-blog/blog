//! Backup restore — verify first, snapshot second, then swap.
//!
//! ```text
//! manifest parse → version/dialect compatibility → full checksum pass
//!   → pre-restore snapshot (database-only backup of the current instance)
//!   → staged file changes (media merge, extension dir swap w/ rollback)
//!   → single-transaction database replace (all core tables)
//!   → post: migrate → cache clear → settings/theme/plugin reload →
//!           search index rebuild
//! ```
//!
//! Failure policy:
//! - Any verification/compat error → nothing was touched.
//! - File-phase errors → DB untouched, moved extension dirs rolled back;
//!   media files added so far are harmless orphans (`polaris media cleanup`).
//! - Database-phase errors → the transaction rolls back; extension dirs are
//!   rolled back; pre-restore snapshot remains for belt-and-braces.
//! - `security.secret` is never restored from the backup — the current
//!   instance key must decrypt the archive's encrypted settings. This is
//!   checked before a snapshot or any file/database mutation.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use zip::ZipArchive;

use crate::backup::format::{self, DumpReader};
use crate::backup::manifest::{BackupManifest, DATABASE_ENTRY};
use crate::backup::service::{APP_VERSION, BackupService};
use crate::backup::verify::verify_archive;
use crate::db::bind_all;
use crate::error::{AppError, AppResult};
use crate::extension::manifest as ext_manifest;
use crate::state::App;

/// Which parts of an archive to apply. Each option is also intersected with
/// what the archive actually contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestoreOptions {
    pub database: bool,
    pub media: bool,
    pub themes: bool,
    pub plugins: bool,
}

impl RestoreOptions {
    pub fn all() -> Self {
        Self {
            database: true,
            media: true,
            themes: true,
            plugins: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    /// Name of the automatic pre-restore snapshot (a database-only backup).
    pub snapshot: Option<String>,
    pub database_restored: bool,
    pub db_rows: i64,
    pub media_files: usize,
    pub theme_files: usize,
    pub plugin_files: usize,
    pub migrations_applied: Vec<i64>,
    pub warnings: Vec<String>,
}

impl RestoreReport {
    pub fn summary(&self) -> String {
        use crate::i18n::tr;
        let mut parts = Vec::new();
        if self.database_restored {
            parts.push(tr(
                "restore.report.database",
                &[("rows", &self.db_rows.to_string())],
            ));
        }
        if self.media_files > 0 {
            parts.push(tr(
                "restore.report.media",
                &[("n", &self.media_files.to_string())],
            ));
        }
        if self.theme_files > 0 {
            parts.push(tr(
                "restore.report.themes",
                &[("n", &self.theme_files.to_string())],
            ));
        }
        if self.plugin_files > 0 {
            parts.push(tr(
                "restore.report.plugins",
                &[("n", &self.plugin_files.to_string())],
            ));
        }
        if parts.is_empty() {
            return tr("restore.report.nothing", &[]);
        }
        let joined = parts.join(", ");
        let mut s = tr("restore.report.restored", &[("parts", &joined)]);
        if !self.migrations_applied.is_empty() {
            s.push_str(&tr(
                "restore.report.migrations",
                &[("n", &self.migrations_applied.len().to_string())],
            ));
        }
        s
    }
}

/// Extension directories moved aside during the swap, tracked for rollback.
struct DirSwap {
    /// Final destination (e.g. `themes/aurora`).
    dest: PathBuf,
    /// Where the previous version was moved (`None` when there was none).
    old: Option<PathBuf>,
    /// Whether the new version was already moved into `dest` (needs removal
    /// on rollback vs. simply not promoting the staged copy).
    applied: bool,
}

impl DirSwap {
    fn rollback(self) -> std::io::Result<()> {
        if self.applied && self.dest.exists() {
            std::fs::remove_dir_all(&self.dest)?;
        }
        if let Some(old) = &self.old {
            crate::utils::fs::rename_dir_or_copy(old, &self.dest)?;
        }
        Ok(())
    }
}

/// Keeps originals until the database commit; early returns roll back files.
struct FileRestore {
    stage_root: PathBuf,
    swaps: Vec<DirSwap>,
    media_staging: Vec<PathBuf>,
    extension_staging: Vec<PathBuf>,
    committed: bool,
}

impl FileRestore {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for FileRestore {
    fn drop(&mut self) {
        let mut recovered = true;
        if self.committed {
            for swap in &self.swaps {
                if let Some(old) = &swap.old
                    && let Err(error) = std::fs::remove_dir_all(old)
                {
                    tracing::warn!(%error, path = %old.display(), "could not remove previous extension");
                }
            }
        } else {
            for swap in self.swaps.drain(..).rev() {
                let old = swap.old.clone();
                if let Err(error) = swap.rollback() {
                    recovered = false;
                    tracing::error!(%error, recovery = ?old,
                        "extension rollback failed; retained recovery files");
                }
            }
        }
        for path in &self.media_staging {
            let _ = std::fs::remove_file(path);
        }
        for path in &self.extension_staging {
            let _ = std::fs::remove_dir_all(path);
        }
        // Never discard originals when rollback itself fails.
        if recovered && let Err(error) = std::fs::remove_dir_all(&self.stage_root) {
            tracing::warn!(%error, staging = %self.stage_root.display(),
                "could not remove restore staging");
        }
    }
}

pub(crate) async fn run(
    svc: &BackupService<'_>,
    app: &App,
    path: &std::path::Path,
    opts: RestoreOptions,
) -> AppResult<RestoreReport> {
    let mut report = RestoreReport::default();

    // ---- 1. Manifest & compatibility (nothing touched yet) ----------------
    let manifest = svc.read_manifest(path)?;
    if let Some(err) = manifest.compatibility_error(APP_VERSION) {
        return Err(AppError::BadRequest(format!(
            "backup is not compatible: {err}"
        )));
    }
    if manifest.dialect != svc.dialect.name() {
        return Err(AppError::BadRequest(format!(
            "backup was taken from a '{}' database but this instance runs '{}' — \
             cross-dialect restore is not supported",
            manifest.dialect,
            svc.dialect.name()
        )));
    }

    // ---- 2. Full integrity pass (streamed, nothing extracted yet) ---------
    let verify = verify_archive(path, &manifest, &svc.limits())?;
    if !verify.ok() {
        return Err(AppError::BadRequest(format!(
            "backup failed integrity checks ({}); refusing to restore",
            verify.summary()
        )));
    }

    // Intersect the requested options with what the archive contains.
    let do_db = opts.database && manifest.includes.database;
    let do_media = opts.media && manifest.includes.media;
    let do_themes = opts.themes && manifest.includes.themes;
    let do_plugins = opts.plugins && manifest.includes.plugins;
    if !do_db && !do_media && !do_themes && !do_plugins {
        return Err(AppError::BadRequest(
            "nothing to restore: the archive does not contain the selected parts".into(),
        ));
    }
    for (want, has, name) in [
        (opts.database, manifest.includes.database, "database"),
        (opts.media, manifest.includes.media, "media"),
        (opts.themes, manifest.includes.themes, "themes"),
        (opts.plugins, manifest.includes.plugins, "plugins"),
    ] {
        if want && !has {
            report
                .warnings
                .push(format!("backup contains no {name} data — skipped"));
        }
    }

    if do_db {
        validate_backup_settings(app, path)?;
    }

    // ---- 3. Pre-restore safety snapshot -----------------------------------
    // Any restore that touches the database (full or media-metadata only)
    // snapshots the current database first. Files are merged/replaced per
    // id and additionally rolled back in-process on failure.
    let db_scope = if do_db {
        Some(DbScope::All)
    } else if do_media {
        Some(DbScope::MediaOnly)
    } else {
        None
    };
    let snapshot = if db_scope.is_some() {
        let snap = svc
            .create(crate::backup::service::BackupKind::Database, "pre-restore")
            .await?;
        report.snapshot = Some(snap.name.clone());
        Some(snap.name)
    } else {
        None
    };

    // ---- 4. File phase (before the DB swap; merge-only for media) ---------
    let files = apply_files(
        svc,
        app,
        path,
        &manifest,
        do_media,
        do_themes,
        do_plugins,
        &mut report,
    )
    .await?;

    // ---- 5. Database phase (single transaction) ---------------------------
    if let Some(scope) = db_scope {
        match restore_database(svc, app, path, scope).await {
            Ok(rows) => {
                if do_db {
                    report.database_restored = true;
                }
                report.db_rows = rows;
            }
            Err(e) => {
                if let Some(snap) = snapshot {
                    report.warnings.push(format!(
                        "database restore failed and was rolled back; pre-restore snapshot '{snap}' is available"
                    ));
                }
                return Err(e);
            }
        }
    }

    files.commit();

    // ---- 6. Post-restore: migrate, caches, runtime state, search ----------
    post_restore(app, do_db, do_media, do_themes, do_plugins, &mut report).await;

    tracing::info!(
        snapshot = ?report.snapshot,
        database = report.database_restored,
        media = report.media_files,
        "backup restored"
    );
    Ok(report)
}

fn validate_backup_settings(app: &App, path: &std::path::Path) -> AppResult<()> {
    let file = std::fs::File::open(path)?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| AppError::BadRequest("invalid backup archive".into()))?;
    let dump = archive
        .by_name(DATABASE_ENTRY)
        .map_err(|_| AppError::BadRequest("backup has no database dump".into()))?;
    let mut reader = DumpReader::new(std::io::BufReader::new(dump))?;
    while let Some(chunk) = reader.next_chunk()? {
        if chunk.table != "settings" {
            continue;
        }
        for row in chunk.rows {
            if let (Some(name), Some(value)) = (
                row.first().and_then(|v| v.as_str()),
                row.get(1).and_then(|v| v.as_str()),
            ) && (name.starts_with("theme.") || name.starts_with("plugin."))
                && value.starts_with("enc:")
            {
                app.configs.validate_stored_secret(value).map_err(|_| {
                    AppError::BadRequest("backup contains encrypted configuration that this instance cannot decrypt; configure the original security.secret before restoring".into())
                })?;
            }
        }
    }
    Ok(())
}

/// Extract + apply file parts. Media merges into the storage provider;
/// theme/plugin id-directories are swapped atomically with rollback.
///
/// The zip reader borrows the archive, so *all extraction happens
/// synchronously* before any await point — the borrowed reader never
/// crosses an `.await`, which keeps the future `Send` and the logic easy to
/// reason about. The async phase only commits already-staged files.
#[allow(clippy::too_many_arguments)]
async fn apply_files(
    svc: &BackupService<'_>,
    app: &App,
    path: &std::path::Path,
    manifest: &BackupManifest,
    do_media: bool,
    do_themes: bool,
    do_plugins: bool,
    report: &mut RestoreReport,
) -> AppResult<FileRestore> {
    // ---- Phase 1 (sync): extract entries into staging -----------------------
    struct StagedMedia {
        key: String,
        staged: PathBuf,
    }
    struct StagedExt {
        part: &'static str,
        id: String,
        staged_dir: PathBuf,
        files: usize,
    }

    let mut staged_media: Vec<StagedMedia> = Vec::new();
    let mut staged_exts: Vec<StagedExt> = Vec::new();
    let stage_root = {
        let base = PathBuf::from(&svc.cfg.tmp_dir);
        std::fs::create_dir_all(&base)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("cannot create tmp dir: {e}")))?;
        base.join(format!(
            "restore-{}",
            crate::utils::cookies::random_token(12)
        ))
    };
    std::fs::create_dir_all(&stage_root)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("cannot create restore staging: {e}")))?;
    let mut files = FileRestore {
        stage_root: stage_root.clone(),
        swaps: Vec::new(),
        media_staging: Vec::new(),
        extension_staging: Vec::new(),
        committed: false,
    };

    let extract_result = (|| -> AppResult<()> {
        let file = std::fs::File::open(path)
            .map_err(|e| AppError::BadRequest(format!("cannot open backup: {e}")))?;
        let mut archive = ZipArchive::new(file)
            .map_err(|e| AppError::BadRequest(format!("invalid backup archive: {e}")))?;
        let budget = svc.limits().max_uncompressed_bytes;

        // Group extension entries by (part, id) first so a partial
        // extraction never promotes a half-restored extension.
        let mut ext_parts: BTreeMap<(String, String), Vec<(usize, String)>> = BTreeMap::new();
        let mut media_entries: Vec<(usize, String)> = Vec::new();
        for i in 0..archive.len() {
            let entry = archive
                .by_index(i)
                .map_err(|e| AppError::BadRequest(format!("cannot read backup entry #{i}: {e}")))?;
            let name = entry.name().to_string();
            if entry.is_dir() {
                continue;
            }
            match manifest.part_of(&name) {
                "media" if do_media => media_entries.push((i, name)),
                "themes" if do_themes => {
                    if let Some((id, rel)) = name["themes/".len()..].split_once('/') {
                        ext_parts
                            .entry(("themes".into(), id.to_string()))
                            .or_default()
                            .push((i, rel.to_string()));
                    }
                }
                "plugins" if do_plugins => {
                    if let Some((id, rel)) = name["plugins/".len()..].split_once('/') {
                        ext_parts
                            .entry(("plugins".into(), id.to_string()))
                            .or_default()
                            .push((i, rel.to_string()));
                    }
                }
                _ => {}
            }
        }

        // Media: stream each entry into a storage staging file.
        for (idx, name) in &media_entries {
            let key = name["media/".len()..].to_string();
            let staged = app.media.storage().staging_file()?;
            files.media_staging.push(staged.clone());
            write_entry_to_file(&mut archive, *idx, &staged, budget)?;
            staged_media.push(StagedMedia { key, staged });
        }

        // Extensions: validate ids + paths, then stage the whole tree.
        for ((part, id), entries) in &ext_parts {
            if !ext_manifest::valid_id(id) {
                return Err(AppError::BadRequest(format!(
                    "backup contains an invalid extension id '{id}'"
                )));
            }
            let staged_dir = stage_root.join(part).join(id);
            for (idx, rel) in entries {
                let rel_clean = rel.replace('\\', "/");
                if !crate::backup::manifest::is_safe_entry_name(&format!("{id}/{rel_clean}")) {
                    return Err(AppError::BadRequest(format!(
                        "backup contains an unsafe path '{part}/{rel}'"
                    )));
                }
                let dest = staged_dir.join(&rel_clean);
                let Some(parent) = dest.parent() else {
                    return Err(AppError::BadRequest("unsafe staged path".into()));
                };
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::Internal(anyhow::anyhow!("cannot stage restore: {e}"))
                })?;
                write_entry_to_file(&mut archive, *idx, &dest, budget)?;
            }
            let (s, f) = (part.as_str(), entries.len());
            staged_exts.push(StagedExt {
                part: if s == "themes" { "themes" } else { "plugins" },
                id: id.clone(),
                staged_dir,
                files: f,
            });
        }
        Ok(())
    })();

    extract_result?;

    // ---- Phase 2 (async): commit staged files into place -------------------
    // Media: merge (add/overwrite, never delete) via atomic rename.
    for m in &staged_media {
        let res = app.media.storage().put_staged(&m.key, &m.staged).await;
        let _ = tokio::fs::remove_file(&m.staged).await;
        res?;
        report.media_files += 1;
    }

    // Extensions: swap directories atomically with rollback tracking.
    for staged in &staged_exts {
        let root = match staged.part {
            "themes" => svc.themes_dir.clone(),
            _ => svc.plugins_dir.clone(),
        };
        std::fs::create_dir_all(&root)?;
        // The configured staging directory may be on another filesystem.
        // Copy into a unique sibling first; all destructive swaps and their
        // rollback then use same-filesystem rename, never copy-and-delete.
        let token = crate::utils::cookies::random_token(12);
        let ready = root.join(format!(".restore-{token}-new"));
        files.extension_staging.push(ready.clone());
        crate::utils::copy_dir_recursive(&staged.staged_dir, &ready)?;
        let dest = root.join(&staged.id);
        let mut swap = DirSwap {
            dest: dest.clone(),
            old: None,
            applied: false,
        };
        if dest.exists() {
            let old_side = root.join(format!(".restore-{token}-old"));
            // The staging root (data volume) and the install target (image
            // layer) can be different filesystems — fall back to copying.
            crate::utils::fs::rename_dir_or_copy(&dest, &old_side).map_err(|e| {
                AppError::Internal(anyhow::anyhow!(
                    "cannot move the current '{}/{}' aside: {e}",
                    staged.part,
                    staged.id
                ))
            })?;
            swap.old = Some(old_side);
        }
        files.swaps.push(swap);
        crate::utils::fs::rename_dir_or_copy(&ready, &dest).map_err(|e| {
            AppError::Internal(anyhow::anyhow!(
                "cannot install '{}/{}' from the backup: {e}",
                staged.part,
                staged.id
            ))
        })?;
        files.swaps.last_mut().expect("just inserted swap").applied = true;
        match staged.part {
            "themes" => report.theme_files += staged.files,
            _ => report.plugin_files += staged.files,
        }
    }

    Ok(files)
}

/// Stream one archive entry into a file, bounded by `budget` bytes.
///
/// The budget is charged by *actual* bytes read: the declared size in the
/// ZIP central directory can lie, so it is only a cheap pre-check. The read
/// loop itself hard-stops once the real byte count exceeds the limit —
/// mirroring `extension::package::LimitedWriter`.
fn write_entry_to_file(
    archive: &mut ZipArchive<std::fs::File>,
    index: usize,
    dest: &std::path::Path,
    budget: u64,
) -> AppResult<()> {
    let mut entry = archive
        .by_index(index)
        .map_err(|e| AppError::BadRequest(format!("cannot re-read backup entry #{index}: {e}")))?;
    if entry.size() > budget {
        return Err(AppError::BadRequest(
            "backup entry expands beyond the size limit — refusing".into(),
        ));
    }
    let mut out = std::fs::File::create(dest)
        .map_err(|e| AppError::BadRequest(format!("cannot create staging file: {e}")))?;
    let mut remaining = budget;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|e| AppError::BadRequest(format!("corrupt backup entry: {e}")))?;
        if n == 0 {
            break;
        }
        if n as u64 > remaining {
            return Err(AppError::BadRequest(
                "backup entry expands beyond the size limit — refusing".into(),
            ));
        }
        remaining -= n as u64;
        std::io::Write::write_all(&mut out, &buf[..n])
            .map_err(|e| AppError::BadRequest(format!("cannot write staging file: {e}")))?;
    }
    std::io::Write::flush(&mut out).map_err(io_err)?;
    Ok(())
}

fn io_err(e: std::io::Error) -> AppError {
    AppError::Internal(anyhow::anyhow!("restore file write failed: {e}"))
}

/// Which tables a database-phase restore touches.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DbScope {
    /// Every core table (full database restore).
    All,
    /// Only the media metadata tables — rides along with a media-only
    /// restore so restored *files* have their records back (without this,
    /// restored bytes would be invisible orphans).
    MediaOnly,
}

/// Tables restored in [`DbScope::MediaOnly`], in FK-safe insert order.
const MEDIA_TABLES: &[&str] = &["media_folders", "media", "media_tags", "media_references"];

/// Tables deleted for [`DbScope::MediaOnly`] (children first).
const MEDIA_DELETE_ORDER: &[&str] = &["media_references", "media_tags", "media", "media_folders"];

/// Replace every core table with the dump's contents inside one
/// transaction. Any error rolls the whole swap back.
async fn restore_database(
    svc: &BackupService<'_>,
    app: &App,
    path: &std::path::Path,
    scope: DbScope,
) -> AppResult<i64> {
    // Keep the current instance secret — it is never taken from a backup.
    let current_secret = crate::repositories::settings::get(&app.db, "security.secret").await?;

    // Extract the dump to a temp file first (synchronous): the zip entry
    // reader borrows the archive and must never be held across an await.
    let tmp_dump = {
        let dir = PathBuf::from(&svc.cfg.tmp_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("cannot create tmp dir: {e}")))?;
        dir.join(format!(
            "restore-dump-{}",
            crate::utils::cookies::random_token(12)
        ))
    };
    let extract = (|| -> AppResult<()> {
        let file = std::fs::File::open(path)
            .map_err(|e| AppError::BadRequest(format!("cannot open backup: {e}")))?;
        let mut archive = ZipArchive::new(file)
            .map_err(|e| AppError::BadRequest(format!("invalid backup archive: {e}")))?;
        let idx = archive
            .index_for_name(DATABASE_ENTRY)
            .ok_or_else(|| AppError::BadRequest("backup has no database dump".into()))?;
        write_entry_to_file(
            &mut archive,
            idx,
            &tmp_dump,
            svc.limits().max_uncompressed_bytes,
        )
    })();
    if let Err(e) = extract {
        let _ = std::fs::remove_file(&tmp_dump);
        return Err(e);
    }

    // Replay the dump. Rows are bound through the portable layer; the SQL
    // uses `?` placeholders translated to the active dialect.
    let result = restore_replay(svc, app, &tmp_dump, current_secret, scope).await;
    let _ = std::fs::remove_file(&tmp_dump);
    result
}

async fn restore_replay(
    svc: &BackupService<'_>,
    app: &App,
    dump_path: &std::path::Path,
    current_secret: Option<String>,
    scope: DbScope,
) -> AppResult<i64> {
    let dump = std::fs::File::open(dump_path)
        .map_err(|e| AppError::BadRequest(format!("cannot read extracted dump: {e}")))?;
    let mut reader = DumpReader::new(std::io::BufReader::new(dump))?;

    let mut tx = app.db.pool().begin().await?;

    // FK-safe delete order (children first).
    let delete_order: Vec<&str> = match scope {
        DbScope::All => format::DELETE_ORDER.to_vec(),
        DbScope::MediaOnly => MEDIA_DELETE_ORDER.to_vec(),
    };
    for table in delete_order {
        let sql = format!("DELETE FROM {table}");
        sqlx::query(&sql).execute(&mut *tx).await?;
    }

    let mut total_rows = 0i64;
    while let Some(chunk) = reader.next_chunk()? {
        if scope == DbScope::MediaOnly && !MEDIA_TABLES.contains(&chunk.table.as_str()) {
            continue;
        }
        let Some(spec) = format::spec_of(&chunk.table) else {
            return Err(AppError::BadRequest(format!(
                "dump references unknown table '{}'",
                chunk.table
            )));
        };
        let binds = chunk
            .binds()
            .ok_or_else(|| AppError::BadRequest("corrupt database dump".into()))?;
        for group in binds.chunks(format::CHUNK_ROWS) {
            if group.is_empty() {
                continue;
            }
            let sql = format::insert_sql(spec.name, spec.cols, group.len());
            let translated = svc.dialect.translate(&sql);
            let flat: Vec<crate::db::Bind> = group.concat();
            bind_all(sqlx::query(translated.as_ref()), &flat)
                .execute(&mut *tx)
                .await?;
            total_rows += group.len() as i64;
        }
    }

    // Restore keeps the *current* secret: re-insert it after the settings
    // swap so encrypted configuration values keep decrypting. (A media-only
    // restore never touches `settings`.)
    if scope == DbScope::All
        && let Some(secret) = current_secret.filter(|s| !s.is_empty())
    {
        let sql = svc
            .dialect
            .translate("INSERT INTO settings (name, value) VALUES (?, ?)");
        sqlx::query(sql.as_ref())
            .bind("security.secret")
            .bind(secret)
            .execute(&mut *tx)
            .await?;
    }

    // PostgreSQL sequences do not follow explicit-id inserts — re-sync them
    // so post-restore inserts cannot collide.
    if svc.dialect.name() == "postgres" {
        for (table, fix) in format::pg_sequence_fixes() {
            match scope {
                DbScope::All => {
                    sqlx::query(&fix).execute(&mut *tx).await?;
                }
                DbScope::MediaOnly => {
                    if table == "media" || table == "media_folders" {
                        sqlx::query(&fix).execute(&mut *tx).await?;
                    }
                }
            }
        }
    }

    tx.commit().await?;
    Ok(total_rows)
}

/// Bring the running instance back in sync with the restored data:
/// migrations, cache, settings cache, theme/plugin state, search index.
async fn post_restore(
    app: &App,
    database_restored: bool,
    media_restored: bool,
    themes_restored: bool,
    plugins_restored: bool,
    report: &mut RestoreReport,
) {
    // A media-only restore leaves every namespace intact except media
    // metadata and the search results (media rows live in the index).
    if !database_restored && media_restored {
        app.cache
            .invalidate(&[crate::cache::ns::MEDIA, crate::cache::ns::SEARCH])
            .await;
    }
    if database_restored {
        // 1. Schema: the backup may predate the current version.
        match crate::db::migrate::run(&app.db).await {
            Ok(applied) => report.migrations_applied = applied,
            Err(e) => report
                .warnings
                .push(format!("post-restore migration failed: {}", e.message())),
        }

        // 2. Flush every cached object/page/namespace.
        if let Err(e) = app.cache.clear_all().await {
            report.warnings.push(format!("cache clear failed: {e}"));
        }

        // 3. Reload the in-memory settings snapshot.
        match crate::repositories::settings::all(&app.db).await {
            Ok(map) => app.settings.reload(map),
            Err(e) => report
                .warnings
                .push(format!("settings reload failed: {}", e.message())),
        }
    }

    if database_restored || themes_restored || plugins_restored {
        // Names may be unchanged while scripts, templates and config differ.
        for ns in app.configs.loaded_namespaces() {
            app.configs.unload_namespace(&ns);
        }
        let theme = app
            .settings
            .get_str("theme.active", &app.config.theme.active);
        if std::path::Path::new(&app.config.theme.dir)
            .join(&theme)
            .is_dir()
        {
            if let Err(e) = app.set_active_theme(&theme).await {
                report.warnings.push(format!(
                    "theme reload after restore failed: {}",
                    e.message()
                ));
            }
        } else if theme != app.theme.current_name() {
            report.warnings.push(format!(
                "restored theme '{theme}' is not installed — keeping '{}'",
                app.theme.current_name()
            ));
        }
        let plugins = app.settings.plugins_enabled();
        if let Err(e) = app.set_plugins_enabled(&plugins).await {
            report.warnings.push(format!(
                "plugin reload after restore failed: {}",
                e.message()
            ));
        }
        app.invalidate_content().await;
    }

    if database_restored || media_restored {
        // 5. Rebuild the search index from the restored content (the index
        // tables are not part of the backup).
        match app.search.rebuild(app, |_, _| {}).await {
            Ok(_) => {}
            Err(e) => report.warnings.push(format!(
                "search index rebuild failed — run `polaris search rebuild`: {}",
                e.message()
            )),
        }
    }
}
