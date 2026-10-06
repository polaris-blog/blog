pub mod admin;
pub mod admin_backup;
pub mod admin_extensions;
pub mod admin_jobs;
pub mod admin_media;
pub mod api;
pub mod comments;
pub mod media;
pub mod media_api;
pub mod plugins_http;
pub mod public;
pub mod search;
pub mod seo;
pub mod static_files;

use std::io::Write;
use std::time::Instant;

use axum::Router;
use axum::body::HttpBody;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tera::Context;

use crate::cache::ns;
use crate::error::AppResult;
use crate::models::Post;
use crate::state::App;
use crate::utils::time;

pub fn router(app: App) -> Router {
    let public = Router::new()
        .route("/", get(public::index))
        .route("/page/{page}", get(public::index_page))
        .route("/posts/{slug}", get(public::post))
        .route("/category/{slug}", get(public::category))
        .route("/category/{slug}/page/{page}", get(public::category_page))
        .route("/tag/{slug}", get(public::tag))
        .route("/tag/{slug}/page/{page}", get(public::tag_page))
        .route("/{slug}", get(public::page))
        .route("/search", get(search::page))
        .route("/rss.xml", get(seo::rss))
        .route("/atom.xml", get(seo::atom))
        .route("/sitemap.xml", get(seo::sitemap))
        .route("/robots.txt", get(seo::robots))
        .route("/static/{*path}", get(static_files::theme_static))
        .route("/favicon.ico", get(static_files::favicon))
        .route("/media/{path}", get(media::serve))
        .route("/comments", post(comments::submit))
        .route("/plugins/{*path}", get(plugins_http::public_route))
        .fallback(public::not_found)
        .layer(middleware::from_fn_with_state(
            app.clone(),
            setup_redirect_mw,
        ));

    let api_read = Router::new()
        .route("/api/posts", get(api::list_posts))
        .route("/api/posts/{id}", get(api::get_post))
        .route("/api/pages", get(api::list_pages))
        .route("/api/categories", get(api::list_categories))
        .route("/api/tags", get(api::list_tags))
        .route("/api/search", get(search::api_search))
        .route("/api/search/suggest", get(search::api_suggest));

    let api_write = Router::new()
        .route("/api/posts", post(api::create_post))
        .route(
            "/api/posts/{id}",
            axum::routing::put(api::update_post).delete(api::delete_post),
        )
        .route("/api/comments", get(api::list_comments))
        .route("/api/cache/stats", get(api::cache_stats))
        .route("/api/cache/clear", post(api::cache_clear))
        .route(
            "/api/cache/{key}",
            axum::routing::delete(api::cache_delete_key),
        )
        .route("/api/search/status", get(search::api_status))
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            crate::auth::api_auth_mw,
        ));

    Router::new()
        .merge(public)
        .merge(api_read)
        .merge(api_write)
        .merge(media_api::router(app.clone()))
        .merge(admin::router(app.clone()))
        .merge(admin_extensions::router(app.clone()))
        .merge(admin_backup::router(app.clone()))
        .merge(admin_jobs::router(app.clone()))
        // Layer order (outermost first): log → compress → security → page cache.
        // The page cache sits innermost so stored bodies are pre-compression
        // and security headers apply to cached responses too.
        .layer(middleware::from_fn_with_state(app.clone(), page_cache_mw))
        .layer(middleware::from_fn(security_headers_mw))
        .layer(middleware::from_fn(api_error_mw))
        .layer(middleware::from_fn(compress_mw))
        .layer(middleware::from_fn(log_mw))
        .with_state(app)
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// First-run UX: a fresh instance with no user account redirects every
/// public page to the setup wizard instead of rendering an empty blog.
/// `/admin/*` already redirects through `admin_auth_mw`.
async fn setup_redirect_mw(State(app): State<App>, req: Request, next: Next) -> Response {
    if app.needs_setup() {
        return Redirect::to("/admin/setup").into_response();
    }
    next.run(req).await
}

async fn security_headers_mw(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    h.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    h.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: https:; script-src 'self'; \
             frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        ),
    );
    resp
}

