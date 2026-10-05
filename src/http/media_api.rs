//! Media management REST API (session-authenticated).
//!
//! ```text
//! GET    /api/media                 paginated library listing
//! POST   /api/media/upload          multipart upload (1+ files)
//! POST   /api/media/batch           batch delete/move/tag
//! GET    /api/media/folders         folder list
//! POST   /api/media/folders         create folder
//! DELETE /api/media/folders/{id}    delete folder
//! GET    /api/media/{id}            detail (+ references)
//! PUT    /api/media/{id}            update metadata
//! DELETE /api/media/{id}            delete (?force=1 when referenced)
//! POST   /api/media/{id}/copy       duplicate
//! POST   /api/media/{id}/move       move to folder
//! GET    /api/media/{id}/download   attachment download
//! GET    /api/media/{id}/thumbnail  redirect to the public thumbnail URL
//! ```
//!
//! Role rules live in `services::media` (core decides): authors see and
//! manage only their own uploads; editors/admins manage everything. The
//! multipart upload endpoint checks the session CSRF token — multipart
//! bodies can be forged cross-origin, JSON bodies cannot.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::extract::{DefaultBodyLimit, Extension, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, ReadBuf};

use crate::auth::{self, AuthCtx};
use crate::error::{ApiError, AppError};
use crate::models::MediaKind;
use crate::repositories::media as repo;
use crate::services;
use crate::state::App;

pub fn router(app: App) -> Router<App> {
    // Early size rejection at the HTTP layer: the body limit sits just above
    // the configured cap so legitimate uploads (plus multipart framing)
    // pass while oversized ones are refused before any disk write.
    let cap = (app.media.config().max_upload_bytes() + 1024 * 1024) as usize;
    Router::new()
        .route("/api/media", get(list))
        .route("/api/media/upload", post(upload))
        .route("/api/media/batch", post(batch))
        .route("/api/media/folders", get(folders_list).post(folders_create))
        .route(
            "/api/media/folders/{id}",
            axum::routing::delete(folders_delete),
        )
        .route("/api/media/{id}", get(detail).put(update).delete(delete))
        .route("/api/media/{id}/copy", post(copy))
        .route("/api/media/{id}/move", post(move_item))
        .route("/api/media/{id}/download", get(download))
        .route("/api/media/{id}/thumbnail", get(thumbnail))
        .layer(DefaultBodyLimit::max(cap))
        .layer(axum::middleware::from_fn_with_state(
            app,
            crate::auth::api_auth_mw,
        ))
}

// ---------------------------------------------------------------------------
// Listing / detail
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    page: Option<i64>,
    per_page: Option<i64>,
    sort: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    /// `all` (default), an id, or `unfiled`.
    folder: Option<String>,
    tag: Option<String>,
    search: Option<String>,
    /// Restrict to the caller's own uploads (already forced for authors).
    mine: Option<bool>,
}

async fn list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let filter = parse_filter(&auth, &q)?;
    let (media, total) = services::media::list(&app, &filter).await?;
    let data: Vec<_> = media
        .iter()
        .map(|m| services::media::media_json(&app, m))
        .collect();
    Ok(Json(json!({
        "data": data,
        "meta": { "total": total, "page": filter.page, "per_page": filter.per_page }
    }))
    .into_response())
}

fn parse_filter(auth: &AuthCtx, q: &ListQuery) -> Result<repo::MediaFilter, ApiError> {
    let kind = match q.kind.as_deref() {
        None | Some("") | Some("all") => None,
        Some(k) => Some(
            MediaKind::parse(k)
                .ok_or_else(|| ApiError(AppError::BadRequest(format!("unknown type '{k}'"))))?,
        ),
    };
    let folder = match q.folder.as_deref() {
        None | Some("") | Some("all") => repo::FolderFilter::All,
        Some("unfiled") => repo::FolderFilter::Unfiled,
        Some(raw) => repo::FolderFilter::Id(
            raw.parse::<i64>()
                .map_err(|_| ApiError(AppError::BadRequest("folder must be an id".into())))?,
        ),
    };
    let sort = match q.sort.as_deref() {
        None | Some("") => repo::MediaSort::default(),
        Some(s) => repo::MediaSort::parse(s)
            .ok_or_else(|| ApiError(AppError::BadRequest(format!("unknown sort '{s}'"))))?,
    };
    let per_page = q.per_page.unwrap_or(24).clamp(1, 100);
    Ok(repo::MediaFilter {
        kind,
        folder,
        tag: q.tag.clone().filter(|t| !t.is_empty()),
        search: q.search.clone().filter(|s| !s.is_empty()),
        // Authors are always scoped to their own uploads server-side.
        uploaded_by: if !services::media::can_view_all(auth.role) || q.mine == Some(true) {
            Some(auth.user_id)
        } else {
            None
        },
        sort,
        page: q.page.unwrap_or(1).max(1),
        per_page,
    })
}

