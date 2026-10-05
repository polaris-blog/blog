use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cache::ns;
use crate::error::{AppError, AppResult};
use crate::models::{Post, PostStatus, TermKind};
use crate::plugins;
use crate::repositories::{pages, posts, terms};
use crate::state::{App, MdKey};
use crate::utils::{slug, time};

use super::media;

/// Reserved top-level slugs (URLs owned by the engine).
pub const RESERVED_SLUGS: &[&str] = &[
    "admin",
    "api",
    "static",
    "plugins",
    "posts",
    "page",
    "category",
    "categories",
    "tag",
    "tags",
    "rss.xml",
    "atom.xml",
    "sitemap.xml",
    "robots.txt",
    "comments",
    "feed",
    "favicon.ico",
    "login",
    "logout",
    "media",
    "search",
];

#[derive(Clone, Debug, Default)]
pub struct PostInput {
    pub title: String,
    pub slug: Option<String>,
    pub summary: String,
    pub content_md: String,
    pub status: PostStatus,
    pub featured_image: Option<String>,
    pub publish_at: Option<i64>,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PageInput {
    pub title: String,
    pub slug: Option<String>,
    pub summary: String,
    pub content_md: String,
    pub status: PostStatus,
    pub sort_order: i64,
}

fn validate_title(title: &str) -> AppResult<()> {
    let t = title.trim();
    if t.is_empty() {
        return Err(AppError::BadRequest("title must not be empty".into()));
    }
    if t.chars().count() > 255 {
        return Err(AppError::BadRequest(
            "title is too long (max 255 chars)".into(),
        ));
    }
    Ok(())
}

fn normalize_cover(value: Option<&str>) -> AppResult<Option<String>> {
    let Some(url) = value.map(str::trim).filter(|url| !url.is_empty()) else {
        return Ok(None);
    };
    if url.len() > 512 || !crate::config_schema::is_valid_image_url(url) {
        return Err(AppError::BadRequest(
            "Cover image must be an http(s) URL or a site-relative path (max 512 bytes)".into(),
        ));
    }
    Ok(Some(url.to_string()))
}

/// Generate a slug, falling back when the title has no usable characters.
/// `kind` is "post" or "page".
pub async fn unique_slug(
    app: &App,
    title: &str,
    desired: Option<&str>,
    except_id: Option<i64>,
    kind: &str,
) -> AppResult<String> {
    let base = match desired {
        Some(d) if !d.trim().is_empty() => slug::slugify(d),
        _ => slug::slugify(title),
    };
    let base = if base.is_empty() {
        slug::fallback_slug(kind)
    } else {
        base
    };
    async fn taken(app: &App, kind: &str, s: &str, except_id: Option<i64>) -> AppResult<bool> {
        match kind {
            "page" => pages::slug_taken(&app.db, s, except_id).await,
            _ => posts::slug_taken(&app.db, s, except_id).await,
        }
    }
    if !taken(app, kind, &base, except_id).await? {
        return Ok(base);
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !taken(app, kind, &candidate, except_id).await? {
            return Ok(candidate);
        }
    }
    Err(AppError::Conflict(
        "could not generate a unique slug".into(),
    ))
}

fn input_to_json(
    title: &str,
    s: &str,
    content: &str,
    status: &PostStatus,
    tags: &[String],
) -> Value {
    json!({
        "title": title,
        "summary": s,
        "content": content,
        "status": status.as_str(),
        "tags": tags,
    })
}

fn read_back(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

// ---------------------------------------------------------------------------
// Posts
// ---------------------------------------------------------------------------

pub async fn create_post(app: &App, author_id: i64, input: PostInput) -> AppResult<Post> {
    validate_title(&input.title)?;
    let featured_image = normalize_cover(input.featured_image.as_deref())?;
    let mut payload = input_to_json(
        &input.title,
        &input.summary,
        &input.content_md,
        &input.status,
        &input.tags,
    );
    if let Some(s) = input.slug.as_deref() {
        payload["slug"] = json!(s);
    }
    plugins::hook_json(app, "before_post_create", &mut payload);

    let title = read_back(&payload, "title").unwrap_or_else(|| input.title.clone());
    validate_title(&title)?;
    let summary = read_back(&payload, "summary").unwrap_or_else(|| input.summary.clone());
    let content = read_back(&payload, "content").unwrap_or_else(|| input.content_md.clone());
    let status = read_back(&payload, "status")
        .and_then(|s| PostStatus::parse(&s))
        .unwrap_or(input.status);
    let desired = payload
        .get("slug")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tags = payload
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| input.tags.clone());

    let published_at = match status {
        PostStatus::Published => Some(time::now()),
        PostStatus::Scheduled => Some(input.publish_at.unwrap_or_else(|| time::now() + 3600)),
        PostStatus::Draft => None,
    };
    let slug_str = unique_slug(app, &title, desired.as_deref(), None, "post").await?;
    let new = posts::NewPost {
        title,
        slug: slug_str.clone(),
        summary,
        content_md: content,
        author_id,
        status,
        featured_image,
        published_at,
    };
    let id = posts::insert(&app.db, &new).await?;
    sync_terms(app, id, input.category.as_deref(), &tags).await?;
    let media_content = format!(
        "{}\n{}",
        new.content_md,
        new.featured_image.as_deref().unwrap_or("")
    );
    if let Err(e) = media::sync_references(app, "post", id, &media_content).await {
        tracing::warn!(error = %e, id, "media reference sync failed");
    }
    let mut post = posts::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("post vanished after insert")))?;
    post.terms = terms::list_for_post(&app.db, id).await?;
    if let Err(e) = app.search.index_post(&app.db, &post).await {
        tracing::warn!(error = %e, id, "search indexing failed — run `polaris search rebuild` to fix");
    }

    let event = post_json(app, &post).await;
    plugins::event_json(app, "after_post_create", &event);
    app.invalidate_content().await;
    Ok(post)
}