// Axum extractor and routing failures must use the same API envelope.
async fn api_error_mw(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut resp = next.run(req).await;
    if api
        && (resp.status().is_client_error() || resp.status().is_server_error())
        && !resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"))
    {
        let status = resp.status();
        let bytes = serde_json::json!({"error": {
            "code": status.as_u16(),
            "message": status.canonical_reason().unwrap_or("Request failed")
        }})
        .to_string();
        *resp.body_mut() = axum::body::Body::from(bytes);
        resp.headers_mut().remove(header::CONTENT_LENGTH);
        resp.headers_mut().remove(header::CONTENT_ENCODING);
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    resp
}

async fn log_mw(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let resp = next.run(req).await;
    if !path.starts_with("/static") && !path.starts_with("/admin/static") {
        tracing::info!(
            method = %method,
            path = %path,
            status = resp.status().as_u16(),
            ms = start.elapsed().as_millis() as u64,
            "request"
        );
    }
    resp
}

// ---------------------------------------------------------------------------
// gzip compression
// ---------------------------------------------------------------------------

const MIN_COMPRESS_SIZE: usize = 512;
const MAX_BUFFER_SIZE: usize = 4 * 1024 * 1024;

fn accepts_gzip(value: &str) -> bool {
    let mut wildcard = false;
    for encoding in value.split(',') {
        let mut fields = encoding.split(';').map(str::trim);
        let name = fields.next().unwrap_or("");
        let mut quality = 1.0_f32;
        for field in fields {
            if let Some((key, value)) = field.split_once('=')
                && key.trim().eq_ignore_ascii_case("q")
            {
                quality = value.trim().parse().unwrap_or(0.0);
            }
        }
        let allowed = quality > 0.0 && quality <= 1.0;
        if name.eq_ignore_ascii_case("gzip") {
            return allowed;
        }
        if name == "*" {
            wildcard = allowed;
        }
    }
    wildcard
}

fn bounded_body(resp: &Response) -> bool {
    resp.body()
        .size_hint()
        .upper()
        .is_some_and(|n| n <= MAX_BUFFER_SIZE as u64)
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    #[tokio::test]
    async fn compression_respects_quality_ranges_and_large_bodies() {
        for (encoding, range, size, compressed) in [
            ("gzip", false, 2048, true),
            ("gzip;q=0, br", false, 2048, false),
            ("*;q=1, gzip;q=0", false, 2048, false),
            ("gzip", true, 2048, false),
            ("gzip", false, MAX_BUFFER_SIZE + 1, false),
        ] {
            let router = Router::new()
                .route("/", get(move || async move { "x".repeat(size) }))
                .layer(middleware::from_fn(compress_mw));
            let mut req = Request::builder()
                .uri("/")
                .header(header::ACCEPT_ENCODING, encoding);
            if range {
                req = req.header(header::RANGE, "bytes=0-1");
            }
            let resp = router
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), axum::http::StatusCode::OK);
            assert_eq!(
                resp.headers().contains_key(header::CONTENT_ENCODING),
                compressed
            );
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            if !compressed {
                assert_eq!(bytes.len(), size);
            }
        }
    }

    #[tokio::test]
    async fn streaming_responses_are_not_buffered_and_private_pages_are_not_cached() {
        let router = Router::new()
            .route(
                "/",
                get(|| async {
                    let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![
                        b'x';
                        MAX_BUFFER_SIZE
                            + 1
                    ])]);
                    (
                        [(header::CONTENT_TYPE, "text/plain")],
                        Body::from_stream(chunks),
                    )
                }),
            )
            .layer(middleware::from_fn(compress_mw));
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert!(!resp.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(
            axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap()
                .len(),
            MAX_BUFFER_SIZE + 1
        );
        let cache = crate::cache::CacheManager::build(&crate::config::CacheConfig::default()).await;
        for size in [100, MAX_BUFFER_SIZE + 1] {
            let mut resp =
                ([(header::CONTENT_TYPE, "text/html")], "x".repeat(size)).into_response();
            if size == 100 {
                resp.headers_mut().insert(
                    header::SET_COOKIE,
                    HeaderValue::from_static("private=value"),
                );
            }
            let fill = cache.begin_fill(ns::PAGECACHE, "private").await;
            let resp = cache_page(&cache, fill, resp).await;
            assert_eq!(
                axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .len(),
                size
            );
            assert!(
                cache
                    .get_json::<CachedPage>(ns::PAGECACHE, "private")
                    .await
                    .is_none()
            );
        }
    }
}

