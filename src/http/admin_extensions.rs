//! Extension management: upload / install / uninstall (admin UI + REST API).
//!
//! ```text
//! Admin UI (session + CSRF)
//!   POST /admin/extensions/upload      multipart fallback (no-JS)
//!   POST /admin/extensions/uninstall    form post
//!   GET  /admin/extensions/logs         audit log
//!
//! REST API (session via api_auth_mw; multipart checked against CSRF)
//!   POST   /api/admin/extensions/upload          JSON install result
//!   GET    /api/admin/extensions?kind=          status listing
//!   DELETE /api/admin/extensions/{kind}/{id}     uninstall (?remove_data=1)
//!   POST   /api/admin/extensions/verify         integrity check
//! ```
//!
//! Uploads stream to `data/tmp/extensions/` — a package is never buffered
//! whole in memory — and are validated by the installer before anything
//! reaches `themes/` or `plugins/`.

use std::path::PathBuf;

use axum::extract::{DefaultBodyLimit, Extension, Multipart, Path, Query, State};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{self, AuthCtx};
use crate::error::{ApiError, AppError, AppResult};
use crate::extension::ExtensionKind;
use crate::models::Role;
use crate::services;
use crate::state::App;
use crate::templates;
use crate::utils::{cookies, time};

pub fn router(app: App) -> Router<App> {
    // Body limit just above the configured package cap: multipart framing
    // overhead must pass, oversized archives are refused before any write.
    let cap = (app.config.extensions.upload.max_file_bytes() + 1024 * 1024) as usize;

    let admin = Router::new()
        .route("/admin/extensions/upload", post(upload_form))
        .route("/admin/extensions/uninstall", post(uninstall_form))
        .route("/admin/extensions/logs", get(logs_page))
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            crate::auth::admin_auth_mw,
        ))
        .layer(DefaultBodyLimit::max(cap));

    let api = Router::new()
        .route("/api/admin/extensions/upload", post(api_upload))
        .route("/api/admin/extensions", get(api_list))
        .route("/api/admin/extensions/verify", post(api_verify))
        .route(
            "/api/admin/extensions/{kind}/{id}",
            axum::routing::delete(api_uninstall),
        )
        .layer(DefaultBodyLimit::max(cap))
        .layer(middleware::from_fn_with_state(
            app,
            crate::auth::api_auth_mw,
        ));

    admin.merge(api)
}

// ---------------------------------------------------------------------------
// Shared upload staging
// ---------------------------------------------------------------------------

/// Stream one multipart file field to a temp file under
/// `data/tmp/extensions/`, enforcing the configured size cap while writing.
async fn stage_upload(
    app: &App,
    field: &mut axum::extract::multipart::Field<'_>,
) -> AppResult<PathBuf> {
    let dir = PathBuf::from(&app.config.extensions.tmp_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::BadRequest(format!("cannot create staging directory: {e}")))?;
    let path = dir.join(format!("upload-{}.zip", cookies::random_token(16)));
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| AppError::BadRequest(format!("cannot create staging file: {e}")))?;
    let cap = app.config.extensions.upload.max_file_bytes();
    let mut written: u64 = 0;
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| AppError::BadRequest(format!("malformed upload: {e}")))?
    {
        written += chunk.len() as u64;
        if written > cap {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AppError::BadRequest(format!(
                "package exceeds the size limit ({} bytes)",
                cap
            )));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::BadRequest(format!("upload failed: {e}")))?;
    }
    file.flush()
        .await
        .map_err(|e| AppError::BadRequest(format!("upload failed: {e}")))?;
    Ok(path)
}

/// Install a staged upload. `actor` is the authenticated username.
async fn install_staged(
    app: &App,
    staged: &std::path::Path,
    actor: &str,
    force: bool,
    allow_downgrade: bool,
) -> AppResult<crate::extension::InstallOutcome> {
    let out = services::extensions::install(app, staged, actor, force, allow_downgrade).await;
    let _ = tokio::fs::remove_file(staged).await;
    out
}

fn require_admin(auth: &AuthCtx) -> AppResult<()> {
    if auth.role.at_least(Role::Admin) {
        Ok(())
    } else {
        Err(AppError::Forbidden("admin role required".into()))
    }
}

// ---------------------------------------------------------------------------
// Admin UI (no-JS fallback)
// ---------------------------------------------------------------------------

