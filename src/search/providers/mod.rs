//! Search providers — one implementation per database engine.
//!
//! Business code never touches a dialect directly: it goes through
//! [`SearchProvider`], an enum dispatch (same convention as
//! `CacheBackend` — monomorphic calls, no `async-trait` dependency).
//! Adding an external engine later (Meilisearch, Typesense, …) means
//! adding a variant here behind a cargo feature.

pub mod mysql;
pub mod postgres;
pub mod sqlite;

use sqlx::Row;
use sqlx::any::AnyRow;

use crate::config::SearchWeights;
use crate::db::{Bind, Db, Dialect};
use crate::error::AppResult;

use super::query::{ParsedQuery, SearchQuery, SearchSort};
use super::result::{IndexedDoc, SearchRow};

/// Everything a provider needs to execute one search.
pub struct SearchContext<'a> {
    pub parsed: &'a ParsedQuery,
    pub query: &'a SearchQuery,
    pub weights: &'a SearchWeights,
    pub language: &'a str,
    pub page: u32,
    pub per_page: u32,
}

pub enum SearchProvider {
    Sqlite(sqlite::SqliteSearch),
    Mysql(mysql::MySqlSearch),
    Postgres(postgres::PostgresSearch),
}

impl SearchProvider {
    /// Select the provider for a configuration + dialect.
    /// `provider = "auto" | "database"` (or any unknown/external name with
    /// fallback enabled) resolves to the dialect's native engine.
    pub fn select(cfg: &crate::config::SearchConfig, dialect: Dialect) -> Self {
        let native = match dialect {
            Dialect::Sqlite => Self::Sqlite(sqlite::SqliteSearch),
            Dialect::MySql => Self::Mysql(mysql::MySqlSearch),
            Dialect::Postgres => Self::Postgres(postgres::PostgresSearch::new(
                &cfg.language,
                cfg.weights.clone(),
            )),
        };
        match cfg.provider.as_str() {
            "auto" | "database" | "" => native,
            other => {
                if cfg.fallback {
                    tracing::warn!(
                        provider = other,
                        "search provider not available in this build — falling back to database search"
                    );
                } else {
                    tracing::error!(
                        provider = other,
                        "search provider not available and fallback = false — searches will fail"
                    );
                }
                native
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "SQLite FTS5",
            Self::Mysql(_) => "MySQL FULLTEXT",
            Self::Postgres(_) => "PostgreSQL Full-Text Search",
        }
    }

    /// Fail fast with a clear error when the search tables are missing
    /// (e.g. `auto_migrate = false` and migrations were not applied).
    pub async fn ensure_index(&self, db: &Db) -> AppResult<()> {
        db.fetch_one("SELECT COUNT(*) AS total FROM search_index", &[])
            .await?;
        Ok(())
    }

    pub async fn upsert(&self, db: &Db, doc: &IndexedDoc) -> AppResult<()> {
        match self {
            Self::Sqlite(p) => p.upsert(db, doc).await,
            Self::Mysql(p) => p.upsert(db, doc).await,
            Self::Postgres(p) => p.upsert(db, doc).await,
        }
    }

    pub async fn remove(&self, db: &Db, ref_type: &str, ref_id: i64) -> AppResult<()> {
        match self {
            Self::Sqlite(p) => p.remove(db, ref_type, ref_id).await,
            Self::Mysql(p) => p.remove(db, ref_type, ref_id).await,
            Self::Postgres(p) => p.remove(db, ref_type, ref_id).await,
        }
    }

    /// Wipe the whole index (first step of a rebuild).
    pub async fn clear(&self, db: &Db) -> AppResult<()> {
        match self {
            Self::Sqlite(p) => p.clear(db).await,
            Self::Mysql(p) => p.clear(db).await,
            Self::Postgres(p) => p.clear(db).await,
        }
    }

    /// Execute a search: returns (rows, total).
    pub async fn search(
        &self,
        db: &Db,
        ctx: &SearchContext<'_>,
    ) -> AppResult<(Vec<SearchRow>, i64)> {
        match self {
            Self::Sqlite(p) => p.search(db, ctx).await,
            Self::Mysql(p) => p.search(db, ctx).await,
            Self::Postgres(p) => p.search(db, ctx).await,
        }
    }

    /// `(healthy, indexed_posts, indexed_pages, indexed_media)`.
    pub async fn health(&self, db: &Db) -> AppResult<(bool, i64, i64, i64)> {
        let rows = db
            .fetch_all(
                "SELECT ref_type, COUNT(*) AS total FROM search_index GROUP BY ref_type",
                &[],
            )
            .await?;
        let mut posts = 0i64;
        let mut pages = 0i64;
        let mut media = 0i64;
        for r in rows {
            let kind: String = r.try_get("ref_type").unwrap_or_default();
            let total: i64 = r.try_get("total").unwrap_or(0);
            match kind.as_str() {
                "post" => posts = total,
                "page" => pages = total,
                "media" => media = total,
                _ => {}
            }
        }
        // Providers with a cheap integrity probe override this via `probe`.
        let healthy = self.probe(db).await;
        Ok((healthy, posts, pages, media))
    }

    /// Engine-specific corruption probe (default: healthy).
    async fn probe(&self, db: &Db) -> bool {
        match self {
            Self::Sqlite(p) => p.probe(db).await,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Uniform result column list (plus a per-dialect `score` expression).
const RESULT_COLS: &str = "si.ref_type, si.ref_id, si.title, si.slug, si.excerpt, \
                           si.content, si.author, si.category, si.tags, \
                           si.published_at, si.updated_at";

pub(super) fn result_cols() -> &'static str {
    RESULT_COLS
}

pub(super) fn row_to_search_row(r: &AnyRow) -> sqlx::Result<SearchRow> {
    Ok(SearchRow {
        ref_type: r.try_get("ref_type")?,
        ref_id: r.try_get("ref_id")?,
        title: r.try_get("title")?,
        slug: r.try_get("slug")?,
        excerpt: crate::db::text(r, "excerpt")?,
        content: crate::db::text(r, "content")?,
        author: r.try_get("author")?,
        category: r.try_get("category")?,
        tags: crate::db::text(r, "tags")?,
        score: r.try_get::<f64, _>("score").unwrap_or(0.0),
        published_at: r.try_get("published_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// Dialect-neutral filter clauses shared by all providers. Category and
/// tag filters resolve through the canonical `post_terms`/`terms` tables
/// (slugs), so the denormalized text columns stay search-only. The
/// visibility filter is applied here (not in the providers) so internal
/// callers (media library search) can include hidden rows.
pub(super) fn filter_clauses(q: &SearchQuery) -> (Vec<String>, Vec<Bind>) {
    let mut clauses = Vec::new();
    let mut binds = Vec::new();
    if !q.include_hidden {
        clauses.push("si.visible = 1".into());
    }
    if let Some(kind) = q.kind {
        clauses.push("si.ref_type = ?".into());
        binds.push(Bind::S(kind.as_str().to_string()));
    }
    if let Some(author) = q.author.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        clauses.push("si.author = ?".into());
        binds.push(Bind::S(author.to_string()));
    }
    if let Some(slug) = q
        .category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        clauses.push(
            "si.ref_type = 'post' AND EXISTS (SELECT 1 FROM post_terms pt \
             JOIN terms t ON t.id = pt.term_id \
             WHERE pt.post_id = si.ref_id AND t.kind = 'category' AND t.slug = ?)"
                .into(),
        );
        binds.push(Bind::S(slug.to_string()));
    }
    if let Some(slug) = q.tag.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        clauses.push(
            "si.ref_type = 'post' AND EXISTS (SELECT 1 FROM post_terms pt \
             JOIN terms t ON t.id = pt.term_id \
             WHERE pt.post_id = si.ref_id AND t.kind = 'tag' AND t.slug = ?)"
                .into(),
        );
        binds.push(Bind::S(slug.to_string()));
    }
    (clauses, binds)
}

/// Dialect-neutral ORDER BY (aliases like `score` are legal in ORDER BY on
/// all three engines; COALESCE keeps NULL publish dates last).
pub(super) fn order_clause(sort: SearchSort) -> &'static str {
    match sort {
        SearchSort::Relevance => "ORDER BY score DESC, si.updated_at DESC",
        SearchSort::Date => "ORDER BY COALESCE(si.published_at, si.updated_at) DESC",
        SearchSort::Updated => "ORDER BY si.updated_at DESC",
        SearchSort::Title => "ORDER BY LOWER(si.title) ASC",
    }
}

/// Format a configured weight as a safe SQL float literal.
pub(super) fn sqlf(w: f64) -> String {
    if w.is_finite() && w >= 0.0 {
        format!("{w:.4}")
    } else {
        "1.0".to_string()
    }
}