fn is_compressible(ct: Option<&str>) -> bool {
    let Some(ct) = ct else { return false };
    let ct = ct.split(';').next().unwrap_or("").trim();
    matches!(
        ct,
        "text/html"
            | "text/css"
            | "text/plain"
            | "text/javascript"
            | "application/javascript"
            | "application/json"
            | "application/xml"
            | "application/atom+xml"
            | "application/rss+xml"
            | "image/svg+xml"
    )
}

/// Compress compressible responses for clients that accept gzip, and mark
/// every compressible response with `Vary: Accept-Encoding` so shared caches
/// store separate representations.
async fn compress_mw(req: Request, next: Next) -> Response {
    let accepts_gzip = req
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(accepts_gzip);
    let skip =
        req.method() == axum::http::Method::HEAD || req.headers().contains_key(header::RANGE);

    let mut resp = next.run(req).await;
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !is_compressible(ct.as_deref()) {
        return resp;
    }

    // Append to an existing Vary instead of clobbering it.
    let vary = match resp
        .headers()
        .get(header::VARY)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) if v.to_ascii_lowercase().contains("accept-encoding") => None,
        Some("") => Some("Accept-Encoding".to_string()),
        Some(v) => Some(format!("{v}, Accept-Encoding")),
        None => Some("Accept-Encoding".to_string()),
    };
    if let Some(v) = vary
        && let Ok(hv) = HeaderValue::from_str(&v)
    {
        resp.headers_mut().insert(header::VARY, hv);
    }

    if skip
        || !accepts_gzip
        || resp.headers().contains_key(header::CONTENT_ENCODING)
        || resp.status().is_redirection()
        || resp.status() == axum::http::StatusCode::PARTIAL_CONTENT
        || resp.status() == axum::http::StatusCode::NO_CONTENT
        || !bounded_body(&resp)
    {
        return resp;
    }

    let (mut parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BUFFER_SIZE).await {
        Ok(b) => b,
        Err(_) => {
            // Body could not be buffered; nothing sensible to send.
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "response body unavailable",
            )
                .into_response();
        }
    };
    if bytes.len() < MIN_COMPRESS_SIZE {
        parts.headers.remove(header::CONTENT_LENGTH);
        if let Ok(hv) = HeaderValue::from_str(&bytes.len().to_string()) {
            parts.headers.insert(header::CONTENT_LENGTH, hv);
        }
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    }

    let mut enc = GzEncoder::new(Vec::new(), Compression::new(6));
    if enc.write_all(&bytes).is_err() || enc.try_finish().is_err() {
        parts.headers.remove(header::CONTENT_LENGTH);
        if let Ok(hv) = HeaderValue::from_str(&bytes.len().to_string()) {
            parts.headers.insert(header::CONTENT_LENGTH, hv);
        }
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    }
    let Ok(compressed) = enc.finish() else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };

    parts
        .headers
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    parts.headers.remove(header::CONTENT_LENGTH);
    if let Ok(hv) = HeaderValue::from_str(&compressed.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, hv);
    }
    // The compressed representation differs from the identity one, so weaken
    // the ETag (clients ignore the W/ prefix when comparing).
    if let Some(etag) = resp_etag_weak(parts.headers.get(header::ETAG)) {
        parts.headers.insert(header::ETAG, etag);
    }
    Response::from_parts(parts, axum::body::Body::from(compressed))
}

fn resp_etag_weak(etag: Option<&HeaderValue>) -> Option<HeaderValue> {
    let etag = etag?.to_str().ok()?.trim().to_string();
    if etag.starts_with("W/") {
        return None;
    }
    HeaderValue::from_str(&format!("W/{etag}")).ok()
}