pub async fn update_post(app: &App, id: i64, input: PostInput) -> AppResult<Post> {
    let existing = posts::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("post not found".into()))?;
    validate_title(&input.title)?;
    let featured_image = normalize_cover(input.featured_image.as_deref())?;

    let mut payload = input_to_json(
        &input.title,
        &input.summary,
        &input.content_md,
        &input.status,
        &input.tags,
    );
    payload["id"] = json!(id);
    if let Some(s) = input.slug.as_deref() {
        payload["slug"] = json!(s);
    }
    plugins::hook_json(app, "before_post_update", &mut payload);

    let title = read_back(&payload, "title").unwrap_or_else(|| input.title.clone());
    validate_title(&title)?;
    let summary = read_back(&payload, "summary").unwrap_or_else(|| input.summary.clone());
    let content = read_back(&payload, "content").unwrap_or_else(|| input.content_md.clone());
    let status = read_back(&payload, "status")
        .and_then(|s| PostStatus::parse(&s))
        .unwrap_or(input.status);
    let desired = payload
        .get("slug")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tags = payload
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| input.tags.clone());

    // Keep the original publish date when staying published.
    let published_at = match status {
        PostStatus::Published => Some(existing.published_at.unwrap_or_else(time::now)),
        PostStatus::Scheduled => Some(
            input
                .publish_at
                .or(existing.published_at)
                .unwrap_or_else(|| time::now() + 3600),
        ),
        PostStatus::Draft => None,
    };

    let slug_str = if desired.as_deref() == Some(existing.slug.as_str()) {
        existing.slug.clone()
    } else {
        unique_slug(app, &title, desired.as_deref(), Some(id), "post").await?
    };
    let new = posts::NewPost {
        title,
        slug: slug_str,
        summary,
        content_md: content,
        author_id: existing.author_id,
        status,
        featured_image,
        published_at,
    };
    posts::update(&app.db, id, &new).await?;
    sync_terms(app, id, input.category.as_deref(), &tags).await?;
    let media_content = format!(
        "{}\n{}",
        new.content_md,
        new.featured_image.as_deref().unwrap_or("")
    );
    if let Err(e) = media::sync_references(app, "post", id, &media_content).await {
        tracing::warn!(error = %e, id, "media reference sync failed");
    }
    let mut post = posts::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("post vanished after update")))?;
    post.terms = terms::list_for_post(&app.db, id).await?;
    if let Err(e) = app.search.index_post(&app.db, &post).await {
        tracing::warn!(error = %e, id, "search reindexing failed — run `polaris search rebuild` to fix");
    }

    let event = post_json(app, &post).await;
    plugins::event_json(app, "after_post_update", &event);
    app.invalidate_content().await;
    Ok(post)
}