async fn detail(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    let references = repo::references_detailed(&app.db, media.id)
        .await
        .map_err(ApiError)?;
    let mut v = services::media::media_json(&app, &media);
    if let serde_json::Value::Object(o) = &mut v {
        o.insert("references".into(), json!(references));
    }
    Ok(Json(json!({ "data": v })).into_response())
}

async fn find_media(app: &App, auth: &AuthCtx, id: i64) -> Result<crate::models::Media, ApiError> {
    let Some(media) = services::media::get(app, id).await.map_err(ApiError)? else {
        return Err(ApiError(AppError::NotFound("media not found".into())));
    };
    if !services::media::can_view_all(auth.role) && media.uploaded_by != auth.user_id {
        return Err(ApiError(AppError::NotFound("media not found".into())));
    }
    Ok(media)
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Adapts a multipart field into `AsyncRead` for the streaming upload path
/// (chunks are pulled lazily; nothing is buffered beyond one chunk).
/// Shared by the REST upload endpoint and the admin no-JS fallback.
pub(super) struct FieldReader<'a> {
    field: axum::extract::multipart::Field<'a>,
    buf: axum::body::Bytes,
    pos: usize,
}

impl<'a> FieldReader<'a> {
    pub(super) fn new(field: axum::extract::multipart::Field<'a>) -> Self {
        Self {
            field,
            buf: Default::default(),
            pos: 0,
        }
    }
}

impl AsyncRead for FieldReader<'_> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.pos < this.buf.len() {
                let n = (this.buf.len() - this.pos).min(out.remaining());
                let start = this.pos;
                out.put_slice(&this.buf[start..start + n]);
                this.pos += n;
                return Poll::Ready(Ok(()));
            }
            let fut = this.field.chunk();
            match std::pin::pin!(fut).as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
                Poll::Ready(Ok(Some(bytes))) => {
                    this.buf = bytes;
                    this.pos = 0;
                }
                Poll::Ready(Ok(None)) => return Poll::Ready(Ok(())),
            }
        }
    }
}

/// `POST /api/media/upload` — multipart with one or more file fields (any
/// field name carrying a filename is treated as a file), an optional
/// `folder_id`, and the session `csrf` token.
async fn upload(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    if !services::media::can_upload(auth.role) {
        return Err(ApiError(AppError::Forbidden(
            "upload permission required".into(),
        )));
    }
    let mut csrf = String::new();
    let mut folder_id: Option<i64> = None;
    let mut uploaded: Vec<serde_json::Value> = Vec::new();
    let mut failures: Vec<serde_json::Value> = Vec::new();
    let mut any_file = false;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError(AppError::BadRequest(format!(
            "malformed multipart body: {e}"
        )))
    })? {
        let name = field.name().unwrap_or_default().to_string();
        let filename = field.file_name().map(str::to_string);
        if filename.is_none() {
            // Control field: csrf / folder_id.
            let text = field
                .text()
                .await
                .map_err(|e| ApiError(AppError::BadRequest(format!("malformed field: {e}"))))?;
            match name.as_str() {
                "csrf" => csrf = text,
                "folder_id" if !text.trim().is_empty() => {
                    folder_id = text.trim().parse().ok();
                }
                _ => {}
            }
            continue;
        }
        if csrf.is_empty() {
            return Err(ApiError(AppError::Forbidden("missing CSRF token".into())));
        }
        if let Err(e) = auth::ensure_csrf(&auth, &csrf) {
            return Err(ApiError(e));
        }
        any_file = true;
        let filename = filename.unwrap_or_default();
        let mime = field.content_type().unwrap_or_default().to_string();
        // Stream the field straight through hashing into staging — the file
        // is never buffered whole (except size-capped images).
        let mut reader = FieldReader::new(field);
        match services::media::upload_streamed(&app, auth.user_id, &filename, &mime, &mut reader)
            .await
        {
            Ok(outcome) => {
                let media = outcome.media;
                let mut v = services::media::media_json(&app, &media);
                if let serde_json::Value::Object(o) = &mut v {
                    o.insert("deduplicated".into(), json!(outcome.deduplicated));
                }
                uploaded.push(v);
                if let Some(folder) = folder_id {
                    let _ = services::media::move_to_folder(&app, &[media.id], Some(folder)).await;
                }
            }
            Err(e) => failures.push(json!({
                "filename": filename,
                "error": e.message(),
            })),
        }
    }

    if !any_file {
        return Err(ApiError(AppError::BadRequest(
            "no file fields in upload".into(),
        )));
    }
    if uploaded.is_empty() {
        return Err(ApiError(AppError::BadRequest(format!(
            "all uploads rejected: {}",
            failures
                .iter()
                .filter_map(|f| f.get("error").and_then(|e| e.as_str()))
                .collect::<Vec<_>>()
                .join("; ")
        ))));
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "data": uploaded, "rejected": failures })),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Update / delete / copy / move
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UpdateBody {
    filename: Option<String>,
    title: Option<String>,
    description: Option<String>,
    alt: Option<String>,
    caption: Option<String>,
    folder_id: Option<i64>,
    tags: Option<Vec<String>>,
}