// ---------------------------------------------------------------------------
// Response cache (anonymous public pages)
// ---------------------------------------------------------------------------

const CACHEABLE_PREFIXES: [&str; 4] = ["/posts/", "/category/", "/tag/", "/page/"];
const RESERVED_ROOTS: [&str; 12] = [
    "admin",
    "api",
    "static",
    "plugins",
    "comments",
    "favicon.ico",
    "rss.xml",
    "atom.xml",
    "sitemap.xml",
    "robots.txt",
    "search",
    "media",
];

fn is_cacheable_path(p: &str) -> bool {
    if p == "/" {
        return true;
    }
    if CACHEABLE_PREFIXES
        .iter()
        .any(|prefix| p.starts_with(prefix))
    {
        return true;
    }
    // Custom pages: a single path segment outside the reserved set.
    if let Some(seg) = p.strip_prefix('/')
        && !seg.is_empty()
        && !seg.contains('/')
        && !RESERVED_ROOTS.contains(&seg)
    {
        return true;
    }
    false
}

fn etag_matches(inm: &str, etag: &str) -> bool {
    inm.split(',')
        .any(|c| c.trim().trim_start_matches("W/") == etag.trim().trim_start_matches("W/"))
}

/// A rendered HTML response stored in the cache (JSON-encoded).
#[derive(Serialize, Deserialize)]
struct CachedPage {
    status: u16,
    content_type: Option<String>,
    etag: Option<String>,
    cache_control: Option<String>,
    body: String,
}

fn cached_page_response(page: CachedPage, inm: Option<&str>) -> Response {
    if let (Some(etag), Some(inm)) = (page.etag.as_deref(), inm)
        && etag_matches(inm, etag)
    {
        let mut resp = Response::builder().status(304);
        let h = resp.headers_mut().unwrap();
        if let Some(cc) = &page.cache_control
            && let Ok(hv) = HeaderValue::from_str(cc)
        {
            h.insert(header::CACHE_CONTROL, hv);
        }
        if let Ok(hv) = HeaderValue::from_str(etag) {
            h.insert(header::ETAG, hv);
        }
        return resp.body(axum::body::Body::empty()).unwrap();
    }
    let mut resp = Response::new(axum::body::Body::from(page.body));
    *resp.status_mut() =
        axum::http::StatusCode::from_u16(page.status).unwrap_or(axum::http::StatusCode::OK);
    let h = resp.headers_mut();
    if let Some(ct) = &page.content_type
        && let Ok(hv) = HeaderValue::from_str(ct)
    {
        h.insert(header::CONTENT_TYPE, hv);
    }
    if let Some(etag) = &page.etag
        && let Ok(hv) = HeaderValue::from_str(etag)
    {
        h.insert(header::ETAG, hv);
    }
    if let Some(cc) = &page.cache_control
        && let Ok(hv) = HeaderValue::from_str(cc)
    {
        h.insert(header::CACHE_CONTROL, hv);
    }
    resp
}

/// Outcome of a coalesced page load: either the leader filled the cache
/// (followers reuse it) or this request rendered the page itself.
enum PageOutcome {
    FromCache(CachedPage),
    Fresh(Response),
}

