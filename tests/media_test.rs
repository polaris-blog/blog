//! Media system end-to-end: upload pipeline (validation, SHA-256, dedup,
//! image processing), storage abstraction, public serving (immutable cache
//! headers, ETag/304, Range), the management REST API, RBAC scoping,
//! folders/tags, post references and maintenance (orphan/cleanup/verify).

mod common;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use polaris::error::AppError;
use polaris::models::Role;
use polaris::repositories::media::{FolderFilter, MediaFilter};
use polaris::services::{media, posts};
use polaris::state::App;

/// App + admin user + seeded sample content (kept-alive temp dir).
async fn setup() -> (App, tempfile::TempDir, i64) {
    let (app, dir) = common::init_app().await;
    let admin = common::create_user(&app, "admin", "password123", Role::Admin).await;
    (app, dir, admin.id)
}

/// A real PNG of the given dimensions (larger than the default thumbnail
/// sizes so the pipeline generates variants).
fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 0x7f, 0xff])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode png");
    out
}

/// Service-level upload (streams through the same code path as HTTP).
async fn upload(
    app: &App,
    user_id: i64,
    filename: &str,
    mime: &str,
    bytes: &[u8],
) -> Result<media::UploadOutcome, AppError> {
    let mut reader = bytes;
    media::upload_streamed(app, user_id, filename, mime, &mut reader).await
}

// ---------------------------------------------------------------------------
// Upload pipeline: metadata, hashing, thumbnails
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_records_metadata_and_generates_thumbnails() {
    let (app, _dir, admin) = setup().await;
    let png = png_bytes(1200, 630);
    let out = upload(&app, admin, "cover.png", "image/png", &png)
        .await
        .unwrap();
    let m = out.media;

    assert!(!out.deduplicated);
    assert_eq!(m.filename, "cover.png");
    assert_eq!(m.mime_type, "image/png");
    assert_eq!(m.extension, "png");
    assert_eq!(m.width, Some(1200));
    assert_eq!(m.height, Some(630));
    assert_eq!(m.hash.len(), 64, "SHA-256 hex digest");
    assert_eq!(m.uploaded_by, admin);
    // Storage key is date-bucketed, never the raw filename.
    assert!(!m.storage_key.contains("cover"), "{}", m.storage_key);
    assert!(m.storage_key.ends_with(".png"), "{}", m.storage_key);

    // Public URL and storage key are decoupled.
    assert_eq!(media::url_for(&app, &m), format!("/media/{}.png", m.uuid));

    // The original plus every non-upscaled size is stored; "large" (1920px)
    // is skipped because the source is smaller.
    assert!(app.media.storage().exists(&m.storage_key).await.unwrap());
    assert_eq!(m.thumbnails, vec!["thumb", "small", "medium"]);
    for size in &m.thumbnails {
        let key = m.thumb_storage_key(size);
        let bytes = app.media.storage().read(&key).await.unwrap().unwrap();
        let thumbnail = image::load_from_memory(&bytes).unwrap();
        let ratio = thumbnail.width() as f64 / thumbnail.height() as f64;
        assert!(
            (ratio - 1200.0 / 630.0).abs() < 0.03,
            "thumbnail must preserve aspect ratio"
        );
        assert!(
            app.media.storage().exists(&key).await.unwrap(),
            "thumbnail {size} missing: {key}"
        );
    }
    assert!(
        !app.media
            .storage()
            .exists(&m.thumb_storage_key("large"))
            .await
            .unwrap(),
        "must never upscale"
    );

    // Thumbnails really are smaller images.
    let thumb = app
        .media
        .storage()
        .read(&m.thumb_storage_key("thumb"))
        .await
        .unwrap()
        .unwrap();
    let dims = image::load_from_memory(&thumb).unwrap();
    assert!(dims.width() <= 240 && dims.height() <= 240);

    // The stored original is byte-identical to the upload (PNG stays PNG
    // with no preferred_format configured).
    let stored = app
        .media
        .storage()
        .read(&m.storage_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, png);
}

