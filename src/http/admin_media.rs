//! Admin media library — server-rendered file manager with a thin JS layer
//! for uploads only (progress, drag-drop, paste); every operation works
//! without it through plain form posts.
//!
//! Routes (all session-protected, merged into the admin router):
//! ```text
//! GET  /admin/media                     library (grid/list, filters, search)
//! POST /admin/media/upload              no-JS multipart upload fallback
//! POST /admin/media/batch               batch delete / move / tag
//! POST /admin/media/folder/create       new virtual folder
//! POST /admin/media/folder/{id}/delete  delete folder
//! GET  /admin/media/storage             storage topology (active + mirrors)
//! POST /admin/media/storage/activate    switch the active storage (restart to apply)
//! GET  /admin/media/{id}                detail + metadata form
//! POST /admin/media/{id}                save metadata
//! POST /admin/media/{id}/copy           duplicate
//! POST /admin/media/{id}/delete         delete (force via checkbox)
//! ```

use axum::Router;
use axum::extract::{DefaultBodyLimit, Extension, Form, Multipart, Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

use super::admin::{PageQuery, base_ctx, redirect_err, redirect_ok, require_role};
use crate::auth::{self, AuthCtx};
use crate::error::{AppError, AppResult};
use crate::models::{Media, MediaKind};
use crate::repositories::media as repo;
use crate::services;
use crate::state::App;
use crate::templates;

pub fn router(app: &App) -> Router<App> {
    Router::new()
        .route("/admin/media", get(library))
        .route("/admin/media/upload", post(upload_fallback))
        .route("/admin/media/batch", post(batch))
        .route("/admin/media/folder/create", post(folder_create))
        .route("/admin/media/folder/{id}/delete", post(folder_delete))
        .route("/admin/media/storage", get(storage_page))
        .route("/admin/media/storage/save", post(storage_save))
        .route("/admin/media/storage/activate", post(storage_activate))
        .route("/admin/media/storage/delete", post(storage_delete))
        .route("/admin/media/{id}", get(detail).post(save))
        .route("/admin/media/{id}/copy", post(copy))
        .route("/admin/media/{id}/delete", post(delete))
        // Same HTTP-layer size guard as the REST upload endpoint.
        .layer(DefaultBodyLimit::max(body_limit(app)))
}

fn body_limit(app: &App) -> usize {
    (app.media.config().max_upload_bytes() + 1024 * 1024) as usize
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LibraryQuery {
    #[serde(flatten)]
    page: PageQuery,
    #[serde(default)]
    view: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    sort: Option<String>,
}

async fn library(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<LibraryQuery>,
) -> AppResult<Response> {
    if !app.media.enabled() {
        return Ok(redirect_err(
            "/admin",
            "Media is disabled in the configuration.",
        ));
    }
    if !services::media::can_upload(auth.role) {
        return Ok(redirect_err(
            "/admin",
            "Media access requires the author role.",
        ));
    }

    let kind = q.kind.as_deref().and_then(MediaKind::parse);
    let folder = match q.folder.as_deref() {
        None | Some("") | Some("all") => repo::FolderFilter::All,
        Some("unfiled") => repo::FolderFilter::Unfiled,
        Some(raw) => raw
            .parse::<i64>()
            .map(repo::FolderFilter::Id)
            .unwrap_or(repo::FolderFilter::All),
    };
    let sort = q
        .sort
        .as_deref()
        .and_then(repo::MediaSort::parse)
        .unwrap_or_default();
    let search = q.search.clone().filter(|s| !s.trim().is_empty());
    let page = q.page.page.unwrap_or(1).max(1);
    let view = if q.view.as_deref() == Some("list") {
        "list"
    } else {
        "grid"
    };

    let filter = repo::MediaFilter {
        kind,
        folder,
        tag: q.tag.clone().filter(|t| !t.is_empty()),
        search: search.clone(),
        // Authors see their own library only.
        uploaded_by: if services::media::can_view_all(auth.role) {
            None
        } else {
            Some(auth.user_id)
        },
        sort,
        page,
        per_page: 36,
    };
    let (media, total) = services::media::list(&app, &filter).await?;
    let folders = services::media::folders(&app).await?;
    let (stats_total, stats_bytes) = services::media::stats(&app).await?;
    let tags = repo::tag_counts(&app.db).await?;

    let items: Vec<_> = media.iter().map(|m| media_tile(&app, m)).collect();
    let pages = (total + 35) / 36;
    let view_param: Option<&str> = if view == "list" { Some("list") } else { None };
    // Pagination keeps every filter; the toolbar links replace one at a time.
    let qs = filter_qs(&q, view_param, true, true);
    let qs_noview = filter_qs(&q, None, true, true);
    let qs_folders = filter_qs(&q, view_param, false, false);
    let qs_tags = filter_qs(&q, view_param, true, false);

    let mut ctx = base_ctx(&app, &auth, "media", &q.page);
    ctx.insert("items", &items);
    ctx.insert("total", &total);
    ctx.insert("pages", &pages.max(1));
    ctx.insert("current_page", &page);
    ctx.insert("view", &view);
    ctx.insert("search", &search.clone().unwrap_or_default());
    ctx.insert("kind_filter", &q.kind.clone().unwrap_or_default());
    ctx.insert(
        "folder_filter",
        &q.folder.clone().unwrap_or_else(|| "all".into()),
    );
    ctx.insert("tag_filter", &q.tag.clone().unwrap_or_default());
    ctx.insert("sort", &sort_key(&filter.sort));
    ctx.insert("qs", &qs);
    ctx.insert("qs_noview", &qs_noview);
    ctx.insert("qs_folders", &qs_folders);
    ctx.insert("qs_tags", &qs_tags);
    ctx.insert("folders", &folders);
    ctx.insert("tags", &tags);
    ctx.insert("stats_total", &stats_total);
    ctx.insert("stats_bytes", &services::media::human_size(stats_bytes));
    ctx.insert(
        "can_manage_folders",
        &services::media::can_manage_folders(auth.role),
    );
    ctx.insert("max_upload", &human_limit(&app));

    let body = templates::render_admin("media.html", &ctx)?;
    Ok(Html(body).into_response())
}

/// Current filter state as a query string. `view` writes the view param
/// (grid is the default and stays implicit); `folder`/`tag` control whether
/// those params are included, so links can replace one filter at a time.
fn filter_qs(q: &LibraryQuery, view: Option<&str>, folder: bool, tag: bool) -> String {
    let mut out = String::new();
    if let Some(v) = view {
        out.push_str(&format!("view={v}&"));
    }
    if let Some(k) = &q.kind {
        out.push_str(&format!("type={}&", urlencode_min(k)));
    }
    if folder && let Some(f) = &q.folder {
        out.push_str(&format!("folder={}&", urlencode_min(f)));
    }
    if tag && let Some(t) = &q.tag {
        out.push_str(&format!("tag={}&", urlencode_min(t)));
    }
    if let Some(s) = &q.search {
        out.push_str(&format!("search={}&", urlencode_min(s)));
    }
    if let Some(s) = &q.sort {
        out.push_str(&format!("sort={}&", urlencode_min(s)));
    }
    out
}

fn sort_key(s: &repo::MediaSort) -> &'static str {
    match s {
        repo::MediaSort::Newest => "newest",
        repo::MediaSort::Oldest => "oldest",
        repo::MediaSort::Name => "name",
        repo::MediaSort::Size => "size",
    }
}

fn human_limit(app: &App) -> String {
    let bytes = app.media.config().max_upload_bytes();
    services::media::human_size(bytes as i64)
}

fn urlencode_min(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// JSON tile for one media item in the library grid/list.
fn media_tile(app: &App, m: &Media) -> serde_json::Value {
    let mut v = services::media::media_json(app, m);
    if let serde_json::Value::Object(o) = &mut v {
        o.insert("is_image".into(), json!(m.kind() == MediaKind::Image));
        o.insert(
            "uploaded_by_name".into(),
            json!(m.uploader_name.clone().unwrap_or_default()),
        );
        o.insert(
            "folder_name".into(),
            json!(m.folder_name.clone().unwrap_or_default()),
        );
        let dims = match (m.width, m.height) {
            (Some(w), Some(h)) => format!("{w} × {h}"),
            _ => String::new(),
        };
        o.insert("dimensions".into(), json!(dims));
        o.insert("has_references".into(), json!(m.ref_count.unwrap_or(0) > 0));
    }
    v
}

// ---------------------------------------------------------------------------
// Upload (no-JS fallback — the JS layer posts to the REST endpoint instead)
// ---------------------------------------------------------------------------

async fn upload_fallback(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    if !services::media::can_upload(auth.role) {
        return Ok(redirect_err("/admin/media", "Upload permission required."));
    }
    let mut csrf = String::new();
    let mut folder_id: Option<i64> = None;
    let mut uploaded = 0usize;
    let mut errors: Vec<String> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("malformed upload: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        let Some(filename) = field.file_name().map(str::to_string) else {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::BadRequest(format!("malformed field: {e}")))?;
            match name.as_str() {
                "csrf" => csrf = text,
                "folder_id" => folder_id = text.trim().parse().ok(),
                _ => {}
            }
            continue;
        };
        if let Err(e) = auth::ensure_csrf(&auth, &csrf) {
            return Ok(e.into_response());
        }
        let mime = field.content_type().unwrap_or_default().to_string();
        let mut reader = super::media_api::FieldReader::new(field);
        match services::media::upload_streamed(&app, auth.user_id, &filename, &mime, &mut reader)
            .await
        {
            Ok(outcome) => {
                uploaded += 1;
                if let Some(folder) = folder_id {
                    let _ =
                        services::media::move_to_folder(&app, &[outcome.media.id], Some(folder))
                            .await;
                }
            }
            Err(e) => errors.push(format!("{filename}: {}", e.message())),
        }
    }

    let back = match folder_id {
        Some(f) => format!("/admin/media?folder={f}"),
        None => "/admin/media".to_string(),
    };
    if uploaded > 0 && errors.is_empty() {
        let n = uploaded.to_string();
        Ok(redirect_ok(
            &back,
            &crate::i18n::tr("admin.flash.uploaded_n", &[("n", &n)]),
        ))
    } else if uploaded > 0 {
        let n = uploaded.to_string();
        let all = errors.join("; ");
        Ok(redirect_err(
            &back,
            &crate::i18n::tr(
                "admin.flash.uploaded_rejected",
                &[("n", &n), ("errors", &all)],
            ),
        ))
    } else {
        let all = errors.join("; ");
        Ok(redirect_err(
            &back,
            &crate::i18n::tr("admin.flash.upload_failed", &[("errors", &all)]),
        ))
    }
}

// ---------------------------------------------------------------------------
// Batch operations (plain form post)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BatchForm {
    csrf: String,
    action: String,
    #[serde(default)]
    ids: Vec<i64>,
    /// Current folder view (redirect target after the operation).
    #[serde(default)]
    folder_id: Option<i64>,
    /// Move destination folder (action = "move"; empty = unfile).
    #[serde(default)]
    target: Option<i64>,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    force: Option<String>,
}