/// Response cache for anonymous GETs on public pages.
///
/// - Only `GET` + cacheable public paths (never `/admin/*`, `/api/*`).
/// - Requests carrying a session cookie bypass the shared cache entirely
///   (privacy: cached HTML must never leak between users).
/// - Cache misses are coalesced per key: one leader renders while
///   concurrent requests wait and then serve the cached result.
/// - Feeds (rss/atom/sitemap) are cached by their handlers instead — their
///   absolute URLs depend on the Host header.
async fn page_cache_mw(State(app): State<App>, req: Request, next: Next) -> Response {
    if req.method() != axum::http::Method::GET {
        return next.run(req).await;
    }
    // Logged-in visitors never touch the shared response cache.
    let has_session = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains(crate::auth::SESSION_COOKIE));
    if has_session
        || req.headers().contains_key(header::AUTHORIZATION)
        || req.uri().query().is_some()
    {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    if !is_cacheable_path(&path) {
        return next.run(req).await;
    }
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let sub = format!("{host}|{path}");
    let inm = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    if let Some(page) = app.cache.get_json::<CachedPage>(ns::PAGECACHE, &sub).await {
        return cached_page_response(page, inm.as_deref());
    }

    // Miss → single-flight. Followers re-check the cache after the leader.
    let cache = app.cache.clone();
    let cache_in = cache.clone();
    let sub_for_lock = sub.clone();
    let outcome = cache
        .coalesce(format!("{}:{sub}", ns::PAGECACHE), async move {
            if let Some(page) = cache_in
                .get_json::<CachedPage>(ns::PAGECACHE, &sub_for_lock)
                .await
            {
                return PageOutcome::FromCache(page);
            }
            let fill = cache_in.begin_fill(ns::PAGECACHE, &sub_for_lock).await;
            let resp = next.run(req).await;
            PageOutcome::Fresh(cache_page(&cache_in, fill, resp).await)
        })
        .await;

    match outcome {
        PageOutcome::FromCache(page) => cached_page_response(page, inm.as_deref()),
        PageOutcome::Fresh(resp) => resp,
    }
}

async fn cache_page(
    cache: &crate::cache::CacheManager,
    fill: crate::cache::CacheFill,
    resp: Response,
) -> Response {
    // Store fresh 200 HTML responses for anonymous visitors.
    if resp.status() != axum::http::StatusCode::OK
        || !cache.enabled()
        || !bounded_body(&resp)
        || resp.headers().contains_key(header::SET_COOKIE)
        || resp.headers().contains_key(header::VARY)
        || resp
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',').any(|d| {
                    matches!(
                        d.trim()
                            .split('=')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .as_str(),
                        "private" | "no-store" | "no-cache"
                    )
                })
            })
    {
        return resp;
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !matches!(content_type.as_deref(), Some(ct) if ct.starts_with("text/html")) {
        return resp;
    }
    let etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cache_control = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_BUFFER_SIZE).await;
    let Ok(bytes) = bytes else {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "cache error").into_response();
    };
    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        cache
            .finish_fill(
                fill,
                &CachedPage {
                    status: parts.status.as_u16(),
                    content_type,
                    etag,
                    cache_control,
                    body: text,
                },
            )
            .await;
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}

// ---------------------------------------------------------------------------
// Shared template context helpers
// ---------------------------------------------------------------------------

