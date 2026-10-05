//! Extension installation pipeline.
//!
//! ```text
//! Upload → size/ZIP validation → manifest validation → id/version checks
//!        → dependency & compatibility checks → extract to staging
//!        → re-validate → backup old version → atomic swap
//!        → plugin migrations → registry + audit log
//! ```
//!
//! Files only ever reach `themes/<id>/` or `plugins/<id>/` after every check
//! passed. Updates keep a backup under `data/backups/extensions/` and are
//! rolled back automatically when a plugin migration fails.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;

use crate::config::Config;
use crate::db::{Db, migrate};
use crate::error::{AppError, AppResult};
use crate::repositories::extensions as registry;

use super::ExtensionKind;
use super::manifest::{self, ExtensionManifest};
use super::package::{self, PackageLimits};

/// Serializes install/uninstall operations process-wide.
///
/// The pipeline performs several non-atomic filesystem steps (extract →
/// backup → rename → migrate). Without this lock, two concurrent installs of
/// the same extension interleave and can leave the install directory
/// half-swapped — or, with the version comparison in `install()` done before
/// the swap, produce a bogus "install" audit record for what was really an
/// update. Installs are rare admin actions, so one global lock is cheap.
static INSTALL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Running Polaris version, checked against manifest compatibility ranges.
pub fn polaris_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is valid semver")
}

/// Tables a plugin can never drop through `uninstall_tables`.
///
/// The manifest is untrusted input: `valid_table_name` only guarantees the
/// string is a bare SQL identifier, and `posts` is a bare identifier too.
/// Without this guard a hostile package could wipe the whole site the first
/// time an admin ticks "remove data".
///
/// Extension tables are plugin-owned and therefore absent from this list —
/// plugins are expected to declare and clean up after themselves.
const PROTECTED_TABLES: &[&str] = &[
    "posts",
    "pages",
    "users",
    "settings",
    "terms",
    "post_terms",
    "comments",
    "media",
    "media_folders",
    "media_tags",
    "media_references",
    "search_index",
    "search_stats",
    "extensions",
    "extension_logs",
    "extension_migrations",
    "schema_migrations",
    "scheduler_jobs",
    "scheduler_attempts",
];

/// Case-insensitive membership test for [`PROTECTED_TABLES`].
fn is_protected_table(name: &str) -> bool {
    PROTECTED_TABLES
        .iter()
        .any(|t| t.eq_ignore_ascii_case(name))
}

#[derive(Clone, Debug)]
pub struct InstallOptions {
    /// Who performed the action (username / "cli"), recorded in the audit log.
    pub actor: String,
    /// Skip the Polaris compatibility check (`minimum/maximum_polaris_version`).
    pub force: bool,
    /// Explicitly allow installing an older version than the installed one.
    pub allow_downgrade: bool,
}

impl InstallOptions {
    pub fn new(actor: &str) -> Self {
        Self {
            actor: actor.to_string(),
            force: false,
            allow_downgrade: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallAction {
    Installed,
    Updated { from: Version },
    Downgraded { from: Version },
}

impl InstallAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Installed => "install",
            Self::Updated { .. } => "update",
            Self::Downgraded { .. } => "downgrade",
        }
    }
}

#[derive(Debug)]
pub struct InstallOutcome {
    pub kind: ExtensionKind,
    pub manifest: ExtensionManifest,
    pub action: InstallAction,
    pub package_hash: String,
    /// Backup of the previous version (updates/downgrades only).
    pub backup: Option<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct UninstallOutcome {
    pub kind: ExtensionKind,
    pub id: String,
    pub removed_tables: Vec<String>,
}

/// One extension found on disk during a scan.
pub struct ScanEntry {
    pub id: String,
    pub manifest: Option<ExtensionManifest>,
    /// Parse/validation error when the manifest is broken.
    pub error: Option<String>,
}

/// Verification report for `polaris extension verify`.
pub struct VerifyEntry {
    pub kind: ExtensionKind,
    pub id: String,
    pub ok: bool,
    pub issues: Vec<String>,
}

pub struct ExtensionInstaller {
    db: Db,
    themes_dir: PathBuf,
    plugins_dir: PathBuf,
    tmp_dir: PathBuf,
    backup_dir: PathBuf,
    limits: PackageLimits,
}

impl ExtensionInstaller {
    pub fn new(db: Db, cfg: &Config) -> Self {
        let limits = PackageLimits {
            max_file_bytes: cfg.extensions.upload.max_file_bytes(),
            max_uncompressed_bytes: cfg.extensions.upload.max_uncompressed_bytes(),
            max_files: cfg.extensions.upload.max_files,
        };
        Self {
            db,
            themes_dir: Path::new(&cfg.theme.dir).to_path_buf(),
            plugins_dir: Path::new(&cfg.plugin.dir).to_path_buf(),
            tmp_dir: Path::new(&cfg.extensions.tmp_dir).to_path_buf(),
            backup_dir: Path::new(&cfg.extensions.backup_dir).to_path_buf(),
            limits,
        }
    }

