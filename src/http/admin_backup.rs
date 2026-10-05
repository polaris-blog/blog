//! Backup & restore admin UI (session + CSRF, admin role required).
//!
//! ```text
//! GET  /admin/backups                     listing + schedule + create/upload
//! POST /admin/backups/create              create a backup (kind selector)
//! POST /admin/backups/upload              upload an archive (multipart)
//! GET  /admin/backups/{name}/download     stream the archive back
//! GET  /admin/backups/{name}/verify       run integrity verification
//! GET  /admin/backups/{name}/restore      restore confirmation page
//! POST /admin/backups/{name}/restore      perform the restore (typed confirm)
//! POST /admin/backups/{name}/delete       delete an archive
//! POST /admin/backups/schedule            save the automatic schedule
//! ```

use std::path::PathBuf;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Extension, Multipart, Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{self, AuthCtx};
use crate::backup::{BackupKind, storage::valid_backup_name};
use crate::error::{AppError, AppResult};
use crate::http::admin::{PageQuery, base_ctx, redirect_err, redirect_ok};
use crate::models::Role;
use crate::state::App;
use crate::templates;
use crate::utils::time;

pub fn router(app: App) -> Router<App> {
    let cap = (app.config.backup.max_upload_bytes() + 1024 * 1024) as usize;

    Router::new()
        .route("/admin/backups", get(backups_page))
        .route("/admin/backups/create", post(create_backup))
        .route("/admin/backups/schedule", post(schedule_save))
        .route("/admin/backups/{name}/download", get(download))
        .route("/admin/backups/{name}/verify", get(verify))
        .route(
            "/admin/backups/{name}/restore",
            get(restore_confirm).post(restore_run),
        )
        .route("/admin/backups/{name}/delete", post(delete_backup))
        .route("/admin/backups/upload", post(upload))
        .route_layer(axum::middleware::from_fn_with_state(
            app.clone(),
            crate::auth::admin_auth_mw,
        ))
        .layer(DefaultBodyLimit::max(cap))
}

fn require_admin(auth: &AuthCtx) -> AppResult<()> {
    if auth.role.at_least(Role::Admin) {
        Ok(())
    } else {
        Err(AppError::Forbidden("admin role required".into()))
    }
}

fn valid_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if !valid_backup_name(name) {
        return Err(AppError::BadRequest("invalid backup file name".into()));
    }
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// Listing page
// ---------------------------------------------------------------------------

async fn backups_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    require_admin(&auth)?;
    let svc = app.backup();
    let list = svc.list().await?;
    let rows: Vec<_> = list
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "kind": b.kind,
                "dialect": b.dialect,
                "created": if b.ok { time::format(b.created_at, "datetime") } else { String::new() },
                "created_by": b.created_by,
                "version": b.polaris_version,
                "size": crate::services::media::human_size(b.zip_bytes as i64),
                "files": b.counts.files,
                "db_rows": b.counts.db_rows,
                "media_files": b.counts.media_files,
                "includes": {
                    "database": b.includes.database,
                    "media": b.includes.media,
                    "themes": b.includes.themes,
                    "plugins": b.includes.plugins,
                },
                "ok": b.ok,
                "error": b.error.clone().unwrap_or_default(),
            })
        })
        .collect();

    let auto = &app.config.backup.auto;
    let schedule = json!({
        "enabled": app.settings.get_bool("backup.auto.enabled", auto.enabled),
        "interval_hours": app.settings.get("backup.auto.interval_hours")
            .and_then(|v| v.parse::<u64>().ok()).unwrap_or(auto.interval_hours),
        "kind": app.settings.get_str("backup.auto.kind", &auto.kind),
        "keep": app.settings.get("backup.auto.keep")
            .and_then(|v| v.parse::<usize>().ok()).unwrap_or(auto.keep),
    });

    let mut ctx = base_ctx(&app, &auth, "backups", &q);
    ctx.insert("backups", &rows);
    ctx.insert("schedule", &schedule);
    ctx.insert("backup_dir", &app.config.backup.dir);
    ctx.insert("storage_provider", app.backup().storage_name());
    ctx.insert("max_upload", &app.config.backup.max_upload_size);
    Ok(Html(templates::render_admin("backups.html", &ctx)?).into_response())
}

// ---------------------------------------------------------------------------
// Create / schedule
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateForm {
    csrf: String,
    kind: String,
}

async fn create_backup(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    axum::Form(form): axum::Form<CreateForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let kind = BackupKind::parse(form.kind.trim()).ok_or_else(|| {
        AppError::BadRequest("unknown backup kind (expected full | database | media)".into())
    })?;
    match app.backup().create(kind, &auth.username).await {
        Ok(summary) => Ok(redirect_ok(
            "/admin/backups",
            &crate::i18n::tr("admin.flash.backup_created", &[("name", &summary.name)]),
        )),
        Err(e) => Ok(redirect_err("/admin/backups", &e.message())),
    }
}

