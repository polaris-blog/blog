//! Search endpoints.
//!
//! ```text
//! GET /api/search            JSON results (query, page, per_page, sort, …)
//! GET /api/search/suggest    JSON suggestions for a prefix
//! GET /search                server-rendered results page (theme template)
//! ```
//!
//! `/api/search/status` lives in the authenticated API group (admin-only);
//! the HTML page renders through the normal theme pipeline. The shared
//! page cache never stores `/search` — the cache key ignores query
//! strings, so per-query caching is handled inside [`SearchService`]
//! instead (keys hash the full query shape).

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::error::{ApiError, AppError, AppResult};
use crate::search::{SearchKind, SearchQuery, SearchSort};
use crate::state::App;
use crate::utils::time;

use super::{page_context, render_theme};

#[derive(Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub per_page: Option<u32>,
    #[serde(default)]
    pub sort: Option<String>,
    /// `post` or `page`.
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

fn build_query(app: &App, p: &SearchParams) -> Result<SearchQuery, AppError> {
    let sort = match p.sort.as_deref() {
        None | Some("") => SearchSort::Relevance,
        Some(s) => SearchSort::parse(s)
            .ok_or_else(|| AppError::BadRequest(format!("unknown sort: {s}")))?,
    };
    let kind = match p.r#type.as_deref() {
        None | Some("") => None,
        Some(t) => Some(
            SearchKind::parse(t)
                .ok_or_else(|| AppError::BadRequest(format!("unknown type: {t}")))?,
        ),
    };
    // The public API never exposes hidden rows: media index entries are
    // stored with `visible = 0` (library-internal) and drafts must stay
    // invisible regardless of the requested type.
    if kind == Some(SearchKind::Media) {
        return Err(AppError::Forbidden("media search is admin-only".into()));
    }
    Ok(SearchQuery {
        query: p.q.clone(),
        page: p.page.unwrap_or(1).max(1),
        per_page: p.per_page.unwrap_or_else(|| app.search.default_per_page()),
        sort,
        kind,
        category: clean_filter(p.category.as_deref()),
        tag: clean_filter(p.tag.as_deref()),
        author: clean_filter(p.author.as_deref()),
        include_hidden: false,
    })
}

fn clean_filter(v: Option<&str>) -> Option<String> {
    v.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().count() <= 100)
}

// ---------------------------------------------------------------------------
// JSON API
// ---------------------------------------------------------------------------

pub async fn api_search(
    State(app): State<App>,
    Query(p): Query<SearchParams>,
) -> Result<Response, ApiError> {
    if !app.search.enabled() {
        return Err(ApiError(AppError::Forbidden("search is disabled".into())));
    }
    let query = build_query(&app, &p).map_err(ApiError)?;
    let resp = app.search.search(&app, query).await.map_err(ApiError)?;
    Ok(Json(json!({
        "query": resp.query,
        "sort": resp.sort,
        "page": resp.page,
        "per_page": resp.per_page,
        "total": resp.total,
        "pages": resp.pages,
        "results": resp.results,
    }))
    .into_response())
}

#[derive(Deserialize)]
pub struct SuggestParams {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn api_suggest(
    State(app): State<App>,
    Query(p): Query<SuggestParams>,
) -> Result<Response, ApiError> {
    if !app.search.enabled() {
        return Err(ApiError(AppError::Forbidden("search is disabled".into())));
    }
    let limit = p
        .limit
        .unwrap_or(app.search.suggestion_limit())
        .clamp(1, 20);
    let out = app.search.suggest(&app, &p.q).await.map_err(ApiError)?;
    let limited: Vec<String> = out.into_iter().take(limit).collect();
    Ok(Json(json!(limited)).into_response())
}

/// `GET /api/search/status` — admin-only (authenticated API group).
pub async fn api_status(
    State(app): State<App>,
    Extension(_auth): Extension<crate::auth::AuthCtx>,
) -> Result<Response, ApiError> {
    let status = app.search.status(&app).await.map_err(ApiError)?;
    let stats = app.cache.stats();
    Ok(Json(json!({
        "provider": status.provider,
        "enabled": app.search.enabled(),
        "healthy": status.healthy,
        "indexed_posts": status.indexed_posts,
        "indexed_pages": status.indexed_pages,
        "last_rebuild": status.last_rebuild.map(|ts| time::format(ts, "rfc3339")),
        "cache": {
            "hits": stats.hits,
            "misses": stats.misses,
            "hit_rate": stats.hit_rate,
        },
    }))
    .into_response())
}

// ---------------------------------------------------------------------------
// HTML page
// ---------------------------------------------------------------------------

pub async fn page(
    State(app): State<App>,
    Query(p): Query<SearchParams>,
    headers: HeaderMap,
) -> AppResult<Response> {
    if !app.search.enabled() {
        return super::public::not_found_response(&app).await;
    }
    let raw_query = p.q.trim().to_string();
    let query = match build_query(&app, &p) {
        Ok(q) => q,
        Err(e) => return Ok(bad_request_response(&e.message())),
    };
    let resp = app.search.search(&app, query).await?;

    let results: Vec<_> = resp
        .results
        .iter()
        .map(|r| {
            let ts = r.published_at.unwrap_or(r.updated_at);
            json!({
                "title": r.title,
                "url": r.url,
                "type": r.kind,
                "author": r.author,
                "category": r.category,
                "tags": r.tags,
                "highlight": r.highlight.clone().unwrap_or_else(|| r.excerpt.clone()),
                "date": time::format(ts, "date"),
                "datetime": time::format(ts, "datetime"),
            })
        })
        .collect();
    let sort = resp.sort.clone();
    let sorts: Vec<&str> = vec!["relevance", "date", "updated", "title"];

    let mut ctx = page_context(&app).await;
    ctx.insert("search_query", &raw_query);
    ctx.insert("search_total", &resp.total);
    ctx.insert("results", &results);
    ctx.insert("sort", &sort);
    ctx.insert("sorts", &sorts);
    ctx.insert(
        "pagination",
        &json!({
            "current": resp.page,
            "pages": resp.pages,
            "has_prev": resp.page > 1,
            "has_next": (resp.page as i64) < resp.pages,
            "prev": resp.page.saturating_sub(1),
            "next": resp.page + 1,
        }),
    );
    // Pagination links keep the query string: pre-encoded by the handler.
    ctx.insert("search_qs", &serde_urlencode_search(&raw_query, &sort));
    let body = render_theme(&app, "search.html", &ctx)?;
    Ok(html_response(&headers, body))
}

/// Build a URL-encoded `q=…&sort=…` prefix for pagination links.
fn serde_urlencode_search(q: &str, sort: &str) -> String {
    let mut out = String::from("q=");
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    if sort != "relevance" {
        out.push_str("&sort=");
        out.push_str(sort);
    }
    out
}

fn bad_request_response(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("bad request: {msg}"),
    )
        .into_response()
}

/// HTML with ETag / 304 support (same contract as other public pages).
fn html_response(headers: &HeaderMap, body: String) -> Response {
    let etag = crate::utils::hash::etag(body.as_bytes());
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        let hit = inm
            .trim()
            .trim_start_matches("W/")
            .split(',')
            .any(|c| c.trim() == etag);
        if hit {
            let mut resp = StatusCode::NOT_MODIFIED.into_response();
            resp.headers_mut().insert(
                header::ETAG,
                axum::http::HeaderValue::from_str(&etag).unwrap(),
            );
            return resp;
        }
    }
    let mut resp = (StatusCode::OK, axum::body::Body::from(body)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(
        header::ETAG,
        axum::http::HeaderValue::from_str(&etag).unwrap(),
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    resp
}
