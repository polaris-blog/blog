use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tera::Context;

use crate::error::AppResult;
use crate::repositories::{comments as comments_repo, terms};
use crate::services;
use crate::state::App;
use crate::utils::hash;

use super::{excerpt, page_context, post_to_json, render_theme};

/// Serve an HTML page with ETag / 304 support.
fn html_response(headers: &HeaderMap, body: String) -> Response {
    let etag = hash::etag(body.as_bytes());
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        let hit = inm
            .trim()
            .trim_start_matches("W/")
            .split(',')
            .any(|c| c.trim() == etag);
        if hit {
            let mut resp = StatusCode::NOT_MODIFIED.into_response();
            resp.headers_mut().insert(
                header::ETAG,
                axum::http::HeaderValue::from_str(&etag).unwrap(),
            );
            return resp;
        }
    }
    let mut resp = (StatusCode::OK, axum::body::Body::from(body)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(
        header::ETAG,
        axum::http::HeaderValue::from_str(&etag).unwrap(),
    );
    h.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    resp
}

async fn render_page(
    app: &App,
    template: &str,
    ctx: &Context,
    headers: &HeaderMap,
) -> AppResult<Response> {
    let body = render_theme(app, template, ctx)?;
    Ok(html_response(headers, body))
}

fn paginate(current: i64, pages: i64) -> serde_json::Value {
    json!({
        "current": current,
        "pages": pages,
        "has_prev": current > 1,
        "has_next": current < pages,
        "prev": (current - 1).max(1),
        "next": (current + 1),
    })
}

async fn posts_list_json(
    app: &App,
    posts: &[crate::models::Post],
) -> AppResult<Vec<serde_json::Value>> {
    let mut out = Vec::with_capacity(posts.len());
    for post in posts {
        let html = services::posts::render_content(
            app,
            services::posts::KIND_POST,
            post.id,
            post.updated_at,
            &post.content_md,
        );
        let mut v = post_to_json(post, None);
        v["excerpt"] = json!(excerpt(post, &html));
        out.push(v);
    }
    services::media::attach_featured_media(app, &mut out).await;
    Ok(out)
}

pub async fn index(State(app): State<App>, headers: HeaderMap) -> AppResult<Response> {
    index_impl(app, 1, headers).await
}

pub async fn index_page(
    State(app): State<App>,
    Path(page): Path<i64>,
    headers: HeaderMap,
) -> AppResult<Response> {
    index_impl(app, page, headers).await
}

async fn index_impl(app: App, page: i64, headers: HeaderMap) -> AppResult<Response> {
    let list = services::posts::list_public(&app, page, None).await?;
    let mut ctx = page_context(&app).await;
    ctx.insert("posts", &posts_list_json(&app, &list.posts).await?);
    ctx.insert("pagination", &paginate(list.page, list.pages));
    ctx.insert("pagination_base", "");
    ctx.insert("is_home", &true);
    render_page(&app, "index.html", &ctx, &headers).await
}

/// Flatten approved comments into a depth-first list with a `depth` field
/// for template rendering (Tera has no recursion). Children follow their
/// parent; replies whose parent is missing or not approved are promoted to
/// the top level so no comment is silently hidden.
///
/// The source query is `created_at ASC`, so siblings stay chronological.
fn flatten_comments(comments: Vec<crate::models::Comment>) -> Vec<serde_json::Value> {
    use std::collections::{HashMap, HashSet};

    // Mirrors the submission cap in services::comments (root = depth 0).
    const MAX_DEPTH: usize = 4;

    fn walk(
        nodes: &mut Vec<serde_json::Value>,
        level: &[crate::models::Comment],
        children: &HashMap<i64, Vec<crate::models::Comment>>,
        depth: usize,
    ) {
        for c in level {
            let depth = depth.min(MAX_DEPTH);
            nodes.push(json!({
                "id": c.id,
                "author": c.author_name,
                "author_url": if crate::markdown::url_is_safe(&c.author_url) {
                    c.author_url.clone()
                } else {
                    String::new()
                },
                "content": c.content,
                "date": crate::utils::time::format(c.created_at, "date"),
                "depth": depth,
            }));
            if let Some(kids) = children.get(&c.id) {
                walk(nodes, kids, children, depth + 1);
            }
        }
    }

    let ids: HashSet<i64> = comments.iter().map(|c| c.id).collect();
    let mut children: HashMap<i64, Vec<crate::models::Comment>> = HashMap::new();
    let mut roots: Vec<crate::models::Comment> = Vec::new();
    for c in comments {
        match c.parent_id {
            Some(pid) if ids.contains(&pid) => children.entry(pid).or_default().push(c),
            _ => roots.push(c),
        }
    }
    let mut nodes = Vec::new();
    walk(&mut nodes, &roots, &children, 0);
    nodes
}

