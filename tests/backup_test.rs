//! Backup & restore end-to-end: create → verify → restore round-trips,
//! integrity enforcement, compatibility gates, selective restore and the
//! retention policy.

mod common;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use sha2::Digest;
use tower::ServiceExt;

use polaris::backup::BackupKind;
use polaris::models::Role;
use polaris::state::App;

/// A real PNG (kept tiny; the backup pipeline does not decode images).
fn png_bytes() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(8, 8, |x, y| {
        image::Rgba([((x * 31) % 256) as u8, ((y * 31) % 256) as u8, 0x7f, 0xff])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode png");
    out
}

async fn upload_media(app: &App, user_id: i64, name: &str) {
    let bytes = png_bytes();
    let mut reader: &[u8] = &bytes;
    polaris::services::media::upload_streamed(app, user_id, name, "image/png", &mut reader)
        .await
        .expect("media upload");
}

/// Seed a site worth backing up: user, published post with a comment,
/// settings tweak, one media object, one custom theme directory, and a
/// security secret stored in the settings table.
async fn seed(app: &App) -> (i64, String, String) {
    let admin = common::create_user(app, "admin", "password123", Role::Admin).await;
    upload_media(app, admin.id, "photo.png").await;
    let (media, _) = polaris::repositories::media::list(
        &app.db,
        &polaris::repositories::media::MediaFilter::default(),
    )
    .await
    .unwrap();
    let uuid = media[0].uuid.clone();

    // The post embeds the media by URL so `media_references` is populated.
    let post = polaris::services::posts::create_post(
        app,
        admin.id,
        polaris::services::posts::PostInput {
            title: "Backed up post".into(),
            content_md: format!("![photo](/media/{uuid}.png)"),
            status: polaris::models::PostStatus::Published,
            ..Default::default()
        },
    )
    .await
    .expect("create post");
    let comment = polaris::services::comments::create(
        app,
        polaris::services::comments::NewComment {
            post_id: post.id,
            parent_id: None,
            author_name: "Visitor".into(),
            author_email: String::new(),
            author_url: String::new(),
            content: "great post".into(),
        },
        false,
    )
    .await
    .expect("comment");
    assert_eq!(comment.status, polaris::models::CommentStatus::Approved);

    // A secret stored in the settings table (the restore must keep it).
    app.settings
        .set(&app.db, "security.secret", "current-instance-secret")
        .await
        .unwrap();
    app.settings
        .set(&app.db, "site.title", "Backed up site")
        .await
        .unwrap();

    // A default theme directory (as a real deployment ships) plus a custom
    // one — both picked up by full backups.
    for (name, asset) in [("default", "base.css"), ("custom", "a.txt")] {
        let theme = std::path::Path::new(&app.config.theme.dir).join(name);
        std::fs::create_dir_all(theme.join("static")).unwrap();
        std::fs::write(
            theme.join("theme.toml"),
            format!("name = \"{name}\"\nversion = \"1.0.0\"\n"),
        )
        .unwrap();
        std::fs::write(theme.join("static").join(asset), "theme asset").unwrap();
    }

    (
        post.id,
        "Backed up site".to_string(),
        "current-instance-secret".to_string(),
    )
}

// ---------------------------------------------------------------------------
// Full round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backup_verify_restore_roundtrip() {
    let (app, _dir) = common::init_app().await;
    let (post_id, site_title, secret) = seed(&app).await;
    let svc = app.backup();

    // --- create + verify ------------------------------------------------
    let summary = svc
        .create(BackupKind::Full, "admin")
        .await
        .expect("create backup");
    assert!(summary.ok);
    assert_eq!(summary.kind, "full");
    assert!(summary.counts.db_rows > 0);
    assert_eq!(summary.counts.media_files, 1);
    assert_eq!(summary.counts.theme_files, 4);

    let path = svc.storage_path_of(&summary.name).unwrap();
    let manifest = svc.read_manifest(&path).expect("manifest parses");
    assert_eq!(manifest.dialect, "sqlite");
    assert_eq!(manifest.created_by, "admin");
    assert!(
        manifest
            .checksums
            .contains_key(polaris::backup::manifest::DATABASE_ENTRY)
    );

    let report = svc.verify_file(&path).expect("verify runs");
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.files_checked, manifest.counts.files);

    // --- destructive changes after the backup ---------------------------
    let extra = polaris::services::posts::create_post(
        &app,
        1,
        polaris::services::posts::PostInput {
            title: "Post after backup".into(),
            status: polaris::models::PostStatus::Published,
            ..Default::default()
        },
    )
    .await
    .expect("create extra post");
    let media = polaris::services::media::get_by_uuid(&app, &{
        let (m, _) = polaris::repositories::media::list(
            &app.db,
            &polaris::repositories::media::MediaFilter::default(),
        )
        .await
        .unwrap();
        m.first().expect("media exists").uuid.clone()
    })
    .await
    .unwrap()
    .expect("media record");
    polaris::services::media::delete(&app, &media, true)
        .await
        .unwrap();
    app.settings
        .set(&app.db, "site.title", "Changed site")
        .await
        .unwrap();
    std::fs::remove_file(std::path::Path::new(&app.config.theme.dir).join("custom/static/a.txt"))
        .unwrap();

    // --- restore ---------------------------------------------------------
    let report = svc
        .restore(&app, &path, polaris::backup::restore::RestoreOptions::all())
        .await
        .expect("restore succeeds");
    assert!(report.database_restored);
    assert!(report.snapshot.is_some(), "pre-restore snapshot created");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // The extra post is gone; the backed-up post is back.
    assert!(
        polaris::repositories::posts::find_by_id(&app.db, extra.id)
            .await
            .unwrap()
            .is_none(),
        "post created after the backup must not survive a restore"
    );
    let restored = polaris::repositories::posts::find_by_id(&app.db, post_id)
        .await
        .unwrap()
        .expect("backed-up post restored");

    // Settings restored (but the instance secret is preserved).
    assert_eq!(app.settings.get("site.title").unwrap(), site_title);
    assert_eq!(
        app.settings.get("security.secret").unwrap(),
        secret,
        "security.secret must never be replaced by a restore"
    );

    // Media restored to storage and DB (bytes merge back).
    let (media, _) = polaris::repositories::media::list(
        &app.db,
        &polaris::repositories::media::MediaFilter::default(),
    )
    .await
    .unwrap();
    assert_eq!(media.len(), 1, "media record restored");
    let m = &media[0];
    assert!(
        app.media.storage().exists(&m.storage_key).await.unwrap(),
        "media bytes restored"
    );
    let stored = app
        .media
        .storage()
        .read(&m.storage_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, png_bytes());

    // Theme files restored.
    assert!(
        std::path::Path::new(&app.config.theme.dir)
            .join("custom/static/a.txt")
            .is_file(),
        "theme asset restored"
    );

    // Comment count matches (one approved comment).
    let comments = polaris::repositories::comments::list_approved_for_post(&app.db, restored.id)
        .await
        .unwrap();
    assert_eq!(comments.len(), 1);

    // Search index was rebuilt from restored content.
    let status = app.search.status(&app).await.unwrap();
    assert!(status.healthy);
    assert!(
        status.indexed_posts >= 1,
        "search index rebuilt after restore"
    );

    // The pre-restore snapshot is listed and verified.
    let list = svc.list().await.unwrap();
    let snapshot = list
        .iter()
        .find(|b| b.created_by == "pre-restore")
        .expect("snapshot listed");
    let snap_path = svc.storage_path_of(&snapshot.name).unwrap();
    assert!(svc.verify_file(&snap_path).unwrap().ok());
    // Comment reference tracking was restored along with the post.
    let refs = polaris::repositories::media::references_of(&app.db, m.id)
        .await
        .unwrap();
    assert!(
        refs.iter().any(|(t, id)| t == "post" && *id == post_id),
        "media references restored"
    );
}