async fn upload_form(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    if let Err(e) = require_admin(&auth) {
        return Ok(Redirect::to(&format!("/admin?err={}", urlenc(&e.message()))).into_response());
    }
    let mut csrf = String::new();
    let mut force = false;
    let mut allow_downgrade = false;
    let mut back = "/admin/themes".to_string();
    let mut staged: Option<PathBuf> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("malformed upload: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        if field.file_name().is_some() {
            if csrf.is_empty() {
                return Ok(redirect_flash("/admin/themes", "missing CSRF token", false));
            }
            if let Err(e) = auth::ensure_csrf(&auth, &csrf) {
                return Ok(e.into_response());
            }
            staged = Some(stage_upload(&app, &mut field).await?);
        } else {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::BadRequest(format!("malformed field: {e}")))?;
            match name.as_str() {
                "csrf" => csrf = text,
                "force" => force = text == "on" || text == "true",
                "confirm_downgrade" => allow_downgrade = text == "on" || text == "true",
                "return_to" if text == "plugins" => back = "/admin/plugins".to_string(),
                _ => {}
            }
        }
    }

    let Some(staged) = staged else {
        return Ok(redirect_flash(&back, "no package file in upload", false));
    };
    match install_staged(&app, &staged, &auth.username, force, allow_downgrade).await {
        Ok(out) => {
            let mut msg = services::extensions::outcome_summary(&out);
            if !out.manifest.permissions.is_empty() {
                let perms = out.manifest.permissions.join(", ");
                msg.push_str(&crate::i18n::tr(
                    "admin.flash.review_permissions",
                    &[("perms", &perms)],
                ));
            }
            if out.kind == ExtensionKind::Theme {
                msg.push_str(&crate::i18n::tr("admin.flash.activate_below", &[]));
            } else {
                msg.push_str(&crate::i18n::tr("admin.flash.enable_below", &[]));
            }
            Ok(redirect_flash(&back, &msg, true))
        }
        Err(e) => Ok(redirect_flash(&back, &e.message(), false)),
    }
}

fn urlenc(s: &str) -> String {
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

fn redirect_flash(path: &str, msg: &str, ok: bool) -> Response {
    let key = if ok { "ok" } else { "err" };
    Redirect::to(&format!("{path}?{key}={}", urlenc(msg))).into_response()
}

#[derive(Deserialize)]
struct UninstallForm {
    csrf: String,
    kind: String,
    id: String,
    #[serde(default)]
    remove_data: Option<String>,
}

async fn uninstall_form(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    axum::extract::Form(form): axum::extract::Form<UninstallForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_flash("/admin", &e.message(), false));
    }
    let kind = match ExtensionKind::parse(&form.kind) {
        Some(k) => k,
        None => return Ok(redirect_flash("/admin", "unknown extension kind", false)),
    };
    let back = match kind {
        ExtensionKind::Theme => "/admin/themes",
        ExtensionKind::Plugin => "/admin/plugins",
    };
    let remove_data = matches!(
        form.remove_data.as_deref(),
        Some("on") | Some("true") | Some("1")
    );
    match services::extensions::uninstall(&app, kind, &form.id, remove_data, &auth.username).await {
        Ok(out) => {
            let what = match kind {
                ExtensionKind::Theme => "Theme",
                ExtensionKind::Plugin => "Plugin",
            };
            let mut msg = format!("{what} '{}' removed", out.id);
            if !out.removed_tables.is_empty() {
                msg.push_str(&format!(
                    " (dropped tables: {})",
                    out.removed_tables.join(", ")
                ));
            }
            Ok(redirect_flash(back, &msg, true))
        }
        Err(e) => Ok(redirect_flash(back, &e.message(), false)),
    }
}

// ---------------------------------------------------------------------------
// Audit log page
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct LogsQuery {
    kind: Option<String>,
}

async fn logs_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<LogsQuery>,
    Query(page): Query<crate::http::admin::PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_admin(&auth) {
        return Ok(Redirect::to(&format!("/admin?err={}", urlenc(&e.message()))).into_response());
    }
    let kind = q.kind.as_deref().and_then(ExtensionKind::parse);
    let entries = services::extensions::logs(&app, kind, 100).await?;
    let logs: Vec<_> = entries
        .iter()
        .map(|e| {
            json!({
                "time": time::format(e.created_at, "datetime"),
                "actor": e.actor,
                "action": e.action,
                "kind": e.kind,
                "ext_id": e.ext_id,
                "version": e.version,
                "result": e.result,
                "detail": e.detail,
            })
        })
        .collect();
    let mut ctx = crate::http::admin::base_ctx(&app, &auth, "themes", &page);
    ctx.insert("logs", &logs);
    ctx.insert("filter_kind", &q.kind.clone().unwrap_or_default());
    let body = templates::render_admin("extensions_logs.html", &ctx)?;
    Ok(Html(body).into_response())
}