async fn batch(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<BatchForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if form.ids.is_empty() {
        return Ok(redirect_err("/admin/media", "Select at least one item."));
    }
    let tags: Vec<String> = form
        .tags
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let back = match form.folder_id {
        Some(f) if f > 0 => format!("/admin/media?folder={f}"),
        _ => "/admin/media".to_string(),
    };
    match form.action.as_str() {
        "delete" => {
            let force = matches!(form.force.as_deref(), Some("on") | Some("1") | Some("true"));
            let mut ok = 0;
            let mut errors = Vec::new();
            for id in &form.ids {
                match services::media::get(&app, *id).await {
                    Ok(Some(m)) if services::media::can_modify(auth.role, &m, auth.user_id) => {
                        match services::media::delete(&app, &m, force).await {
                            Ok(()) => ok += 1,
                            Err(e) => errors.push(e.message()),
                        }
                    }
                    Ok(Some(_)) => errors.push(format!("#{id}: forbidden")),
                    _ => errors.push(format!("#{id}: not found")),
                }
            }
            if errors.is_empty() {
                let n = ok.to_string();
                Ok(redirect_ok(
                    &back,
                    &crate::i18n::tr("admin.flash.deleted_n", &[("n", &n)]),
                ))
            } else if ok > 0 {
                let n = ok.to_string();
                let all = errors.join("; ");
                Ok(redirect_err(
                    &back,
                    &crate::i18n::tr(
                        "admin.flash.deleted_partial",
                        &[("n", &n), ("errors", &all)],
                    ),
                ))
            } else {
                Ok(redirect_err(&back, &errors.join("; ")))
            }
        }
        "move" => {
            let mut permitted = Vec::new();
            for id in &form.ids {
                if let Ok(Some(m)) = services::media::get(&app, *id).await
                    && services::media::can_modify(auth.role, &m, auth.user_id)
                {
                    permitted.push(*id);
                }
            }
            let folder = match form.target {
                Some(f) if f > 0 => Some(f),
                _ => None,
            };
            services::media::move_to_folder(&app, &permitted, folder).await?;
            let n = permitted.len().to_string();
            Ok(redirect_ok(
                &back,
                &crate::i18n::tr("admin.flash.moved_n", &[("n", &n)]),
            ))
        }
        "tag" => {
            let mut ok = 0;
            for id in &form.ids {
                if let Ok(Some(m)) = services::media::get(&app, *id).await
                    && services::media::can_modify(auth.role, &m, auth.user_id)
                {
                    services::media::set_tags(&app, &[*id], &tags).await?;
                    ok += 1;
                }
            }
            let n = ok.to_string();
            Ok(redirect_ok(
                &back,
                &crate::i18n::tr("admin.flash.tagged_n", &[("n", &n)]),
            ))
        }
        other => Ok(redirect_err(
            "/admin/media",
            &crate::i18n::tr("admin.flash.unknown_action", &[("action", other)]),
        )),
    }
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct FolderForm {
    csrf: String,
    name: String,
}

async fn folder_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<FolderForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if !services::media::can_manage_folders(auth.role) {
        return Ok(redirect_err(
            "/admin/media",
            "Editor role required to manage folders.",
        ));
    }
    match services::media::folder_create(&app, &form.name).await {
        Ok(f) => Ok(redirect_ok(
            &format!("/admin/media?folder={}", f.id),
            "Folder created.",
        )),
        Err(e) => Ok(redirect_err("/admin/media", &e.message())),
    }
}

