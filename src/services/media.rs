//! Media service — the orchestration layer between HTTP and the media
//! subsystem. Everything here composes: validation → storage → database →
//! search index → cache invalidation.

use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::cache::ns;
use crate::error::{AppError, AppResult};
use crate::media::{image as imgproc, validate};
use crate::models::{Media, MediaFolder, MediaKind, Role};
use crate::repositories::media as repo;
use crate::state::App;
use crate::utils::{cookies, slug};

/// Outcome of an upload: the (possibly pre-existing) record and whether the
/// content hash matched an earlier upload.
#[derive(Clone, Debug)]
pub struct UploadOutcome {
    pub media: Media,
    pub deduplicated: bool,
}

/// Media permission check. Role rules are centralized here (core decides,
/// not the theme/plugin):
/// - Admin: everything, all media.
/// - Editor: upload, update, delete, view all.
/// - Author: upload and manage own uploads only (the library list is
///   scoped server-side via `MediaFilter::uploaded_by`).
pub fn can_upload(role: Role) -> bool {
    matches!(role, Role::Admin | Role::Editor | Role::Author)
}

pub fn can_view_all(role: Role) -> bool {
    matches!(role, Role::Admin | Role::Editor)
}

pub fn can_modify(role: Role, media: &Media, user_id: i64) -> bool {
    match role {
        Role::Admin | Role::Editor => true,
        Role::Author => media.uploaded_by == user_id,
    }
}

pub fn can_manage_folders(role: Role) -> bool {
    matches!(role, Role::Admin | Role::Editor)
}

/// Upload from an async reader (multipart fields are wrapped into one by
/// the HTTP layer). Streams through SHA-256 into a staging file — the
/// content is never buffered whole except for images (size-capped) and SVGs.
pub async fn upload_streamed<R: AsyncRead + Unpin>(
    app: &App,
    uploader_id: i64,
    filename: &str,
    declared_mime: &str,
    reader: &mut R,
) -> AppResult<UploadOutcome> {
    let staged = app.media.storage().staging_file()?;
    let result = upload_staged(app, uploader_id, filename, declared_mime, reader, &staged).await;
    tokio::fs::remove_file(&staged).await.ok();
    result
}

async fn upload_staged<R: AsyncRead + Unpin>(
    app: &App,
    uploader_id: i64,
    filename: &str,
    declared_mime: &str,
    reader: &mut R,
    staged: &std::path::Path,
) -> AppResult<UploadOutcome> {
    let cfg = app.media.config();
    let cap = cfg.max_upload_bytes();

    let (head, total) = stream_to_staging(reader, staged, cap).await?;

    // Content-based validation (extension + magic bytes + limits).
    let validated = validate::validate(cfg, filename, &head, total, declared_mime)?;

    // Post-process content: SVG sanitization and image conversion produce
    // the final bytes to store.
    let processed: ProcessedUpload = match validated.kind {
        MediaKind::Image if validated.mime == "image/svg+xml" => {
            sanitize_svg_upload(app, staged).await?
        }
        MediaKind::Image => process_image_upload(app, staged, &validated.extension).await?,
        _ => ProcessedUpload::Passthrough,
    };

    // Content hash of the *stored* bytes (dedup + ETag source). Images hash
    // their processed buffer; everything else streams the staged file.
    let hash = match &processed {
        ProcessedUpload::Image { out } => {
            let mut h = Sha256::new();
            h.update(&out.bytes);
            format!("{:x}", h.finalize())
        }
        ProcessedUpload::Passthrough => hash_staged(staged).await?,
    };

    // Deduplication: identical content reuses the existing record.
    if cfg.upload.deduplicate
        && let Some(existing) = repo::find_by_hash_for_user(&app.db, &hash, uploader_id).await?
        && let Some(existing) = repo::find_by_id(&app.db, existing.id).await?
    {
        return Ok(UploadOutcome {
            media: existing,
            deduplicated: true,
        });
    }

    let uuid = cookies::random_token(8);
    let (storage_ext, mime, size, width, height, thumbnails) = match &processed {
        ProcessedUpload::Image { out } => (
            out.extension.clone(),
            out.mime.clone(),
            out.bytes.len() as i64,
            out.width,
            out.height,
            out.thumbnails
                .iter()
                .map(|t| t.size_name.clone())
                .collect::<Vec<_>>(),
        ),
        ProcessedUpload::Passthrough => {
            // SVGs get best-effort dimensions; other formats have none
            // without decoding (video/audio duration needs ffprobe — a
            // plugin concern, not core).
            let dims = if validated.mime == "image/svg+xml" {
                imgproc::svg_dimensions(&tokio::fs::read(&staged).await?)
            } else {
                (None, None)
            };
            (
                validated.extension.clone(),
                validated.mime.to_string(),
                tokio::fs::metadata(staged).await?.len() as i64,
                dims.0,
                dims.1,
                Vec::new(),
            )
        }
    };

    let storage_key = app.media.storage_key(&uuid, &storage_ext);
    match &processed {
        ProcessedUpload::Image { out } => {
            app.media.storage().put(&storage_key, &out.bytes).await?;
            for t in &out.thumbnails {
                let key = thumbnail_key(&storage_key, &t.size_name);
                app.media.storage().put(&key, &t.bytes).await?;
            }
        }
        ProcessedUpload::Passthrough => {
            app.media.storage().put_staged(&storage_key, staged).await?;
        }
    }
    tokio::fs::remove_file(&staged).await.ok();

    let clean_name = validated.filename.clone();
    let new = repo::NewMedia {
        uuid: uuid.clone(),
        filename: clean_name.clone(),
        original_filename: clean_name,
        storage_key,
        mime_type: mime,
        extension: storage_ext,
        size,
        width,
        height,
        duration: None, // video/audio durations need ffprobe: plugin territory
        hash,
        thumbnails,
        folder_id: None,
        uploaded_by: uploader_id,
    };
    let id = repo::insert(&app.db, &new).await?;
    let Some(media) = repo::find_by_id(&app.db, id).await? else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "media row vanished after insert"
        )));
    };
    index_media(app, &media).await;
    invalidate_media_caches(app).await;
    Ok(UploadOutcome {
        media,
        deduplicated: false,
    })
}

