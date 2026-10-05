//! Extension (theme & plugin) package lifecycle: install, update, downgrade,
//! uninstall, plugin migrations, dependency and Polaris compatibility checks.

mod common;

use std::io::Write;
use std::path::Path;

use polaris::extension::{ExtensionKind, InstallAction};
use polaris::state::App;

// ---------------------------------------------------------------------------
// ZIP builders
// ---------------------------------------------------------------------------

fn write_zip(path: &Path, entries: Vec<(String, Vec<u8>)>) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        zip.start_file(name, options).unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.finish().unwrap();
}

fn theme_zip(path: &Path, id: &str, version: &str, extra_manifest: &str) {
    let manifest = format!(
        "id = \"{id}\"\nname = \"Test Theme\"\nversion = \"{version}\"\nauthor = \"tester\"\n\
         description = \"a test theme\"\nlicense = \"MIT\"\n{extra_manifest}\n"
    );
    write_zip(
        path,
        vec![
            (format!("{id}/theme.toml"), manifest.into_bytes()),
            (
                format!("{id}/templates/index.html"),
                b"<h1>hello</h1>".to_vec(),
            ),
            (format!("{id}/README.md"), b"# test".to_vec()),
        ],
    );
}

fn plugin_zip(path: &Path, id: &str, version: &str, extra_manifest: &str) {
    let manifest = format!(
        "id = \"{id}\"\nname = \"Test Plugin\"\nversion = \"{version}\"\nauthor = \"tester\"\n\
         description = \"a test plugin\"\nlicense = \"MIT\"\n{extra_manifest}\n"
    );
    let mut entries = vec![
        (format!("{id}/plugin.toml"), manifest.into_bytes()),
        (format!("{id}/main.rhai"), b"fn register(api) {}".to_vec()),
    ];
    if extra_manifest.contains("uninstall_tables") {
        entries.push((
            format!("{id}/migrations/001_init.sql"),
            b"CREATE TABLE IF NOT EXISTS ext_test_data (id INTEGER PRIMARY KEY, note TEXT NOT NULL)".to_vec(),
        ));
    }
    write_zip(path, entries);
}

async fn install(app: &App, zip: &Path) -> polaris::extension::InstallOutcome {
    polaris::services::extensions::install(app, zip, "tester", false, false)
        .await
        .unwrap()
}

async fn install_opts(
    app: &App,
    zip: &Path,
    force: bool,
    allow_downgrade: bool,
) -> Result<polaris::extension::InstallOutcome, polaris::error::AppError> {
    polaris::services::extensions::install(app, zip, "tester", force, allow_downgrade).await
}

// ---------------------------------------------------------------------------
// Theme lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn theme_install_update_downgrade_uninstall() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();
    let themes = dir.path().join("themes");
    let backups = dir.path().join("data").join("backups").join("extensions");

    // Install 1.0.0.
    let v1 = zips.join("aurora-1.0.0.zip");
    theme_zip(&v1, "aurora", "1.0.0", "");
    let out = install(&app, &v1).await;
    assert_eq!(out.manifest.id, "aurora");
    assert_eq!(out.action, InstallAction::Installed);
    assert!(themes.join("aurora").join("theme.toml").is_file());
    assert!(out.package_hash.len() == 64);

    // Registered + logged, not active.
    let status = polaris::services::extensions::status(&app, ExtensionKind::Theme)
        .await
        .unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0]["dir"], "aurora");
    assert_eq!(status[0]["status"], "disabled");
    let logs = polaris::services::extensions::logs(&app, Some(ExtensionKind::Theme), 10)
        .await
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].action, "install");
    assert_eq!(logs[0].result, "success");

    // Update to 1.1.0: old version is backed up.
    let v2 = zips.join("aurora-1.1.0.zip");
    theme_zip(&v2, "aurora", "1.1.0", "");
    let out = install(&app, &v2).await;
    assert!(matches!(out.action, InstallAction::Updated { .. }));
    assert!(out.backup.is_some());
    assert!(backups.join("aurora-1.0.0.zip").is_file());

    // Older version refused without the downgrade opt-in.
    let v3 = zips.join("aurora-1.0.5.zip");
    theme_zip(&v3, "aurora", "1.0.5", "");
    let err = install_opts(&app, &v3, false, false).await.unwrap_err();
    assert!(err.message().contains("older"), "{}", err.message());

    // With the opt-in it downgrades and backs up 1.1.0.
    let out = install_opts(&app, &v3, false, true).await.unwrap();
    assert!(matches!(out.action, InstallAction::Downgraded { .. }));
    assert!(backups.join("aurora-1.1.0.zip").is_file());

    // Uninstall removes files and registry entry.
    polaris::services::extensions::uninstall(&app, ExtensionKind::Theme, "aurora", false, "tester")
        .await
        .unwrap();
    assert!(!themes.join("aurora").exists());
    let status = polaris::services::extensions::status(&app, ExtensionKind::Theme)
        .await
        .unwrap();
    assert!(status.is_empty());
}