async fn folder_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<FolderForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if !services::media::can_manage_folders(auth.role) {
        return Ok(redirect_err(
            "/admin/media",
            "Editor role required to manage folders.",
        ));
    }
    match services::media::folder_delete(&app, id).await {
        Ok(()) => Ok(redirect_ok(
            "/admin/media",
            "Folder deleted — its items are now unfiled.",
        )),
        Err(e) => Ok(redirect_err("/admin/media", &e.message())),
    }
}

// ---------------------------------------------------------------------------
// Storage topology: GUI-managed storages (active + mirrors)
// ---------------------------------------------------------------------------

const STORAGE_CONFIG_KEY: &str = "media.storage.config";
const STORAGE_MIRROR_KEY: &str = "media.storage.mirror";

/// GUI-configured storages straight from the settings table (S3 secrets
/// still encrypted). The GUI owns everything written through this page;
/// entries that exist only in polaris.toml are read-only here.
fn gui_storages(app: &App) -> Vec<crate::config::StorageEntry> {
    app.settings
        .get(STORAGE_CONFIG_KEY)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn gui_mirror(app: &App) -> Vec<String> {
    app.settings
        .get(STORAGE_MIRROR_KEY)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Full storage management page: topology table (activate / edit / delete),
/// mirror selection and the add-or-edit form. Secrets are never echoed —
/// the form shows a placeholder and an empty field keeps the stored one.
async fn storage_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<StoragePageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, crate::models::Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    if !app.media.enabled() {
        return Ok(redirect_err(
            "/admin",
            "Media is disabled in the configuration.",
        ));
    }

    let gui_ids: Vec<String> = gui_storages(&app).iter().map(|e| e.id.clone()).collect();
    let mirrors = gui_mirror(&app);
    let storages: Vec<_> = app
        .media
        .storage_infos()
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "kind": i.kind,
                "summary": i.summary,
                "active": i.active,
                "mirror": i.mirror,
                "editable": gui_ids.contains(&i.id),
            })
        })
        .collect();

    // Editing an existing GUI storage: prefill the form (secret masked).
    let editing = q
        .edit
        .as_deref()
        .and_then(|id| gui_storages(&app).into_iter().find(|e| e.id == id));

    let mut ctx = base_ctx(&app, &auth, "media", &q.page);
    ctx.insert("storages", &storages);
    ctx.insert("active_id", &app.media.active_storage_id());
    ctx.insert("mirrors", &mirrors);
    ctx.insert("editing", &editing);
    let body = templates::render_admin("media_storage.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct StoragePageQuery {
    #[serde(flatten)]
    page: PageQuery,
    /// Prefill the form with this GUI storage id.
    #[serde(default)]
    edit: Option<String>,
}

