//! Portable database dump format.
//!
//! ```text
//! database.dump (newline-delimited JSON, UTF-8):
//!   {"format":"polaris-dump","version":1,"dialect":"sqlite","app_version":"0.1.0","created_at":...}
//!   {"table":"users","cols":["id","username",…],"rows":[[1,"admin",…],…]}
//!   {"table":"users","cols":[…],"rows":[[…]]}      ← continuation chunk
//!   …
//! ```
//!
//! Why not raw SQL? The dump must replay on all three dialects. Polaris
//! keeps every timestamp as an epoch integer and every id as an i64, so a
//! row is exactly `i64 | string | null` — which JSON expresses without any
//! escaping pitfalls. Restore binds values back through the portable `Db`
//! layer (placeholders translated per dialect), so a dump taken from SQLite
//! replays on MySQL and PostgreSQL unchanged.
//!
//! Rows stream in bounded chunks (never a whole table in memory), and the
//! restore side replays them in FK-safe table order inside one transaction.

use std::io::{BufRead, Write};

use serde_json::json;
use sqlx::Row;

use crate::db::{Bind, Dialect};
use crate::error::{AppError, AppResult};

/// Dump format version (`version` field of the header line).
pub const DUMP_VERSION: i64 = 1;
/// Marker string of the header line.
pub const DUMP_FORMAT: &str = "polaris-dump";
/// Rows per JSON chunk / per multi-row INSERT statement.
pub const CHUNK_ROWS: usize = 250;

/// One core table with its column order. The order of [`TABLES`] is the
/// FK-safe *insert* order (parents first); [`DELETE_ORDER`] is the reverse.
pub struct TableSpec {
    pub name: &'static str,
    pub cols: &'static [&'static str],
    /// Column for keyset pagination (`WHERE <col> > ? ORDER BY <col>`).
    /// `None` → the table has no single integer PK (composite-PK tables are
    /// small and bounded by content, so they are read whole).
    pub paginate: Option<&'static str>,
    /// The table's PK is a BIGSERIAL on PostgreSQL → sequence must be
    /// re-synced after restoring explicit ids.
    pub pg_sequence: bool,
}

/// Sensitive settings are never written to a backup in plaintext or at all:
/// `security.secret` is excluded (restore keeps the *current* instance
/// secret; everything else in `settings` that is sensitive was already
/// stored AES-GCM-encrypted by the config system).
pub const EXCLUDED_SETTINGS: &[&str] = &["security.secret"];

