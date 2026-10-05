//! Shared contracts against SQLite locally, or an EMPTY dedicated server
//! database selected by POLARIS_TEST_DATABASE_{DRIVER,URL} in CI.
mod common;

use polaris::{
    backup::{BackupKind, restore::RestoreOptions},
    db::{Db, Dialect, migrate},
    models::{PostStatus, Role},
    repositories::{jobs::SqlJobRepository, settings, users},
    scheduler::{JobRequest, JobStatus, repository::JobRepository},
    search::SearchQuery,
    services::posts::{self, PostInput},
    state::AppState,
};
use sqlx::Row;

#[tokio::test]
async fn database_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::test_config(dir.path());
    match (
        std::env::var("POLARIS_TEST_DATABASE_DRIVER"),
        std::env::var("POLARIS_TEST_DATABASE_URL"),
    ) {
        (Ok(driver), Ok(url)) => {
            assert!(matches!(driver.as_str(), "mysql" | "postgres"));
            cfg.database.driver = driver;
            cfg.database.url = url;
        }
        (Err(_), Err(_)) => {}
        _ => panic!("set both POLARIS_TEST_DATABASE_DRIVER and POLARIS_TEST_DATABASE_URL"),
    }
    cfg.security.secret.clear(); // Exercise persisted-key restore on every dialect.
    let db = Db::connect(&cfg.database)
        .await
        .expect("connect to test database");
    let tables_sql = match db.dialect() {
        Dialect::Sqlite => {
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
        }
        Dialect::MySql => {
            "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema = DATABASE()"
        }
        Dialect::Postgres => {
            "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema = current_schema()"
        }
    };
    assert_eq!(
        db.fetch_one(tables_sql, &[])
            .await
            .unwrap()
            .try_get::<i64, _>("n")
            .unwrap(),
        0,
        "refusing to modify a nonempty database; use a fresh dedicated test database"
    );
    assert_eq!(migrate::run(&db).await.unwrap(), vec![1, 2, 3, 4, 5]);
    assert!(migrate::run(&db).await.unwrap().is_empty());
    let app = AppState::init(cfg).await.unwrap();
    let first = common::create_user(&app, "first", "password123", Role::Admin).await;
    let second = common::create_user(&app, "second", "password123", Role::Admin).await;

    settings::set(&db, "matrix.rollback", "before")
        .await
        .unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE settings SET value = 'after' WHERE name = 'matrix.rollback'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        settings::get(&db, "matrix.rollback")
            .await
            .unwrap()
            .as_deref(),
        Some("before")
    );

    let published = posts::create_post(
        &app,
        first.id,
        PostInput {
            title: "Quasar journal".into(),
            content_md: "quasar observatory".into(),
            status: PostStatus::Published,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    posts::create_post(
        &app,
        first.id,
        PostInput {
            title: "Quasar private draft".into(),
            content_md: "quasar unpublished".into(),
            status: PostStatus::Draft,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let found = app
        .search
        .search(
            &app,
            SearchQuery {
                query: "quasar".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.total, 1);
    assert_eq!(found.results[0].id, published.id);

    let utf8 = "多语言正文 — café 🦀";
    settings::set(&db, "matrix.unicode", utf8).await.unwrap();
    assert_eq!(
        settings::get(&db, "matrix.unicode")
            .await
            .unwrap()
            .as_deref(),
        Some(utf8)
    );
    let page = posts::create_page(
        &app,
        first.id,
        posts::PageInput {
            title: "Matrix page".into(),
            content_md: utf8.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.content_md, utf8);
    let comment = polaris::services::comments::create(
        &app,
        polaris::services::comments::NewComment {
            post_id: published.id,
            parent_id: None,
            author_name: "reader".into(),
            author_email: String::new(),
            author_url: String::new(),
            content: utf8.into(),
        },
        false,
    )
    .await
    .unwrap();
    assert_eq!(comment.content, utf8);
    let media_id = polaris::repositories::media::insert(
        &db,
        &polaris::repositories::media::NewMedia {
            uuid: "matrix-media".into(),
            filename: "matrix.png".into(),
            original_filename: "matrix.png".into(),
            storage_key: "matrix.png".into(),
            mime_type: "image/png".into(),
            extension: "png".into(),
            size: 1,
            width: None,
            height: None,
            duration: None,
            hash: "matrix-hash".into(),
            thumbnails: Vec::new(),
            folder_id: None,
            uploaded_by: first.id,
        },
    )
    .await
    .unwrap();
    db.execute(
        "UPDATE media SET description = ?, alt = ?, caption = ? WHERE id = ?",
        &[
            polaris::db::Bind::S(utf8.into()),
            polaris::db::Bind::S(utf8.into()),
            polaris::db::Bind::S(utf8.into()),
            polaris::db::Bind::I(media_id),
        ],
    )
    .await
    .unwrap();
    let media = polaris::repositories::media::find_by_id(&db, media_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            media.description.as_str(),
            media.alt.as_str(),
            media.caption.as_str()
        ),
        (utf8, utf8, utf8)
    );
    polaris::repositories::extensions::upsert(
        &db,
        "plugin",
        "matrix",
        "1.0.0",
        "",
        &["cache".into()],
    )
    .await
    .unwrap();
    assert_eq!(
        polaris::repositories::extensions::find(&db, "plugin", "matrix")
            .await
            .unwrap()
            .unwrap()
            .permissions,
        vec!["cache"]
    );

    let schema = polaris::config_schema::parse_schema("[api_key]\ntype = 'password'\n").unwrap();
    app.configs
        .load_namespace("plugin.matrix", schema.clone(), Default::default())
        .await
        .unwrap();
    app.configs
        .save(
            "plugin.matrix",
            &std::collections::HashMap::from([("api_key".into(), utf8.into())]),
            polaris::config_schema::Permission::Admin,
        )
        .await
        .unwrap();
    app.configs
        .load_namespace("plugin.matrix", schema, Default::default())
        .await
        .unwrap();
    assert_eq!(
        app.configs
            .get_string("plugin.matrix", "api_key")
            .as_deref(),
        Some(utf8)
    );

    let (a, b) = tokio::join!(
        users::update_role(&db, first.id, Role::Author),
        users::update_role(&db, second.id, Role::Author)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        users::list(&db)
            .await
            .unwrap()
            .iter()
            .filter(|u| u.role == Role::Admin)
            .count(),
        1
    );

    let job = app
        .scheduler
        .create(JobRequest {
            name: "concurrent claim".into(),
            job_type: "cleanup".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let repo = SqlJobRepository::new(db.clone());
    let mut claim_a = job.clone();
    claim_a.status = JobStatus::Running;
    claim_a.locked_by = Some("worker-a".into());
    let mut claim_b = claim_a.clone();
    claim_b.locked_by = Some("worker-b".into());
    let (a, b) = tokio::join!(repo.save(claim_a, None), repo.save(claim_b, None));
    assert_ne!(
        a.unwrap(),
        b.unwrap(),
        "one version can be claimed only once"
    );
    assert_eq!(repo.get(&job.id).await.unwrap().version, job.version + 1);
    let current = repo.get(&job.id).await.unwrap();
    let mut changed = current.clone();
    changed.status = JobStatus::Success;
    let missing = polaris::scheduler::job::Attempt {
        id: "missing-attempt".into(),
        job_id: job.id.clone(),
        run_key: job.run_key.clone(),
        status: JobStatus::Failed,
        started_at: 1,
        finished_at: Some(2),
        error: None,
    };
    assert!(repo.save(changed, Some(missing)).await.is_err());
    assert_eq!(repo.get(&job.id).await.unwrap().version, current.version);

    let key = app.settings.get("security.secret").unwrap();
    let backup = app
        .backup()
        .create(BackupKind::Database, "matrix")
        .await
        .unwrap();
    let path = app.backup().storage_path_of(&backup.name).unwrap();
    posts::delete_post(&app, published.id).await.unwrap();
    app.backup()
        .restore(
            &app,
            &path,
            RestoreOptions {
                database: true,
                media: false,
                themes: false,
                plugins: false,
            },
        )
        .await
        .unwrap();
    assert!(
        polaris::repositories::posts::find_by_id(&db, published.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        settings::get(&db, "security.secret")
            .await
            .unwrap()
            .as_deref(),
        Some(key.as_str())
    );
    let after = posts::create_post(
        &app,
        first.id,
        PostInput {
            title: "After restore".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        after.id > published.id,
        "restored sequences must permit new inserts"
    );
}