async fn update(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &media, auth.user_id) {
        return Err(ApiError(AppError::Forbidden(
            "you may only modify your own media".into(),
        )));
    }
    // `folder_id` may be null or 0 (unfile); distinguish "absent" from "null".
    let folder_id = body.folder_id.map(|f| if f > 0 { Some(f) } else { None });
    let updated = services::media::update(
        &app,
        &media,
        services::media::MediaUpdate {
            filename: body.filename,
            title: body.title,
            description: body.description,
            alt: body.alt,
            caption: body.caption,
            folder_id,
            tags: body.tags,
        },
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(json!({ "data": services::media::media_json(&app, &updated) })).into_response())
}

#[derive(Deserialize)]
struct DeleteQuery {
    force: Option<String>,
}

async fn delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &media, auth.user_id) {
        return Err(ApiError(AppError::Forbidden(
            "you may only delete your own media".into(),
        )));
    }
    let force = matches!(q.force.as_deref(), Some("1") | Some("true") | Some("yes"));
    match services::media::delete(&app, &media, force).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(e) => Err(ApiError(e)),
    }
}

async fn copy(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &media, auth.user_id) {
        return Err(ApiError(AppError::Forbidden(
            "you may only copy your own media".into(),
        )));
    }
    let copy = services::media::copy(&app, &media, auth.user_id)
        .await
        .map_err(ApiError)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "data": services::media::media_json(&app, &copy) })),
    )
        .into_response())
}

#[derive(Deserialize)]
struct MoveBody {
    folder_id: Option<i64>,
}

async fn move_item(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Json(body): Json<MoveBody>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &media, auth.user_id) {
        return Err(ApiError(AppError::Forbidden(
            "you may only move your own media".into(),
        )));
    }
    let folder = match body.folder_id {
        None => None,
        Some(f) if f <= 0 => None,
        Some(f) => Some(
            repo::folder_find(&app.db, f)
                .await
                .map_err(ApiError)?
                .ok_or_else(|| ApiError(AppError::NotFound("folder not found".into())))?,
        ),
    };
    services::media::move_to_folder(&app, &[media.id], folder.map(|f| f.id))
        .await
        .map_err(ApiError)?;
    Ok(Json(json!({ "data": { "id": media.id, "moved": true } })).into_response())
}

// ---------------------------------------------------------------------------
// Download / thumbnail
// ---------------------------------------------------------------------------

async fn download(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    let obj = app
        .media
        .storage()
        .open(&media.storage_key)
        .await
        .map_err(ApiError)?
        .ok_or_else(|| ApiError(AppError::NotFound("file missing from storage".into())))?;
    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .body(obj.body)
        .unwrap();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&media.mime_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(&obj.size.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    h.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&media.original_filename),
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    Ok(resp)
}