// ---------------------------------------------------------------------------
// Verification & compatibility gates
// ---------------------------------------------------------------------------

/// Build a hand-crafted archive with a manifest whose checksums do not
/// match (a tampered backup), plus manifests with a foreign dialect and a
/// newer Polaris version.
fn craft_backup(
    dir: &std::path::Path,
    name: &str,
    dialect: &str,
    version: &str,
    corrupt_checksum: bool,
) -> std::path::PathBuf {
    use polaris::backup::manifest::{
        BackupManifest, DATABASE_ENTRY, FORMAT_VERSION, MANIFEST_ENTRY,
    };

    let payload = b"{\"format\":\"polaris-dump\",\"version\":1}\nnot really a dump\n".to_vec();
    let mut hasher = sha2::Sha256::new();
    hasher.update(&payload);
    let mut sum = format!("{:x}", hasher.finalize());
    if corrupt_checksum {
        sum = "f".repeat(64);
    }
    let manifest = BackupManifest {
        format_version: FORMAT_VERSION,
        polaris_version: version.to_string(),
        created_at: 1,
        dialect: dialect.to_string(),
        kind: "database".into(),
        backup_id: format!("crafted-{name}"),
        created_by: "test".into(),
        includes: polaris::backup::manifest::BackupIncludes {
            database: true,
            media: false,
            themes: false,
            plugins: false,
        },
        counts: polaris::backup::manifest::BackupCounts {
            files: 1,
            db_tables: 1,
            db_rows: 0,
            media_files: 0,
            theme_files: 0,
            plugin_files: 0,
        },
        sizes: Default::default(),
        checksums: [(DATABASE_ENTRY.to_string(), sum)].into_iter().collect(),
    };

    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file(DATABASE_ENTRY, opts).unwrap();
    std::io::Write::write_all(&mut zip, &payload).unwrap();
    zip.start_file(MANIFEST_ENTRY, opts).unwrap();
    std::io::Write::write_all(&mut zip, manifest.to_toml().unwrap().as_bytes()).unwrap();
    zip.finish().unwrap();
    path
}