#[derive(Deserialize)]
struct StorageActivateForm {
    csrf: String,
    id: String,
}

/// Switch the active storage. The choice is persisted in the settings
/// table; like every other runtime setting it takes effect on restart
/// (the storage topology is built once at startup).
async fn storage_activate(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<StorageActivateForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, crate::models::Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let id = form.id.trim();
    // Known through the file topology or the GUI-managed list (a storage
    // added through this page only joins the running topology on restart,
    // but it can be activated right away).
    let gui_ids: Vec<String> = gui_storages(&app).iter().map(|e| e.id.clone()).collect();
    let known =
        app.media.storage_infos().iter().any(|i| i.id == id) || gui_ids.iter().any(|g| g == id);
    if !known || app.media.active_storage_id() == id {
        return Ok(redirect_err(
            "/admin/media/storage",
            "Unknown or already-active storage.",
        ));
    }
    let values =
        std::collections::HashMap::from([("media.storage.active".to_string(), id.to_string())]);
    app.settings.set_many(&app.db, &values).await?;
    Ok(redirect_ok(
        "/admin/media/storage",
        &format!("Active storage set to '{id}' — restart Polaris to apply."),
    ))
}

#[derive(Deserialize)]
struct StorageSaveForm {
    csrf: String,
    /// Existing id when editing; the id of a new storage otherwise.
    id: String,
    #[serde(default)]
    original_id: String,
    r#type: String,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    access_key_id: String,
    /// Empty keeps the stored secret (never echoed back to the form).
    #[serde(default)]
    secret_access_key: String,
    #[serde(default)]
    public_base_url: String,
    /// Mirror selections submitted with the form.
    #[serde(default)]
    mirror: Vec<String>,
}