pub async fn delete_post(app: &App, id: i64) -> AppResult<()> {
    let existing = posts::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("post not found".into()))?;
    let event = post_json(app, &existing).await;
    plugins::event_json(app, "before_post_delete", &event);
    posts::delete(&app.db, id).await?;
    if let Err(e) = media::sync_references(app, "post", id, "").await {
        tracing::warn!(error = %e, id, "media reference cleanup failed");
    }
    if let Err(e) = app.search.remove(&app.db, "post", id).await {
        tracing::warn!(error = %e, id, "search index removal failed — run `polaris search rebuild` to fix");
    }
    plugins::event_json(app, "after_post_delete", &event);
    app.invalidate_content().await;
    Ok(())
}

async fn sync_terms(
    app: &App,
    post_id: i64,
    category: Option<&str>,
    tags: &[String],
) -> AppResult<()> {
    let mut ids = Vec::new();
    if let Some(cat) = category.map(str::trim).filter(|c| !c.is_empty()) {
        let cslug = slug::slugify(cat);
        if !cslug.is_empty() {
            ids.push(terms::ensure(&app.db, TermKind::Category, cat, &cslug).await?);
        }
    }
    for tag in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        let tslug = slug::slugify(tag);
        if tslug.is_empty() {
            continue;
        }
        let id = terms::ensure(&app.db, TermKind::Tag, tag, &tslug).await?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    terms::set_post_terms(&app.db, post_id, &ids).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

pub async fn create_page(
    app: &App,
    author_id: i64,
    input: PageInput,
) -> AppResult<crate::models::Page> {
    validate_title(&input.title)?;
    let mut payload = json!({
        "title": input.title, "summary": input.summary,
        "content": input.content_md, "status": input.status.as_str(),
    });
    if let Some(s) = input.slug.as_deref() {
        payload["slug"] = json!(s);
    }
    plugins::hook_json(app, "before_page_create", &mut payload);
    let title = read_back(&payload, "title").unwrap_or_else(|| input.title.clone());
    validate_title(&title)?;
    let summary = read_back(&payload, "summary").unwrap_or_else(|| input.summary.clone());
    let content = read_back(&payload, "content").unwrap_or_else(|| input.content_md.clone());
    let desired = payload
        .get("slug")
        .and_then(Value::as_str)
        .map(str::to_string);

    let slug_str = page_slug(app, &title, desired.as_deref(), None).await?;
    let new = pages::NewPage {
        title,
        slug: slug_str,
        summary,
        content_md: content,
        author_id,
        status: input.status,
        sort_order: input.sort_order,
    };
    let id = pages::insert(&app.db, &new).await?;
    if let Err(e) = media::sync_references(app, "page", id, &new.content_md).await {
        tracing::warn!(error = %e, id, "media reference sync failed");
    }
    let page = pages::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("page vanished after insert")))?;
    let event = serde_json::to_value(&page).unwrap_or(Value::Null);
    plugins::event_json(app, "after_page_create", &event);
    if let Err(e) = app.search.index_page(&app.db, &page).await {
        tracing::warn!(error = %e, id, "search indexing failed — run `polaris search rebuild` to fix");
    }
    app.invalidate_content().await;
    Ok(page)
}