#[tokio::test]
async fn restore_rejects_tampered_foreign_and_newer_backups() {
    let (app, dir) = common::init_app().await;
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    let svc = app.backup();

    // Tampered content: manifest checksum does not match the entry.
    let tampered = craft_backup(dir.path(), "tampered.zip", "sqlite", "0.1.0", true);
    let err = svc
        .restore(
            &app,
            &tampered,
            polaris::backup::restore::RestoreOptions::all(),
        )
        .await
        .unwrap_err();
    assert!(err.message().contains("integrity"), "{err}");
    // Nothing was touched.
    assert!(
        svc.list()
            .await
            .unwrap()
            .iter()
            .all(|b| b.created_by != "pre-restore")
    );

    // Foreign dialect: a MySQL backup cannot restore into SQLite.
    let foreign = craft_backup(dir.path(), "foreign.zip", "mysql", "0.1.0", false);
    let err = svc
        .restore(
            &app,
            &foreign,
            polaris::backup::restore::RestoreOptions::all(),
        )
        .await
        .unwrap_err();
    assert!(err.message().contains("dialect"), "{err}");

    // Newer Polaris version: blocked until this build is upgraded.
    let newer = craft_backup(dir.path(), "newer.zip", "sqlite", "99.0.0", false);
    let err = svc
        .restore(
            &app,
            &newer,
            polaris::backup::restore::RestoreOptions::all(),
        )
        .await
        .unwrap_err();
    assert!(err.message().contains("newer"), "{err}");

    // Same shape with correct checksum and version verifies cleanly.
    let good = craft_backup(dir.path(), "good.zip", "sqlite", "0.1.0", false);
    // The payload is not a real dump: verification passes but the DB replay
    // fails preflight without touching the database or files.
    let err = svc
        .restore(&app, &good, polaris::backup::restore::RestoreOptions::all())
        .await
        .unwrap_err();
    assert!(
        !err.message().is_empty(),
        "restore of a broken dump fails without corrupting the database"
    );
    // Preflight failures do not create a snapshot or change the database.
    let list = svc.list().await.unwrap();
    assert!(list.iter().all(|b| b.created_by != "pre-restore"));
}

// ---------------------------------------------------------------------------
// Selective restore
// ---------------------------------------------------------------------------

fn write_restore_extensions(root: &std::path::Path, version: &str) {
    let theme = root.join("themes/shared");
    std::fs::create_dir_all(theme.join("templates")).unwrap();
    std::fs::write(
        theme.join("theme.toml"),
        "name = 'shared'\nversion = '1.0.0'\n",
    )
    .unwrap();
    std::fs::write(theme.join("templates/index.html"), version).unwrap();
    let plugin = root.join("plugins/shared");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.toml"),
        "name = 'shared'\nversion = '1.0.0'\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("main.rhai"),
        format!("fn markdown_before(md) {{ md + \"{version}\" }}"),
    )
    .unwrap();
    std::fs::write(
        plugin.join("config.schema.toml"),
        "[tagline]\ntype = 'string'\n",
    )
    .unwrap();
}

