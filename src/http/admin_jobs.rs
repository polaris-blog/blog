use super::admin::{PageQuery, base_ctx, redirect_ok};
use crate::scheduler::{Action, JobRequest, repository::JobFilter};
use crate::{
    auth::{self, AuthCtx},
    error::{ApiError, AppError, AppResult},
    models::Role,
    state::App,
    templates,
};
use axum::{
    Form, Json, Router,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

pub fn router(app: App) -> Router<App> {
    let pages = Router::new()
        .route("/admin/jobs", get(page))
        .route("/admin/jobs/{id}", get(detail))
        .route("/admin/jobs/{id}/{action}", get(confirm).post(control))
        .route_layer(axum::middleware::from_fn_with_state(
            app.clone(),
            auth::admin_auth_mw,
        ));
    let api = Router::new()
        .route("/api/admin/jobs", get(api_list).post(api_create))
        .route("/api/admin/jobs/{id}", get(api_detail).delete(api_delete))
        .route("/api/admin/jobs/{id}/{action}", post(api_control))
        .route_layer(axum::middleware::from_fn_with_state(app, auth::api_auth_mw));
    pages.merge(api)
}

fn require_admin(auth: &AuthCtx) -> AppResult<()> {
    if auth.role != Role::Admin {
        return Err(AppError::Forbidden("admin role required".into()));
    }
    Ok(())
}
fn mutation(auth: &AuthCtx, headers: &HeaderMap) -> AppResult<()> {
    require_admin(auth)?;
    auth::ensure_csrf(
        auth,
        headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
    )
}

async fn page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(filter): Query<JobFilter>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    require_admin(&auth)?;
    let status = filter.status.map(|s| s.as_str()).unwrap_or_default();
    let mut ctx = base_ctx(&app, &auth, "jobs", &q);
    let page = app.scheduler.list(filter.clone()).await?;
    ctx.insert("jobs", &page.jobs);
    ctx.insert("next_cursor", &page.next_cursor);
    ctx.insert("status_filter", status);
    ctx.insert(
        "type_filter",
        &filter.job_type.as_deref().unwrap_or_default(),
    );
    ctx.insert("limit_filter", &filter.limit.unwrap_or(25).clamp(1, 100));
    ctx.insert("scheduler_enabled", &app.config.scheduler.enabled);
    Ok(Html(templates::render_admin("jobs.html", &ctx)?).into_response())
}

async fn detail(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    require_admin(&auth)?;
    let mut ctx = base_ctx(&app, &auth, "jobs", &q);
    let job = app.scheduler.get(&id).await?;
    ctx.insert("job", &job);
    ctx.insert(
        "payload",
        &serde_json::to_string_pretty(&job.payload).unwrap_or_default(),
    );
    ctx.insert("attempts", &app.scheduler.history(&id).await?);
    Ok(Html(templates::render_admin("job_detail.html", &ctx)?).into_response())
}

async fn confirm(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path((id, action)): Path<(String, Action)>,
) -> AppResult<Response> {
    require_admin(&auth)?;
    let mut ctx = base_ctx(&app, &auth, "jobs", &PageQuery::default());
    ctx.insert("job", &app.scheduler.get(&id).await?);
    ctx.insert("action", &action);
    Ok(Html(templates::render_admin("job_confirm.html", &ctx)?).into_response())
}

#[derive(Deserialize)]
struct Confirm {
    csrf: String,
    confirm: String,
}
async fn control(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path((id, action)): Path<(String, Action)>,
    Form(form): Form<Confirm>,
) -> AppResult<Response> {
    require_admin(&auth)?;
    auth::ensure_csrf(&auth, &form.csrf)?;
    if form.confirm != id {
        return Err(AppError::BadRequest("confirmation required".into()));
    }
    app.scheduler.action(&id, action, None).await?;
    let target = if matches!(action, Action::Delete) {
        "/admin/jobs".into()
    } else {
        format!("/admin/jobs/{id}")
    };
    Ok(redirect_ok(&target, "Job updated"))
}

async fn api_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(filter): Query<JobFilter>,
) -> Result<Response, ApiError> {
    require_admin(&auth)?;
    Ok(Json(app.scheduler.list(filter).await?).into_response())
}
async fn api_detail(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    require_admin(&auth)?;
    Ok(Json(json!({ "job": app.scheduler.get(&id).await?, "attempts": app.scheduler.history(&id).await? })).into_response())
}
async fn api_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    headers: HeaderMap,
    Json(request): Json<JobRequest>,
) -> Result<Response, ApiError> {
    mutation(&auth, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(app.scheduler.create(request).await?),
    )
        .into_response())
}
async fn api_control(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path((id, action)): Path<(String, Action)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    mutation(&auth, &headers)?;
    if matches!(action, Action::Delete) {
        return Err(AppError::BadRequest("use DELETE to delete a job".into()).into());
    }
    app.scheduler.action(&id, action, None).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn api_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    mutation(&auth, &headers)?;
    if headers.get("x-confirm-job").and_then(|v| v.to_str().ok()) != Some(&id) {
        return Err(AppError::BadRequest("x-confirm-job must match job id".into()).into());
    }
    app.scheduler.action(&id, Action::Delete, None).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