// ---------------------------------------------------------------------------
// REST API
// ---------------------------------------------------------------------------

async fn api_upload(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    require_admin(&auth).map_err(ApiError)?;
    let mut csrf = String::new();
    let mut force = false;
    let mut allow_downgrade = false;
    let mut staged: Option<PathBuf> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        ApiError(AppError::BadRequest(format!(
            "malformed multipart body: {e}"
        )))
    })? {
        let name = field.name().unwrap_or_default().to_string();
        if field.file_name().is_some() {
            if csrf.is_empty() {
                return Err(ApiError(AppError::Forbidden("missing CSRF token".into())));
            }
            auth::ensure_csrf(&auth, &csrf).map_err(ApiError)?;
            staged = Some(stage_upload(&app, &mut field).await.map_err(ApiError)?);
        } else {
            let text = field
                .text()
                .await
                .map_err(|e| ApiError(AppError::BadRequest(format!("malformed field: {e}"))))?;
            match name.as_str() {
                "csrf" => csrf = text,
                "force" => force = text == "on" || text == "true",
                "confirm_downgrade" => allow_downgrade = text == "on" || text == "true",
                _ => {}
            }
        }
    }

    let Some(staged) = staged else {
        return Err(ApiError(AppError::BadRequest(
            "no package file in upload".into(),
        )));
    };
    let out = install_staged(&app, &staged, &auth.username, force, allow_downgrade)
        .await
        .map_err(ApiError)?;
    Ok(Json(json!({ "installed": services::extensions::outcome_json(&out) })).into_response())
}

#[derive(Deserialize, Default)]
struct ListQuery {
    kind: Option<String>,
}

async fn api_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    require_admin(&auth).map_err(ApiError)?;
    let mut data = Vec::new();
    match q.kind.as_deref().and_then(ExtensionKind::parse) {
        Some(kind) => {
            let mut items = services::extensions::status(&app, kind)
                .await
                .map_err(ApiError)?;
            data.append(&mut items);
        }
        None => {
            for kind in [ExtensionKind::Theme, ExtensionKind::Plugin] {
                let mut items = services::extensions::status(&app, kind)
                    .await
                    .map_err(ApiError)?;
                data.append(&mut items);
            }
        }
    }
    Ok(Json(json!({ "data": data })).into_response())
}

#[derive(Deserialize, Default)]
struct UninstallQuery {
    /// `1` / `true` — also drop plugin tables declared in `uninstall_tables`.
    #[serde(default)]
    remove_data: Option<String>,
}

async fn api_uninstall(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path((kind, id)): Path<(String, String)>,
    Query(q): Query<UninstallQuery>,
) -> Result<Response, ApiError> {
    require_admin(&auth).map_err(ApiError)?;
    let kind = ExtensionKind::parse(&kind)
        .ok_or_else(|| ApiError(AppError::BadRequest(format!("unknown kind '{kind}'"))))?;
    let remove_data = matches!(
        q.remove_data.as_deref(),
        Some("1") | Some("true") | Some("on")
    );
    let out = services::extensions::uninstall(&app, kind, &id, remove_data, &auth.username)
        .await
        .map_err(ApiError)?;
    Ok(Json(json!({
        "uninstalled": {
            "kind": out.kind.as_str(),
            "id": out.id,
            "removed_tables": out.removed_tables,
        }
    }))
    .into_response())
}

async fn api_verify(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
) -> Result<Response, ApiError> {
    require_admin(&auth).map_err(ApiError)?;
    let entries = services::extensions::verify(&app).await.map_err(ApiError)?;
    let all_ok = entries.iter().all(|e| e.ok);
    let data: Vec<_> = entries
        .iter()
        .map(|e| {
            json!({
                "kind": e.kind.as_str(),
                "id": e.id,
                "ok": e.ok,
                "issues": e.issues,
            })
        })
        .collect();
    Ok(Json(json!({ "ok": all_ok, "data": data })).into_response())
}
