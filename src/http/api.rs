//! Lightweight REST API.
//!
//! ```text
//! GET    /api/posts         published posts (paginated)
//! GET    /api/posts/{id}    a published post
//! POST   /api/posts         create            ┐
//! PUT    /api/posts/{id}    update            │ session required
//! DELETE /api/posts/{id}    delete            ┘
//! GET    /api/pages         published pages
//! GET    /api/categories    categories + counts
//! GET    /api/tags          tags + counts
//! GET    /api/comments      comments (session required)
//! ```

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthCtx;
use crate::error::{ApiError, AppResult};
use crate::models::{Role, TermKind};
use crate::repositories;
use crate::services;
use crate::state::App;
use crate::utils::time;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    10
}

fn list_meta(total: i64, page: i64, per_page: i64) -> serde_json::Value {
    json!({ "total": total, "page": page, "per_page": per_page })
}

pub async fn list_posts(
    State(app): State<App>,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let per = q.per_page.clamp(1, 50);
    let page = q.page.max(1);
    // Cache-aside read (shares the `posts` namespace with the site itself).
    let list = services::posts::list_public_full(&app, page, per, None).await?;
    let data: Vec<_> = list
        .posts
        .iter()
        .map(|p| {
            let mut v = json!(p);
            if let serde_json::Value::Object(o) = &mut v {
                o.insert("url".into(), json!(format!("/posts/{}", p.slug)));
                o.insert("cover_image".into(), json!(p.featured_image.clone()));
                o.insert(
                    "published_at_iso".into(),
                    json!(time::format(
                        p.published_at.unwrap_or(p.created_at),
                        "rfc3339"
                    )),
                );
            }
            v
        })
        .collect();
    Ok(Json(json!({ "data": data, "meta": list_meta(list.total, page, per) })).into_response())
}

pub async fn get_post(State(app): State<App>, Path(id): Path<i64>) -> Result<Response, ApiError> {
    // Cache-aside read; only published posts are exposed here.
    let Some(post) = services::posts::get_public_post_by_id(&app, id).await? else {
        return Err(ApiError(crate::error::AppError::NotFound(
            "post not found".into(),
        )));
    };
    let html = services::posts::render_content(
        &app,
        services::posts::KIND_POST,
        post.id,
        post.updated_at,
        &post.content_md,
    );
    let mut v = json!(post);
    if let serde_json::Value::Object(o) = &mut v {
        o.insert("content_html".into(), json!(html.to_string()));
        o.insert("url".into(), json!(format!("/posts/{}", post.slug)));
        o.insert("cover_image".into(), json!(post.featured_image.clone()));
    }
    Ok(Json(json!({ "data": v })).into_response())
}

/// JSON body accepted by POST/PUT /api/posts.
#[derive(Deserialize)]
pub struct ApiPostBody {
    pub title: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub featured_image: Option<String>,
    /// Alias for `featured_image`, used by clients that call it a cover image.
    #[serde(default)]
    pub cover_image: Option<String>,
    /// `YYYY-MM-DDTHH:MM[:SS]` — publish time for scheduled posts.
    #[serde(default)]
    pub publish_at: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ApiPostBody {
    fn into_input(self) -> AppResult<services::posts::PostInput> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err(crate::error::AppError::BadRequest(
                "title must not be empty".into(),
            ));
        }
        let status = if self.status.is_empty() {
            crate::models::PostStatus::Draft
        } else {
            crate::models::PostStatus::parse(self.status.trim())
                .ok_or_else(|| crate::error::AppError::BadRequest("invalid status".into()))?
        };
        let publish_at = match self.publish_at.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s) => Some(time::parse_datetime_local(s).ok_or_else(|| {
                crate::error::AppError::BadRequest(
                    "publish_at must be formatted YYYY-MM-DDTHH:MM[:SS]".into(),
                )
            })?),
        };
        Ok(services::posts::PostInput {
            title,
            slug: self.slug,
            summary: self.summary,
            content_md: self.content,
            status,
            featured_image: self.featured_image.or(self.cover_image),
            publish_at,
            category: self.category,
            tags: self.tags,
        })
    }
}

pub async fn create_post(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Json(body): Json<ApiPostBody>,
) -> Result<Response, ApiError> {
    let input = body.into_input().map_err(ApiError)?;
    let post = services::posts::create_post(&app, auth.user_id, input)
        .await
        .map_err(ApiError)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "data": { "id": post.id, "slug": post.slug, "url": format!("/posts/{}", post.slug) } })),
    )
        .into_response())
}

pub async fn update_post(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Json(body): Json<ApiPostBody>,
) -> Result<Response, ApiError> {
    let existing = repositories::posts::find_by_id(&app.db, id)
        .await
        .map_err(ApiError)?
        .ok_or_else(|| ApiError(crate::error::AppError::NotFound("post not found".into())))?;
    if !can_edit(&auth, existing.author_id) {
        return Err(ApiError(crate::error::AppError::Forbidden(
            "you may only edit your own posts".into(),
        )));
    }
    let input = body.into_input().map_err(ApiError)?;
    let post = services::posts::update_post(&app, id, input)
        .await
        .map_err(ApiError)?;
    Ok(Json(json!({ "data": { "id": post.id, "slug": post.slug } })).into_response())
}