#[tokio::test]
async fn identical_content_is_deduplicated() {
    let (app, _dir, admin) = setup().await;
    let png = png_bytes(64, 64);

    let first = upload(&app, admin, "one.png", "image/png", &png)
        .await
        .unwrap();
    assert!(!first.deduplicated);
    // Same bytes, different filename: the existing record is reused.
    let second = upload(&app, admin, "two.png", "image/png", &png)
        .await
        .unwrap();
    assert!(second.deduplicated, "hash match must reuse the record");
    assert_eq!(first.media.id, second.media.id);
    assert_eq!(first.media.uuid, second.media.uuid);
    assert_eq!(first.media.hash, second.media.hash);

    // Different content → a new record.
    let other = upload(&app, admin, "three.png", "image/png", &png_bytes(65, 64))
        .await
        .unwrap();
    assert!(!other.deduplicated);
    assert_ne!(first.media.id, other.media.id);

    let (items, total) = media::list(
        &app,
        &MediaFilter {
            per_page: 50,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 2);
    assert_eq!(items.len(), 2);

    let author = common::create_user(&app, "other-uploader", "password123", Role::Author).await;
    let owned = upload(&app, author.id, "my-image.png", "image/png", &png)
        .await
        .unwrap();
    assert!(!owned.deduplicated);
    assert_ne!(owned.media.id, first.media.id);
    assert_eq!(owned.media.uploaded_by, author.id);
    assert_eq!(owned.media.filename, "my-image.png");
}

#[tokio::test]
async fn non_image_files_pass_through_without_dimensions() {
    let (app, _dir, admin) = setup().await;
    let pdf = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< >>";
    let m = upload(&app, admin, "manual.pdf", "application/pdf", pdf)
        .await
        .unwrap()
        .media;
    assert_eq!(m.mime_type, "application/pdf");
    assert_eq!(m.kind().as_str(), "document");
    assert_eq!(m.width, None);
    assert_eq!(m.height, None);
    assert_eq!(m.size, pdf.len() as i64);
    assert!(m.thumbnails.is_empty());
    let stored = app
        .media
        .storage()
        .read(&m.storage_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, pdf);
}

// ---------------------------------------------------------------------------
// Validation: the upload gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uploads_are_validated_beyond_the_extension() {
    let (app, _dir, admin) = setup().await;

    // PHP payload disguised as a JPEG: magic bytes say otherwise.
    let err = upload(
        &app,
        admin,
        "evil.jpg",
        "image/jpeg",
        b"<?php system($_GET['c']); ?>",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");

    // Extension outside the allow-list.
    let err = upload(
        &app,
        admin,
        "shell.php",
        "application/x-php",
        b"<?php echo 1; ?>",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");

    // Executable disguised as a text document.
    let err = upload(
        &app,
        admin,
        "notes.txt",
        "text/plain",
        &[0x4d, 0x5a, 0x90, 0x00, 0x03, 0x00, 0x00, 0x00],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");

    // Nothing was stored and no records exist.
    assert!(app.media.storage().list("").await.unwrap().is_empty());
    let (_, total) = media::list(&app, &MediaFilter::default()).await.unwrap();
    assert_eq!(total, 0);
}

#[tokio::test]
async fn oversized_uploads_are_rejected_while_streaming() {
    // A tiny cap (defaults are 20MB/500MB — too slow to exceed in a test)
    // proves the stream aborts as soon as the limit is crossed, before any
    // validation or storage write.
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::test_config(dir.path());
    cfg.media.upload.max_file_size = "64KB".into();
    cfg.media.upload.limits = polaris::config::MediaSizeLimits {
        image: "64KB".into(),
        video: "64KB".into(),
        audio: "64KB".into(),
        document: "64KB".into(),
        archive: "64KB".into(),
        other: "64KB".into(),
    };
    let app = polaris::state::AppState::init(cfg).await.unwrap();
    let admin = common::create_user(&app, "admin", "password123", Role::Admin).await;

    // The payload has valid PNG magic bytes — only the size can reject it.
    // (The streaming cap floors at 1 MB globally, so the file must exceed that.)
    let mut huge = png_bytes(8, 8);
    huge.resize(1536 * 1024, 0);
    let err = upload(&app, admin.id, "big.png", "image/png", &huge)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::PayloadTooLarge(_)), "{err:?}");
    // Nothing was stored and no record is left behind.
    assert!(app.media.storage().list("").await.unwrap().is_empty());
    let (_, total) = media::list(&app, &MediaFilter::default()).await.unwrap();
    assert_eq!(total, 0);
}

#[tokio::test]
async fn malicious_svgs_are_sanitized_or_rejected() {
    let (app, _dir, admin) = setup().await;

    // Scripted SVG: sanitized (the payload is dropped, the graphic survives).
    let dirty = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
        <rect width="40" height="40" fill="red"/>
        <script>alert(1)</script>
        <circle onload="alert(2)" r="5" fill="blue"/>
    </svg>"##;
    let m = upload(&app, admin, "icon.svg", "image/svg+xml", dirty)
        .await
        .unwrap()
        .media;
    assert_eq!(m.mime_type, "image/svg+xml");
    let stored = app
        .media
        .storage()
        .read(&m.storage_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        m.size,
        stored.len() as i64,
        "sanitized length must match metadata"
    );
    let text = String::from_utf8(stored).unwrap();
    assert!(!text.contains("<script"), "script must be stripped");
    assert!(!text.contains("onload"), "event handlers must be stripped");
    assert!(text.contains("<rect"), "the graphic itself survives");
    let invalid = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect></svg>"#;
    assert!(
        upload(&app, admin, "broken.svg", "image/svg+xml", invalid)
            .await
            .is_err()
    );
    let staging = app.media.storage().local_root().unwrap().join(".tmp");
    assert_eq!(
        std::fs::read_dir(staging).unwrap().count(),
        0,
        "failed sanitization must clean staging"
    );

    // A text/plain payload named .svg is rejected outright.
    let err = upload(
        &app,
        admin,
        "fake.svg",
        "image/svg+xml",
        b"just text, no markup at all",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");
}

// ---------------------------------------------------------------------------
// Public serving: cache, ETag, 304, Range, traversal
// ---------------------------------------------------------------------------

use axum::response::Response;

async fn send(router: &axum::Router, req: Request) -> Response {
    router.clone().oneshot(req).await.expect("oneshot")
}

async fn get(router: &axum::Router, uri: &str) -> Response {
    send(
        router,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

async fn body(resp: Response) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn set_cookie(resp: &Response, name: &str) -> Option<String> {
    for v in resp.headers().get_all(header::SET_COOKIE) {
        let s = v.to_str().ok()?;
        if let Some(rest) = s.strip_prefix(name)
            && let Some(val) = rest.strip_prefix('=')
        {
            return Some(val.split(';').next().unwrap_or("").to_string());
        }
    }
    None
}

#[tokio::test]
async fn public_media_serving_with_cache_and_range_semantics() {
    let (app, _dir, admin) = setup().await;
    // Larger than the default thumbnail sizes so variants exist to serve.
    let png = png_bytes(1200, 600);
    let m = upload(&app, admin, "pic.png", "image/png", &png)
        .await
        .unwrap()
        .media;
    let router = polaris::http::router(app.clone());

    // Original: immutable long-cache + strong ETag.
    let url = format!("/media/{}.png", m.uuid);
    let resp = get(&router, &url).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    let etag = resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(etag, format!("\"{}\"", m.hash));
    assert_eq!(body(resp).await, png);

    // Conditional GET → 304 with the same validators.
    let req = Request::builder()
        .uri(&url)
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
        etag
    );

    // Single range → 206 with exactly the requested bytes.
    let req = Request::builder()
        .uri(&url)
        .header(header::RANGE, "bytes=10-49")
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers().get(header::CONTENT_RANGE).unwrap(),
        format!("bytes 10-49/{}", png.len()).as_str()
    );
    assert_eq!(body(resp).await, png[10..50].to_vec());

    // Suffix range (last 16 bytes).
    let req = Request::builder()
        .uri(&url)
        .header(header::RANGE, "bytes=-16")
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body(resp).await, png[png.len() - 16..].to_vec());

    // Unknowable start → 416.
    let req = Request::builder()
        .uri(&url)
        .header(header::RANGE, format!("bytes={}-", png.len() + 10))
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);

    // Thumbnail URL serves the derived object.
    let resp = get(&router, &format!("/media/{}.thumb.png", m.uuid)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!body(resp).await.is_empty());

    // A size that was never generated does not exist.
    let resp = get(&router, &format!("/media/{}.large.png", m.uuid)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hostile_media_paths_are_rejected() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "pic.png", "image/png", &png_bytes(20, 20))
        .await
        .unwrap()
        .media;
    let router = polaris::http::router(app.clone());

    // Path traversal — the URL can never steer toward a filesystem path.
    for uri in [
        "/media/..%2F..%2Fetc%2Fpasswd",
        "/media/../../../etc/passwd.png",
        "/media/....//....//etc/passwd",
        "/media/.png",
        "/media/way-too-long-uuid-that-cannot-exist-anywhere-at-all-here.png",
    ] {
        let resp = get(&router, uri).await;
        assert!(
            resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND,
            "{uri} must be rejected, got {}",
            resp.status()
        );
    }

    // Wrong extension for a known uuid: no content sniffing, straight 404.
    let resp = get(&router, &format!("/media/{}.php", m.uuid)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Unknown uuid: 404.
    let resp = get(&router, "/media/aaaaaaaa.php").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Management REST API
// ---------------------------------------------------------------------------

fn local_addr(port: u16) -> axum::extract::ConnectInfo<std::net::SocketAddr> {
    axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
}

/// Log in over HTTP and return `(session cookie, session csrf token)` —
/// the csrf is read from the media library page's meta tag.
async fn login(router: &axum::Router, username: &str, password: &str) -> (String, String) {
    let resp = get(router, "/admin/login").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let login_csrf = set_cookie(&resp, "polaris_csrf").expect("login csrf cookie");
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={login_csrf}"))
        .extension(local_addr(41000))
        .body(Body::from(format!(
            "username={username}&password={password}&csrf={login_csrf}"
        )))
        .unwrap();
    let resp = send(router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "login must succeed");
    let session = set_cookie(&resp, "polaris_session").expect("session cookie");

    let req = Request::builder()
        .uri("/admin/media")
        .header(header::COOKIE, format!("polaris_session={session}"))
        .body(Body::empty())
        .unwrap();
    let resp = send(router, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = String::from_utf8(body(resp).await).unwrap();
    let marker = "<meta name=\"csrf\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let end = start + html[start..].find('"').expect("closing quote");
    (session, html[start..end].to_string())
}

/// Build a multipart body with a `csrf` control field and one file field.
fn multipart(boundary: &str, csrf: &str, filename: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!("--{boundary}\r\ncontent-disposition: form-data; name=\"csrf\"\r\n\r\n{csrf}\r\n")
            .as_bytes(),
    );
    out.extend_from_slice(
        format!(
            "--{boundary}\r\ncontent-disposition: form-data; name=\"file\"; \
             filename=\"{filename}\"\r\ncontent-type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(data);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

async fn api_json(
    router: &axum::Router,
    session: &str,
    method: &str,
    uri: &str,
    body: Option<Vec<u8>>,
    content_type: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("polaris_session={session}"));
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    let req = builder
        .body(match body {
            Some(b) => Body::from(b),
            None => Body::empty(),
        })
        .unwrap();
    let resp = send(router, req).await;
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, v)
}

#[tokio::test]
async fn media_api_upload_list_update_delete_lifecycle() {
    let (app, _dir, _admin) = setup().await;
    let router = polaris::http::router(app.clone());
    let (session, csrf) = login(&router, "admin", "password123").await;

    // Upload over HTTP (multipart + session csrf).
    let png = png_bytes(300, 150);
    let mp = multipart("polarisboundary", &csrf, "api.png", "image/png", &png);
    let (status, v) = api_json(
        &router,
        &session,
        "POST",
        "/api/media/upload",
        Some(mp),
        Some("multipart/form-data; boundary=polarisboundary"),
    )
    .await;
    assert_eq!(status, 201, "{v}");
    let data = v["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    let id = data[0]["id"].as_i64().unwrap();
    assert_eq!(data[0]["filename"], "api.png");
    assert_eq!(data[0]["deduplicated"], false);
    assert_eq!(data[0]["kind"], "image");
    let url = data[0]["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("/media/"), "{url}");

    // Upload without a session → 401.
    let mp = multipart("polarisboundary", "irrelevant", "x.png", "image/png", &png);
    let req = Request::builder()
        .method("POST")
        .uri("/api/media/upload")
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=polarisboundary",
        )
        .body(Body::from(mp))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // The uploaded file is publicly reachable at the reported URL.
    let resp = get(&router, &url).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body(resp).await, png);

    // Listing: one item, paginated meta.
    let (status, v) = api_json(
        &router,
        &session,
        "GET",
        "/api/media?page=1&per_page=10",
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(v["meta"]["total"].as_i64().unwrap(), 1);
    assert_eq!(v["data"][0]["id"].as_i64().unwrap(), id);

    // Detail includes the reference list.
    let (status, v) = api_json(
        &router,
        &session,
        "GET",
        &format!("/api/media/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(v["data"]["references"].is_array(), "{v}");

    // Metadata update.
    let payload = serde_json::json!({
        "title": "Cover art",
        "alt": "A generated test image",
        "tags": ["rust", "screenshot"]
    });
    let (status, v) = api_json(
        &router,
        &session,
        "PUT",
        &format!("/api/media/{id}"),
        Some(serde_json::to_vec(&payload).unwrap()),
        Some("application/json"),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["data"]["title"], "Cover art");
    assert_eq!(v["data"]["alt"], "A generated test image");

    // Filter by tag finds it again.
    let (status, v) = api_json(&router, &session, "GET", "/api/media?tag=rust", None, None).await;
    assert_eq!(status, 200);
    assert_eq!(v["meta"]["total"].as_i64().unwrap(), 1);

    // Delete (unreferenced → allowed without force).
    let (status, _) = api_json(
        &router,
        &session,
        "DELETE",
        &format!("/api/media/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = api_json(
        &router,
        &session,
        "GET",
        &format!("/api/media/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
    // The public URL is gone and the storage object with it.
    let resp = get(&router, &url).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(app.media.storage().list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn api_rejects_bad_requests_and_unknown_routes() {
    let (app, _dir, _admin) = setup().await;
    let router = polaris::http::router(app.clone());
    let (session, csrf) = login(&router, "admin", "password123").await;

    // A rejected payload (PHP disguised as PNG) surfaces in `rejected`.
    let mp = multipart(
        "polarisboundary",
        &csrf,
        "evil.png",
        "image/png",
        b"<?php system('ls'); ?>",
    );
    let (status, v) = api_json(
        &router,
        &session,
        "POST",
        "/api/media/upload",
        Some(mp),
        Some("multipart/form-data; boundary=polarisboundary"),
    )
    .await;
    assert_eq!(status, 400, "{v}");

    // Wrong csrf → 403.
    let mp = multipart(
        "polarisboundary",
        "forged-token",
        "ok.png",
        "image/png",
        &png_bytes(8, 8),
    );
    let (status, _) = api_json(
        &router,
        &session,
        "POST",
        "/api/media/upload",
        Some(mp),
        Some("multipart/form-data; boundary=polarisboundary"),
    )
    .await;
    assert_eq!(status, 403);

    // Unknown kind filter → 400, not 500.
    let (status, _) = api_json(
        &router,
        &session,
        "GET",
        "/api/media?type=nonsense",
        None,
        None,
    )
    .await;
    assert_eq!(status, 400);

    // Unknown media id → 404.
    let (status, _) = api_json(&router, &session, "GET", "/api/media/9999", None, None).await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// RBAC: authors live inside their own sandbox
// ---------------------------------------------------------------------------

#[tokio::test]
async fn authors_only_see_and_manage_their_own_uploads() {
    let (app, _dir, admin) = setup().await;
    let author = common::create_user(&app, "author", "password123", Role::Author).await;

    let mine = upload(&app, author.id, "mine.png", "image/png", &png_bytes(30, 30))
        .await
        .unwrap()
        .media;
    let theirs = upload(&app, admin, "theirs.png", "image/png", &png_bytes(31, 31))
        .await
        .unwrap()
        .media;

    // Permission rules (decided by core, not by themes/plugins).
    assert!(media::can_upload(Role::Author));
    assert!(!media::can_view_all(Role::Author));
    assert!(media::can_view_all(Role::Editor));
    assert!(media::can_modify(Role::Author, &mine, author.id));
    assert!(!media::can_modify(Role::Author, &theirs, author.id));
    assert!(media::can_modify(Role::Admin, &mine, admin));

    // Author-scoped listing: only own uploads, server-side.
    let filter = MediaFilter {
        uploaded_by: Some(author.id),
        per_page: 50,
        ..Default::default()
    };
    let (items, total) = media::list(&app, &filter).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(items[0].id, mine.id);

    // Over HTTP: the author's list is scoped, the foreign item is 403.
    let router = polaris::http::router(app.clone());
    let (session, _) = login(&router, "author", "password123").await;
    let (status, v) = api_json(&router, &session, "GET", "/api/media", None, None).await;
    assert_eq!(status, 200);
    assert_eq!(v["meta"]["total"].as_i64().unwrap(), 1);
    assert_eq!(v["data"][0]["id"].as_i64().unwrap(), mine.id);

    // Foreign items answer 404, not 403 — authors must not even learn
    // that another user's media exists.
    let (status, _) = api_json(
        &router,
        &session,
        "PUT",
        &format!("/api/media/{}", theirs.id),
        Some(br#"{"title":"hijack"}"#.to_vec()),
        Some("application/json"),
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = api_json(
        &router,
        &session,
        "DELETE",
        &format!("/api/media/{}", theirs.id),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// Folders, tags, copy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn folders_are_virtual_and_move_media_without_touching_storage() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "a.png", "image/png", &png_bytes(32, 32))
        .await
        .unwrap()
        .media;
    let key_before = m.storage_key.clone();

    let folder = media::folder_create(&app, "Covers").await.unwrap();
    assert_eq!(folder.name, "Covers");
    assert!(!folder.slug.is_empty());

    // A second folder with the same name gets a deduplicated slug.
    let folder2 = media::folder_create(&app, "Covers").await.unwrap();
    assert_ne!(folder.slug, folder2.slug);

    // Moving is metadata-only: the storage key never changes.
    media::move_to_folder(&app, &[m.id], Some(folder.id))
        .await
        .unwrap();
    let moved = media::get(&app, m.id).await.unwrap().unwrap();
    assert_eq!(moved.folder_id, Some(folder.id));
    assert_eq!(moved.storage_key, key_before);

    // Folder filter + unfiled filter.
    let filter = MediaFilter {
        folder: FolderFilter::Id(folder.id),
        per_page: 50,
        ..Default::default()
    };
    let (_, total) = media::list(&app, &filter).await.unwrap();
    assert_eq!(total, 1);
    let filter = MediaFilter {
        folder: FolderFilter::Unfiled,
        per_page: 50,
        ..Default::default()
    };
    let (_, total) = media::list(&app, &filter).await.unwrap();
    assert_eq!(total, 0);

    // Deleting the folder unfiles its contents, keeps the media.
    media::folder_delete(&app, folder.id).await.unwrap();
    let m = media::get(&app, m.id).await.unwrap().unwrap();
    assert_eq!(m.folder_id, None);

    // Invalid folder names are rejected.
    assert!(media::folder_create(&app, "  ").await.is_err());
    let long = "x".repeat(81);
    assert!(media::folder_create(&app, &long).await.is_err());
}

#[tokio::test]
async fn tags_filter_and_copy_duplicaes_storage() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "orig.png", "image/png", &png_bytes(40, 40))
        .await
        .unwrap()
        .media;

    media::set_tags(&app, &[m.id], &["avatar".into(), "rust".into()])
        .await
        .unwrap();
    let tagged = media::get(&app, m.id).await.unwrap().unwrap();
    assert_eq!(tagged.tags, vec!["avatar".to_string(), "rust".to_string()]);

    // Tag filter.
    let filter = MediaFilter {
        tag: Some("rust".into()),
        per_page: 50,
        ..Default::default()
    };
    let (items, total) = media::list(&app, &filter).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(items[0].id, m.id);

    // Copy: new uuid/keys, identical bytes and metadata.
    let copy = media::copy(&app, &m, admin).await.unwrap();
    assert_ne!(copy.id, m.id);
    assert_ne!(copy.uuid, m.uuid);
    assert_ne!(copy.storage_key, m.storage_key);
    assert_eq!(copy.hash, m.hash);
    assert_eq!(copy.filename, m.filename);
    let original = app
        .media
        .storage()
        .read(&m.storage_key)
        .await
        .unwrap()
        .unwrap();
    let copied = app
        .media
        .storage()
        .read(&copy.storage_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original, copied);
    // Thumbnails were duplicated too.
    for size in &m.thumbnails {
        assert!(
            app.media
                .storage()
                .exists(&copy.thumb_storage_key(size))
                .await
                .unwrap()
        );
    }

    let (_, total) = media::list(
        &app,
        &MediaFilter {
            per_page: 50,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 2);
}

// ---------------------------------------------------------------------------
// References: posts embedding media
// ---------------------------------------------------------------------------

#[tokio::test]
async fn referenced_media_is_protected_from_accidental_deletion() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "cover.png", "image/png", &png_bytes(50, 50))
        .await
        .unwrap()
        .media;
    let missing = upload(&app, admin, "unused.png", "image/png", &png_bytes(51, 51))
        .await
        .unwrap()
        .media;

    // Saving a post that embeds the media records the reference.
    let post = posts::create_post(
        &app,
        admin,
        posts::PostInput {
            title: "With media".into(),
            summary: String::new(),
            content_md: format!("![cover](/media/{}.png)", m.uuid),
            status: polaris::models::PostStatus::Published,
            featured_image: Some(format!("/media/{}.png", m.uuid)),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let listed = media::list(
        &app,
        &MediaFilter {
            per_page: 50,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    let with_ref = listed.iter().find(|x| x.id == m.id).unwrap();
    assert_eq!(with_ref.ref_count, Some(1));

    // Deleting referenced media without force → conflict.
    let err = media::delete(&app, &m, false).await.unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "{err:?}");

    // Force delete succeeds and the post survives.
    media::delete(&app, &m, true).await.unwrap();
    assert!(media::get(&app, m.id).await.unwrap().is_none());
    assert!(
        posts::get_public_post_by_id(&app, post.id)
            .await
            .unwrap()
            .is_some()
    );

    // Unreferenced media deletes without force.
    media::delete(&app, &missing, false).await.unwrap();
    let (_, total) = media::list(&app, &MediaFilter::default()).await.unwrap();
    assert_eq!(total, 0);

    // Removing the reference (post deleted) clears the count for others.
    let m2 = upload(&app, admin, "second.png", "image/png", &png_bytes(52, 52))
        .await
        .unwrap()
        .media;
    let p2 = posts::create_post(
        &app,
        admin,
        posts::PostInput {
            title: "Ref then drop".into(),
            summary: String::new(),
            content_md: format!("![](/media/{}.png)", m2.uuid),
            status: polaris::models::PostStatus::Draft,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let listed = media::list(
        &app,
        &MediaFilter {
            per_page: 50,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    assert_eq!(
        listed.iter().find(|x| x.id == m2.id).unwrap().ref_count,
        Some(1)
    );
    posts::delete_post(&app, p2.id).await.unwrap();
    let listed = media::list(
        &app,
        &MediaFilter {
            per_page: 50,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    assert_eq!(
        listed.iter().find(|x| x.id == m2.id).unwrap().ref_count,
        Some(0)
    );
}

#[tokio::test]
async fn featured_media_object_is_built_for_themes() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "hero.png", "image/png", &png_bytes(1200, 600))
        .await
        .unwrap()
        .media;
    let url = format!("/media/{}.png", m.uuid);

    let featured = media::featured_media(&app, &url)
        .await
        .expect("featured object");
    assert_eq!(featured["url"], url);
    assert_eq!(featured["width"], 1200);
    assert_eq!(featured["height"], 600);
    assert_eq!(featured["is_image"], true);
    // srcset only advertises sizes that were actually generated.
    let srcset = featured["srcset"].as_str().unwrap();
    assert!(srcset.contains("thumb"), "{srcset}");
    assert!(srcset.contains("/media/"), "{srcset}");
    assert!(
        !srcset.contains("large"),
        "must not advertise ungenerated sizes"
    );

    // External/unknown URLs resolve to nothing (no fallback guessing).
    assert!(
        media::featured_media(&app, "https://example.com/x.png")
            .await
            .is_none()
    );
    assert!(
        media::featured_media(&app, "/media/nope1234.png")
            .await
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Search integration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn media_search_finds_uploads_by_metadata() {
    let (app, _dir, admin) = setup().await;
    let m = upload(
        &app,
        admin,
        "rust-logo.png",
        "image/png",
        &png_bytes(60, 60),
    )
    .await
    .unwrap()
    .media;
    upload(
        &app,
        admin,
        "unrelated.png",
        "image/png",
        &png_bytes(61, 61),
    )
    .await
    .unwrap();

    let (hits, total) = media::search(&app, "rust", 1, 20).await.unwrap();
    assert_eq!(total, 1, "only the rust upload matches");
    assert_eq!(hits[0].id, m.id);

    // Metadata updates are searchable too.
    media::update(
        &app,
        &m,
        media::MediaUpdate {
            filename: None,
            title: Some("Ferris the crab".into()),
            description: None,
            alt: None,
            caption: None,
            folder_id: None,
            tags: None,
        },
    )
    .await
    .unwrap();
    let (hits, total) = media::search(&app, "ferris", 1, 20).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(hits[0].id, m.id);

    // Media never leaks into the public site search.
    let public = app
        .search
        .search(
            &app,
            polaris::search::SearchQuery {
                query: "rust".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        public.results.iter().all(|r| r.kind != "media"),
        "public search must not mix in library items"
    );
}

// ---------------------------------------------------------------------------
// Maintenance: orphan detection, cleanup, verification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn orphan_cleanup_defaults_to_dry_run() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "kept.png", "image/png", &png_bytes(70, 70))
        .await
        .unwrap()
        .media;

    // A storage object with no record.
    app.media
        .storage()
        .put("2099/12/stray.bin", b"junk")
        .await
        .unwrap();

    let report = media::orphan_report(&app).await.unwrap();
    assert!(report.records.is_empty(), "every record has its object");
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].key, "2099/12/stray.bin");

    // Dry run: reports, deletes nothing.
    let would = media::cleanup_orphans(&app, false).await.unwrap();
    assert_eq!(would, vec!["2099/12/stray.bin".to_string()]);
    assert!(
        app.media
            .storage()
            .exists("2099/12/stray.bin")
            .await
            .unwrap()
    );

    // Apply: only the orphan goes away; real media is untouched.
    media::cleanup_orphans(&app, true).await.unwrap();
    assert!(
        !app.media
            .storage()
            .exists("2099/12/stray.bin")
            .await
            .unwrap()
    );
    assert!(app.media.storage().exists(&m.storage_key).await.unwrap());
    assert!(media::get(&app, m.id).await.unwrap().is_some());
}

#[tokio::test]
async fn verify_detects_missing_objects_and_records_them_as_orphans() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "gone.png", "image/png", &png_bytes(80, 80))
        .await
        .unwrap()
        .media;

    // Healthy state.
    assert!(media::verify(&app, false).await.unwrap().is_empty());

    // Someone deletes the object behind the database's back.
    app.media.storage().delete(&m.storage_key).await.unwrap();

    let issues = media::verify(&app, false).await.unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].media_id, m.id);
    assert!(
        issues[0].problem.contains("missing"),
        "{}",
        issues[0].problem
    );

    // The same record shows up as an orphan record.
    let report = media::orphan_report(&app).await.unwrap();
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.records[0].id, m.id);
}

#[tokio::test]
async fn deleting_media_removes_every_derived_object() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "full.png", "image/png", &png_bytes(1200, 900))
        .await
        .unwrap()
        .media;
    assert!(!m.thumbnails.is_empty());

    media::delete(&app, &m, false).await.unwrap();
    let keys = app.media.storage().list("").await.unwrap();
    assert!(
        keys.is_empty(),
        "originals and thumbnails must all be removed: {keys:?}"
    );
}

/// Regression: the uuid→record lookup is cached (it sits on the
/// `/media/{uuid}` serving path and the per-post `featured_media` N+1).
/// Cached records must never outlive a metadata change or a deletion —
/// the `MEDIA` namespace is invalidated wholesale on every mutation.
#[tokio::test]
async fn uuid_lookup_cache_stays_consistent_with_mutations() {
    let (app, _dir, admin) = setup().await;
    let m = upload(&app, admin, "cached.png", "image/png", &png_bytes(600, 400))
        .await
        .unwrap()
        .media;

    // Warm the uuid cache.
    let hit = media::get_by_uuid(&app, &m.uuid).await.unwrap().unwrap();
    assert_eq!(hit.id, m.id);

    // A metadata change invalidates the cached record.
    media::update(
        &app,
        &hit,
        media::MediaUpdate {
            filename: None,
            title: Some("Renamed".into()),
            description: None,
            alt: None,
            caption: None,
            folder_id: None,
            tags: None,
        },
    )
    .await
    .unwrap();
    let fresh = media::get_by_uuid(&app, &m.uuid).await.unwrap().unwrap();
    assert_eq!(fresh.title, "Renamed", "stale uuid cache after update");

    // Deletion removes it from both the database and the cache.
    media::delete(&app, &fresh, false).await.unwrap();
    assert!(media::get_by_uuid(&app, &m.uuid).await.unwrap().is_none());
}
