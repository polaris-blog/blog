//! Database abstraction layer.
//!
//! ```text
//! HTTP → Service → Repository → Db (this module) → SQLx → SQLite / MySQL / PostgreSQL
//! ```
//!
//! Portability strategy:
//! - All timestamps are Unix epoch seconds stored as 64-bit integers.
//! - All IDs are 64-bit integers.
//! - Repository SQL is written with `?` placeholders; this module rewrites
//!   them to `$N` for PostgreSQL.
//! - Inserts use `last_insert_id()` on SQLite/MySQL and `RETURNING id` on
//!   PostgreSQL.
//! - DDL lives in per-dialect migration files under `migrations/`.

pub mod migrate;

use std::borrow::Cow;
use std::str::FromStr;
use std::sync::Once;
use std::time::Duration;

use sqlx::any::{AnyConnectOptions, AnyPoolOptions, AnyRow};
use sqlx::query::Query;
use sqlx::{AnyPool, Row};

use crate::config::DatabaseConfig;
use crate::error::AppResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    MySql,
    Postgres,
}

impl Dialect {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::MySql => "mysql",
            Self::Postgres => "postgres",
        }
    }

    /// Rewrite `?` placeholders to `$N` for PostgreSQL. Skips string literals.
    pub fn translate<'a>(self, sql: &'a str) -> Cow<'a, str> {
        if self != Dialect::Postgres {
            return Cow::Borrowed(sql);
        }
        let mut out = String::with_capacity(sql.len() + 8);
        let mut n = 0usize;
        let mut chars = sql.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' | '"' => {
                    let quote = c;
                    out.push(c);
                    while let Some(inner) = chars.next() {
                        out.push(inner);
                        if inner == quote {
                            // A doubled quote stays inside the literal.
                            if chars.peek() == Some(&quote) {
                                out.push(chars.next().unwrap());
                            } else {
                                break;
                            }
                        }
                        if inner == '\n' {
                            break; // unterminated literal — bail out safely
                        }
                    }
                }
                '?' => {
                    n += 1;
                    out.push_str(&format!("${n}"));
                }
                _ => out.push(c),
            }
        }
        Cow::Owned(out)
    }
}

#[derive(Clone)]
pub struct Db {
    pool: AnyPool,
    dialect: Dialect,
}

/// Bind values for parameterized queries.
#[derive(Clone, Debug)]
pub enum Bind {
    S(String),
    OptS(Option<String>),
    I(i64),
    OptI(Option<i64>),
}

pub type AnyQuery<'a> = Query<'a, sqlx::Any, sqlx::any::AnyArguments<'a>>;

/// SQLx Any maps MySQL TEXT/MEDIUMTEXT/LONGTEXT to Blob. Decode these
/// explicitly as UTF-8 at textual field boundaries; never replace invalid
/// bytes or silently turn failed decoding into an empty string.
pub fn optional_text(row: &AnyRow, column: &str) -> Result<Option<String>, sqlx::Error> {
    match row.try_get::<Option<String>, _>(column) {
        Ok(value) => Ok(value),
        Err(original) => match row.try_get::<Option<Vec<u8>>, _>(column) {
            Ok(value) => value.map(String::from_utf8).transpose().map_err(|error| {
                sqlx::Error::ColumnDecode {
                    index: column.into(),
                    source: Box::new(error),
                }
            }),
            Err(_) => Err(original),
        },
    }
}

pub fn text(row: &AnyRow, column: &str) -> Result<String, sqlx::Error> {
    optional_text(row, column)?.ok_or_else(|| sqlx::Error::ColumnDecode {
        index: column.into(),
        source: Box::new(sqlx::error::UnexpectedNullError),
    })
}

pub fn bind_all<'a>(mut q: AnyQuery<'a>, binds: &[Bind]) -> AnyQuery<'a> {
    for b in binds {
        q = match b {
            Bind::S(v) => q.bind(v.clone()),
            Bind::OptS(v) => q.bind(v.clone()),
            Bind::I(v) => q.bind(*v),
            Bind::OptI(v) => q.bind(*v),
        };
    }
    q
}