pub async fn delete_post(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let existing = repositories::posts::find_by_id(&app.db, id)
        .await
        .map_err(ApiError)?
        .ok_or_else(|| ApiError(crate::error::AppError::NotFound("post not found".into())))?;
    if !can_edit(&auth, existing.author_id) {
        return Err(ApiError(crate::error::AppError::Forbidden(
            "you may only delete your own posts".into(),
        )));
    }
    services::posts::delete_post(&app, id)
        .await
        .map_err(ApiError)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn can_edit(auth: &AuthCtx, owner_id: i64) -> bool {
    auth.role.at_least(Role::Editor) || auth.user_id == owner_id
}

pub async fn list_pages(State(app): State<App>) -> Result<Response, ApiError> {
    let pages = repositories::pages::list(&app.db, true)
        .await
        .map_err(ApiError)?;
    Ok(Json(json!({ "data": pages })).into_response())
}

pub async fn list_categories(State(app): State<App>) -> Result<Response, ApiError> {
    // Cache-aside over the `category` namespace.
    let terms: Vec<crate::models::Term> = app
        .cache
        .get_or_load(crate::cache::ns::CATEGORY, "all", async {
            repositories::terms::list_with_counts(&app.db, TermKind::Category)
                .await
                .map_err(anyhow::Error::from)
        })
        .await
        .map_err(|e| ApiError(crate::error::AppError::Internal(e)))?;
    Ok(Json(json!({ "data": terms })).into_response())
}

pub async fn list_tags(State(app): State<App>) -> Result<Response, ApiError> {
    let terms: Vec<crate::models::Term> = app
        .cache
        .get_or_load(crate::cache::ns::TAG, "all", async {
            repositories::terms::list_with_counts(&app.db, TermKind::Tag)
                .await
                .map_err(anyhow::Error::from)
        })
        .await
        .map_err(|e| ApiError(crate::error::AppError::Internal(e)))?;
    Ok(Json(json!({ "data": terms })).into_response())
}

#[derive(Deserialize)]
pub struct CommentsQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

pub async fn list_comments(
    State(app): State<App>,
    Extension(_auth): Extension<AuthCtx>,
    Query(q): Query<CommentsQuery>,
) -> Result<Response, ApiError> {
    // Only editors and admins may browse comments through the API.
    if !_auth.role.at_least(Role::Editor) {
        return Err(ApiError(crate::error::AppError::Forbidden(
            "editor role required".into(),
        )));
    }
    let status = q
        .status
        .as_deref()
        .and_then(crate::models::CommentStatus::parse);
    let per = q.per_page.clamp(1, 50);
    let page = q.page.max(1);
    let (comments, total) = repositories::comments::list(&app.db, status, page, per)
        .await
        .map_err(ApiError)?;
    Ok(Json(json!({ "data": comments, "meta": list_meta(total, page, per) })).into_response())
}

// ---------------------------------------------------------------------------
// Cache management (admin)
// ---------------------------------------------------------------------------

/// `GET /api/cache/stats` — hit rate, entries, driver. Editor+.
pub async fn cache_stats(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
) -> Result<Response, ApiError> {
    if !auth.role.at_least(Role::Editor) {
        return Err(ApiError(crate::error::AppError::Forbidden(
            "editor role required".into(),
        )));
    }
    Ok(Json(json!({ "data": app.cache.stats() })).into_response())
}

/// `POST /api/cache/clear` — drop every cached entry. Admin only.
///
/// The endpoint takes no body, so a cross-site form or "simple" fetch could
/// forge it against a logged-in admin session (JSON endpoints are protected
/// by the browser's preflight rules; body-less POSTs are not). The session
/// CSRF token is therefore required in the `X-CSRF-Token` header.
pub async fn cache_clear(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    if !auth.role.at_least(Role::Admin) {
        return Err(ApiError(crate::error::AppError::Forbidden(
            "admin role required".into(),
        )));
    }
    let token = headers
        .get(axum::http::HeaderName::from_static("x-csrf-token"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    crate::auth::ensure_csrf(&auth, token).map_err(ApiError)?;
    app.cache
        .clear_all()
        .await
        .map_err(|e| ApiError(crate::error::AppError::Internal(e)))?;
    tracing::info!(by = auth.username, "cache cleared");
    Ok(Json(json!({ "data": app.cache.stats() })).into_response())
}

/// `DELETE /api/cache/{key}` — remove one logical key (e.g.
/// `post:slug:hello`, `rss:http://example.com`). Admin only.
pub async fn cache_delete_key(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(key): Path<String>,
) -> Result<Response, ApiError> {
    if !auth.role.at_least(Role::Admin) {
        return Err(ApiError(crate::error::AppError::Forbidden(
            "admin role required".into(),
        )));
    }
    let deleted = app.cache.delete_logical(&key).await;
    Ok(Json(json!({ "data": { "key": key, "deleted": deleted } })).into_response())
}