/// Create or update a GUI-managed storage. S3 secrets are AES-256-GCM
/// encrypted under the instance key before they reach the settings table.
async fn storage_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<StorageSaveForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, crate::models::Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }

    let id = form.id.trim().to_string();
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || id.len() > 64
    {
        return Ok(redirect_err(
            "/admin/media/storage",
            "Storage id must be 1-64 characters: letters, digits, '-' or '_'.",
        ));
    }

    let kind = match form.r#type.trim().to_ascii_lowercase().as_str() {
        "" | "local" => "local",
        "s3" | "r2" | "minio" => "s3",
        other => {
            return Ok(redirect_err(
                "/admin/media/storage",
                &format!("Unknown storage type '{other}' (expected local or s3)."),
            ));
        }
    };

    let mut storages = gui_storages(&app);
    let existing = storages.iter().find(|e| e.id == id).cloned();
    // Editing under a new id: the old row is replaced, not duplicated.
    let original = form.original_id.trim();
    if !original.is_empty() && original != id {
        storages.retain(|e| e.id != original);
    }
    if storages.iter().any(|e| e.id == id) && existing.is_none() {
        return Ok(redirect_err(
            "/admin/media/storage",
            &format!("Storage '{id}' already exists — pick another id."),
        ));
    }

    let instance_secret = &app.config.security.secret;
    let mut entry = crate::config::StorageEntry {
        id: id.clone(),
        r#type: kind.to_string(),
        dir: form.dir.trim().to_string(),
        endpoint: form.endpoint.trim().trim_end_matches('/').to_string(),
        region: form.region.trim().to_string(),
        bucket: form.bucket.trim().to_string(),
        access_key_id: form.access_key_id.trim().to_string(),
        // An empty submission keeps the stored (encrypted) secret.
        secret_access_key: form.secret_access_key.trim().to_string(),
        public_base_url: form.public_base_url.trim().to_string(),
    };
    if entry.secret_access_key.is_empty() {
        entry.secret_access_key = existing
            .as_ref()
            .map(|e| e.secret_access_key.clone())
            .unwrap_or_default();
    } else if entry.secret_access_key.starts_with("enc:") {
        // Already-encrypted value submitted (defensive; the form never sends one).
    } else {
        entry.secret_access_key =
            crate::config_store::encrypt_setting(instance_secret, &entry.secret_access_key)?;
    }

    // Validate exactly like a startup-time configuration would.
    if let Err(e) = crate::media::storage::Storage::build_entry(&entry) {
        return Ok(redirect_err("/admin/media/storage", &e.message()));
    }

    storages.retain(|e| e.id != id);
    storages.push(entry);

    // Mirror set: only ids that exist and are not the active storage.
    let active_id = app.media.active_storage_id().to_string();
    let mirror: Vec<String> = form
        .mirror
        .iter()
        .map(|m| m.trim().to_string())
        .filter(|m| storages.iter().any(|e| &e.id == m) && *m != active_id)
        .collect();

    let mut values = std::collections::HashMap::new();
    values.insert(
        STORAGE_CONFIG_KEY.to_string(),
        serde_json::to_string(&storages)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize storages: {e}")))?,
    );
    values.insert(
        STORAGE_MIRROR_KEY.to_string(),
        serde_json::to_string(&mirror)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize mirrors: {e}")))?,
    );
    app.settings.set_many(&app.db, &values).await?;

    Ok(redirect_ok(
        "/admin/media/storage",
        &format!("Storage '{id}' saved — restart Polaris to apply."),
    ))
}