#[derive(Deserialize)]
struct ScheduleForm {
    csrf: String,
    enabled: Option<String>,
    interval_hours: String,
    kind: String,
    keep: String,
}

async fn schedule_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let enabled = matches!(form.enabled.as_deref(), Some("on") | Some("true"));
    let interval: u64 = form
        .interval_hours
        .trim()
        .parse()
        .ok()
        .filter(|h| *h >= 1)
        .ok_or_else(|| AppError::BadRequest("interval must be at least 1 hour".into()))?;
    let kind = BackupKind::parse(form.kind.trim())
        .ok_or_else(|| AppError::BadRequest("unknown backup kind".into()))?
        .as_str()
        .to_string();
    let keep: usize = form
        .keep
        .trim()
        .parse()
        .ok()
        .filter(|k| *k >= 1)
        .ok_or_else(|| AppError::BadRequest("keep must be at least 1".into()))?;

    app.settings
        .set_many(
            &app.db,
            &[
                ("backup.auto.enabled".to_string(), enabled.to_string()),
                (
                    "backup.auto.interval_hours".to_string(),
                    interval.to_string(),
                ),
                ("backup.auto.kind".to_string(), kind),
                ("backup.auto.keep".to_string(), keep.to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .await?;
    Ok(redirect_ok("/admin/backups", "Backup schedule saved."))
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Stream one multipart file field to the backup staging area (never
/// buffered whole).
async fn stage_upload(
    app: &App,
    field: &mut axum::extract::multipart::Field<'_>,
) -> AppResult<PathBuf> {
    let staging = app.backup().upload_staging_path()?;
    let mut file = tokio::fs::File::create(&staging)
        .await
        .map_err(|e| AppError::BadRequest(format!("cannot create staging file: {e}")))?;
    let cap = app.config.backup.max_upload_bytes();
    let mut written: u64 = 0;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| AppError::BadRequest(format!("malformed upload: {e}")))?
    {
        written += chunk.len() as u64;
        if written > cap {
            let _ = tokio::fs::remove_file(&staging).await;
            return Err(AppError::BadRequest(format!(
                "backup exceeds the size limit ({cap} bytes)"
            )));
        }
        use tokio::io::AsyncWriteExt;
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::BadRequest(format!("upload failed: {e}")))?;
    }
    use tokio::io::AsyncWriteExt;
    file.flush()
        .await
        .map_err(|e| AppError::BadRequest(format!("upload failed: {e}")))?;
    Ok(staging)
}

#[derive(Deserialize)]
struct BackupUploadQuery {
    #[serde(default)]
    verify_only: Option<String>,
}

async fn upload(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    axum::extract::Query(q): axum::extract::Query<BackupUploadQuery>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let verify_only = matches!(q.verify_only.as_deref(), Some("1") | Some("true"));

    let mut csrf = String::new();
    let mut staged: Option<PathBuf> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("malformed upload: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        if field.file_name().is_some() {
            if csrf.is_empty() {
                return Ok(redirect_err("/admin/backups", "missing CSRF token"));
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
            if name == "csrf" {
                csrf = text;
            }
        }
    }

    let Some(staged_path) = staged else {
        return Ok(redirect_err("/admin/backups", "no backup file in upload"));
    };

    let result = async {
        let svc = app.backup();
        let manifest = svc.read_manifest(&staged_path)?;
        let report = crate::backup::verify::verify_archive(
            &staged_path,
            &manifest,
            &crate::backup::verify::ArchiveLimits::from_backup_cfg(&app.config.backup),
        )?;
        if !report.ok() {
            return Err(AppError::BadRequest(format!(
                "backup failed integrity checks: {}",
                report.summary()
            )));
        }
        if verify_only {
            Ok((manifest, None))
        } else {
            // Commit into the listing under its own backup id.
            let name = svc.commit_uploaded(&staged_path, &manifest).await?;
            Ok((manifest, Some(name)))
        }
    }
    .await;

    match result {
        Ok((manifest, committed)) => {
            let _ = tokio::fs::remove_file(&staged_path).await;
            let msg = match committed {
                Some(name) => {
                    let name = name.display().to_string();
                    let files = manifest.counts.files.to_string();
                    crate::i18n::tr(
                        "admin.flash.backup_imported",
                        &[
                            ("name", &name),
                            ("files", &files),
                            ("version", &manifest.polaris_version),
                        ],
                    )
                }
                None => {
                    let files = manifest.counts.files.to_string();
                    crate::i18n::tr(
                        "admin.flash.backup_verified",
                        &[("files", &files), ("version", &manifest.polaris_version)],
                    )
                }
            };
            Ok(redirect_ok("/admin/backups", &msg))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&staged_path).await;
            Ok(redirect_err("/admin/backups", &e.message()))
        }
    }
}

// ---------------------------------------------------------------------------
// Download / verify / delete
// ---------------------------------------------------------------------------

async fn download(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    AxumPath(name): AxumPath<String>,
) -> AppResult<Response> {
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let name = valid_name(&name)?;
    let path = app.backup().storage_path_of(&name)?;
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return Ok(redirect_err("/admin/backups", "backup file not found")),
    };
    let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
    let mut resp = axum::http::Response::builder()
        .status(axum::http::StatusCode::OK)
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("cannot build response: {e}")))?;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/zip"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .unwrap_or(header::HeaderValue::from_static("attachment")),
    );
    if let Ok(v) = header::HeaderValue::from_str(&size.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    Ok(resp)
}

