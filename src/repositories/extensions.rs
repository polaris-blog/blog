//! Extension registry: installed theme/plugin packages, the install log
//! and per-plugin migration history. Extension *files* live on disk under
//! `themes/` / `plugins/`; this layer tracks metadata only (version, package
//! hash, declared permissions, audit trail).

use std::collections::HashSet;

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::utils::time;

#[derive(Clone, Debug)]
pub struct ExtensionRecord {
    pub id: i64,
    pub ext_id: String,
    /// "theme" | "plugin"
    pub kind: String,
    pub version: String,
    pub package_hash: String,
    pub permissions: Vec<String>,
    pub installed_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct ExtensionLogEntry {
    pub id: i64,
    pub ext_id: String,
    pub kind: String,
    pub action: String,
    pub version: String,
    pub actor: String,
    pub result: String,
    pub detail: String,
    pub created_at: i64,
}

fn row_to_record(row: &sqlx::any::AnyRow) -> AppResult<ExtensionRecord> {
    let permissions_raw: String = crate::db::text(row, "permissions")?;
    let permissions = serde_json::from_str::<Vec<String>>(&permissions_raw)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    Ok(ExtensionRecord {
        id: row.try_get("id")?,
        ext_id: row.try_get("ext_id")?,
        kind: row.try_get("kind")?,
        version: row.try_get("version")?,
        package_hash: row.try_get("package_hash")?,
        permissions,
        installed_at: row.try_get("installed_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Insert or update the registry row for one extension. The version/hash are
/// the freshly installed ones; `first_install` keeps the original
/// `installed_at` on updates.
pub async fn upsert(
    db: &Db,
    kind: &str,
    ext_id: &str,
    version: &str,
    package_hash: &str,
    permissions: &[String],
) -> AppResult<()> {
    let now = time::now();
    let perms = serde_json::to_string(permissions).unwrap_or_else(|_| "[]".into());
    let existing = find(db, kind, ext_id).await?;
    match existing {
        Some(rec) => {
            db.execute(
                "UPDATE extensions SET version = ?, package_hash = ?, permissions = ?, \
                 updated_at = ? WHERE id = ?",
                &[
                    Bind::S(version.to_string()),
                    Bind::S(package_hash.to_string()),
                    Bind::S(perms),
                    Bind::I(now),
                    Bind::I(rec.id),
                ],
            )
            .await?;
        }
        None => {
            db.execute(
                "INSERT INTO extensions (ext_id, kind, version, package_hash, permissions, \
                 installed_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                &[
                    Bind::S(ext_id.to_string()),
                    Bind::S(kind.to_string()),
                    Bind::S(version.to_string()),
                    Bind::S(package_hash.to_string()),
                    Bind::S(perms),
                    Bind::I(now),
                    Bind::I(now),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn find(db: &Db, kind: &str, ext_id: &str) -> AppResult<Option<ExtensionRecord>> {
    let row = db
        .fetch_optional(
            "SELECT id, ext_id, kind, version, package_hash, permissions, installed_at, \
             updated_at FROM extensions WHERE kind = ? AND ext_id = ?",
            &[Bind::S(kind.to_string()), Bind::S(ext_id.to_string())],
        )
        .await?;
    match row {
        Some(r) => Ok(Some(row_to_record(&r)?)),
        None => Ok(None),
    }
}

pub async fn list(db: &Db) -> AppResult<Vec<ExtensionRecord>> {
    let rows = db
        .fetch_all(
            "SELECT id, ext_id, kind, version, package_hash, permissions, installed_at, \
             updated_at FROM extensions ORDER BY kind, ext_id",
            &[],
        )
        .await?;
    rows.iter().map(row_to_record).collect()
}

pub async fn delete(db: &Db, kind: &str, ext_id: &str) -> AppResult<()> {
    db.execute(
        "DELETE FROM extensions WHERE kind = ? AND ext_id = ?",
        &[Bind::S(kind.to_string()), Bind::S(ext_id.to_string())],
    )
    .await?;
    Ok(())
}

/// Record one install/update/uninstall/rollback event (success or failure).
#[allow(clippy::too_many_arguments)]
pub async fn log(
    db: &Db,
    kind: &str,
    ext_id: &str,
    action: &str,
    version: &str,
    actor: &str,
    result: &str,
    detail: &str,
) -> AppResult<()> {
    db.execute(
        "INSERT INTO extension_logs (ext_id, kind, action, version, actor, result, detail, \
         created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Bind::S(ext_id.to_string()),
            Bind::S(kind.to_string()),
            Bind::S(action.to_string()),
            Bind::S(version.to_string()),
            Bind::S(actor.to_string()),
            Bind::S(result.to_string()),
            Bind::S(detail.to_string()),
            Bind::I(time::now()),
        ],
    )
    .await?;
    Ok(())
}

/// Most recent log entries, optionally scoped to one kind ("theme"/"plugin").
pub async fn logs(db: &Db, kind: Option<&str>, limit: i64) -> AppResult<Vec<ExtensionLogEntry>> {
    let limit = limit.clamp(1, 200);
    let sql = match kind {
        Some(_) => {
            "SELECT id, ext_id, kind, action, version, actor, result, detail, created_at \
             FROM extension_logs WHERE kind = ? ORDER BY id DESC LIMIT ?"
        }
        None => {
            "SELECT id, ext_id, kind, action, version, actor, result, detail, created_at \
             FROM extension_logs ORDER BY id DESC LIMIT ?"
        }
    };
    let binds = match kind {
        Some(k) => vec![Bind::S(k.to_string()), Bind::I(limit)],
        None => vec![Bind::I(limit)],
    };
    let rows = db.fetch_all(sql, &binds).await?;
    Ok(rows
        .iter()
        .map(|row| ExtensionLogEntry {
            id: row.try_get("id").unwrap_or_default(),
            ext_id: row.try_get("ext_id").unwrap_or_default(),
            kind: row.try_get("kind").unwrap_or_default(),
            action: row.try_get("action").unwrap_or_default(),
            version: row.try_get("version").unwrap_or_default(),
            actor: row.try_get("actor").unwrap_or_default(),
            result: row.try_get("result").unwrap_or_default(),
            detail: crate::db::text(row, "detail").unwrap_or_default(),
            created_at: row.try_get("created_at").unwrap_or_default(),
        })
        .collect())
}

// -- plugin migration history -----------------------------------------------

/// Names of migrations already applied for one plugin.
pub async fn migrations_applied(db: &Db, ext_id: &str) -> AppResult<HashSet<String>> {
    let rows = db
        .fetch_all(
            "SELECT name FROM extension_migrations WHERE ext_id = ?",
            &[Bind::S(ext_id.to_string())],
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("name").ok())
        .collect())
}

pub async fn migration_record(db: &Db, ext_id: &str, name: &str) -> AppResult<()> {
    db.execute(
        "INSERT INTO extension_migrations (ext_id, name, applied_at) VALUES (?, ?, ?)",
        &[
            Bind::S(ext_id.to_string()),
            Bind::S(name.to_string()),
            Bind::I(time::now()),
        ],
    )
    .await?;
    Ok(())
}

pub async fn migration_record_in_transaction(
    db: &Db,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    ext_id: &str,
    name: &str,
) -> AppResult<()> {
    let sql = db
        .dialect()
        .translate("INSERT INTO extension_migrations (ext_id, name, applied_at) VALUES (?, ?, ?)");
    sqlx::query(sql.as_ref())
        .bind(ext_id)
        .bind(name)
        .bind(time::now())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Drop the migration history (used by "remove plugin + data").
pub async fn migrations_clear(db: &Db, ext_id: &str) -> AppResult<()> {
    db.execute(
        "DELETE FROM extension_migrations WHERE ext_id = ?",
        &[Bind::S(ext_id.to_string())],
    )
    .await?;
    Ok(())
}