pub async fn update_page(app: &App, id: i64, input: PageInput) -> AppResult<crate::models::Page> {
    let existing = pages::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("page not found".into()))?;
    validate_title(&input.title)?;
    let slug_str = if input.slug.as_deref() == Some(existing.slug.as_str()) {
        existing.slug.clone()
    } else {
        page_slug(app, &input.title, input.slug.as_deref(), Some(id)).await?
    };
    let new = pages::NewPage {
        title: input.title,
        slug: slug_str,
        summary: input.summary,
        content_md: input.content_md,
        author_id: existing.author_id,
        status: input.status,
        sort_order: input.sort_order,
    };
    pages::update(&app.db, id, &new).await?;
    if let Err(e) = media::sync_references(app, "page", id, &new.content_md).await {
        tracing::warn!(error = %e, id, "media reference sync failed");
    }
    let page = pages::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("page vanished after update")))?;
    let event = serde_json::to_value(&page).unwrap_or(Value::Null);
    plugins::event_json(app, "after_page_update", &event);
    if let Err(e) = app.search.index_page(&app.db, &page).await {
        tracing::warn!(error = %e, id, "search reindexing failed — run `polaris search rebuild` to fix");
    }
    app.invalidate_content().await;
    Ok(page)
}

pub async fn delete_page(app: &App, id: i64) -> AppResult<()> {
    let existing = pages::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("page not found".into()))?;
    let event = serde_json::to_value(&existing).unwrap_or(Value::Null);
    plugins::event_json(app, "before_page_delete", &event);
    pages::delete(&app.db, id).await?;
    if let Err(e) = media::sync_references(app, "page", id, "").await {
        tracing::warn!(error = %e, id, "media reference cleanup failed");
    }
    if let Err(e) = app.search.remove(&app.db, "page", id).await {
        tracing::warn!(error = %e, id, "search index removal failed — run `polaris search rebuild` to fix");
    }
    plugins::event_json(app, "after_page_delete", &event);
    app.invalidate_content().await;
    Ok(())
}

