//! [`SearchService`] — the only search entry point business code touches.
//!
//! ```text
//! Search request → normalize/validate → cache → provider → rank/filter
//!                                → highlight → pagination → response
//! ```
//!
//! Index maintenance (create/update/delete of posts and pages) also goes
//! through this service, keeping the denormalized index consistent with
//! the database (the source of truth). Failures are logged and never
//! block content operations — `polaris search rebuild` reconciles drift.

use sqlx::Row;

use crate::cache::ns;
use crate::config::SearchConfig;
use crate::db::{Bind, Db, Dialect};
use crate::error::{AppError, AppResult};
use crate::models::{Page, Post, PostStatus};
use crate::repositories;
use crate::state::App;
use crate::utils::{hash, time};

use super::analytics;
use super::highlight;
use super::providers::{SearchContext, SearchProvider};
use super::query::{ParsedQuery, SearchQuery as Q, normalize};
use super::result::{
    IndexStatus, IndexedDoc, RebuildStats, SearchResponse, SearchResult, SearchRow,
};

pub struct SearchService {
    cfg: SearchConfig,
    provider: SearchProvider,
}

impl SearchService {
    pub fn new(cfg: SearchConfig, dialect: Dialect) -> Self {
        Self {
            provider: SearchProvider::select(&cfg, dialect),
            cfg,
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// Startup probe: fails when the search tables are missing.
    pub async fn provider_probe(&self, db: &Db) -> AppResult<()> {
        self.provider.ensure_index(db).await
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn max_per_page(&self) -> u32 {
        self.cfg.max_per_page
    }

    pub fn default_per_page(&self) -> u32 {
        self.cfg.default_per_page
    }

    pub fn suggestion_limit(&self) -> usize {
        self.cfg.suggestion_limit
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Execute a search with caching and (optional) analytics.
    /// Invalid queries (too short / no usable terms) return an empty
    /// response rather than an error — friendlier for HTML pages.
    pub async fn search(&self, app: &App, q: Q) -> AppResult<SearchResponse> {
        if !self.cfg.enabled {
            return Err(AppError::Forbidden("search is disabled".into()));
        }
        let page = q.page.max(1);
        let per = q.per_page.clamp(1, self.cfg.max_per_page.clamp(1, 100));
        let Ok(parsed) = ParsedQuery::parse(&q.query, self.cfg.minimum_query_length) else {
            return Ok(SearchResponse::empty(&q.query, q.sort, page, per));
        };

        // Versioned cache key: stable hash of the complete query shape
        // (terms, page, per-page, sort, filters). Content mutations bump
        // the `search` namespace version, invalidating everything at once.
        let cache_key = cache_key(&parsed.normalized, page, per, &q);
        if self.cfg.cache.enabled
            && let Some(hit) = app
                .cache
                .get_json::<SearchResponse>(ns::SEARCH, &cache_key)
                .await
        {
            self.record_analytics(app, &parsed.normalized, hit.total > 0)
                .await;
            return Ok(hit);
        }

        let fill = app.cache.begin_fill(ns::SEARCH, &cache_key).await;
        let ctx = SearchContext {
            parsed: &parsed,
            query: &q,
            weights: &self.cfg.weights,
            language: &self.cfg.language,
            page,
            per_page: per,
        };
        let (rows, total) = self.provider.search(&app.db, &ctx).await?;
        let results: Vec<SearchResult> = rows
            .iter()
            .map(|r| self.to_result(r, &parsed.terms))
            .collect();
        let pages = if total == 0 {
            0
        } else {
            (total + per as i64 - 1) / per as i64
        };
        let resp = SearchResponse {
            query: parsed.normalized.clone(),
            sort: q.sort.as_str().to_string(),
            page,
            per_page: per,
            total,
            pages,
            results,
        };
        self.record_analytics(app, &parsed.normalized, total > 0)
            .await;
        if self.cfg.cache.enabled {
            app.cache.finish_fill(fill, &resp).await;
        }
        Ok(resp)
    }

    /// Build the public result: pick the snippet source (excerpt when it
    /// matches, otherwise a window of the content around the first match)
    /// and highlight it with XSS-safe `<mark>` output.
    fn to_result(&self, row: &SearchRow, terms: &[String]) -> SearchResult {
        let kind = row.ref_type.clone();
        let url = if kind == "page" {
            format!("/{}", row.slug)
        } else {
            format!("/posts/{}", row.slug)
        };
        let excerpt = row.excerpt.trim().to_string();
        let highlight_html = if self.cfg.highlight {
            let source = if !excerpt.is_empty() && highlight::contains_any(&excerpt, terms) {
                &excerpt
            } else {
                row.content.as_str()
            };
            Some(highlight::snippet(source, terms, 240))
        } else {
            None
        };
        SearchResult {
            id: row.ref_id,
            kind,
            title: row.title.clone(),
            slug: row.slug.clone(),
            excerpt,
            url,
            score: row.score as f32,
            highlight: highlight_html,
            published_at: row.published_at,
            updated_at: row.updated_at,
            author: row.author.clone(),
            category: row.category.clone(),
            tags: split_tags(&row.tags),
        }
    }

    async fn record_analytics(&self, app: &App, normalized: &str, found: bool) {
        if !self.cfg.analytics.enabled {
            return;
        }
        if let Err(e) = analytics::record(&app.db, normalized, found).await {
            tracing::warn!(error = %e, "search analytics record failed (ignored)");
        }
    }

    // -----------------------------------------------------------------------
    // Suggestions
    // -----------------------------------------------------------------------

    pub async fn suggest(&self, app: &App, q: &str) -> AppResult<Vec<String>> {
        if !self.cfg.enabled {
            return Ok(Vec::new());
        }
        let normalized = normalize(q);
        if normalized.chars().count() < self.cfg.minimum_query_length.max(1) {
            return Ok(Vec::new());
        }
        let limit = self.cfg.suggestion_limit.max(1);
        let sub = format!("sug:{}", normalized.chars().take(50).collect::<String>());
        let db = app.db.clone();
        let pattern = normalized.clone();
        app.cache
            .get_or_load(ns::SEARCH, &sub, async move {
                super::suggest::suggest(&db, &pattern, limit)
                    .await
                    .map_err(anyhow::Error::from)
            })
            .await
            .map_err(AppError::Internal)
    }

    // -----------------------------------------------------------------------
    // Index maintenance
    // -----------------------------------------------------------------------

    /// Index (or re-index) a post. Terms must be attached by the caller.
    pub async fn index_post(&self, db: &Db, post: &Post) -> AppResult<()> {
        let now = time::now();
        let visible = post.status == PostStatus::Published
            && post.published_at.map(|t| t <= now).unwrap_or(false);
        let category = post.category().map(|c| c.name.clone()).unwrap_or_default();
        let tags = post
            .tags()
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let doc = IndexedDoc {
            ref_type: "post",
            ref_id: post.id,
            title: post.title.clone(),
            slug: post.slug.clone(),
            excerpt: excerpt_of(&post.summary, &post.content_md),
            content: strip_markdown(&post.content_md),
            author: post.author_name.clone().unwrap_or_default(),
            category,
            tags,
            visible,
            published_at: post.published_at,
            updated_at: post.updated_at,
        };
        self.provider.upsert(db, &doc).await
    }

    pub async fn index_page(&self, db: &Db, page: &Page) -> AppResult<()> {
        let doc = IndexedDoc {
            ref_type: "page",
            ref_id: page.id,
            title: page.title.clone(),
            slug: page.slug.clone(),
            excerpt: excerpt_of(&page.summary, &page.content_md),
            content: strip_markdown(&page.content_md),
            author: page.author_name.clone().unwrap_or_default(),
            category: String::new(),
            tags: String::new(),
            visible: page.status == PostStatus::Published,
            published_at: None,
            updated_at: page.updated_at,
        };
        self.provider.upsert(db, &doc).await
    }

    /// Index a media item for the media library search API. Rows are stored
    /// with `visible = 0`: the public site search must not mix library
    /// items into its results — the media API searches with
    /// `include_hidden = true` + `kind = Media`.
    pub async fn index_media(&self, db: &Db, media: &crate::models::Media) -> AppResult<()> {
        let doc = IndexedDoc {
            ref_type: "media",
            ref_id: media.id,
            title: if media.title.is_empty() {
                media.filename.clone()
            } else {
                media.title.clone()
            },
            slug: media.uuid.clone(),
            excerpt: media.caption.clone(),
            content: format!(
                "{} {} {} {} {}",
                media.filename,
                media.alt,
                media.description,
                media.mime_type,
                media.tags.join(" ")
            ),
            author: media.uploader_name.clone().unwrap_or_default(),
            category: media.folder_name.clone().unwrap_or_default(),
            tags: media.tags.join(", "),
            visible: false,
            published_at: None,
            updated_at: media.updated_at,
        };
        self.provider.upsert(db, &doc).await
    }

    pub async fn remove(&self, db: &Db, ref_type: &str, ref_id: i64) -> AppResult<()> {
        self.provider.remove(db, ref_type, ref_id).await
    }

    /// Scheduled→published transitions bypass the service layer; flip the
    /// visibility flag for anything now public (single statement, all rows).
    pub async fn promote_visible(&self, db: &Db, now: i64) -> AppResult<u64> {
        db.execute(
            "UPDATE search_index SET visible = 1 WHERE ref_type = 'post' AND visible = 0 \
             AND EXISTS (SELECT 1 FROM posts p WHERE p.id = search_index.ref_id \
             AND p.status = 'published' AND p.published_at IS NOT NULL AND p.published_at <= ?)",
            &[Bind::I(now)],
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Status & rebuild
    // -----------------------------------------------------------------------

    pub async fn status(&self, app: &App) -> AppResult<IndexStatus> {
        let (healthy, posts, pages, media) = self.provider.health(&app.db).await?;
        let last_rebuild = repositories::settings::get(&app.db, "search.last_rebuild")
            .await?
            .and_then(|s| s.parse::<i64>().ok());
        Ok(IndexStatus {
            provider: self.provider.name().to_string(),
            healthy,
            indexed_posts: posts,
            indexed_pages: pages,
            indexed_media: media,
            last_rebuild,
        })
    }

    /// Drop → scan → re-index everything, with progress callbacks
    /// (`done`, `total`) and a final verification pass.
    pub async fn rebuild(
        &self,
        app: &App,
        mut progress: impl FnMut(usize, usize),
    ) -> AppResult<RebuildStats> {
        self.provider.clear(&app.db).await?;

        let total_posts = count(&app.db, "SELECT COUNT(*) AS total FROM posts").await?;
        let total_pages = count(&app.db, "SELECT COUNT(*) AS total FROM pages").await?;
        let total_media = count(&app.db, "SELECT COUNT(*) AS total FROM media").await?;
        let total = (total_posts + total_pages + total_media) as usize;

        let mut done = 0usize;
        let mut last_id = 0i64;
        const BATCH: i64 = 200;
        loop {
            let rows = app
                .db
                .fetch_all(
                    "SELECT p.id, p.title, p.slug, p.summary, p.content_md, p.author_id, \
                     p.status, p.featured_image, p.published_at, p.created_at, p.updated_at, \
                     COALESCE(NULLIF(u.display_name, ''), u.username) AS author_name \
                     FROM posts p JOIN users u ON u.id = p.author_id WHERE p.id > ? \
                     ORDER BY p.id LIMIT ?",
                    &[Bind::I(last_id), Bind::I(BATCH)],
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            let mut posts: Vec<Post> = rows
                .iter()
                .map(Post::from_row)
                .collect::<sqlx::Result<Vec<_>>>()
                .map_err(AppError::Db)?;
            repositories::terms::attach(&app.db, &mut posts).await?;
            for post in &posts {
                if let Err(e) = self.index_post(&app.db, post).await {
                    tracing::warn!(id = post.id, error = %e, "search index: post skipped");
                }
                done += 1;
                last_id = post.id;
            }
            progress(done, total);
        }

        let mut last_id = 0i64;
        loop {
            let rows = app
                .db
                .fetch_all(
                    "SELECT g.id, g.title, g.slug, g.summary, g.content_md, g.author_id, \
                     g.status, g.sort_order, g.created_at, g.updated_at, \
                     COALESCE(NULLIF(u.display_name, ''), u.username) AS author_name \
                     FROM pages g JOIN users u ON u.id = g.author_id WHERE g.id > ? \
                     ORDER BY g.id LIMIT ?",
                    &[Bind::I(last_id), Bind::I(BATCH)],
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            let pages: Vec<Page> = rows
                .iter()
                .map(Page::from_row)
                .collect::<sqlx::Result<Vec<_>>>()
                .map_err(AppError::Db)?;
            for page in &pages {
                if let Err(e) = self.index_page(&app.db, page).await {
                    tracing::warn!(id = page.id, error = %e, "search index: page skipped");
                }
                done += 1;
                last_id = page.id;
            }
            progress(done, total);
        }

        // Media rows: visible = 0 (library-only, never in public search).
        let mut last_id = 0i64;
        loop {
            let media = crate::repositories::media::scan(&app.db, last_id, BATCH).await?;
            if media.is_empty() {
                break;
            }
            for m in &media {
                if let Err(e) = self.index_media(&app.db, m).await {
                    tracing::warn!(id = m.id, error = %e, "search index: media skipped");
                }
                done += 1;
                last_id = m.id;
            }
            progress(done, total);
        }

        let (_, indexed_posts, indexed_pages, indexed_media) =
            self.provider.health(&app.db).await?;
        let stats = RebuildStats {
            posts: indexed_posts,
            pages: indexed_pages,
            media: indexed_media,
            verified: indexed_posts == total_posts
                && indexed_pages == total_pages
                && indexed_media == total_media,
        };
        if !stats.verified {
            tracing::warn!(
                expected_posts = total_posts,
                indexed_posts,
                expected_pages = total_pages,
                indexed_pages,
                expected_media = total_media,
                indexed_media,
                "search index rebuild verification mismatch"
            );
        }
        let _ =
            repositories::settings::set(&app.db, "search.last_rebuild", &time::now().to_string())
                .await;
        app.cache.invalidate(&[ns::SEARCH]).await;
        Ok(stats)
    }

    /// Popular searches (admin dashboard). Empty when analytics is off.
    pub async fn popular_searches(&self, app: &App, limit: i64) -> Vec<analytics::SearchStat> {
        analytics::popular(&app.db, limit).await.unwrap_or_default()
    }

    pub async fn no_result_searches(&self, app: &App, limit: i64) -> Vec<analytics::SearchStat> {
        analytics::top_no_results(&app.db, limit)
            .await
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn count(db: &Db, sql: &str) -> AppResult<i64> {
    db.fetch_one(sql, &[])
        .await?
        .try_get::<i64, _>("total")
        .map_err(AppError::Db)
}

/// Stable, compact cache sub-key from the full query shape.
fn cache_key(normalized: &str, page: u32, per: u32, q: &Q) -> String {
    let canonical = format!(
        "q={}|p={page}|n={per}|s={}|t={}|c={}|g={}|a={}",
        normalized,
        q.sort.as_str(),
        q.kind.map(|k| k.as_str()).unwrap_or(""),
        q.category.as_deref().unwrap_or(""),
        q.tag.as_deref().unwrap_or(""),
        q.author.as_deref().unwrap_or(""),
    );
    format!("res:{:016x}", hash::fnv1a64(canonical.as_bytes()))
}

fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Summary if present, otherwise a head of the (stripped) content.
fn excerpt_of(summary: &str, content_md: &str) -> String {
    if !summary.trim().is_empty() {
        return summary.trim().to_string();
    }
    crate::markdown::truncate_chars(&strip_markdown(content_md), 220)
}

/// Remove the loudest Markdown syntax so indexed text and snippets read
/// like prose. Tokenizers ignore most of these characters anyway; this is
/// for snippet quality, not correctness.
fn strip_markdown(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let chars: Vec<char> = md.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // [text](url) → text
            '[' => {
                if let Some(close) = chars[i..].iter().position(|&c| c == ']') {
                    let after = i + close + 1;
                    if after < chars.len()
                        && chars[after] == '('
                        && let Some(paren_end) = chars[after..].iter().position(|&c| c == ')')
                    {
                        out.extend(&chars[i + 1..i + close]);
                        i = after + paren_end + 1;
                        continue;
                    }
                }
                out.push('[');
                i += 1;
            }
            // ![alt](url) → alt
            '!' if i + 1 < chars.len() && chars[i + 1] == '[' => {
                i += 1;
            }
            '#' | '*' | '_' | '`' | '~' | '>' => {
                i += 1;
            }
            _ => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_stable_and_distinct() {
        let q1 = Q {
            query: "rust".into(),
            ..Default::default()
        };
        let q2 = Q {
            query: "rust".into(),
            ..Default::default()
        };
        let q3 = Q {
            query: "rust".into(),
            page: 2,
            ..Default::default()
        };
        assert_eq!(cache_key("rust", 1, 10, &q1), cache_key("rust", 1, 10, &q2));
        assert_ne!(cache_key("rust", 1, 10, &q1), cache_key("rust", 2, 10, &q3));
    }

    #[test]
    fn strips_markdown_syntax() {
        assert_eq!(strip_markdown("# Hello *world*"), " Hello world");
        assert_eq!(strip_markdown("see [docs](http://x) now"), "see docs now");
        assert_eq!(strip_markdown("![alt](img.png)"), "alt");
        assert_eq!(strip_markdown("plain text"), "plain text");
    }

    #[test]
    fn excerpt_falls_back_to_content() {
        assert_eq!(excerpt_of("sum", "# body"), "sum");
        let e = excerpt_of("", "word ".repeat(100).as_str());
        assert!(e.chars().count() <= 220);
    }

    #[test]
    fn tags_split() {
        assert_eq!(split_tags("rust, web, axum"), vec!["rust", "web", "axum"]);
        assert!(split_tags("").is_empty());
    }
}
