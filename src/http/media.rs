//! Public media serving: `/media/{uuid}.{ext}` and `/media/{uuid}.{size}.{ext}`.
//!
//! The URL identifies the *record*; the storage key always comes from the
//! database — a hostile URL can never steer the reader toward a filesystem
//! path (traversal is doubly blocked by the storage layer's key resolution).
//!
//! Caching: URLs embed an immutable per-upload uuid, so responses carry
//! `Cache-Control: public, max-age=31536000, immutable` plus a strong
//! content-hash ETag (`/media` never enters the HTML response cache — the
//! bytes stream straight from storage). Single-range `Range` requests are
//! honored so audio/video can seek.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::models::Media;
use crate::services;
use crate::state::App;

/// Parsed `/media/` path: `(uuid, size, ext)`.
struct MediaPath<'a> {
    uuid: &'a str,
    size: Option<&'a str>,
    ext: &'a str,
}

fn parse_media_path(path: &str) -> Option<MediaPath<'_>> {
    let is_token =
        |s: &str| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric());
    let (uuid, rest) = path.split_once('.')?;
    if !is_token(uuid) {
        return None;
    }
    match rest.split_once('.') {
        // `uuid.size.ext` — a thumbnail variant.
        Some((size, ext)) if is_token(size) && is_token(ext) => Some(MediaPath {
            uuid,
            size: Some(size),
            ext,
        }),
        // `uuid.ext` — the original.
        _ if is_token(rest) => Some(MediaPath {
            uuid,
            size: None,
            ext: rest,
        }),
        _ => None,
    }
}

/// A parsed `Range: bytes=…` header (single range only; multi-range falls
/// back to a full 200 response).
struct ByteRange {
    start: u64,
    /// Exclusive end.
    end: u64,
}

fn parse_range(value: &str, size: u64) -> Option<ByteRange> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // multi-range unsupported: serve the whole object
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        // Suffix range: the last N bytes.
        let n: u64 = end.parse().ok()?;
        let n = n.min(size);
        return Some(ByteRange {
            start: size - n,
            end: size,
        });
    }
    let start: u64 = start.parse().ok()?;
    if start >= size {
        return None;
    }
    let end = match end.parse::<u64>() {
        Ok(e) => e.min(size - 1) + 1,
        Err(_) => size, // open-ended: bytes=N-
    };
    Some(ByteRange {
        start,
        end: end.max(start),
    })
}

fn etag_matches(inm: &str, etag: &str) -> bool {
    inm.split(',')
        .any(|c| c.trim() == etag || c.trim() == format!("W/{etag}"))
}

fn not_modified(etag: &str, cache_control: &str) -> Response {
    let mut resp = Response::builder().status(StatusCode::NOT_MODIFIED);
    let h = resp.headers_mut().unwrap();
    if let Ok(v) = header::HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    if let Ok(v) = header::HeaderValue::from_str(cache_control) {
        h.insert(header::CACHE_CONTROL, v);
    }
    resp.body(axum::body::Body::empty()).unwrap()
}

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

pub async fn serve(
    State(app): State<App>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !app.media.enabled() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let Some(p) = parse_media_path(&path) else {
        return (StatusCode::BAD_REQUEST, "bad request").into_response();
    };
    let media = match services::media::get_by_uuid(&app, p.uuid).await {
        Ok(Some(m)) => m,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.message()).into_response(),
    };
    if media.extension != p.ext {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    // Resolve the storage key from the record — thumbnails only when the
    // size was actually generated for this item.
    let (key, etag) = match p.size {
        None => (media.storage_key.clone(), format!("\"{}\"", media.hash)),
        Some(size) if media.thumbnails.iter().any(|t| t == size) => {
            // Thumbnails are derived deterministically from the same upload
            // (uuid-immutable), so hash+size is a stable strong ETag.
            (
                media.thumb_storage_key(size),
                format!("\"{}-{size}\"", media.hash),
            )
        }
        Some(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };

    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if inm.is_some_and(|inm| etag_matches(&inm, &etag)) {
        return not_modified(&etag, IMMUTABLE);
    }

    // Range request: stream just the requested bytes (206).
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(spec) = range {
        return range_response(&app, &media, &key, &etag, &spec).await;
    }

    // Full response.
    let obj = match app.media.storage().open(&key).await {
        Ok(Some(o)) => o,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.message()).into_response(),
    };
    full_response(&media, &etag, obj.size, obj.body)
}

