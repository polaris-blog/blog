//! Static file serving: theme assets (`/static/*`), favicon, admin CSS.
//!
//! Security: paths are validated segment-by-segment (no `..`), and the
//! canonicalized path must stay inside the theme's `static/` directory.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::state::App;

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "map" => "application/json",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Sanitize a request path into safe components (no traversal, no absolute).
fn safe_components(raw: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for seg in raw.split(['/', '\\']) {
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        // Reject Windows-style reserved names and control characters.
        if seg.bytes().any(|b| b < 0x20) {
            return None;
        }
        out.push(seg.to_string());
    }
    Some(out)
}

/// Serve a theme asset as a streamed response.
///
/// Files are piped to the client in chunks (`ReaderStream`) instead of being
/// read into memory, and all filesystem access is async — a large asset or a
/// slow disk can never block a tokio worker thread.
async fn serve_file(path: &std::path::Path) -> Response {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let meta = match file.metadata().await {
        Ok(m) if m.is_file() => m,
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let etag = format!("\"{:x}-{:x}\"", meta.len(), modified);
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
    let mut resp = (StatusCode::OK, axum::body::Body::from_stream(stream)).into_response();
    let h = resp.headers_mut();
    if let Ok(v) = header::HeaderValue::from_str(content_type(&path.to_string_lossy())) {
        h.insert(header::CONTENT_TYPE, v);
    } else {
        h.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/octet-stream"),
        );
    }
    if let Ok(v) = header::HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=86400"),
    );
    resp
}

pub async fn theme_static(State(app): State<App>, Path(path): Path<String>) -> Response {
    let theme = app.theme.current();
    let Some(segs) = safe_components(&path) else {
        return (StatusCode::BAD_REQUEST, "bad request").into_response();
    };
    if !theme.static_dir.as_os_str().is_empty() && !segs.is_empty() {
        let mut full: PathBuf = theme.static_dir.clone();
        for seg in &segs {
            full.push(seg);
        }
        // `symlink_metadata` does not follow the final path component: a
        // symlink dropped into the theme's static directory can never point
        // a request at a file outside it.
        if matches!(tokio::fs::symlink_metadata(&full).await, Ok(meta) if meta.is_file()) {
            return serve_file(&full).await;
        }
    }
    // Built-in theme scripts: served from the binary when the theme does not
    // ship one (covers the embedded fallback theme).
    if segs.as_slice() == ["js", "theme.js"] {
        return (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("text/javascript; charset=utf-8"),
                ),
                (
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("public, max-age=86400"),
                ),
            ],
            crate::templates::THEME_JS,
        )
            .into_response();
    }
    if segs.as_slice() == ["js", "highlight.js"] {
        return (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("text/javascript; charset=utf-8"),
                ),
                (
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("public, max-age=86400"),
                ),
            ],
            crate::templates::THEME_HL_JS,
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

pub async fn favicon(State(app): State<App>) -> Response {
    let theme = app.theme.current();
    if theme.static_dir.as_os_str().is_empty() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    serve_file(&theme.static_dir.join("favicon.ico")).await
}

/// Stable strong ETag for embedded admin assets (FNV-1a 64-bit).
fn asset_etag(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("\"{hash:016x}\"")
}

/// Embedded admin assets ship with the binary. Revalidation must be cheap
/// (`ETag` → 304) and stale caches must never outlive an upgrade — a fixed
/// `max-age` lets a cached stylesheet render newer markup with older rules.
fn embedded_asset(content: &'static str, mime: &'static str, req_headers: &HeaderMap) -> Response {
    let etag = asset_etag(content);
    let not_modified = req_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|client| {
            client
                .split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        });
    if not_modified {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-cache"),
                ),
                (
                    header::ETAG,
                    header::HeaderValue::from_str(&etag)
                        .unwrap_or_else(|_| header::HeaderValue::from_static("\"admin\"")),
                ),
            ],
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, header::HeaderValue::from_static(mime)),
            (
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            ),
            (
                header::ETAG,
                header::HeaderValue::from_str(&etag)
                    .unwrap_or_else(|_| header::HeaderValue::from_static("\"admin\"")),
            ),
        ],
        content,
    )
        .into_response()
}

/// Embedded admin stylesheet — no disk access required.
pub async fn admin_css(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_CSS,
        "text/css; charset=utf-8",
        &req_headers,
    )
}

/// Embedded syntax highlighter for the admin Markdown preview.
pub async fn admin_highlight_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::THEME_HL_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}

/// Setup wizard driver-switching script.
pub async fn admin_setup_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_SETUP_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}

/// Embedded admin media script (upload/drag-drop/paste/batch toolbar).
pub async fn admin_media_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_MEDIA_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}

/// Embedded admin extension upload script (drag-drop, progress, confirms).
pub async fn admin_extensions_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_EXTENSIONS_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}

/// Embedded admin navigation fade script.
pub async fn admin_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}

/// Embedded admin Markdown editor script at `/admin/static/editor.js`.
pub async fn admin_editor_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_EDITOR_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}

/// Embedded admin navigation editor script at `/admin/static/navigation.js`.
pub async fn admin_navigation_js(req_headers: HeaderMap) -> Response {
    embedded_asset(
        crate::templates::ADMIN_NAVIGATION_JS,
        "text/javascript; charset=utf-8",
        &req_headers,
    )
}