pub const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "settings",
        cols: &["name", "value"],
        paginate: None,
        pg_sequence: false,
    },
    TableSpec {
        name: "users",
        cols: &[
            "id",
            "username",
            "email",
            "password_hash",
            "role",
            "display_name",
            "bio",
            "created_at",
            "updated_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "media_folders",
        cols: &["id", "name", "slug", "created_at"],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "posts",
        cols: &[
            "id",
            "title",
            "slug",
            "summary",
            "content_md",
            "author_id",
            "status",
            "featured_image",
            "published_at",
            "created_at",
            "updated_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "pages",
        cols: &[
            "id",
            "title",
            "slug",
            "summary",
            "content_md",
            "author_id",
            "status",
            "sort_order",
            "created_at",
            "updated_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "terms",
        cols: &["id", "kind", "name", "slug"],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "post_terms",
        cols: &["post_id", "term_id"],
        paginate: None,
        pg_sequence: false,
    },
    TableSpec {
        name: "comments",
        cols: &[
            "id",
            "post_id",
            "parent_id",
            "author_name",
            "author_email",
            "author_url",
            "content",
            "status",
            "created_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "media",
        cols: &[
            "id",
            "uuid",
            "filename",
            "original_filename",
            "storage_key",
            "mime_type",
            "extension",
            "size",
            "width",
            "height",
            "duration",
            "hash",
            "title",
            "description",
            "alt",
            "caption",
            "thumbnails",
            "folder_id",
            "uploaded_by",
            "created_at",
            "updated_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "media_tags",
        cols: &["media_id", "tag"],
        paginate: None,
        pg_sequence: false,
    },
    TableSpec {
        name: "media_references",
        cols: &["media_id", "ref_type", "ref_id", "created_at"],
        paginate: None,
        pg_sequence: false,
    },
    TableSpec {
        name: "extensions",
        cols: &[
            "id",
            "ext_id",
            "kind",
            "version",
            "package_hash",
            "permissions",
            "installed_at",
            "updated_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "extension_logs",
        cols: &[
            "id",
            "ext_id",
            "kind",
            "action",
            "version",
            "actor",
            "result",
            "detail",
            "created_at",
        ],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "extension_migrations",
        cols: &["id", "ext_id", "name", "applied_at"],
        paginate: Some("id"),
        pg_sequence: true,
    },
    TableSpec {
        name: "search_stats",
        cols: &["query", "hits", "no_results", "last_searched_at"],
        paginate: None,
        pg_sequence: false,
    },
];

/// Tables whose rows are never backed up (rebuildable/derived):
/// `search_index` + `search_fts` are rebuilt from content after restore;
/// `schema_migrations` is owned by the running instance's migration runner.
pub const SKIPPED_TABLES: &[&str] = &["search_index", "search_fts", "schema_migrations"];

/// FK-safe delete order (children first).
pub const DELETE_ORDER: &[&str] = &[
    "post_terms",
    "comments",
    "media_references",
    "media_tags",
    "media",
    "posts",
    "pages",
    "terms",
    "extension_migrations",
    "extension_logs",
    "extensions",
    "media_folders",
    "users",
    "search_stats",
    "settings",
];

pub fn spec_of(name: &str) -> Option<&'static TableSpec> {
    TABLES.iter().find(|t| t.name == name)
}

/// Nullable TEXT columns across the core schema. When such a column is
/// NULL the restore must bind a typed NULL the database accepts — binding
/// NULL as an integer on PostgreSQL would reject the expression (`column is
/// of type text but expression is of type bigint`). All other nullable
/// columns are integers, so `OptI(None)` is correct for them.
const NULLABLE_TEXT_COLS: &[&str] = &["featured_image"];

fn dump_value(v: &serde_json::Value, col: &str) -> Option<Bind> {
    match v {
        serde_json::Value::Null => Some(if NULLABLE_TEXT_COLS.contains(&col) {
            Bind::OptS(None)
        } else {
            Bind::OptI(None)
        }),
        serde_json::Value::Number(n) => n.as_i64().map(Bind::I),
        serde_json::Value::String(s) => Some(Bind::S(s.clone())),
        _ => None,
    }
}

/// Convert one row into dump JSON values. Column types cascade
/// `Option<i64>` → `Option<String>` (→ NULL), which covers the whole core
/// schema: every column is INTEGER, TEXT or NULL.
pub fn row_values(row: &sqlx::any::AnyRow, cols: &[&str]) -> sqlx::Result<Vec<serde_json::Value>> {
    cols.iter()
        .map(|c| {
            if let Ok(v) = row.try_get::<Option<i64>, _>(c) {
                return Ok(v
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null));
            }
            if let Ok(v) = crate::db::optional_text(row, c) {
                return Ok(v
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null));
            }
            Err(sqlx::Error::ColumnDecode {
                index: c.to_string(),
                source: format!("column '{c}' has an unsupported type for backup").into(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Streaming dump writer. One JSON header line, then one line per chunk of
/// rows. The sink is whatever `Write` the caller hands over (the hashed zip
/// entry writer), so nothing is buffered beyond one chunk.
pub struct DumpWriter<W: Write> {
    sink: W,
    tables: usize,
    rows: i64,
}

impl<W: Write> DumpWriter<W> {
    pub fn new(mut sink: W, dialect: Dialect, app_version: &str) -> AppResult<Self> {
        let header = json!({
            "format": DUMP_FORMAT,
            "version": DUMP_VERSION,
            "dialect": dialect.name(),
            "app_version": app_version,
            "created_at": crate::utils::time::now(),
        });
        writeln!(sink, "{header}").map_err(io_err)?;
        Ok(Self {
            sink,
            tables: 0,
            rows: 0,
        })
    }

    /// Append one chunk of rows for `table` (column order must match the
    /// spec). An empty `rows` slice writes nothing. Distinct tables are
    /// counted by the caller via [`Self::note_table`] — the writer only
    /// sees chunks and cannot know when a new table starts.
    pub fn write_chunk(
        &mut self,
        table: &str,
        cols: &[&str],
        rows: &[Vec<serde_json::Value>],
    ) -> AppResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let line = json!({ "table": table, "cols": cols, "rows": rows });
        writeln!(self.sink, "{line}").map_err(io_err)?;
        self.rows += rows.len() as i64;
        Ok(())
    }

    pub fn finish(mut self) -> AppResult<(usize, i64)> {
        self.sink.flush().map_err(io_err)?;
        Ok((self.tables, self.rows))
    }

    pub fn note_table(&mut self) {
        self.tables += 1;
    }
}

fn io_err(e: std::io::Error) -> AppError {
    AppError::Internal(anyhow::anyhow!("database dump failed: {e}"))
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// One decoded chunk: rows for a single table.
pub struct DumpChunk {
    pub table: String,
    pub cols: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

impl DumpChunk {
    /// Convert to binds for a multi-row INSERT. Returns `None` when a value
    /// has an impossible JSON shape (corrupt dump).
    pub fn binds(&self) -> Option<Vec<Vec<Bind>>> {
        let mut out = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            if row.len() != self.cols.len() {
                return None;
            }
            let mut binds = Vec::with_capacity(row.len());
            for (i, v) in row.iter().enumerate() {
                binds.push(dump_value(v, &self.cols[i])?);
            }
            out.push(binds);
        }
        Some(out)
    }
}

/// Streaming dump reader over any `BufRead` (a zip entry reader).
pub struct DumpReader<R: BufRead> {
    src: R,
    pub dialect: Option<String>,
    pub app_version: Option<String>,
    done: bool,
}

impl<R: BufRead> DumpReader<R> {
    pub fn new(mut src: R) -> AppResult<Self> {
        let mut header = String::new();
        src.read_line(&mut header).map_err(|e| {
            AppError::BadRequest(format!("backup database dump is unreadable: {e}"))
        })?;
        let v: serde_json::Value = serde_json::from_str(header.trim())
            .map_err(|_| AppError::BadRequest("backup database dump has no valid header".into()))?;
        if v.get("format").and_then(|f| f.as_str()) != Some(DUMP_FORMAT) {
            return Err(AppError::BadRequest(
                "backup database dump has an unknown format marker".into(),
            ));
        }
        let version = v.get("version").and_then(|x| x.as_i64()).unwrap_or(0);
        if version != DUMP_VERSION {
            return Err(AppError::BadRequest(format!(
                "database dump version {version} is not supported (expected {DUMP_VERSION})"
            )));
        }
        Ok(Self {
            src,
            dialect: v
                .get("dialect")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            app_version: v
                .get("app_version")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            done: false,
        })
    }

    /// Next chunk, or `None` at end of dump. Lines that are blank are
    /// skipped; anything unparseable is a hard error (fail closed).
    pub fn next_chunk(&mut self) -> AppResult<Option<DumpChunk>> {
        if self.done {
            return Ok(None);
        }
        loop {
            let mut line = String::new();
            let n = self
                .src
                .read_line(&mut line)
                .map_err(|e| AppError::BadRequest(format!("corrupt database dump: {e}")))?;
            if n == 0 {
                self.done = true;
                return Ok(None);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|_| {
                AppError::BadRequest("corrupt database dump: malformed row chunk".into())
            })?;
            let table = v
                .get("table")
                .and_then(|x| x.as_str())
                .ok_or_else(|| {
                    AppError::BadRequest("corrupt database dump: chunk without table".into())
                })?
                .to_string();
            if spec_of(&table).is_none() {
                return Err(AppError::BadRequest(format!(
                    "database dump references unknown table '{table}'"
                )));
            }
            let cols: Vec<String> = v
                .get("cols")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or_else(|| {
                    AppError::BadRequest("corrupt database dump: chunk without columns".into())
                })?;
            let rows: Vec<Vec<serde_json::Value>> = v
                .get("rows")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .map(|r| r.as_array().cloned().unwrap_or_default())
                        .collect()
                })
                .ok_or_else(|| {
                    AppError::BadRequest("corrupt database dump: chunk without rows".into())
                })?;
            let spec = spec_of(&table).unwrap();
            if cols.len() != spec.cols.len() || cols.iter().zip(spec.cols).any(|(a, b)| a != *b) {
                return Err(AppError::BadRequest(format!(
                    "database dump column mismatch for table '{table}'"
                )));
            }
            return Ok(Some(DumpChunk { table, cols, rows }));
        }
    }
}

/// Build the multi-row INSERT for a chunk (`?` placeholders; the `Db` layer
/// translates them per dialect).
pub fn insert_sql(table: &str, cols: &[&str], row_count: usize) -> String {
    let row = format!("({})", vec!["?"; cols.len()].join(", "));
    let rows = vec![row; row_count].join(",");
    format!("INSERT INTO {table} ({}) VALUES {rows}", cols.join(", "))
}

/// PostgreSQL sequence re-sync for tables restored with explicit ids.
pub fn pg_sequence_fixes() -> Vec<(&'static str, String)> {
    TABLES
        .iter()
        .filter(|t| t.pg_sequence)
        .map(|t| {
            let name = t.name;
            (
                name,
                format!(
                    "SELECT setval(pg_get_serial_sequence('{name}','id'), \
                     GREATEST(COALESCE((SELECT MAX(id) FROM {name}), 0), 1), true)"
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> &'static TableSpec {
        spec_of(name).unwrap()
    }

    #[test]
    fn table_specs_match_migrations() {
        // Spot-check column lists against migrations/sqlite/*.sql.
        assert_eq!(spec("users").cols.len(), 9);
        assert_eq!(spec("posts").cols.len(), 11);
        assert_eq!(spec("media").cols.len(), 21);
        assert_eq!(spec("extensions").cols.len(), 8);
        assert!(spec("post_terms").paginate.is_none());
        assert!(spec("users").paginate.is_some());
        // Every delete-order table exists and every table is deletable.
        for t in TABLES {
            assert!(
                DELETE_ORDER.contains(&t.name),
                "{} missing from DELETE_ORDER",
                t.name
            );
        }
        assert_eq!(DELETE_ORDER.len(), TABLES.len());
    }

    #[test]
    fn settings_secret_excluded() {
        assert!(EXCLUDED_SETTINGS.contains(&"security.secret"));
    }

    #[test]
    fn writer_reader_roundtrip() {
        let mut buf: Vec<u8> = Vec::new();
        // Full users row: id, username, email, password_hash, role,
        // display_name, bio, created_at, updated_at.
        let rows = vec![
            vec![
                json!(1),
                json!("admin"),
                serde_json::Value::Null,
                json!("argon2hash"),
                json!("admin"),
                json!("Admin"),
                json!(""),
                json!(100),
                json!(100),
            ],
            vec![
                json!(2),
                json!("bob"),
                json!("b@x"),
                json!("argon2hash"),
                json!("author"),
                json!("Bob"),
                json!("bio"),
                json!(101),
                json!(101),
            ],
        ];
        {
            let mut w = DumpWriter::new(&mut buf, Dialect::Sqlite, "0.1.0").unwrap();
            w.note_table();
            w.write_chunk("users", spec("users").cols, &rows).unwrap();
            let (tables, n) = w.finish().unwrap();
            assert_eq!(tables, 1);
            assert_eq!(n, 2);
        }
        let mut r = DumpReader::new(&buf[..]).unwrap();
        assert_eq!(r.dialect.as_deref(), Some("sqlite"));
        let chunk = r.next_chunk().unwrap().unwrap();
        assert_eq!(chunk.table, "users");
        assert_eq!(chunk.cols, spec("users").cols);
        assert_eq!(chunk.rows.len(), 2);
        let binds = chunk.binds().unwrap();
        assert_eq!(binds.len(), 2);
        assert!(matches!(binds[0][0], Bind::I(1)));
        assert!(matches!(binds[0][2], Bind::OptI(None)), "NULL email binds");
        assert!(matches!(binds[1][1], Bind::S(_)));
        assert!(r.next_chunk().unwrap().is_none());
    }

    #[test]
    fn reader_rejects_garbage_and_unknown_tables() {
        let bad = b"not json\n";
        assert!(DumpReader::new(&bad[..]).is_err());

        let header = format!(
            "{{\"format\":\"{DUMP_FORMAT}\",\"version\":{DUMP_VERSION},\"dialect\":\"sqlite\",\"app_version\":\"0.1.0\",\"created_at\":1}}\n"
        );
        let mut buf = header.clone().into_bytes();
        buf.extend_from_slice(b"{\"table\":\"not_a_table\",\"cols\":[],\"rows\":[]}\n");
        assert!(DumpReader::new(&buf[..]).unwrap().next_chunk().is_err());

        // Column mismatch against the schema is rejected.
        let mut buf = header.into_bytes();
        buf.extend_from_slice(b"{\"table\":\"users\",\"cols\":[\"id\"],\"rows\":[[1]]}\n");
        assert!(DumpReader::new(&buf[..]).unwrap().next_chunk().is_err());
    }

    #[test]
    fn insert_sql_builds_multirow() {
        let sql = insert_sql("users", &["id", "name"], 2);
        assert_eq!(sql, "INSERT INTO users (id, name) VALUES (?, ?),(?, ?)");
        // Placeholder translation on PG must renumber across row groups.
        assert_eq!(
            Dialect::Postgres.translate(&sql),
            "INSERT INTO users (id, name) VALUES ($1, $2),($3, $4)"
        );
    }

    #[test]
    fn pg_sequence_fixes_cover_serial_tables() {
        let names: Vec<_> = pg_sequence_fixes().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"users"));
        assert!(names.contains(&"posts"));
        assert!(names.contains(&"media"));
        assert!(!names.contains(&"post_terms"));
    }
}
