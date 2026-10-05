//! Shared helpers for Polaris integration tests.
// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use polaris::config::Config;
use polaris::models::{Role, User};
use polaris::state::{App, AppState};

/// A config pointing at a temp directory (isolated SQLite DB, empty theme and
/// plugin dirs — the embedded fallback theme is used).
pub fn test_config(root: &std::path::Path) -> Config {
    let mut cfg = Config::default();
    cfg.database.driver = "sqlite".into();
    cfg.database.url = root.join("test.db").to_string_lossy().into_owned();
    cfg.database.max_connections = 2;
    cfg.theme.dir = root.join("themes").to_string_lossy().into_owned();
    cfg.plugin.dir = root.join("plugins").to_string_lossy().into_owned();
    cfg.media.storage.dir = root.join("media").to_string_lossy().into_owned();
    cfg.extensions.tmp_dir = root
        .join("data")
        .join("tmp")
        .join("extensions")
        .to_string_lossy()
        .into_owned();
    cfg.extensions.backup_dir = root
        .join("data")
        .join("backups")
        .join("extensions")
        .to_string_lossy()
        .into_owned();
    cfg.backup.dir = root
        .join("data")
        .join("backups")
        .to_string_lossy()
        .into_owned();
    cfg.backup.tmp_dir = root
        .join("data")
        .join("tmp")
        .join("backups")
        .to_string_lossy()
        .into_owned();
    cfg.security.secret = "integration-test-secret".into();
    cfg.site.base_url = "http://polaris.test".into();
    // The setup wizard writes environment changes back to this file — keep
    // tests away from the developer's real polaris.toml.
    cfg.config_path = Some(root.join("polaris.toml"));
    cfg
}

/// Fresh app backed by a temp SQLite database (auto-migrated).
pub async fn init_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = AppState::init(test_config(dir.path()))
        .await
        .expect("app init");
    (app, dir)
}

/// Create a user through the service layer.
/// The first user created also seeds sample content.
pub async fn create_user(app: &App, username: &str, password: &str, role: Role) -> User {
    polaris::services::users::create_user(app, username, "user@example.test", password, role)
        .await
        .expect("create user")
}

/// An empty ConfigManager (no loaded namespaces) for plugin/theme tests that
/// need a PluginManager but never touch configuration storage.
pub fn empty_config_manager() -> std::sync::Arc<polaris::config_store::ConfigManager> {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = test_config(dir.path());
    std::mem::forget(dir);
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let db = polaris::db::Db::connect(&cfg.database)
            .await
            .expect("db connect");
        std::sync::Arc::new(polaris::config_store::ConfigManager::new(
            db,
            "test-secret".into(),
        ))
    })
}

/// App + router + kept-alive temp dir (admin user + seeded content).
pub async fn init_http() -> (App, axum::Router, tempfile::TempDir) {
    let (app, dir) = init_app().await;
    create_user(&app, "admin", "password123", Role::Admin).await;
    let router = polaris::http::router(app.clone());
    (app, router, dir)
}