#[tokio::test]
async fn database_failure_rolls_back_existing_and_new_extensions() {
    let (app, dir) = common::init_app().await;
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    write_restore_extensions(dir.path(), "archived");
    let new_theme = dir.path().join("themes/new-theme");
    std::fs::create_dir_all(&new_theme).unwrap();
    std::fs::write(new_theme.join("theme.toml"), "name = 'new-theme'\n").unwrap();
    let backup = app.backup().create(BackupKind::Full, "test").await.unwrap();
    let path = app.backup().storage_path_of(&backup.name).unwrap();
    write_restore_extensions(dir.path(), "current");
    std::fs::remove_dir_all(&new_theme).unwrap();
    app.settings
        .set(&app.db, "site.title", "current database")
        .await
        .unwrap();
    // Fail during SQL replay, after file promotion and table deletion.
    app.db.execute("CREATE TRIGGER fail_restore BEFORE INSERT ON users BEGIN SELECT RAISE(ABORT, 'injected restore failure'); END", &[]).await.unwrap();
    assert!(app.backup().verify_file(&path).unwrap().ok());
    assert!(
        app.backup()
            .restore(&app, &path, polaris::backup::restore::RestoreOptions::all())
            .await
            .is_err()
    );
    assert_eq!(
        polaris::repositories::settings::get(&app.db, "site.title")
            .await
            .unwrap()
            .as_deref(),
        Some("current database")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("themes/shared/templates/index.html")).unwrap(),
        "current"
    );
    assert!(
        std::fs::read_to_string(dir.path().join("plugins/shared/main.rhai"))
            .unwrap()
            .contains("current")
    );
    assert!(
        !new_theme.exists(),
        "newly restored extensions must also roll back"
    );
    assert_eq!(
        std::fs::read_dir(&app.config.backup.tmp_dir)
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn restore_reloads_unchanged_extension_names_and_configuration() {
    let (app, dir) = common::init_app().await;
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    write_restore_extensions(dir.path(), "archived");
    app.settings
        .set(&app.db, "plugin.shared.tagline", "archived config")
        .await
        .unwrap();
    app.set_active_theme("shared").await.unwrap();
    app.set_plugins_enabled(&["shared".into()]).await.unwrap();
    let backup = app.backup().create(BackupKind::Full, "test").await.unwrap();
    let path = app.backup().storage_path_of(&backup.name).unwrap();
    for database in [false, true] {
        write_restore_extensions(dir.path(), "current");
        app.settings
            .set(&app.db, "plugin.shared.tagline", "current config")
            .await
            .unwrap();
        app.configs.unload_namespace("plugin.shared");
        app.set_active_theme("shared").await.unwrap();
        app.set_plugins_enabled(&["shared".into()]).await.unwrap();
        assert_eq!(app.plugins.hook_str("markdown_before", ""), "current");
        let report = app
            .backup()
            .restore(
                &app,
                &path,
                polaris::backup::restore::RestoreOptions {
                    database,
                    media: false,
                    themes: true,
                    plugins: true,
                },
            )
            .await
            .unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(
            app.theme
                .current()
                .tera
                .render("index.html", &tera::Context::new())
                .unwrap(),
            "archived"
        );
        assert_eq!(app.plugins.hook_str("markdown_before", ""), "archived");
        assert_eq!(
            app.configs
                .get_string("plugin.shared", "tagline")
                .as_deref(),
            Some(if database {
                "archived config"
            } else {
                "current config"
            })
        );
    }
}

#[tokio::test]
async fn encrypted_backup_requires_original_key_before_any_mutation() {
    let (source, source_dir) = common::init_app().await;
    common::create_user(&source, "admin", "password123", Role::Admin).await;
    write_restore_extensions(source_dir.path(), "archived");
    std::fs::write(
        source_dir.path().join("plugins/shared/config.schema.toml"),
        "[api_key]\ntype = 'password'\n",
    )
    .unwrap();
    source
        .set_plugins_enabled(&["shared".into()])
        .await
        .unwrap();
    source
        .save_plugin_config(
            "shared",
            &std::collections::HashMap::from([("api_key".into(), "private-api-key".into())]),
            polaris::config_schema::Permission::Admin,
        )
        .await
        .unwrap();
    let backup = source
        .backup()
        .create(BackupKind::Full, "test")
        .await
        .unwrap();
    let path = source.backup().storage_path_of(&backup.name).unwrap();

    let destination = tempfile::tempdir().unwrap();
    let mut cfg = common::test_config(destination.path());
    cfg.security.secret = "different-instance-key".into();
    let target = polaris::state::AppState::init(cfg.clone()).await.unwrap();
    write_restore_extensions(destination.path(), "current");
    target
        .settings
        .set(&target.db, "site.title", "current database")
        .await
        .unwrap();
    let error = target
        .backup()
        .restore(
            &target,
            &path,
            polaris::backup::restore::RestoreOptions::all(),
        )
        .await
        .unwrap_err();
    assert!(error.message().contains("original security.secret"));
    assert!(target.backup().list().await.unwrap().is_empty());
    assert_eq!(
        polaris::repositories::settings::get(&target.db, "site.title")
            .await
            .unwrap()
            .as_deref(),
        Some("current database")
    );
    assert_eq!(
        std::fs::read_to_string(
            destination
                .path()
                .join("themes/shared/templates/index.html")
        )
        .unwrap(),
        "current"
    );

    cfg.security.secret = source.config.security.secret.clone();
    let target = polaris::state::AppState::init(cfg).await.unwrap();
    target
        .backup()
        .restore(
            &target,
            &path,
            polaris::backup::restore::RestoreOptions::all(),
        )
        .await
        .unwrap();
    assert_eq!(
        target
            .configs
            .get_string("plugin.shared", "api_key")
            .as_deref(),
        Some("private-api-key")
    );
}

#[tokio::test]
async fn selective_restore_media_only_leaves_database_alone() {
    let (app, dir) = common::init_app().await;
    let admin = common::create_user(&app, "admin", "password123", Role::Admin).await;
    let svc = app.backup();

    upload_media(&app, admin.id, "keep.png").await;
    let full = svc.create(BackupKind::Full, "admin").await.unwrap();
    let full_path = svc.storage_path_of(&full.name).unwrap();

    // Remove both the record and the bytes, then restore media only.
    let (media, _) = polaris::repositories::media::list(
        &app.db,
        &polaris::repositories::media::MediaFilter::default(),
    )
    .await
    .unwrap();
    polaris::services::media::delete(&app, &media[0], false)
        .await
        .unwrap();
    app.settings
        .set(&app.db, "site.title", "Title changed")
        .await
        .unwrap();

    let report = svc
        .restore(
            &app,
            &full_path,
            polaris::backup::restore::RestoreOptions {
                database: false,
                media: true,
                themes: false,
                plugins: false,
            },
        )
        .await
        .unwrap();
    assert!(!report.database_restored);
    assert_eq!(report.media_files, 1);
    // The live title was NOT rolled back (database untouched).
    assert_eq!(app.settings.get("site.title").unwrap(), "Title changed");

    // Media-only restore does not create a pre-restore snapshot by design
    // (the database is not touched), and the media is back.
    let (media, _) = polaris::repositories::media::list(
        &app.db,
        &polaris::repositories::media::MediaFilter::default(),
    )
    .await
    .unwrap();
    assert_eq!(media.len(), 1, "media record restored");
    assert!(
        app.media
            .storage()
            .exists(&media[0].storage_key)
            .await
            .unwrap()
    );

    // Database-only and full backups restore the database (snapshot made).
    let db_only = svc.create(BackupKind::Database, "admin").await.unwrap();
    let db_path = svc.storage_path_of(&db_only.name).unwrap();
    let report = svc
        .restore(
            &app,
            &db_path,
            polaris::backup::restore::RestoreOptions {
                database: true,
                media: false,
                themes: false,
                plugins: false,
            },
        )
        .await
        .unwrap();
    assert!(report.database_restored);
    assert!(report.snapshot.is_some());
    assert_eq!(report.media_files, 0);
    drop(dir);
}

// ---------------------------------------------------------------------------
// Retention & staging hygiene
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retention_keeps_newest_scheduled_backups() {
    let (app, _dir) = common::init_app().await;
    let svc = app.backup();
    for _ in 0..4 {
        svc.create(BackupKind::Database, "scheduler").await.unwrap();
    }
    svc.create(BackupKind::Database, "admin").await.unwrap(); // manual: never pruned

    let pruned = svc.apply_retention(2).await.unwrap();
    assert_eq!(pruned, 2);

    let list = svc.list().await.unwrap();
    let scheduled: Vec<_> = list
        .iter()
        .filter(|b| b.created_by == "scheduler")
        .collect();
    assert_eq!(scheduled.len(), 2, "newest two scheduled backups kept");
    let manual = list.iter().filter(|b| b.created_by == "admin").count();
    assert_eq!(manual, 1, "manual backups are never pruned by retention");

    // Stale staging leftovers are cleaned up.
    let stale = std::path::Path::new(&app.config.backup.tmp_dir).join("stale.zip.part");
    std::fs::write(&stale, b"junk").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    let f = std::fs::File::options().write(true).open(&stale).unwrap();
    f.set_modified(old).unwrap();
    drop(f);
    assert_eq!(svc.purge_stale_staging().await.unwrap(), 1);
    assert!(!stale.exists());
}

// ---------------------------------------------------------------------------
// Admin UI access control
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_backups_page_requires_admin_session() {
    let (app, router, _dir) = common::init_http().await;
    let _ = app;

    // Anonymous → redirected to the login page.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/backups")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(axum::http::header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("/admin/login")
    );

    // The restore page is equally protected.
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/admin/backups/whatever.zip/restore")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
}