pub async fn post(
    State(app): State<App>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let Some(post) = services::posts::get_public_post(&app, &slug).await? else {
        return not_found_response(&app).await;
    };
    let html = services::posts::render_content(
        &app,
        services::posts::KIND_POST,
        post.id,
        post.updated_at,
        &post.content_md,
    );
    let comments_enabled = app.comments_enabled();
    let approved = if comments_enabled {
        comments_repo::list_approved_for_post(&app.db, post.id).await?
    } else {
        Vec::new()
    };
    let comments_json = flatten_comments(approved);

    let mut post_json = post_to_json(&post, Some(html.to_string()));
    services::media::attach_featured_media(&app, std::slice::from_mut(&mut post_json)).await;
    let base = app.base_url();
    let canonical = if base.is_empty() {
        format!("/posts/{}", post.slug)
    } else {
        format!("{}/posts/{}", base.trim_end_matches('/'), post.slug)
    };
    let og_image = post.featured_image.clone().unwrap_or_default();
    let description = if post.summary.is_empty() {
        crate::markdown::truncate_chars(&crate::markdown::html_to_text(&html), 160)
    } else {
        post.summary.clone()
    };
    let json_ld = json!({
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": post.title,
        "description": description,
        "datePublished": crate::utils::time::format(post.published_at.unwrap_or(post.created_at), "rfc3339"),
        "dateModified": crate::utils::time::format(post.updated_at, "rfc3339"),
        "author": { "@type": "Person", "name": post.author_name.clone().unwrap_or_default() },
        "image": og_image,
    })
    .to_string();

    let mut ctx = page_context(&app).await;
    ctx.insert("post", &post_json);
    ctx.insert("comments", &comments_json);
    ctx.insert("comments_enabled", &comments_enabled);
    ctx.insert(
        "seo",
        &json!({
            "title": post.title,
            "description": description,
            "canonical": canonical,
            "og_type": "article",
            "og_image": og_image,
        }),
    );
    ctx.insert("json_ld", &json_ld);
    render_page(&app, "post.html", &ctx, &headers).await
}

pub async fn page(
    State(app): State<App>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let Some(p) = crate::repositories::pages::find_by_slug(&app.db, &slug).await? else {
        return not_found_response(&app).await;
    };
    if p.status != crate::models::PostStatus::Published {
        return not_found_response(&app).await;
    }
    let html: Arc<String> = services::posts::render_content(
        &app,
        services::posts::KIND_PAGE,
        p.id,
        p.updated_at,
        &p.content_md,
    );
    let mut page_json = json!(p);
    if let serde_json::Value::Object(o) = &mut page_json {
        o.insert("content_html".into(), json!(html.to_string()));
        o.insert(
            "date".into(),
            json!(crate::utils::time::format(p.updated_at, "date")),
        );
    }
    let base = app.base_url();
    let canonical = if base.is_empty() {
        format!("/{slug}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), slug)
    };

    let mut ctx = page_context(&app).await;
    ctx.insert("page", &page_json);
    ctx.insert(
        "seo",
        &json!({
            "title": p.title,
            "description": p.summary,
            "canonical": canonical,
            "og_type": "website",
        }),
    );
    render_page(&app, "page.html", &ctx, &headers).await
}

async fn term_page(
    app: App,
    kind: crate::models::TermKind,
    slug: String,
    page: i64,
    headers: HeaderMap,
) -> AppResult<Response> {
    let Some(term) = terms::find_by_slug(&app.db, kind, &slug).await? else {
        return not_found_response(&app).await;
    };
    let list = services::posts::list_public(&app, page, Some(term.id)).await?;
    let mut ctx = page_context(&app).await;
    ctx.insert(
        "term",
        &json!({ "name": term.name, "slug": term.slug, "kind": kind.as_str() }),
    );
    ctx.insert("posts", &posts_list_json(&app, &list.posts).await?);
    ctx.insert("pagination", &paginate(list.page, list.pages));
    ctx.insert(
        "pagination_base",
        &format!("/{}/{}", kind.as_str(), term.slug),
    );
    let template = match kind {
        crate::models::TermKind::Category => "category.html",
        crate::models::TermKind::Tag => "tag.html",
    };
    render_page(&app, template, &ctx, &headers).await
}

pub async fn category(
    State(app): State<App>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    term_page(app, crate::models::TermKind::Category, slug, 1, headers).await
}

pub async fn category_page(
    State(app): State<App>,
    Path((slug, page)): Path<(String, i64)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    term_page(app, crate::models::TermKind::Category, slug, page, headers).await
}

pub async fn tag(
    State(app): State<App>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    term_page(app, crate::models::TermKind::Tag, slug, 1, headers).await
}

pub async fn tag_page(
    State(app): State<App>,
    Path((slug, page)): Path<(String, i64)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    term_page(app, crate::models::TermKind::Tag, slug, page, headers).await
}

pub async fn not_found_response(app: &App) -> AppResult<Response> {
    let ctx = page_context(app).await;
    let body = render_theme(app, "404.html", &ctx)?;
    let mut resp = (StatusCode::NOT_FOUND, axum::body::Body::from(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    Ok(resp)
}

pub async fn not_found(State(app): State<App>) -> Response {
    not_found_response(&app)
        .await
        .unwrap_or_else(|e| e.into_response())
}