enum ProcessedUpload {
    /// Raster image: converted original + thumbnails in memory.
    Image { out: Box<imgproc::ProcessedImage> },
    /// Non-image (or sanitized SVG): staged file holds the final bytes.
    Passthrough,
}

/// Stream a reader into the staging file while hashing and capturing the
/// head bytes. Rejects as soon as the global cap is exceeded.
async fn stream_to_staging<R: AsyncRead + Unpin>(
    reader: &mut R,
    staged: &std::path::Path,
    cap: u64,
) -> AppResult<(Vec<u8>, u64)> {
    let mut file = tokio::fs::File::create(staged).await?;
    let mut head: Vec<u8> = Vec::with_capacity(1024);
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > cap {
            return Err(AppError::PayloadTooLarge(format!(
                "upload exceeds the maximum size of {} bytes",
                cap
            )));
        }
        if head.len() < 1024 {
            let need = (1024 - head.len()).min(n);
            head.extend_from_slice(&buf[..need]);
        }
        file.write_all(&buf[..n]).await?;
    }
    file.flush().await?;
    drop(file);
    if total == 0 {
        return Err(AppError::BadRequest("empty upload".into()));
    }
    Ok((head, total))
}

async fn hash_staged(staged: &std::path::Path) -> AppResult<String> {
    let mut file = tokio::fs::File::open(staged).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Sanitize an SVG upload in place (staging file is rewritten). Fails closed.
async fn sanitize_svg_upload(app: &App, staged: &std::path::Path) -> AppResult<ProcessedUpload> {
    let cfg = &app.media.config().svg;
    let raw = tokio::fs::read(staged).await?;
    if !cfg.enabled {
        return Err(AppError::BadRequest("SVG uploads are disabled".into()));
    }
    let clean = if cfg.sanitize {
        validate::sanitize_svg(cfg, &raw).ok_or_else(|| {
            AppError::BadRequest("SVG could not be sanitized safely (rejected)".into())
        })?
    } else {
        raw
    };
    tokio::fs::write(staged, &clean).await?;
    Ok(ProcessedUpload::Passthrough)
}

/// Process a raster image upload: EXIF orientation, optional conversion,
/// thumbnails. Images are capped by the image size limit, so buffering the
/// staged file is bounded.
async fn process_image_upload(
    app: &App,
    staged: &std::path::Path,
    ext: &str,
) -> AppResult<ProcessedUpload> {
    let mcfg = app.media.config();
    let Some(format) = imgproc::RasterFormat::from_extension(ext) else {
        // AVIF (and other accepted-but-undecodable formats): stored as-is.
        return Ok(ProcessedUpload::Passthrough);
    };
    let permit = app
        .media
        .image_workers
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let bytes = tokio::fs::read(staged).await?;
    let images = mcfg.images.clone();
    let exif = mcfg.exif.clone();
    let processed = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        imgproc::process(&images, &exif, &bytes, format)
    })
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    match processed {
        Some(out) => Ok(ProcessedUpload::Image { out: Box::new(out) }),
        // Decoding failed (corrupt or unsupported): the magic bytes matched,
        // so keep the original — metadata fields stay empty.
        None => Ok(ProcessedUpload::Passthrough),
    }
}