    fn kind_root(&self, kind: ExtensionKind) -> &Path {
        match kind {
            ExtensionKind::Theme => &self.themes_dir,
            ExtensionKind::Plugin => &self.plugins_dir,
        }
    }

    fn dest_dir(&self, kind: ExtensionKind, id: &str) -> PathBuf {
        self.kind_root(kind).join(id)
    }

    /// Manifest of an installed extension (directory name is the id).
    pub fn installed_manifest(
        &self,
        kind: ExtensionKind,
        id: &str,
    ) -> AppResult<Option<ExtensionManifest>> {
        if !manifest::valid_id(id) {
            return Ok(None);
        }
        let dir = self.dest_dir(kind, id);
        if !dir.is_dir() {
            return Ok(None);
        }
        match manifest::load_from_dir(kind, &dir, Some(id)) {
            Ok(m) => Ok(Some(m)),
            Err(e) => Err(e),
        }
    }

    // -----------------------------------------------------------------------
    // Install / update / downgrade
    // -----------------------------------------------------------------------

    /// Install a package ZIP that already lives on disk (an upload staged to
    /// `data/tmp/extensions/`, a CLI path, or a backup to restore).
    pub async fn install(
        &self,
        zip_path: &Path,
        opts: &InstallOptions,
    ) -> AppResult<InstallOutcome> {
        let _guard = INSTALL_LOCK.lock().await;
        // The install roots may not exist on a fresh deployment yet.
        for dir in [&self.themes_dir, &self.plugins_dir, &self.tmp_dir] {
            std::fs::create_dir_all(dir)
                .map_err(|e| AppError::BadRequest(format!("cannot create {dir:?}: {e}")))?;
        }

        let info = package::inspect(zip_path, &self.limits)?;
        let hash = package::sha256_file(zip_path)?;

        // Manifest from the archive (before anything is written).
        let fallback = info.root.as_deref().filter(|r| manifest::valid_id(r));
        let m = manifest::parse(info.kind, &info.manifest_raw, fallback)?;

        // Polaris compatibility (force-overridable).
        if !opts.force
            && let Some(err) = m.polaris_compat_error(&polaris_version())
        {
            return Err(AppError::BadRequest(format!(
                "incompatible with this Polaris version: {err} (use force install to override)"
            )));
        }

        // Dependencies: every declared id must be installed at a matching
        // version (themes and plugins both count as dependency providers).
        let missing = self.check_dependencies(&m)?;
        if !missing.is_empty() {
            return Err(AppError::BadRequest(format!(
                "missing dependencies: {}",
                missing.join(", ")
            )));
        }

        // Existing version comparison.
        let dest = self.dest_dir(info.kind, &m.id);
        let existing = if dest.is_dir() {
            Some(manifest::load_from_dir(info.kind, &dest, Some(&m.id))?)
        } else {
            None
        };
        let action = match &existing {
            None => InstallAction::Installed,
            Some(old) => {
                if m.version < old.version && !opts.allow_downgrade {
                    return Err(AppError::BadRequest(format!(
                        "version {} is older than the installed {} — confirm the downgrade to continue",
                        m.version, old.version
                    )));
                }
                if m.version < old.version {
                    InstallAction::Downgraded {
                        from: old.version.clone(),
                    }
                } else {
                    InstallAction::Updated {
                        from: old.version.clone(),
                    }
                }
            }
        };

        // Extract into staging, then re-validate what landed on disk.
        let staging = self
            .tmp_dir
            .join(format!("{}-{}", m.id, &hash[..12.min(hash.len())]));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)
                .map_err(|e| AppError::BadRequest(format!("cannot clean staging: {e}")))?;
        }
        let result = self
            .extract_and_swap(&m, zip_path, &staging, &dest, action, opts, &hash)
            .await;