impl Db {
    pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<Self> {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(sqlx::any::install_default_drivers);

        let dialect = match cfg.driver_normalized() {
            "mysql" => Dialect::MySql,
            "postgres" => Dialect::Postgres,
            _ => Dialect::Sqlite,
        };
        let url = match dialect {
            Dialect::Sqlite => {
                ensure_sqlite_parent_dir(&cfg.url);
                normalize_sqlite_url(&cfg.url)
            }
            _ => cfg.url.clone(),
        };
        let opts = AnyConnectOptions::from_str(&url)?;

        let mut pool_opts = AnyPoolOptions::new()
            .max_connections(cfg.max_connections.max(1))
            .min_connections(cfg.min_connections)
            .acquire_timeout(Duration::from_secs(8));
        if dialect == Dialect::Sqlite {
            // WAL + sane per-connection defaults (the Any driver cannot carry
            // driver-specific connect options, so pragmas run after connect).
            pool_opts = pool_opts.after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("PRAGMA journal_mode = WAL")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA busy_timeout = 5000")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            });
        }
        let pool = pool_opts.connect_with(opts).await?;
        Ok(Self { pool, dialect })
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    pub fn is_pg(&self) -> bool {
        self.dialect == Dialect::Postgres
    }

    /// Rewrite `?` placeholders for the active dialect.
    pub fn translate<'a>(&self, sql: &'a str) -> Cow<'a, str> {
        self.dialect.translate(sql)
    }

    /// Execute an INSERT and return the new row id.
    ///
    /// PostgreSQL and SQLite use `RETURNING id` (the sqlx Any driver reports
    /// `last_insert_id = None` for both); MySQL has no `RETURNING` but the
    /// Any driver does surface `LAST_INSERT_ID()`.
    pub async fn insert(&self, sql: &str, binds: &[Bind]) -> AppResult<i64> {
        match self.dialect {
            Dialect::Postgres | Dialect::Sqlite => {
                let full = format!("{} RETURNING id", self.translate(sql));
                let q = bind_all(sqlx::query(full.as_str()), binds);
                let row = q.fetch_one(&self.pool).await?;
                Ok(row.try_get::<i64, _>("id")?)
            }
            Dialect::MySql => {
                let t = self.translate(sql);
                let q = bind_all(sqlx::query(t.as_ref()), binds);
                let res = q.execute(&self.pool).await?;
                res.last_insert_id().ok_or_else(|| {
                    crate::error::AppError::Internal(anyhow::anyhow!(
                        "driver returned no insert id"
                    ))
                })
            }
        }
    }

    /// Execute a statement, returning rows affected.
    pub async fn execute(&self, sql: &str, binds: &[Bind]) -> AppResult<u64> {
        let t = self.translate(sql);
        let q = bind_all(sqlx::query(t.as_ref()), binds);
        let res = q.execute(&self.pool).await?;
        Ok(res.rows_affected())
    }

    pub async fn fetch_optional(&self, sql: &str, binds: &[Bind]) -> AppResult<Option<AnyRow>> {
        let t = self.translate(sql);
        let q = bind_all(sqlx::query(t.as_ref()), binds);
        Ok(q.fetch_optional(&self.pool).await?)
    }

    pub async fn fetch_all(&self, sql: &str, binds: &[Bind]) -> AppResult<Vec<AnyRow>> {
        let t = self.translate(sql);
        let q = bind_all(sqlx::query(t.as_ref()), binds);
        Ok(q.fetch_all(&self.pool).await?)
    }

    pub async fn fetch_one(&self, sql: &str, binds: &[Bind]) -> AppResult<AnyRow> {
        let t = self.translate(sql);
        let q = bind_all(sqlx::query(t.as_ref()), binds);
        Ok(q.fetch_one(&self.pool).await?)
    }

    /// Portable UPSERT for the settings table: UPDATE first, INSERT if missing.
    pub async fn upsert_setting(&self, name: &str, value: &str) -> AppResult<()> {
        let updated = self
            .execute(
                "UPDATE settings SET value = ? WHERE name = ?",
                &[Bind::S(value.to_string()), Bind::S(name.to_string())],
            )
            .await?;
        if updated == 0 {
            self.execute(
                "INSERT INTO settings (name, value) VALUES (?, ?)",
                &[Bind::S(name.to_string()), Bind::S(value.to_string())],
            )
            .await?;
        }
        Ok(())
    }
}