fn thumbnail_key(storage_key: &str, size: &str) -> String {
    match storage_key.rsplit_once('.') {
        Some((base, ext)) => format!("{base}.{size}.{ext}"),
        None => format!("{storage_key}.{size}"),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Get a media record by id (cache-aside on the metadata only — never the
/// binary content).
pub async fn get(app: &App, id: i64) -> AppResult<Option<Media>> {
    if let Some(hit) = app
        .cache
        .get_json::<Media>(ns::MEDIA, &format!("id:{id}"))
        .await
    {
        return Ok(Some(hit));
    }
    let fill = app.cache.begin_fill(ns::MEDIA, &format!("id:{id}")).await;
    let Some(m) = repo::find_by_id(&app.db, id).await? else {
        return Ok(None);
    };
    app.cache.finish_fill(fill, &m).await;
    Ok(Some(m))
}

/// Look a media record up by public uuid (cache-aside on the metadata).
///
/// This is on two hot paths — serving `/media/{uuid}` and resolving
/// `featured_image` for every rendered post — so hits must not touch the
/// database. The `MEDIA` namespace is invalidated wholesale on every media
/// mutation, so cached records can never outlive a change.
pub async fn get_by_uuid(app: &App, uuid: &str) -> AppResult<Option<Media>> {
    let sub = format!("uuid:{uuid}");
    if let Some(hit) = app.cache.get_json::<Media>(ns::MEDIA, &sub).await {
        return Ok(Some(hit));
    }
    let fill = app.cache.begin_fill(ns::MEDIA, &sub).await;
    let Some(m) = repo::find_by_uuid(&app.db, uuid).await? else {
        return Ok(None);
    };
    app.cache.finish_fill(fill, &m).await;
    Ok(Some(m))
}

pub async fn list(app: &App, f: &repo::MediaFilter) -> AppResult<(Vec<Media>, i64)> {
    repo::list(&app.db, f).await
}

pub async fn folders(app: &App) -> AppResult<Vec<MediaFolder>> {
    repo::folders(&app.db).await
}

/// Search media through the Polaris search system (FTS-backed, restricted
/// to `ref_type = 'media'` rows; never mixed into public site search).
pub async fn search(
    app: &App,
    query: &str,
    page: i64,
    per_page: i64,
) -> AppResult<(Vec<Media>, i64)> {
    let q = crate::search::SearchQuery {
        query: query.to_string(),
        page: page.clamp(1, u32::MAX as i64) as u32,
        per_page: per_page.clamp(1, 100) as u32,
        kind: Some(crate::search::SearchKind::Media),
        include_hidden: true,
        ..Default::default()
    };
    let resp = app.search.search(app, q).await?;
    // Resolve ids back to full media records (search rows are denormalized).
    let ids: Vec<_> = resp.results.iter().map(|r| r.id).collect();
    let mut records: std::collections::HashMap<_, _> = repo::find_by_ids(&app.db, &ids)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    let out = ids
        .into_iter()
        .filter_map(|id| records.remove(&id))
        .collect();
    Ok((out, resp.total))
}

// ---------------------------------------------------------------------------
// URL building (storage key ↔ public URL stay decoupled)
// ---------------------------------------------------------------------------

/// Absolute-or-relative public URL for the original, honoring the CDN
/// configuration.
pub fn url_for(app: &App, media: &Media) -> String {
    let path = media.url_path();
    prefix_url(app, &path)
}

pub fn thumb_url_for(app: &App, media: &Media, size: &str) -> String {
    prefix_url(app, &media.thumb_url_path(size))
}

fn prefix_url(app: &App, path: &str) -> String {
    let mcfg = app.media.config();
    if mcfg.cdn.enabled && !mcfg.cdn.url.is_empty() {
        format!(
            "{}/{}",
            mcfg.cdn.url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    } else {
        path.to_string()
    }
}

/// Responsive `srcset` value for an image with generated thumbnails.
pub fn srcset_for(app: &App, media: &Media) -> String {
    let mcfg = app.media.config();
    let mut parts = Vec::new();
    for (name, px) in mcfg.images.sizes.as_pairs() {
        if media.thumbnails.iter().any(|t| t == name) {
            parts.push(format!("{} {}w", thumb_url_for(app, media, name), px));
        }
    }
    parts.join(", ")
}

// ---------------------------------------------------------------------------
// Theme integration
// ---------------------------------------------------------------------------

/// Theme-facing media object: responsive URLs + metadata, so templates can do
/// `<img src="{{ p.featured_media.url }}" srcset="{{ p.featured_media.srcset }}">`
/// instead of hand-building variant URLs.
fn theme_media_json(app: &App, m: &Media) -> serde_json::Value {
    let mut v = json!({
        "id": m.id,
        "uuid": m.uuid,
        "filename": m.filename,
        "url": url_for(app, m),
        "srcset": srcset_for(app, m),
        "alt": m.alt,
        "title": m.title,
        "caption": m.caption,
        "width": m.width,
        "height": m.height,
        "mime_type": m.mime_type,
        "is_image": m.kind() == MediaKind::Image,
    });
    if let serde_json::Value::Object(o) = &mut v {
        for size in &m.thumbnails {
            o.insert(format!("url_{size}"), json!(thumb_url_for(app, m, size)));
        }
    }
    v
}

/// Extract the media uuid from a `/media/{uuid}.ext` URL (bare uuids and
/// CDN-prefixed URLs both work).
fn uuid_from_url(url: &str) -> Option<String> {
    let idx = url.find("/media/")?;
    let rest = &url[idx + 7..];
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    let uuid = token.split('.').next()?.to_string();
    (uuid.len() >= 8).then_some(uuid)
}

/// Resolve a `featured_image` URL backed by the media library into the theme
/// media object. `None` when the URL is external or unknown.
pub async fn featured_media(app: &App, url: &str) -> Option<serde_json::Value> {
    let uuid = uuid_from_url(url)?;
    let m = get_by_uuid(app, &uuid).await.ok()??;
    Some(theme_media_json(app, &m))
}

/// Attach `featured_media` objects to serialized posts/pages whose
/// `featured_image` references the media library (cache-aside lookups).
pub async fn attach_featured_media(app: &App, posts: &mut [serde_json::Value]) {
    for p in posts.iter_mut() {
        let Some(obj) = p.as_object_mut() else {
            continue;
        };
        let Some(url) = obj
            .get("featured_image")
            .and_then(|v| v.as_str())
            .filter(|u| !u.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        if let Some(media) = featured_media(app, &url).await {
            obj.insert("featured_media".to_string(), media);
        }
    }
}

// ---------------------------------------------------------------------------
// Mutation
// ---------------------------------------------------------------------------

pub struct MediaUpdate {
    pub filename: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub alt: Option<String>,
    pub caption: Option<String>,
    pub folder_id: Option<Option<i64>>,
    pub tags: Option<Vec<String>>,
}

pub async fn update(app: &App, media: &Media, u: MediaUpdate) -> AppResult<Media> {
    if let Some(filename) = u.filename.as_deref() {
        let f = validate::clean_filename(filename)
            .filter(|f| !f.is_empty())
            .ok_or_else(|| AppError::BadRequest("invalid filename".into()))?;
        if f != media.filename && validate::extension_of(&f).is_none() {
            return Err(AppError::BadRequest(
                "filename must keep a recognizable extension".into(),
            ));
        }
        repo::update(
            &app.db,
            media.id,
            &repo::MediaUpdate {
                filename: Some(f),
                title: u.title.clone(),
                description: u.description.clone(),
                alt: u.alt.clone(),
                caption: u.caption.clone(),
                folder_id: u.folder_id,
                tags: u.tags.clone(),
            },
        )
        .await?;
    } else {
        repo::update(
            &app.db,
            media.id,
            &repo::MediaUpdate {
                filename: None,
                title: u.title,
                description: u.description,
                alt: u.alt,
                caption: u.caption,
                folder_id: u.folder_id,
                tags: u.tags,
            },
        )
        .await?;
    }
    let Some(updated) = repo::find_by_id(&app.db, media.id).await? else {
        return Err(AppError::NotFound("media not found".into()));
    };
    index_media(app, &updated).await;
    invalidate_media_caches(app).await;
    Ok(updated)
}

/// Delete media: removes the storage objects (original + thumbnails), the
/// database row and the search index entry. Referenced media is only
/// deleted with `force = true`.
pub async fn delete(app: &App, media: &Media, force: bool) -> AppResult<()> {
    let refs = repo::references_of(&app.db, media.id).await?;
    if !refs.is_empty() && !force {
        return Err(AppError::Conflict(format!(
            "media is referenced by {} post(s)/page(s) — retry with force",
            refs.len()
        )));
    }
    for key in repo::all_storage_keys(media) {
        app.media.storage().delete(&key).await?;
    }
    repo::delete(&app.db, media.id).await?;
    app.search.remove(&app.db, "media", media.id).await.ok();
    invalidate_media_caches(app).await;
    Ok(())
}

pub async fn folder_create(app: &App, name: &str) -> AppResult<MediaFolder> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::BadRequest(
            "folder name must be 1-80 characters".into(),
        ));
    }
    let base = slug::slugify(name);
    let base = if base.is_empty() {
        "folder".to_string()
    } else {
        base
    };
    let mut candidate = base.clone();
    for n in 2..1000 {
        if repo::folder_find_by_slug(&app.db, &candidate)
            .await?
            .is_none()
        {
            break;
        }
        candidate = format!("{base}-{n}");
    }
    let id = repo::folder_create(&app.db, name, &candidate).await?;
    let Some(f) = repo::folder_find(&app.db, id).await? else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "folder vanished after insert"
        )));
    };
    invalidate_media_caches(app).await;
    Ok(f)
}