        // Never leave staging or failed swaps behind.
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result
    }

    fn check_dependencies(&self, m: &ExtensionManifest) -> AppResult<Vec<String>> {
        if m.dependencies.is_empty() {
            return Ok(Vec::new());
        }
        let mut missing = Vec::new();
        for (dep, req) in &m.dependencies {
            let installed = self
                .installed_manifest(ExtensionKind::Plugin, dep)?
                .or_else(|| {
                    self.installed_manifest(ExtensionKind::Theme, dep)
                        .ok()
                        .flatten()
                });
            match installed {
                Some(dep_manifest) => {
                    let req = semver::VersionReq::parse(req).map_err(|_| {
                        AppError::BadRequest(format!("invalid requirement for '{dep}'"))
                    })?;
                    if !req.matches(&dep_manifest.version) {
                        missing.push(format!("{dep} {req} (installed {})", dep_manifest.version));
                    }
                }
                None => missing.push(format!("{dep} {req}")),
            }
        }
        Ok(missing)
    }

    #[allow(clippy::too_many_arguments)]
    async fn extract_and_swap(
        &self,
        m: &ExtensionManifest,
        zip_path: &Path,
        staging: &Path,
        dest: &Path,
        mut action: InstallAction,
        opts: &InstallOptions,
        package_hash: &str,
    ) -> AppResult<InstallOutcome> {
        let mut warnings = Vec::new();
        let root = package::extract(zip_path, staging, &self.limits)?;

        // Re-validate the extracted tree: the manifest must still parse and
        // the plugin entry script must exist.
        let on_disk = manifest::load_from_dir(m.kind, &root, Some(&m.id))?;
        if on_disk.id != m.id {
            return Err(AppError::BadRequest(
                "package root directory does not match its manifest id".into(),
            ));
        }
        match m.kind {
            ExtensionKind::Plugin => {
                if !root.join(&on_disk.entry).is_file() {
                    return Err(AppError::BadRequest(format!(
                        "plugin entry '{}' not found in package",
                        on_disk.entry
                    )));
                }
            }
            ExtensionKind::Theme => {
                if !root.join("templates").is_dir() {
                    warnings.push(
                        "theme has no templates/ directory — the built-in fallbacks will be used"
                            .into(),
                    );
                }
            }
        }

        // Backup the current version, then swap directories.
        let mut backup = None;
        let mut old_side: Option<PathBuf> = None;
        if dest.exists() {
            // `action` was computed before extraction and must not be
            // trusted here: another install may have won the race in the
            // meantime, or `dest` may be a stray file rather than a
            // directory. Re-derive the previous version from disk and never
            // assume the shape of what is there.
            if !dest.is_dir() {
                return Err(AppError::BadRequest(format!(
                    "cannot install '{}': {} already exists and is not a directory",
                    m.id,
                    dest.display()
                )));
            }
            let from = match manifest::load_from_dir(m.kind, dest, Some(&m.id)) {
                Ok(old) => {
                    // Keep the audit log honest when `install()` believed
                    // this was a fresh install (lost race with a concurrent
                    // install of the same extension).
                    if action == InstallAction::Installed {
                        action = if m.version < old.version {
                            InstallAction::Downgraded {
                                from: old.version.clone(),
                            }
                        } else {
                            InstallAction::Updated {
                                from: old.version.clone(),
                            }
                        };
                    }
                    old.version.to_string()
                }
                Err(e) => {
                    tracing::warn!(
                        extension = m.id,
                        error = %e,
                        "cannot read the manifest of the installed version — backing it up as 'unknown'"
                    );
                    "unknown".to_string()
                }
            };
            let b = self.backup_dir.join(format!("{}-{}.zip", m.id, from));
            package::zip_dir(dest, &b)?;
            backup = Some(b);
            let side = dest.with_extension(format!("old-{}", unique_suffix()));
            std::fs::rename(dest, &side).map_err(|e| {
                AppError::BadRequest(format!("cannot move the current version away: {e}"))
            })?;
            old_side = Some(side);
        }
        if let Err(e) = std::fs::rename(&root, dest) {
            // Roll the previous version back into place.
            if let Some(side) = &old_side {
                let _ = std::fs::rename(side, dest);
            }
            return Err(AppError::BadRequest(format!("cannot install files: {e}")));
        }

        // Plugin migrations run after the files are in place; a failure
        // triggers a rollback to the previous version.
        if m.kind == ExtensionKind::Plugin
            && let Err(e) = self.run_migrations(dest, &m.id).await
        {
            let detail = format!("migration error: {}", e.message());
            self.rollback(dest, old_side.as_deref());
            let _ = registry::log(
                &self.db,
                m.kind.as_str(),
                &m.id,
                action.as_str(),
                &m.version.to_string(),
                &opts.actor,
                "failed",
                &detail,
            )
            .await;
            return Err(AppError::BadRequest(format!(
                "installation rolled back — {detail}"
            )));
        }

        if let Some(side) = &old_side {
            let _ = std::fs::remove_dir_all(side);
        }

        registry::upsert(
            &self.db,
            m.kind.as_str(),
            &m.id,
            &m.version.to_string(),
            package_hash,
            &m.permissions,
        )
        .await?;
        let _ = registry::log(
            &self.db,
            m.kind.as_str(),
            &m.id,
            action.as_str(),
            &m.version.to_string(),
            &opts.actor,
            "success",
            "",
        )
        .await;

        tracing::info!(
            kind = m.kind.as_str(),
            extension = m.id,
            version = %m.version,
            action = action.as_str(),
            "extension installed"
        );
        Ok(InstallOutcome {
            kind: m.kind,
            manifest: on_disk,
            action,
            package_hash: package_hash.to_string(),
            backup,
            warnings,
        })
    }

    fn rollback(&self, dest: &Path, old_side: Option<&Path>) {
        let _ = std::fs::remove_dir_all(dest);
        if let Some(side) = old_side {
            let _ = std::fs::rename(side, dest);
        }
    }

    /// Apply pending SQL migrations from `<plugin>/migrations/*.sql` in name
    /// order. Each file runs in a transaction; applied names are recorded in
    /// `extension_migrations` so updates only run the delta.
    async fn run_migrations(&self, plugin_dir: &Path, ext_id: &str) -> AppResult<()> {
        let dir = plugin_dir.join("migrations");
        if !dir.is_dir() {
            return Ok(());
        }
        let applied = registry::migrations_applied(&self.db, ext_id).await?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| AppError::BadRequest(format!("cannot read migrations: {e}")))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "sql")
                    && p.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy();
                        !n.starts_with('.')
                            && n.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
                            })
                    })
            })
            .collect();
        files.sort();

        for path in files {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if applied.contains(&name) {
                continue;
            }
            let sql = std::fs::read_to_string(&path)
                .map_err(|e| AppError::BadRequest(format!("cannot read {name}: {e}")))?;
            let mut tx = self.db.pool().begin().await?;
            for stmt in migrate::split_statements(&sql) {
                let translated = self.db.dialect().translate(&stmt);
                sqlx::query(translated.as_ref()).execute(&mut *tx).await?;
            }
            registry::migration_record_in_transaction(&self.db, &mut tx, ext_id, &name).await?;
            tx.commit().await?;
            tracing::info!(
                plugin = ext_id,
                migration = name,
                "plugin migration applied"
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Uninstall
    // -----------------------------------------------------------------------

    /// Remove an extension. `remove_data` additionally drops the tables the
    /// plugin declared in `uninstall_tables` (admin opt-in — data survives a
    /// plain uninstall).
    pub async fn uninstall(
        &self,
        kind: ExtensionKind,
        id: &str,
        remove_data: bool,
        actor: &str,
    ) -> AppResult<UninstallOutcome> {
        let _guard = INSTALL_LOCK.lock().await;
        if !manifest::valid_id(id) {
            return Err(AppError::BadRequest("invalid extension id".into()));
        }
        let dest = self.dest_dir(kind, id);
        if !dest.is_dir() {
            return Err(AppError::NotFound(format!(
                "{} '{id}' is not installed",
                kind.as_str()
            )));
        }
        let m = manifest::load_from_dir(kind, &dest, Some(id)).ok();

        let mut removed_tables = Vec::new();
        if remove_data && let Some(m) = &m {
            for table in &m.uninstall_tables {
                // A manifest is untrusted input: it must never be able to
                // name a Polaris core table. `valid_table_name` only proves
                // the string is a bare identifier — `posts` is one too.
                if is_protected_table(table) {
                    tracing::warn!(
                        extension = id,
                        table,
                        "refusing to drop a protected core table declared in uninstall_tables"
                    );
                    continue;
                }
                let drop_sql = format!("DROP TABLE IF EXISTS {table}");
                let sql = self.db.dialect().translate(&drop_sql);
                if let Err(e) = sqlx::query(sql.as_ref()).execute(self.db.pool()).await {
                    tracing::warn!(table = table, error = %e, "cannot drop plugin table");
                    continue;
                }
                removed_tables.push(table.clone());
            }
            let _ = registry::migrations_clear(&self.db, id).await;
        }

        std::fs::remove_dir_all(&dest)
            .map_err(|e| AppError::BadRequest(format!("cannot remove files: {e}")))?;
        registry::delete(&self.db, kind.as_str(), id).await?;
        let _ = registry::log(
            &self.db,
            kind.as_str(),
            id,
            "uninstall",
            m.as_ref()
                .map(|x| x.version.to_string())
                .unwrap_or_default()
                .as_str(),
            actor,
            "success",
            if remove_data {
                "data removed"
            } else {
                "data kept"
            },
        )
        .await;
        tracing::info!(
            kind = kind.as_str(),
            extension = id,
            "extension uninstalled"
        );
        Ok(UninstallOutcome {
            kind,
            id: id.to_string(),
            removed_tables,
        })
    }

    // -----------------------------------------------------------------------
    // Scanning / verification
    // -----------------------------------------------------------------------

    /// All extensions of one kind found on disk.
    pub fn scan(&self, kind: ExtensionKind) -> Vec<ScanEntry> {
        let root = self.kind_root(kind);
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return out;
        };
        for entry in entries.flatten() {
            if !entry.path().join(kind.manifest_name()).is_file() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if !manifest::valid_id(&id) {
                out.push(ScanEntry {
                    id,
                    manifest: None,
                    error: Some("invalid directory name".into()),
                });
                continue;
            }
            match manifest::load_from_dir(kind, &entry.path(), Some(&id)) {
                Ok(m) => out.push(ScanEntry {
                    id,
                    manifest: Some(m),
                    error: None,
                }),
                Err(e) => out.push(ScanEntry {
                    id,
                    manifest: None,
                    error: Some(e.message()),
                }),
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Register extensions that pre-date the registry (shipped with the
    /// initial deployment, restored from a backup, …). Pure bookkeeping:
    /// no files are touched, nothing is activated or enabled.
    pub async fn seed_registry(&self) -> AppResult<usize> {
        let records = registry::list(&self.db).await?;
        let mut seeded = 0;
        for kind in [ExtensionKind::Theme, ExtensionKind::Plugin] {
            for entry in self.scan(kind) {
                if records
                    .iter()
                    .any(|r| r.kind == kind.as_str() && r.ext_id == entry.id)
                {
                    continue;
                }
                let Some(m) = &entry.manifest else { continue };
                registry::upsert(
                    &self.db,
                    kind.as_str(),
                    &entry.id,
                    &m.version.to_string(),
                    "",
                    &m.permissions,
                )
                .await?;
                seeded += 1;
                tracing::info!(
                    kind = kind.as_str(),
                    extension = entry.id.as_str(),
                    version = %m.version,
                    "registered pre-existing extension"
                );
            }
        }
        Ok(seeded)
    }

    /// Integrity check over every installed extension: manifest validity,
    /// registry consistency and (for plugins) entry file presence.
    pub async fn verify(&self) -> AppResult<Vec<VerifyEntry>> {
        let mut out = Vec::new();
        let records = registry::list(&self.db).await?;
        for kind in [ExtensionKind::Theme, ExtensionKind::Plugin] {
            for entry in self.scan(kind) {
                let mut issues = Vec::new();
                if let Some(err) = &entry.error {
                    issues.push(err.clone());
                }
                let Some(m) = &entry.manifest else {
                    out.push(VerifyEntry {
                        kind,
                        id: entry.id,
                        ok: false,
                        issues,
                    });
                    continue;
                };
                if kind == ExtensionKind::Plugin
                    && !self.dest_dir(kind, &entry.id).join(&m.entry).is_file()
                {
                    issues.push(format!("entry script '{}' missing", m.entry));
                }
                match records
                    .iter()
                    .find(|r| r.kind == kind.as_str() && r.ext_id == entry.id)
                {
                    Some(rec) if rec.version != m.version.to_string() => {
                        issues.push(format!(
                            "registry says {} but disk has {}",
                            rec.version, m.version
                        ));
                    }
                    None => issues.push("not recorded in the extension registry".into()),
                    _ => {}
                }
                out.push(VerifyEntry {
                    kind,
                    id: entry.id,
                    ok: issues.is_empty(),
                    issues,
                });
            }
        }
        Ok(out)
    }

    /// Recent install/update/uninstall audit log.
    pub async fn logs(
        &self,
        kind: Option<ExtensionKind>,
        limit: i64,
    ) -> AppResult<Vec<registry::ExtensionLogEntry>> {
        registry::logs(&self.db, kind.map(|k| k.as_str()), limit).await
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