async fn verify(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    AxumPath(name): AxumPath<String>,
) -> AppResult<Response> {
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let name = valid_name(&name)?;
    let path = app.backup().storage_path_of(&name)?;
    match app.backup().verify_file(&path) {
        Ok(report) => Ok(redirect_ok(
            "/admin/backups",
            &crate::i18n::tr("admin.flash.verification", &[("report", &report.summary())]),
        )),
        Err(e) => Ok(redirect_err(
            "/admin/backups",
            &crate::i18n::tr(
                "admin.flash.verification_failed",
                &[("error", &e.message())],
            ),
        )),
    }
}

#[derive(Deserialize)]
struct ConfirmForm {
    csrf: String,
}

async fn delete_backup(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    AxumPath(name): AxumPath<String>,
    axum::Form(form): axum::Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let name = valid_name(&name)?;
    match app.backup().delete(&name).await {
        Ok(true) => Ok(redirect_ok(
            "/admin/backups",
            &crate::i18n::tr("admin.flash.backup_deleted", &[("name", &name)]),
        )),
        Ok(false) => Ok(redirect_err("/admin/backups", "backup not found")),
        Err(e) => Ok(redirect_err("/admin/backups", &e.message())),
    }
}

// ---------------------------------------------------------------------------
// Restore (two-step: confirm page → typed confirmation)
// ---------------------------------------------------------------------------

async fn restore_confirm(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let name = valid_name(&name)?;
    let path = app.backup().storage_path_of(&name)?;
    let svc = app.backup();
    let manifest = match svc.read_manifest(&path) {
        Ok(m) => m,
        Err(e) => return Ok(redirect_err("/admin/backups", &e.message())),
    };
    if let Some(err) = manifest.compatibility_error(crate::backup::service::APP_VERSION) {
        return Ok(redirect_err(
            "/admin/backups",
            &crate::i18n::tr("admin.flash.cannot_restore", &[("error", &err)]),
        ));
    }

    let mut ctx = base_ctx(&app, &auth, "backups", &q);
    ctx.insert(
        "restore",
        &json!({
            "name": name,
            "kind": manifest.kind,
            "dialect": manifest.dialect,
            "created": time::format(manifest.created_at, "datetime"),
            "created_by": manifest.created_by,
            "version": manifest.polaris_version,
            "files": manifest.counts.files,
            "includes": {
                "database": manifest.includes.database,
                "media": manifest.includes.media,
                "themes": manifest.includes.themes,
                "plugins": manifest.includes.plugins,
            },
        }),
    );
    ctx.insert("current_dialect", svc.dialect_name());
    let body = templates::render_admin("backup_restore.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct RestoreForm {
    csrf: String,
    confirm: String,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    media: Option<String>,
    #[serde(default)]
    themes: Option<String>,
    #[serde(default)]
    plugins: Option<String>,
}

async fn restore_run(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    AxumPath(name): AxumPath<String>,
    axum::Form(form): axum::Form<RestoreForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_admin(&auth) {
        return Ok(redirect_err("/admin/backups", &e.message()));
    }
    let name = valid_name(&name)?;
    // Typed confirmation: the admin must re-enter the backup file name.
    if form.confirm.trim() != name {
        return Ok(redirect_err(
            "/admin/backups",
            "confirmation did not match the backup file name — nothing was restored",
        ));
    }
    let path = app.backup().storage_path_of(&name)?;
    let opts = crate::backup::restore::RestoreOptions {
        database: form.database.is_some(),
        media: form.media.is_some(),
        themes: form.themes.is_some(),
        plugins: form.plugins.is_some(),
    };
    tracing::info!(by = auth.username, backup = %name, "restore started via admin UI");
    let started = std::time::Instant::now();
    match app.backup().restore(&app, &path, opts).await {
        Ok(report) => {
            let summary = report.summary();
            let ms = started.elapsed().as_millis().to_string();
            let mut msg = crate::i18n::tr(
                "admin.flash.restore_ms",
                &[("summary", &summary), ("ms", &ms)],
            );
            if let Some(snap) = &report.snapshot {
                msg.push(' ');
                msg.push_str(&crate::i18n::tr(
                    "restore.report.snapshot",
                    &[("name", snap)],
                ));
            }
            for w in &report.warnings {
                msg.push(' ');
                msg.push_str(&crate::i18n::tr("restore.report.note", &[("warning", w)]));
            }
            Ok(redirect_ok("/admin/backups", &msg))
        }
        Err(e) => Ok(redirect_err(
            "/admin/backups",
            &crate::i18n::tr("admin.flash.restore_failed", &[("error", &e.message())]),
        )),
    }
}