pub async fn folder_delete(app: &App, id: i64) -> AppResult<()> {
    repo::folder_delete(&app.db, id).await?;
    invalidate_media_caches(app).await;
    Ok(())
}

/// Duplicate a media item: new uuid + storage keys, same bytes. Copies keep
/// the metadata but start their own reference lifecycle.
pub async fn copy(app: &App, media: &Media, actor_id: i64) -> AppResult<Media> {
    let uuid = cookies::random_token(8);
    let storage_key = app.media.storage_key(&uuid, &media.extension);
    app.media
        .storage()
        .copy(&media.storage_key, &storage_key)
        .await?;
    for size in &media.thumbnails {
        let to = thumbnail_key(&storage_key, size);
        app.media
            .storage()
            .copy(&media.thumb_storage_key(size), &to)
            .await?;
    }
    let new = repo::NewMedia {
        uuid: uuid.clone(),
        filename: media.filename.clone(),
        original_filename: media.original_filename.clone(),
        storage_key,
        mime_type: media.mime_type.clone(),
        extension: media.extension.clone(),
        size: media.size,
        width: media.width,
        height: media.height,
        duration: media.duration,
        hash: media.hash.clone(),
        thumbnails: media.thumbnails.clone(),
        folder_id: media.folder_id,
        uploaded_by: actor_id,
    };
    let id = repo::insert(&app.db, &new).await?;
    let Some(m) = repo::find_by_id(&app.db, id).await? else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "media row vanished after insert"
        )));
    };
    index_media(app, &m).await;
    invalidate_media_caches(app).await;
    Ok(m)
}

