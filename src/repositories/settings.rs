use std::collections::HashMap;

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;

pub async fn all(db: &Db) -> AppResult<HashMap<String, String>> {
    let rows = db
        .fetch_all("SELECT name, value FROM settings", &[])
        .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for r in rows {
        let k: String = r.try_get("name").map_err(crate::error::AppError::Db)?;
        let v: String = crate::db::text(&r, "value").map_err(crate::error::AppError::Db)?;
        out.insert(k, v);
    }
    Ok(out)
}

pub async fn set(db: &Db, name: &str, value: &str) -> AppResult<()> {
    db.upsert_setting(name, value).await
}

pub async fn ensure_defaults(db: &Db, defaults: &[(&str, &str)]) -> AppResult<()> {
    let existing = all(db).await?;
    for (k, v) in defaults {
        if !existing.contains_key(*k) {
            set(db, k, v).await?;
        }
    }
    Ok(())
}

pub async fn get(db: &Db, name: &str) -> AppResult<Option<String>> {
    let row = db
        .fetch_optional(
            "SELECT value FROM settings WHERE name = ?",
            &[Bind::S(name.to_string())],
        )
        .await?;
    row.map(|r| crate::db::text(&r, "value"))
        .transpose()
        .map_err(crate::error::AppError::Db)
}
