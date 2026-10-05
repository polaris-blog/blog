//! Public comment submission (honeypot + rate limited + moderated).

use axum::extract::{ConnectInfo, Form, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use std::net::SocketAddr;

use crate::error::AppResult;
use crate::repositories::posts as posts_repo;
use crate::services;
use crate::state::App;

#[derive(Deserialize)]
pub struct CommentForm {
    pub post_id: i64,
    pub post_slug: String,
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub url: String,
    pub content: String,
    /// Honeypot: hidden field that humans (and our own forms) never fill.
    #[serde(default)]
    pub website: String,
}

pub async fn submit(
    State(app): State<App>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Form(form): Form<CommentForm>,
) -> AppResult<Response> {
    if !app.comments_enabled() {
        return Ok((
            StatusCode::FORBIDDEN,
            crate::i18n::tr_or("comments are disabled"),
        )
            .into_response());
    }
    let back = format!("/posts/{}#comments", form.post_slug);

    // Honeypot triggered: pretend success, store nothing.
    if !form.website.trim().is_empty() {
        return Ok(Redirect::to(&back).into_response());
    }
    // Only allow commenting on published posts.
    let post_ok = posts_repo::find_by_id(&app.db, form.post_id)
        .await?
        .map(|p| p.slug == form.post_slug)
        .unwrap_or(false);
    if !post_ok {
        return Ok((
            StatusCode::NOT_FOUND,
            crate::i18n::tr_or("post not found"),
        )
            .into_response());
    }
    // Simple per-IP rate limit (5 submissions per 15 minutes).
    let ip =
        crate::utils::client_ip::resolve(addr.ip(), &headers, &app.config.security.trusted_proxies);
    let key = format!("comment:{ip}");
    if app.comment_limiter.is_locked(&key) {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            crate::i18n::tr_or("too many comments, try again later"),
        )
            .into_response());
    }
    app.comment_limiter.record_failure(&key);

    let parent_id = form
        .parent_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());

    let result = services::comments::create(
        &app,
        services::comments::NewComment {
            post_id: form.post_id,
            parent_id,
            author_name: form.name,
            author_email: form.email,
            author_url: form.url,
            content: form.content,
        },
        app.comments_moderate(),
    )
    .await;
    match result {
        Ok(_) => Ok(Redirect::to(&back).into_response()),
        Err(e) => Ok((StatusCode::BAD_REQUEST, e.message()).into_response()),
    }
}