/// `attachment; filename="ascii"; filename*=UTF-8''…` — ASCII fallback plus
/// RFC 5987 encoding for the real name.
fn content_disposition(filename: &str) -> header::HeaderValue {
    let safe: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = encode_rfc5987(filename);
    header::HeaderValue::from_str(&format!(
        "attachment; filename=\"{safe}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or(header::HeaderValue::from_static("attachment"))
}

fn encode_rfc5987(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
struct ThumbQuery {
    size: Option<String>,
}

async fn thumbnail(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Query(q): Query<ThumbQuery>,
) -> Result<Response, ApiError> {
    let media = find_media(&app, &auth, id).await?;
    let size = q.size.unwrap_or_else(|| {
        media
            .thumbnails
            .first()
            .cloned()
            .unwrap_or_else(|| "medium".to_string())
    });
    if media.thumbnails.contains(&size) {
        Ok(Redirect::to(&media.thumb_url_path(&size)).into_response())
    } else {
        // No generated thumbnail in that size: fall back to the original.
        Ok(Redirect::to(&media.url_path()).into_response())
    }
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

async fn folders_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
) -> Result<Response, ApiError> {
    if !services::media::can_view_all(auth.role) && !services::media::can_upload(auth.role) {
        return Err(ApiError(AppError::Forbidden(
            "media access required".into(),
        )));
    }
    let folders = services::media::folders(&app).await.map_err(ApiError)?;
    Ok(Json(json!({ "data": folders })).into_response())
}

#[derive(Deserialize)]
struct FolderBody {
    name: String,
}

async fn folders_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Json(body): Json<FolderBody>,
) -> Result<Response, ApiError> {
    if !services::media::can_manage_folders(auth.role) {
        return Err(ApiError(AppError::Forbidden("editor role required".into())));
    }
    let folder = services::media::folder_create(&app, &body.name)
        .await
        .map_err(ApiError)?;
    Ok((StatusCode::CREATED, Json(json!({ "data": folder }))).into_response())
}

async fn folders_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    if !services::media::can_manage_folders(auth.role) {
        return Err(ApiError(AppError::Forbidden("editor role required".into())));
    }
    match services::media::folder_delete(&app, id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(e) => Err(ApiError(e)),
    }
}

// ---------------------------------------------------------------------------
// Batch operations
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BatchBody {
    action: String,
    ids: Vec<i64>,
    folder_id: Option<i64>,
    tags: Option<Vec<String>>,
    force: Option<bool>,
}

async fn batch(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Json(body): Json<BatchBody>,
) -> Result<Response, ApiError> {
    if body.ids.is_empty() {
        return Err(ApiError(AppError::BadRequest(
            "ids must not be empty".into(),
        )));
    }
    if body.ids.len() > 500 {
        return Err(ApiError(AppError::BadRequest(
            "batch limited to 500 items".into(),
        )));
    }
    let action = body.action.trim().to_ascii_lowercase();
    let mut ok = 0usize;
    let mut errors: Vec<String> = Vec::new();

    match action.as_str() {
        "delete" => {
            for id in &body.ids {
                match services::media::get(&app, *id).await {
                    Ok(Some(m)) if services::media::can_modify(auth.role, &m, auth.user_id) => {
                        match services::media::delete(&app, &m, body.force == Some(true)).await {
                            Ok(()) => ok += 1,
                            Err(e) => errors.push(format!("#{}: {}", m.id, e.message())),
                        }
                    }
                    Ok(_) => errors.push(format!("#{id}: not found")),
                    Err(e) => errors.push(format!("#{id}: {}", e.message())),
                }
            }
        }
        "move" => {
            let folder = match body.folder_id {
                None => None,
                Some(f) if f <= 0 => None,
                Some(f) => Some(
                    repo::folder_find(&app.db, f)
                        .await
                        .map_err(ApiError)?
                        .ok_or_else(|| ApiError(AppError::NotFound("folder not found".into())))?,
                ),
            };
            let mut permitted = Vec::new();
            for id in &body.ids {
                match services::media::get(&app, *id).await {
                    Ok(Some(m)) if services::media::can_modify(auth.role, &m, auth.user_id) => {
                        permitted.push(*id);
                    }
                    Ok(Some(_)) => errors.push(format!("#{id}: forbidden")),
                    Ok(None) => errors.push(format!("#{id}: not found")),
                    Err(e) => errors.push(format!("#{id}: {}", e.message())),
                }
            }
            services::media::move_to_folder(&app, &permitted, folder.map(|f| f.id))
                .await
                .map_err(ApiError)?;
            ok = permitted.len();
        }
        "tag" => {
            let tags = body.tags.clone().unwrap_or_default();
            for id in &body.ids {
                match services::media::get(&app, *id).await {
                    Ok(Some(m)) if services::media::can_modify(auth.role, &m, auth.user_id) => {
                        services::media::set_tags(&app, &[*id], &tags)
                            .await
                            .map_err(ApiError)?;
                        ok += 1;
                    }
                    Ok(Some(_)) => errors.push(format!("#{id}: forbidden")),
                    Ok(None) => errors.push(format!("#{id}: not found")),
                    Err(e) => errors.push(format!("#{id}: {}", e.message())),
                }
            }
        }
        other => {
            return Err(ApiError(AppError::BadRequest(format!(
                "unknown batch action '{other}' (delete|move|tag)"
            ))));
        }
    }

    Ok(
        Json(json!({ "data": { "ok": ok, "failed": errors.len() }, "errors": errors }))
            .into_response(),
    )
}