// ---------------------------------------------------------------------------
// Plugin lifecycle with migrations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plugin_migrations_and_data_retention() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();
    let plugins = dir.path().join("plugins");

    let manifest = "permissions = [\"posts.read\"]\nuninstall_tables = [\"ext_test_data\"]";
    let v1 = zips.join("greeter-1.0.0.zip");
    plugin_zip(&v1, "greeter", "1.0.0", manifest);
    let out = install(&app, &v1).await;
    assert_eq!(out.kind, ExtensionKind::Plugin);
    assert_eq!(out.manifest.permissions, vec!["posts.read".to_string()]);
    assert!(plugins.join("greeter").join("main.rhai").is_file());
    assert!(
        plugins
            .join("greeter")
            .join("migrations")
            .join("001_init.sql")
            .is_file()
    );

    // The migration created the table.
    let row: Option<i64> = sqlx::query_scalar("SELECT COUNT(*) FROM ext_test_data")
        .fetch_optional(app.db.pool())
        .await
        .unwrap()
        .flatten();
    assert_eq!(row, Some(0));

    // Installed plugins start disabled.
    assert!(
        !app.settings
            .plugins_enabled()
            .contains(&"greeter".to_string())
    );
    let status = polaris::services::extensions::status(&app, ExtensionKind::Plugin)
        .await
        .unwrap();
    assert_eq!(status[0]["status"], "disabled");

    // Update to 1.1.0: the migration is not applied twice (same file name).
    let v2 = zips.join("greeter-1.1.0.zip");
    plugin_zip(&v2, "greeter", "1.1.0", manifest);
    let out = install(&app, &v2).await;
    assert!(matches!(out.action, InstallAction::Updated { .. }));

    // Uninstall keeps data by default…
    polaris::services::extensions::uninstall(
        &app,
        ExtensionKind::Plugin,
        "greeter",
        false,
        "tester",
    )
    .await
    .unwrap();
    assert!(!plugins.join("greeter").exists());
    let still_there: Option<i64> = sqlx::query_scalar("SELECT COUNT(*) FROM ext_test_data")
        .fetch_optional(app.db.pool())
        .await
        .unwrap()
        .flatten();
    assert_eq!(
        still_there,
        Some(0),
        "keep-data uninstall must not drop tables"
    );

    // …and drops it when explicitly requested.
    let v1b = zips.join("greeter-1.0.0-b.zip");
    plugin_zip(&v1b, "greeter", "1.0.0", manifest);
    install(&app, &v1b).await;
    let out = polaris::services::extensions::uninstall(
        &app,
        ExtensionKind::Plugin,
        "greeter",
        true,
        "tester",
    )
    .await
    .unwrap();
    assert_eq!(out.removed_tables, vec!["ext_test_data".to_string()]);
    let gone: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'ext_test_data'",
    )
    .fetch_one(app.db.pool())
    .await
    .unwrap();
    assert_eq!(gone, 0, "remove-data uninstall must drop the table");
}