async fn page_slug(
    app: &App,
    title: &str,
    desired: Option<&str>,
    except: Option<i64>,
) -> AppResult<String> {
    let s = unique_slug(app, title, desired, except, "page").await?;
    if RESERVED_SLUGS.contains(&s.as_str()) {
        return Err(AppError::Conflict(crate::i18n::tr(
            "'{slug}' is a reserved URL",
            &[("slug", &s)],
        )));
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub const KIND_POST: u8 = 1;
pub const KIND_PAGE: u8 = 2;

/// Render Markdown through the plugin pipeline with an in-memory cache.
pub fn render_content(app: &App, kind: u8, id: i64, updated: i64, md: &str) -> Arc<String> {
    let key = MdKey {
        kind,
        id,
        updated,
        generation: app.plugins.generation(),
    };
    if let Some(cached) = app.md_cache.get(&key) {
        return cached;
    }
    let source = app.plugins.hook_str("markdown_before", md);
    let html = crate::markdown::to_html(&source);
    let html = app.plugins.hook_str("markdown_after", &html);
    let out = Arc::new(html);
    app.md_cache.put(key, out.clone());
    out
}

async fn post_json(app: &App, post: &Post) -> Value {
    let mut v = serde_json::to_value(post).unwrap_or(Value::Null);
    if let Value::Object(ref mut o) = v {
        o.insert("reading_time".into(), json!(post.reading_time()));
        o.insert("url".into(), json!(format!("/posts/{}", post.slug)));
    }
    let _ = app;
    v
}

// ---------------------------------------------------------------------------
// Public queries used by HTTP handlers (cache-aside)
// ---------------------------------------------------------------------------

pub struct PostList {
    pub posts: Vec<Post>,
    pub total: i64,
    pub page: i64,
    pub pages: i64,
}

/// Cache DTO for a post. `Post`'s API JSON deliberately omits
/// `content_md`, so it is carried alongside the flattened object.
#[derive(Serialize, Deserialize)]
struct CachedPost {
    #[serde(flatten)]
    post: Post,
    content_md: String,
}

impl CachedPost {
    fn from(post: &Post) -> Self {
        Self {
            post: post.clone(),
            content_md: post.content_md.clone(),
        }
    }

    fn into_post(self) -> Post {
        let mut p = self.post;
        p.content_md = self.content_md;
        p
    }
}

#[derive(Serialize, Deserialize)]
struct CachedPostList {
    posts: Vec<CachedPost>,
    total: i64,
}

/// Published post list (the homepage is page 1 with no term filter).
/// Cache key: `posts:v{n}:{page}:{per}:{term}`.
pub async fn list_public(app: &App, page: i64, term_id: Option<i64>) -> AppResult<PostList> {
    list_public_full(app, page, app.posts_per_page() as i64, term_id).await
}

pub async fn list_public_full(
    app: &App,
    page: i64,
    per: i64,
    term_id: Option<i64>,
) -> AppResult<PostList> {
    let per = per.clamp(1, 100);
    let page = page.max(1);
    let sub = format!("p{page}:n{per}:t{}", term_id.unwrap_or(0));
    let cached: Option<CachedPostList> = app.cache.get_json(ns::POSTS, &sub).await;
    let list = match cached {
        Some(c) => CachedPostList {
            posts: c.posts,
            total: c.total,
        },
        None => {
            let fill = app.cache.begin_fill(ns::POSTS, &sub).await;
            let (mut list, total) = posts::list(
                &app.db,
                &posts::PostFilter {
                    public: true,
                    term_id,
                    page,
                    per_page: per,
                    ..Default::default()
                },
            )
            .await?;
            terms::attach(&app.db, &mut list).await?;
            let fresh = CachedPostList {
                posts: list.iter().map(CachedPost::from).collect(),
                total,
            };
            app.cache.finish_fill(fill, &fresh).await;
            fresh
        }
    };
    let pages = (list.total + per - 1) / per;
    Ok(PostList {
        posts: list.posts.into_iter().map(CachedPost::into_post).collect(),
        total: list.total,
        page,
        pages,
    })
}

/// A single published post by slug. Cache key: `post:v{n}:slug:{slug}`.
pub async fn get_public_post(app: &App, slug_str: &str) -> AppResult<Option<Post>> {
    let sub = format!("slug:{slug_str}");
    if let Some(hit) = app.cache.get_json::<CachedPost>(ns::POST, &sub).await {
        return Ok(Some(hit.into_post()));
    }
    let fill = app.cache.begin_fill(ns::POST, &sub).await;
    let Some(mut post) = posts::find_by_slug(&app.db, slug_str).await? else {
        return Ok(None);
    };
    let now = time::now();
    let visible = post.status == PostStatus::Published
        && post.published_at.map(|t| t <= now).unwrap_or(false);
    if !visible {
        return Ok(None);
    }
    post.terms = terms::list_for_post(&app.db, post.id).await?;
    app.cache.finish_fill(fill, &CachedPost::from(&post)).await;
    Ok(Some(post))
}

/// A single published post by id (public API reads). Cache key:
/// `post:v{n}:id:{id}`.
pub async fn get_public_post_by_id(app: &App, id: i64) -> AppResult<Option<Post>> {
    let sub = format!("id:{id}");
    if let Some(hit) = app.cache.get_json::<CachedPost>(ns::POST, &sub).await {
        return Ok(Some(hit.into_post()));
    }
    let fill = app.cache.begin_fill(ns::POST, &sub).await;
    let Some(mut post) = posts::find_by_id(&app.db, id).await? else {
        return Ok(None);
    };
    let now = time::now();
    let visible = post.status == PostStatus::Published
        && post.published_at.map(|t| t <= now).unwrap_or(false);
    if !visible {
        return Ok(None);
    }
    post.terms = terms::list_for_post(&app.db, post.id).await?;
    app.cache.finish_fill(fill, &CachedPost::from(&post)).await;
    Ok(Some(post))
}