/// Ensure the SQLite URL carries the `sqlite:` scheme and `mode=rwc`
/// (create the database file when missing).
///
/// The Any driver round-trips connection URLs through the WHATWG URL parser,
/// so the shape matters:
/// - Backslashes are normalized to forward slashes (accepted by SQLite).
/// - Windows drive-letter paths must use the opaque form (`sqlite:C:/x`);
///   with the authority form (`sqlite://C:/x`) the drive letter is parsed
///   as a hostname and the connection fails with "unable to open database
///   file".
fn normalize_sqlite_url(url: &str) -> String {
    let url = url.replace('\\', "/");
    let path = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(&url);
    let is_drive_path =
        path.len() >= 2 && path.as_bytes()[1] == b':' && path.as_bytes()[0].is_ascii_alphabetic();
    let base = if is_drive_path {
        format!("sqlite:{path}")
    } else {
        format!("sqlite://{path}")
    };
    if base.contains("mode=") {
        base
    } else if base.contains('?') {
        format!("{base}&mode=rwc")
    } else {
        format!("{base}?mode=rwc")
    }
}

/// SQLite cannot create the database file when its parent directory is
/// missing, so create it (best effort) before connecting.
fn ensure_sqlite_parent_dir(raw: &str) {
    let cleaned = raw.replace('\\', "/");
    let path = cleaned
        .strip_prefix("sqlite://")
        .or_else(|| cleaned.strip_prefix("sqlite:"))
        .unwrap_or(&cleaned)
        .split('?')
        .next()
        .unwrap_or(&cleaned);
    if path.is_empty() || path == ":memory:" {
        return;
    }
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(dir = %parent.display(), error = %e, "cannot create database directory");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_placeholders() {
        let d = Dialect::Postgres;
        assert_eq!(
            d.translate("SELECT * FROM t WHERE a = ? AND b = ?"),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
        // Placeholders inside string literals must not be rewritten.
        assert_eq!(
            d.translate("SELECT 'a?b' || c FROM t WHERE d = ?"),
            "SELECT 'a?b' || c FROM t WHERE d = $1"
        );
        assert_eq!(
            d.translate("SELECT 'it''s ?' FROM t WHERE d = ?"),
            "SELECT 'it''s ?' FROM t WHERE d = $1"
        );
    }

    #[test]
    fn no_rewrite_for_other_dialects() {
        assert_eq!(Dialect::Sqlite.translate("WHERE a = ?"), "WHERE a = ?");
        assert_eq!(Dialect::MySql.translate("WHERE a = ?"), "WHERE a = ?");
    }

    #[test]
    fn sqlite_url_normalization() {
        // Relative path: scheme + create mode appended.
        assert_eq!(
            normalize_sqlite_url("data/polaris.db"),
            "sqlite://data/polaris.db?mode=rwc"
        );
        // Already-schemed URL: unchanged except mode.
        assert_eq!(
            normalize_sqlite_url("sqlite://data/polaris.db"),
            "sqlite://data/polaris.db?mode=rwc"
        );
        // Windows absolute path: backslashes become forward slashes and the
        // opaque `sqlite:` form keeps the drive letter out of the URL host.
        assert_eq!(
            normalize_sqlite_url(r"D:\blog\data\polaris.db"),
            "sqlite:D:/blog/data/polaris.db?mode=rwc"
        );
        // Existing mode parameter is respected.
        assert_eq!(
            normalize_sqlite_url("sqlite://x.db?mode=ro"),
            "sqlite://x.db?mode=ro"
        );
    }
}