#[derive(Deserialize)]
struct StorageDeleteForm {
    csrf: String,
    id: String,
}

/// Remove a GUI-managed storage. The active storage cannot be removed
/// (activate another one first) and mirror references are dropped.
async fn storage_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<StorageDeleteForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, crate::models::Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let id = form.id.trim();
    let mut storages = gui_storages(&app);
    if !storages.iter().any(|e| e.id == id) {
        return Ok(redirect_err(
            "/admin/media/storage",
            "Only GUI-configured storages can be removed here.",
        ));
    }
    // The active selection may already point at a GUI storage that the
    // running topology does not know yet (applied on restart).
    let active = app
        .settings
        .get_str("media.storage.active", app.media.active_storage_id());
    if active == id {
        return Ok(redirect_err(
            "/admin/media/storage",
            "Activate another storage before removing the active one.",
        ));
    }
    storages.retain(|e| e.id != id);
    let mirror: Vec<String> = gui_mirror(&app).into_iter().filter(|m| m != id).collect();
    let mut values = std::collections::HashMap::new();
    values.insert(
        STORAGE_CONFIG_KEY.to_string(),
        serde_json::to_string(&storages)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize storages: {e}")))?,
    );
    values.insert(
        STORAGE_MIRROR_KEY.to_string(),
        serde_json::to_string(&mirror)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize mirrors: {e}")))?,
    );
    app.settings.set_many(&app.db, &values).await?;
    Ok(redirect_ok(
        "/admin/media/storage",
        &format!("Storage '{id}' removed — restart Polaris to apply."),
    ))
}

