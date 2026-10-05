//! Embedded, dialect-specific schema migrations.
//!
//! Migrations are plain SQL files under `migrations/<dialect>/NNNN_name.sql`,
//! embedded into the binary so a single `polaris` file can bootstrap any
//! database. Applied versions are tracked in `schema_migrations`.

use std::collections::HashSet;

use sqlx::Row;

use super::{Db, Dialect};
use crate::utils::time;

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

static SQLITE: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/sqlite/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "search",
        sql: include_str!("../../migrations/sqlite/0002_search.sql"),
    },
    Migration {
        version: 3,
        name: "media",
        sql: include_str!("../../migrations/sqlite/0003_media.sql"),
    },
    Migration {
        version: 4,
        name: "extensions",
        sql: include_str!("../../migrations/sqlite/0004_extensions.sql"),
    },
    Migration {
        version: 5,
        name: "scheduler",
        sql: include_str!("../../migrations/sqlite/0005_scheduler.sql"),
    },
];

static MYSQL: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/mysql/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "search",
        sql: include_str!("../../migrations/mysql/0002_search.sql"),
    },
    Migration {
        version: 3,
        name: "media",
        sql: include_str!("../../migrations/mysql/0003_media.sql"),
    },
    Migration {
        version: 4,
        name: "extensions",
        sql: include_str!("../../migrations/mysql/0004_extensions.sql"),
    },
    Migration {
        version: 5,
        name: "scheduler",
        sql: include_str!("../../migrations/mysql/0005_scheduler.sql"),
    },
];

static POSTGRES: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/postgres/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "search",
        sql: include_str!("../../migrations/postgres/0002_search.sql"),
    },
    Migration {
        version: 3,
        name: "media",
        sql: include_str!("../../migrations/postgres/0003_media.sql"),
    },
    Migration {
        version: 4,
        name: "extensions",
        sql: include_str!("../../migrations/postgres/0004_extensions.sql"),
    },
    Migration {
        version: 5,
        name: "scheduler",
        sql: include_str!("../../migrations/postgres/0005_scheduler.sql"),
    },
];

pub fn migrations_for(dialect: Dialect) -> &'static [Migration] {
    match dialect {
        Dialect::Sqlite => SQLITE,
        Dialect::MySql => MYSQL,
        Dialect::Postgres => POSTGRES,
    }
}

/// Run all pending migrations. Returns the versions applied in this call.
pub async fn run(db: &Db) -> crate::error::AppResult<Vec<i64>> {
    let dialect = db.dialect();
    // `INTEGER` and `BIGINT` are the same type in SQLite.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
         version BIGINT PRIMARY KEY, name TEXT NOT NULL, applied_at BIGINT NOT NULL)",
    )
    .execute(db.pool())
    .await?;

    let mut applied: HashSet<i64> = HashSet::new();
    for row in sqlx::query("SELECT version FROM schema_migrations")
        .fetch_all(db.pool())
        .await?
    {
        applied.insert(row.try_get::<i64, _>("version")?);
    }

    let mut ran = Vec::new();
    for m in migrations_for(dialect) {
        if applied.contains(&m.version) {
            continue;
        }
        let mut tx = db.pool().begin().await?;
        // Note: MySQL DDL implicitly commits; each statement is applied
        // atomically in practice and the version row is recorded after.
        for stmt in split_statements(m.sql) {
            sqlx::query(&stmt).execute(&mut *tx).await?;
        }
        let sql = dialect.translate(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
        );
        sqlx::query(sql.as_ref())
            .bind(m.version)
            .bind(m.name)
            .bind(time::now())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(
            version = m.version,
            name = m.name,
            dialect = dialect.name(),
            "migration applied"
        );
        ran.push(m.version);
    }
    Ok(ran)
}

/// Split a SQL script into statements on `;`, respecting string literals and
/// `--` line comments.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str: Option<char> = None;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match in_str {
            Some(q) => {
                cur.push(c);
                if c == q {
                    in_str = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    cur.push(c);
                    in_str = Some(c);
                }
                '-' if chars.peek() == Some(&'-') => {
                    while let Some(&c2) = chars.peek() {
                        chars.next();
                        if c2 == '\n' {
                            cur.push('\n');
                            break;
                        }
                    }
                }
                ';' => {
                    let t = cur.trim().to_string();
                    if !t.is_empty() {
                        out.push(t);
                    }
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() {
        out.push(t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        let stmts = split_statements(
            "CREATE TABLE a (x INT);\n-- comment; with semicolon\nINSERT INTO a VALUES (1);\nINSERT INTO a VALUES ('a;b');",
        );
        assert_eq!(stmts.len(), 3);
        assert!(stmts[1].starts_with("INSERT"));
        assert!(stmts[2].contains("'a;b'"));
    }
}