async fn range_response(app: &App, media: &Media, key: &str, etag: &str, spec: &str) -> Response {
    let Ok(total) = app
        .media
        .storage()
        .size_of(key)
        .await
        .map_err(|e| e.message())
    else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
    };
    let Some(total) = total else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Some(r) = parse_range(spec, total) else {
        let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
        if let Ok(v) = header::HeaderValue::from_str(&format!("bytes */{total}")) {
            resp.headers_mut().insert(header::CONTENT_RANGE, v);
        }
        return resp;
    };
    let obj = match app.media.storage().open_range(key, r.start, r.end).await {
        Ok(Some(o)) => o,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.message()).into_response(),
    };
    let mut resp = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .body(obj.body)
        .unwrap();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&media.mime_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    if let Ok(v) =
        header::HeaderValue::from_str(&format!("bytes {}-{}/{}", r.start, r.end - 1, total))
    {
        h.insert(header::CONTENT_RANGE, v);
    }
    h.insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&obj.size.to_string()).unwrap(),
    );
    h.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    if let Ok(v) = header::HeaderValue::from_str(IMMUTABLE) {
        h.insert(header::CACHE_CONTROL, v);
    }
    resp
}

fn full_response(media: &Media, etag: &str, size: u64, body: axum::body::Body) -> Response {
    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .unwrap();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&media.mime_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&size.to_string()).unwrap(),
    );
    h.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    if let Ok(v) = header::HeaderValue::from_str(IMMUTABLE) {
        h.insert(header::CACHE_CONTROL, v);
    }
    if let Ok(v) =
        header::HeaderValue::from_str(&crate::utils::time::format(media.created_at, "http"))
    {
        h.insert(header::LAST_MODIFIED, v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parsing() {
        let p = parse_media_path("abc12345.webp").unwrap();
        assert_eq!((p.uuid, p.size, p.ext), ("abc12345", None, "webp"));
        let p = parse_media_path("abc12345.small.webp").unwrap();
        assert_eq!((p.uuid, p.size, p.ext), ("abc12345", Some("small"), "webp"));
        assert!(parse_media_path("no-extension").is_none());
        assert!(parse_media_path("../etc/passwd").is_none());
        assert!(parse_media_path("..").is_none());
        assert!(parse_media_path("a.b.c.d").is_none());
        assert!(parse_media_path("with space.webp").is_none());
    }

    #[test]
    fn range_parsing() {
        assert_eq!(
            parse_range("bytes=0-499", 1000).map(|r| (r.start, r.end)),
            Some((0, 500))
        );
        assert_eq!(
            parse_range("bytes=500-", 1000).map(|r| (r.start, r.end)),
            Some((500, 1000))
        );
        assert_eq!(
            parse_range("bytes=-200", 1000).map(|r| (r.start, r.end)),
            Some((800, 1000))
        );
        // End clamped to size.
        assert_eq!(
            parse_range("bytes=0-99999", 1000).map(|r| (r.start, r.end)),
            Some((0, 1000))
        );
        // Beyond the end: unsatisfiable.
        assert!(parse_range("bytes=1000-", 1000).is_none());
        // Multi-range and malformed values: full response.
        assert!(parse_range("bytes=0-1,5-9", 1000).is_none());
        assert!(parse_range("chunks=0-1", 1000).is_none());
        assert!(parse_range("bytes=abc-def", 1000).is_none());
    }

    #[test]
    fn etag_comparison() {
        assert!(etag_matches("\"abc\"", "\"abc\""));
        assert!(etag_matches("\"x\", \"abc\"", "\"abc\""));
        assert!(etag_matches("W/\"abc\"", "\"abc\""));
        assert!(!etag_matches("\"abcd\"", "\"abc\""));
    }
}