/// Move items into a folder (`None` = unfiled). Virtual folders only —
/// storage keys never change.
pub async fn move_to_folder(app: &App, ids: &[i64], folder_id: Option<i64>) -> AppResult<()> {
    for id in ids {
        repo::update(
            &app.db,
            *id,
            &repo::MediaUpdate {
                filename: None,
                title: None,
                description: None,
                alt: None,
                caption: None,
                folder_id: Some(folder_id),
                tags: None,
            },
        )
        .await?;
    }
    invalidate_media_caches(app).await;
    Ok(())
}

/// Replace the tag set of items (batch tagging).
pub async fn set_tags(app: &App, ids: &[i64], tags: &[String]) -> AppResult<()> {
    for id in ids {
        repo::set_tags(&app.db, *id, tags).await?;
        if let Some(m) = repo::find_by_id(&app.db, *id).await? {
            index_media(app, &m).await;
        }
    }
    invalidate_media_caches(app).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON shape (REST API + admin templates)
// ---------------------------------------------------------------------------

/// Human-readable byte size ("1.2 MB").
pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes.max(0) as f64;
    let mut unit = 0usize;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Serialize a media record with derived URLs for API/HTMX responses.
pub fn media_json(app: &App, m: &Media) -> serde_json::Value {
    let mut v = serde_json::json!(m);
    if let serde_json::Value::Object(o) = &mut v {
        o.insert("url".into(), serde_json::json!(url_for(app, m)));
        o.insert("path".into(), serde_json::json!(m.url_path()));
        o.insert("kind".into(), serde_json::json!(m.kind().as_str()));
        o.insert("srcset".into(), serde_json::json!(srcset_for(app, m)));
        let thumb = m
            .thumbnails
            .first()
            .map(|s| thumb_url_for(app, m, s))
            .unwrap_or_else(|| url_for(app, m));
        o.insert("thumb_url".into(), serde_json::json!(thumb));
        o.insert(
            "download_url".into(),
            serde_json::json!(format!("/api/media/{}/download", m.id)),
        );
        o.insert("size_human".into(), serde_json::json!(human_size(m.size)));
        o.insert(
            "created".into(),
            serde_json::json!(crate::utils::time::format(m.created_at, "date")),
        );
    }
    v
}

// ---------------------------------------------------------------------------
// References (posts/pages embedding media)
// ---------------------------------------------------------------------------

/// Extract `/media/{uuid}` references from markdown content. The uuid must
/// be at least 8 characters — plain words caught mid-sentence (`/media/short.`
/// before "no") never match.
fn scan_media_uuids(content: &str) -> Vec<String> {
    let mut uuids: Vec<String> = Vec::new();
    let mut rest = content;
    while let Some(pos) = rest.find("/media/") {
        rest = &rest[pos + 7..];
        let token: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
            .collect();
        let uuid = token.split('.').next().unwrap_or("").to_string();
        if uuid.len() >= 8 && !uuids.contains(&uuid) {
            uuids.push(uuid);
        }
    }
    uuids
}

/// Map `/media/{uuid}` references in saved content to media ids and record
/// them (`media_references`), powering the "referenced by N posts" guard.
pub async fn sync_references(
    app: &App,
    ref_type: &str,
    ref_id: i64,
    content: &str,
) -> AppResult<()> {
    let mut ids = Vec::new();
    for u in scan_media_uuids(content) {
        if let Some(m) = repo::find_by_uuid(&app.db, &u).await? {
            ids.push(m.id);
        }
    }
    repo::set_references(&app.db, ref_type, ref_id, &ids).await
}

// ---------------------------------------------------------------------------
// Index / cache maintenance
// ---------------------------------------------------------------------------

/// Keep the search index in sync (best effort — never blocks the mutation).
pub async fn index_media(app: &App, media: &Media) {
    if let Err(e) = app.search.index_media(&app.db, media).await {
        tracing::warn!(media_id = media.id, error = %e, "media search indexing failed");
    }
}

async fn invalidate_media_caches(app: &App) {
    // Media metadata (namespace version bump) + search result caches
    // (media rows live in the search index).
    app.cache.invalidate(&[ns::MEDIA, ns::SEARCH]).await;
}

/// Statistics for the admin dashboard.
pub async fn stats(app: &App) -> AppResult<(i64, i64)> {
    repo::counts(&app.db).await
}

// ---------------------------------------------------------------------------
// Maintenance (CLI: `polaris media orphan|cleanup|verify`)
// ---------------------------------------------------------------------------

/// Storage object with no database record (original or thumbnail).
#[derive(Clone, Debug)]
pub struct OrphanFile {
    pub key: String,
    pub size: u64,
}

/// A mismatch found by `polaris media verify`.
#[derive(Clone, Debug)]
pub struct VerifyIssue {
    pub media_id: i64,
    pub uuid: String,
    pub problem: String,
}

/// Both directions of the orphan scan.
#[derive(Clone, Debug)]
pub struct OrphanReport {
    /// Database records whose storage object is missing.
    pub records: Vec<Media>,
    /// Storage objects no record points at.
    pub files: Vec<OrphanFile>,
}

/// Iterate every media record in id order.
async fn all_records(app: &App) -> AppResult<Vec<Media>> {
    let mut out = Vec::new();
    let mut last = 0i64;
    loop {
        let batch = repo::scan(&app.db, last, 500).await?;
        let Some(tail) = batch.last() else { break };
        last = tail.id;
        out.extend(batch);
    }
    Ok(out)
}

pub async fn orphan_report(app: &App) -> AppResult<OrphanReport> {
    let records = all_records(app).await?;
    let storage_keys: std::collections::HashSet<String> =
        app.media.storage().list("").await?.into_iter().collect();

    let mut db_keys = std::collections::HashSet::new();
    for m in &records {
        db_keys.insert(m.storage_key.clone());
        for t in &m.thumbnails {
            db_keys.insert(m.thumb_storage_key(t));
        }
    }

    let mut files = Vec::new();
    for key in &storage_keys {
        if !db_keys.contains(key) {
            let size = app.media.storage().size_of(key).await?.unwrap_or(0);
            files.push(OrphanFile {
                key: key.clone(),
                size,
            });
        }
    }
    let records = records
        .into_iter()
        .filter(|m| !storage_keys.contains(&m.storage_key))
        .collect();
    Ok(OrphanReport { records, files })
}

/// Delete orphan storage files. `apply = false` is a dry run (returns the
/// keys that *would* be deleted, deletes nothing).
pub async fn cleanup_orphans(app: &App, apply: bool) -> AppResult<Vec<String>> {
    let report = orphan_report(app).await?;
    let mut deleted = Vec::new();
    for f in &report.files {
        if apply {
            app.media.storage().delete(&f.key).await?;
        }
        deleted.push(f.key.clone());
    }
    Ok(deleted)
}

/// Verify database metadata against the stored objects: existence, size and
/// (with `deep`) the SHA-256 hash.
pub async fn verify(app: &App, deep: bool) -> AppResult<Vec<VerifyIssue>> {
    let storage = app.media.storage();
    let mut issues = Vec::new();
    for m in all_records(app).await? {
        match storage.size_of(&m.storage_key).await {
            Ok(Some(s)) if s as i64 == m.size => {}
            Ok(Some(s)) => issues.push(VerifyIssue {
                media_id: m.id,
                uuid: m.uuid.clone(),
                problem: format!("size mismatch: metadata {} B, stored {} B", m.size, s),
            }),
            Ok(None) => issues.push(VerifyIssue {
                media_id: m.id,
                uuid: m.uuid.clone(),
                problem: "storage object missing".into(),
            }),
            Err(e) => issues.push(VerifyIssue {
                media_id: m.id,
                uuid: m.uuid.clone(),
                problem: format!("storage read failed: {}", e.message()),
            }),
        }
        if deep {
            match storage.hash_of(&m.storage_key).await {
                Ok(Some(h)) if h == m.hash => {}
                Ok(Some(h)) => issues.push(VerifyIssue {
                    media_id: m.id,
                    uuid: m.uuid.clone(),
                    problem: format!("hash mismatch: metadata {}, stored {}", m.hash, h),
                }),
                Ok(None) => {}
                Err(_) => {}
            }
        }
        for t in &m.thumbnails {
            let key = m.thumb_storage_key(t);
            if !storage.exists(&key).await.unwrap_or(false) {
                issues.push(VerifyIssue {
                    media_id: m.id,
                    uuid: m.uuid.clone(),
                    problem: format!("thumbnail '{t}' missing ({key})"),
                });
            }
        }
    }
    Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_scanning() {
        // Extracted from markdown: plain, thumbnail and absolute CDN forms.
        let content = "![a](/media/abc12345.webp) and /media/xyz98765.small.png plus \
                       https://cdn.example.com/media/qqq11111.jpg and /media/short. no /media/ match here";
        assert_eq!(
            scan_media_uuids(content),
            vec![
                "abc12345".to_string(),
                "xyz98765".to_string(),
                "qqq11111".to_string()
            ]
        );
        assert!(scan_media_uuids("no media here").is_empty());
    }
}