// ---------------------------------------------------------------------------
// Detail / edit
// ---------------------------------------------------------------------------

async fn load_item(app: &App, auth: &AuthCtx, id: i64) -> AppResult<Media> {
    let Some(m) = services::media::get(app, id).await? else {
        return Err(AppError::NotFound("media not found".into()));
    };
    if !services::media::can_view_all(auth.role) && m.uploaded_by != auth.user_id {
        return Err(AppError::NotFound("media not found".into()));
    }
    Ok(m)
}

async fn detail(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let m = load_item(&app, &auth, id).await?;
    let references = repo::references_detailed(&app.db, m.id).await?;
    let folders = services::media::folders(&app).await?;
    let can_modify = services::media::can_modify(auth.role, &m, auth.user_id);

    let mut ctx = base_ctx(&app, &auth, "media", &q);
    ctx.insert("m", &media_tile(&app, &m));
    ctx.insert("original", &m.original_filename);
    ctx.insert("alt", &m.alt);
    ctx.insert("caption", &m.caption);
    ctx.insert("description", &m.description);
    ctx.insert("title", &m.title);
    ctx.insert("tags_str", &m.tags.join(", "));
    ctx.insert("folders", &folders);
    ctx.insert("can_modify", &can_modify);
    ctx.insert("references", &references);
    ctx.insert("ref_count", &m.ref_count.unwrap_or(0));
    ctx.insert("back_url", &referer_or_library(&m.folder_id));

    let body = templates::render_admin("media_detail.html", &ctx)?;
    Ok(Html(body).into_response())
}

fn referer_or_library(folder: &Option<i64>) -> String {
    match folder {
        Some(f) => format!("/admin/media?folder={f}"),
        None => "/admin/media".to_string(),
    }
}

#[derive(Deserialize)]
struct SaveForm {
    csrf: String,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    alt: String,
    #[serde(default)]
    caption: String,
    #[serde(default)]
    folder_id: String,
    #[serde(default)]
    tags: String,
}

async fn save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<SaveForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let m = load_item(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &m, auth.user_id) {
        return Ok(redirect_err(
            "/admin/media",
            "You may only edit your own media.",
        ));
    }
    let folder_id: Option<i64> = form.folder_id.trim().parse().ok();
    let tags: Vec<String> = form
        .tags
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let back = format!("/admin/media/{id}");
    match services::media::update(
        &app,
        &m,
        services::media::MediaUpdate {
            filename: Some(form.filename),
            title: Some(form.title),
            description: Some(form.description),
            alt: Some(form.alt),
            caption: Some(form.caption),
            folder_id: Some(folder_id),
            tags: Some(tags),
        },
    )
    .await
    {
        Ok(_) => Ok(redirect_ok(&back, "Saved.")),
        Err(e) => Ok(redirect_err(&back, &e.message())),
    }
}

#[derive(Deserialize)]
struct ConfirmForm {
    csrf: String,
    #[serde(default)]
    force: Option<String>,
}

async fn delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let m = load_item(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &m, auth.user_id) {
        return Ok(redirect_err(
            "/admin/media",
            "You may only delete your own media.",
        ));
    }
    let force = matches!(form.force.as_deref(), Some("on") | Some("1") | Some("true"));
    match services::media::delete(&app, &m, force).await {
        Ok(()) => Ok(redirect_ok("/admin/media", "Deleted.")),
        Err(e) => Ok(redirect_err(&format!("/admin/media/{id}"), &e.message())),
    }
}

async fn copy(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let m = load_item(&app, &auth, id).await?;
    if !services::media::can_modify(auth.role, &m, auth.user_id) {
        return Ok(redirect_err(
            "/admin/media",
            "You may only copy your own media.",
        ));
    }
    match services::media::copy(&app, &m, auth.user_id).await {
        Ok(new) => Ok(redirect_ok(
            &format!("/admin/media/{}", new.id),
            "Duplicated.",
        )),
        Err(e) => Ok(redirect_err(&format!("/admin/media/{id}"), &e.message())),
    }
}
