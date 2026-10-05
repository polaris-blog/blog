//! Scheduled (automatic) backups.
//!
//! The scheduler piggybacks on the existing one-minute maintenance tick in
//! `main.rs`: it checks the `settings` table for the schedule state and runs
//! a backup when the interval has elapsed. All knobs live in `settings`
//! (portable across dialects, editable from the admin UI without a restart):
//!
//! - `backup.auto.enabled`        ("true"/"false", default from config)
//! - `backup.auto.interval_hours` (minimum 1)
//! - `backup.auto.kind`           (full | database | media)
//! - `backup.auto.keep`           (retention for scheduled backups)
//! - `backup.last_run`            (epoch seconds, set after each run)

use crate::backup::service::{BackupKind, BackupService};
use crate::state::App;
use crate::utils::time;

/// Serializes scheduled runs so a slow backup never overlaps itself.
static SCHED_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Called once per minute by the background tick. Never fails the caller:
/// every error is logged and swallowed (a failed backup must not take the
/// scheduler — or anything else — down).
pub async fn tick(app: &App) {
    let result = maybe_run(app).await;
    if let Err(e) = result {
        tracing::warn!(error = %e.message(), "scheduled backup failed");
    }
}

async fn maybe_run(app: &App) -> crate::error::AppResult<()> {
    let cfg = &app.config.backup.auto;
    let enabled = app.settings.get_bool("backup.auto.enabled", cfg.enabled);
    if !enabled {
        return Ok(());
    }
    let interval_hours = app
        .settings
        .get("backup.auto.interval_hours")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|h| *h >= 1)
        .unwrap_or(cfg.interval_hours.max(1));
    let kind = match app.settings.get_str("backup.auto.kind", &cfg.kind).as_str() {
        "database" => BackupKind::Database,
        "media" => BackupKind::Media,
        _ => BackupKind::Full,
    };
    let keep = app
        .settings
        .get("backup.auto.keep")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|k| *k >= 1)
        .unwrap_or(cfg.keep.max(1));

    let now = time::now();
    let last_run = app
        .settings
        .get("backup.last_run")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last_run < (interval_hours * 3600) as i64 {
        return Ok(());
    }

    // Re-check after acquiring the lock: a previous tick may have finished a
    // backup while this one waited.
    let _guard = SCHED_LOCK.lock().await;
    let last_run = app
        .settings
        .get("backup.last_run")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last_run < (interval_hours * 3600) as i64 {
        return Ok(());
    }

    let svc = BackupService::new(&app.db, &app.config, app.media.storage())?;
    let summary = svc.create(kind, "scheduler").await?;
    app.settings
        .set(&app.db, "backup.last_run", &time::now().to_string())
        .await?;
    let pruned = svc.apply_retention(keep).await.unwrap_or(0);
    tracing::info!(
        backup = %summary.name,
        kind = summary.kind,
        pruned,
        "scheduled backup completed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal isolated app for the scheduler unit test (same shape as the
    /// integration-test helper, inlined here).
    async fn init_app() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cfg = crate::config::Config::default();
        cfg.database.url = dir.path().join("test.db").to_string_lossy().into_owned();
        cfg.theme.dir = dir.path().join("themes").to_string_lossy().into_owned();
        cfg.plugin.dir = dir.path().join("plugins").to_string_lossy().into_owned();
        cfg.media.storage.dir = dir.path().join("media").to_string_lossy().into_owned();
        cfg.backup.dir = dir.path().join("backups").to_string_lossy().into_owned();
        cfg.backup.tmp_dir = dir
            .path()
            .join("tmp")
            .join("backups")
            .to_string_lossy()
            .into_owned();
        cfg.security.secret = "test-secret".into();
        let app = crate::state::AppState::init(cfg).await.expect("app init");
        (app, dir)
    }

    #[tokio::test]
    async fn disabled_schedule_is_a_noop() {
        let (app, _dir) = init_app().await;
        // Auto backups are disabled by default: tick must do nothing.
        tick(&app).await;
        assert!(
            app.settings.get("backup.last_run").is_none(),
            "no backup may run while the schedule is disabled"
        );
    }

    #[tokio::test]
    async fn enabled_schedule_creates_backup_and_sets_last_run() {
        let (app, _dir) = init_app().await;
        app.settings
            .set(&app.db, "backup.auto.enabled", "true")
            .await
            .unwrap();
        app.settings
            .set(&app.db, "backup.auto.interval_hours", "1")
            .await
            .unwrap();
        app.settings
            .set(&app.db, "backup.auto.kind", "database")
            .await
            .unwrap();

        tick(&app).await;
        assert!(
            app.settings.get("backup.last_run").is_some(),
            "last_run recorded"
        );
        let svc = BackupService::new(&app.db, &app.config, app.media.storage()).unwrap();
        let list = svc.list().await.unwrap();
        assert_eq!(list.len(), 1, "one scheduled backup exists");
        assert_eq!(list[0].created_by, "scheduler");
        assert_eq!(list[0].kind, "database");

        // A second immediate tick must not create another backup (interval).
        tick(&app).await;
        let svc = BackupService::new(&app.db, &app.config, app.media.storage()).unwrap();
        assert_eq!(svc.list().await.unwrap().len(), 1);
    }
}