// ---------------------------------------------------------------------------
// Dependency and compatibility checks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dependency_and_polaris_version_checks() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();

    // Missing dependency blocks the install.
    let v1 = zips.join("dependent-1.0.0.zip");
    theme_zip(
        &v1,
        "dependent",
        "1.0.0",
        "[dependencies]\nsearch-core = \"^1.2\"\n",
    );
    let err = install_opts(&app, &v1, false, false).await.unwrap_err();
    assert!(
        err.message().contains("missing dependencies"),
        "{}",
        err.message()
    );

    // Installing the dependency unblocks it.
    let dep = zips.join("search-core-1.3.0.zip");
    plugin_zip(&dep, "search-core", "1.3.0", "");
    install(&app, &dep).await;
    install(&app, &v1).await;

    // A wrong installed version still fails the requirement.
    let v2 = zips.join("needs-newer-1.0.0.zip");
    theme_zip(
        &v2,
        "needs-newer",
        "1.0.0",
        "[dependencies]\nsearch-core = \"^2.0\"\n",
    );
    let err = install_opts(&app, &v2, false, false).await.unwrap_err();
    assert!(
        err.message().contains("missing dependencies"),
        "{}",
        err.message()
    );

    // Polaris compatibility: too-new requirement refused, force overrides.
    let v3 = zips.join("future-1.0.0.zip");
    theme_zip(
        &v3,
        "future",
        "1.0.0",
        "minimum_polaris_version = \"999.0.0\"\n",
    );
    let err = install_opts(&app, &v3, false, false).await.unwrap_err();
    assert!(err.message().contains("incompatible"), "{}", err.message());
    install_opts(&app, &v3, true, false).await.unwrap();
}

// ---------------------------------------------------------------------------
// Active theme protection + verify
// ---------------------------------------------------------------------------

#[tokio::test]
async fn active_theme_cannot_be_uninstalled_and_verify_reports_state() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();

    let zip = zips.join("activable-1.0.0.zip");
    theme_zip(&zip, "activable", "1.0.0", "");
    install(&app, &zip).await;
    app.set_active_theme("activable").await.unwrap();
    assert_eq!(app.theme.current_name(), "activable");

    // The active theme refuses to uninstall.
    let err = polaris::services::extensions::uninstall(
        &app,
        ExtensionKind::Theme,
        "activable",
        false,
        "tester",
    )
    .await
    .unwrap_err();
    assert!(err.message().contains("active theme"), "{}", err.message());

    // Verify is happy while the manifest matches the registry.
    let entries = polaris::services::extensions::verify(&app).await.unwrap();
    let theme = entries.iter().find(|e| e.id == "activable").unwrap();
    assert!(theme.ok, "issues: {:?}", theme.issues);

    // Tampering with the on-disk manifest makes verify complain.
    let manifest = dir
        .path()
        .join("themes")
        .join("activable")
        .join("theme.toml");
    let raw = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.0.0", "9.9.9");
    std::fs::write(&manifest, raw).unwrap();
    let entries = polaris::services::extensions::verify(&app).await.unwrap();
    let theme = entries.iter().find(|e| e.id == "activable").unwrap();
    assert!(!theme.ok);
}

