//! Database layer: connection, migrations, settings upsert.

mod common;

use polaris::db::{Db, migrate};
use polaris::repositories::settings;
use sqlx::Row;

#[tokio::test]
async fn migrations_apply_once_and_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::test_config(dir.path());
    let db = Db::connect(&cfg.database).await.expect("connect");

    let applied = migrate::run(&db).await.expect("migrate");
    assert_eq!(applied, vec![1, 2, 3, 4, 5]);

    // Re-running applies nothing new.
    let again = migrate::run(&db).await.expect("migrate again");
    assert!(again.is_empty());

    // Version tracking table holds one row per migration.
    let row = db
        .fetch_one("SELECT COUNT(*) AS n FROM schema_migrations", &[])
        .await
        .unwrap();
    assert_eq!(row.try_get::<i64, _>("n").unwrap(), 5);

    // Core tables exist and are empty.
    for table in [
        "users",
        "posts",
        "pages",
        "terms",
        "comments",
        "settings",
        "search_index",
        "media",
        "media_folders",
        "media_tags",
        "media_references",
        "extensions",
        "extension_logs",
        "extension_migrations",
        "scheduler_jobs",
        "scheduler_attempts",
    ] {
        let row = db
            .fetch_one(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])
            .await
            .unwrap_or_else(|e| panic!("table {table} missing: {e}"));
        assert_eq!(row.try_get::<i64, _>("n").unwrap(), 0);
    }
}

#[tokio::test]
async fn settings_upsert_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::test_config(dir.path());
    let db = Db::connect(&cfg.database).await.unwrap();
    migrate::run(&db).await.unwrap();

    // Insert.
    db.upsert_setting("site.title", "First").await.unwrap();
    assert_eq!(
        settings::get(&db, "site.title").await.unwrap().as_deref(),
        Some("First")
    );
    // Update (portable UPDATE-then-INSERT upsert).
    db.upsert_setting("site.title", "Second").await.unwrap();
    assert_eq!(
        settings::get(&db, "site.title").await.unwrap().as_deref(),
        Some("Second")
    );
    // all() sees the final value.
    let all = settings::all(&db).await.unwrap();
    assert_eq!(all.get("site.title").map(String::as_str), Some("Second"));

    // ensure_defaults does not clobber existing values.
    settings::ensure_defaults(&db, &[("site.title", "default")])
        .await
        .unwrap();
    assert_eq!(
        settings::get(&db, "site.title").await.unwrap().as_deref(),
        Some("Second")
    );
    // ...but fills in missing ones.
    settings::ensure_defaults(&db, &[("site.lang", "en")])
        .await
        .unwrap();
    assert_eq!(
        settings::get(&db, "site.lang").await.unwrap().as_deref(),
        Some("en")
    );
}

#[tokio::test]
async fn parameterized_queries_reject_bad_binds_gracefully() {
    // Binds are always parameterized — a value that looks like SQL is stored
    // verbatim and never executed.
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::test_config(dir.path());
    let db = Db::connect(&cfg.database).await.unwrap();
    migrate::run(&db).await.unwrap();

    db.upsert_setting("evil", "x'); DROP TABLE settings; --")
        .await
        .unwrap();
    let val = settings::get(&db, "evil").await.unwrap();
    assert_eq!(
        val.as_deref(),
        Some("x'); DROP TABLE settings; --"),
        "value must be stored literally"
    );
    // The table survived.
    let row = db
        .fetch_one("SELECT COUNT(*) AS n FROM settings", &[])
        .await
        .unwrap();
    assert!(row.try_get::<i64, _>("n").unwrap() >= 1);
}

#[tokio::test]
async fn portable_text_decoding_preserves_utf8_and_rejects_invalid_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::test_config(dir.path());
    let db = Db::connect(&cfg.database).await.unwrap();
    let row = db.fetch_one(
        "SELECT CAST('多语言 café 🦀' AS BLOB) AS bytes, 'plain' AS text_value, NULL AS absent, X'FF' AS invalid, 42 AS number",
        &[],
    ).await.unwrap();
    assert_eq!(polaris::db::text(&row, "bytes").unwrap(), "多语言 café 🦀");
    assert_eq!(polaris::db::text(&row, "text_value").unwrap(), "plain");
    assert_eq!(polaris::db::optional_text(&row, "absent").unwrap(), None);
    assert!(polaris::db::text(&row, "absent").is_err());
    assert!(polaris::db::text(&row, "invalid").is_err());
    assert!(polaris::db::text(&row, "number").is_err());
}