/// Base context for public theme pages.
pub async fn page_context(app: &App) -> Context {
    let mut ctx = Context::new();
    ctx.insert(
        "site",
        &json!({
            "title": app.site_title(),
            "description": app.site_description(),
            "base_url": app.base_url(),
            "version": env!("CARGO_PKG_VERSION"),
        }),
    );
    // Theme configuration: `{{ theme.config.site_title }}` etc. in templates.
    // Secrets are masked (null) — sensitive values never reach public pages.
    ctx.insert(
        "theme",
        &json!({
            "name": app.theme.current().display_name(),
            "config": app.theme_config_json(),
        }),
    );
    // Cache-aside for the navigation page list (rendered on every page).
    // System navigation overrides — edited in the admin navigation page:
    // a map of URL → {hidden, label}. Label overrides rename the entry,
    // `hidden` drops it from the public top bar. Keys pointing at removed
    // pages / disabled plugins simply never match and are ignored.
    let sys_overrides: serde_json::Map<String, serde_json::Value> = app
        .settings
        .get("site.navigation.system")
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let sys_hidden = |url: &str| {
        sys_overrides
            .get(url)
            .and_then(|o| o.get("hidden"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let sys_label = |url: &str, fallback: &str| -> String {
        sys_overrides
            .get(url)
            .and_then(|o| o.get("label"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| fallback.to_owned())
    };
    // Hardcoded Home entry — renamable and hideable; default labels follow
    // the site locale.
    let home_label = crate::i18n::translate(&crate::i18n::locale(), "nav.home");
    let nav_home = if sys_hidden("/") {
        None
    } else {
        Some(json!({ "label": sys_label("/", &home_label) }))
    };
    ctx.insert("nav_home", &nav_home);
    let nav_pages: Vec<serde_json::Value> = app
        .cache
        .get_or_load(ns::PAGE, "nav", async {
            crate::repositories::pages::list(&app.db, true)
                .await
                .map_err(anyhow::Error::from)
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !sys_hidden(&format!("/{}", p.slug)))
        .map(|p| {
            json!({
                "title": sys_label(&format!("/{}", p.slug), &p.title),
                "slug": p.slug,
            })
        })
        .collect();
    ctx.insert("nav_pages", &nav_pages);
    let plugin_nav: Vec<serde_json::Value> = app
        .plugins
        .nav_items()
        .into_iter()
        .filter(|(_, url)| !sys_hidden(url))
        .map(|(label, url)| json!({ "label": sys_label(&url, &label), "url": url }))
        .collect();
    ctx.insert("plugin_nav", &plugin_nav);
    let custom_nav: Vec<serde_json::Value> = app
        .settings
        .get("site.navigation")
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    ctx.insert("custom_nav", &custom_nav);
    // RSS feed link — controlled by the theme config, overridable label.
    let rss_shown = app
        .theme_config_json()
        .get("show_rss_link")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
        && !sys_hidden("/rss.xml");
    let nav_rss = if rss_shown {
        Some(json!({
            "label": sys_label("/rss.xml", &crate::i18n::translate(&crate::i18n::locale(), "nav.rss")),
            "url": "/rss.xml",
        }))
    } else {
        None
    };
    ctx.insert("nav_rss", &nav_rss);
    ctx.insert("current_year", &time::format(time::now(), "year"));
    ctx.insert("feed_url", &"/rss.xml");
    // Every key referenced by theme templates must exist; handlers override
    // this with real SEO values on pages that have them.
    ctx.insert(
        "seo",
        &json!({
            "title": "",
            "description": "",
            "canonical": "",
            "og_type": "website",
            "og_image": "",
        }),
    );
    ctx
}

/// Convert a post into the JSON shape used by theme templates.
pub fn post_to_json(post: &Post, html: Option<String>) -> serde_json::Value {
    let mut v = serde_json::json!(post);
    if let serde_json::Value::Object(o) = &mut v {
        let ts = post.published_at.unwrap_or(post.created_at);
        o.insert("url".into(), json!(format!("/posts/{}", post.slug)));
        o.insert("date".into(), json!(time::format(ts, "date")));
        o.insert("datetime".into(), json!(time::format(ts, "datetime")));
        o.insert("reading_time".into(), json!(post.reading_time()));
        o.insert(
            "author".into(),
            json!(post.author_name.clone().unwrap_or_default()),
        );
        if let Some(c) = post.category() {
            o.insert("category".into(), json!(c));
        } else {
            o.insert("category".into(), serde_json::Value::Null);
        }
        o.insert("tags".into(), json!(post.tags()));
        if let Some(h) = html {
            o.insert("content_html".into(), json!(h));
        }
        // Keep the historical API key while giving themes a semantic alias.
        o.insert("cover_image".into(), json!(post.featured_image.clone()));
    }
    v
}

/// Summary with automatic excerpt fallback.
pub fn excerpt(post: &Post, rendered_html: &str) -> String {
    if !post.summary.is_empty() {
        return post.summary.clone();
    }
    crate::markdown::truncate_chars(&crate::markdown::html_to_text(rendered_html), 220)
}

/// Render a theme template.
pub fn render_theme(app: &App, template: &str, ctx: &Context) -> AppResult<String> {
    app.theme.current().tera.render(template, ctx).map_err(|e| {
        // A theme template failure must be diagnosable: log the template
        // name and the engine error, then surface the sanitized 500.
        tracing::error!(template, error = ?e, "theme template render failed");
        crate::error::AppError::Template(e)
    })
}

/// Absolute base URL for feeds/sitemap: configured value, or derived from the
/// request Host header.
pub fn base_url_for(app: &App, headers: &axum::http::HeaderMap) -> String {
    let configured = app.base_url();
    if !configured.is_empty() {
        return configured.trim_end_matches('/').to_string();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}