// ---------------------------------------------------------------------------
// Regression: a manifest must not be able to destroy Polaris core tables
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uninstall_tables_cannot_name_core_tables() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();

    // Seed a post so there is real data to lose.
    common::create_user(&app, "author", "password123", polaris::models::Role::Admin).await;
    let posts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert!(posts > 0, "the seeded install must contain posts");

    let job = app
        .scheduler
        .create(polaris::scheduler::JobRequest {
            name: "Retained scheduler job".into(),
            job_type: "cleanup".into(),
            delay: Some(3600),
            ..Default::default()
        })
        .await
        .unwrap();
    let manifest = "uninstall_tables = [\"ext_test_data\", \"posts\", \"scheduler_jobs\", \"SCHEDULER_ATTEMPTS\"]";
    let zip = zips.join("hostile-1.0.0.zip");
    plugin_zip(&zip, "hostile", "1.0.0", manifest);
    install(&app, &zip).await;

    let out = polaris::services::extensions::uninstall(
        &app,
        ExtensionKind::Plugin,
        "hostile",
        true, // remove_data = true: the dangerous path
        "tester",
    )
    .await
    .unwrap();

    // The plugin-owned table went away…
    assert_eq!(out.removed_tables, vec!["ext_test_data".to_string()]);
    // …but `posts` survived.
    let still: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'posts'",
    )
    .fetch_one(app.db.pool())
    .await
    .unwrap();
    assert_eq!(still, 1, "`posts` must never be droppable from a manifest");
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(after, posts);
    assert_eq!(
        app.scheduler.get(&job.id).await.unwrap().name,
        "Retained scheduler job"
    );
    assert!(app.scheduler.history(&job.id).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Regression: a stray file where the install directory belongs must error,
// not panic (the old code hit `unreachable!("dest exists")`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn install_over_a_plain_file_errors_instead_of_panicking() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();
    let themes = dir.path().join("themes");

    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("aurora"), b"not a directory").unwrap();

    let zip = zips.join("aurora-1.0.0.zip");
    theme_zip(&zip, "aurora", "1.0.0", "");
    let err = install_opts(&app, &zip, false, false).await.unwrap_err();
    assert!(
        err.message().contains("not a directory"),
        "unexpected error: {}",
        err.message()
    );
    // The stray file is left untouched for an admin to inspect.
    assert!(themes.join("aurora").is_file());
}

// ---------------------------------------------------------------------------
// Regression: concurrent installs of the same extension are serialized and
// both complete (previously the loser could panic or half-swap the tree).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_installs_of_the_same_extension_are_serialized() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();

    let a = zips.join("racer-1.0.0.zip");
    let b = zips.join("racer-1.1.0.zip");
    theme_zip(&a, "racer", "1.0.0", "");
    theme_zip(&b, "racer", "1.1.0", "");

    let app2 = app.clone();
    let (first, second) = tokio::join!(
        install_opts(&app, &a, false, true),
        install_opts(&app2, &b, false, true)
    );
    // Both installs succeed; one is the install and the other an update
    // (or downgrade, depending on interleaving) — never a panic, never a
    // half-written tree.
    let o1 = first.expect("first install");
    let o2 = second.expect("second install");
    assert_ne!(o1.action.as_str(), "", "action must be recorded");
    assert!(
        matches!(
            o2.action,
            InstallAction::Updated { .. } | InstallAction::Downgraded { .. }
        ),
        "the second install must observe the first: {:?}",
        o2.action
    );

    // Whatever the order, the tree on disk is complete and matches its own
    // manifest.
    let themes = dir.path().join("themes").join("racer");
    assert!(themes.join("theme.toml").is_file());
    assert!(themes.join("templates").join("index.html").is_file());
    let raw = std::fs::read_to_string(themes.join("theme.toml")).unwrap();
    let version = raw
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .unwrap_or("")
        .trim_matches('"')
        .to_string();
    assert!(
        !version.is_empty(),
        "installed manifest is corrupted: {raw}"
    );

    // No staging directory was left behind.
    let staging = dir.path().join("data").join("tmp").join("extensions");
    let leftovers = std::fs::read_dir(&staging)
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "staging must be cleaned up");
}

// ---------------------------------------------------------------------------
// Uninstalling a plugin disables it first
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uninstalling_an_enabled_plugin_disables_it() {
    let (app, dir) = common::init_app().await;
    let zips = dir.path().join("zips");
    std::fs::create_dir_all(&zips).unwrap();

    let zip = zips.join("tmpplug-1.0.0.zip");
    plugin_zip(&zip, "tmpplug", "1.0.0", "");
    install(&app, &zip).await;

    app.set_plugins_enabled(&["tmpplug".to_string()])
        .await
        .unwrap();
    assert!(
        app.settings
            .plugins_enabled()
            .contains(&"tmpplug".to_string())
    );

    polaris::services::extensions::uninstall(
        &app,
        ExtensionKind::Plugin,
        "tmpplug",
        false,
        "tester",
    )
    .await
    .unwrap();
    assert!(
        !app.settings
            .plugins_enabled()
            .contains(&"tmpplug".to_string())
    );
    assert!(!dir.path().join("plugins").join("tmpplug").exists());
}
