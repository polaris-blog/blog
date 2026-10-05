use crate::error::AppResult;
use crate::models::PostStatus;
use crate::state::App;

use super::posts::{PageInput, PostInput};

const SAMPLE_POST: &str = r#"# Welcome to Polaris

**Polaris** is a fast, lightweight blog engine written in Rust — a single
binary, with themes and plugins, backed by SQLite, MySQL or PostgreSQL.

## What you can do

- Write in Markdown — it is rendered safely (raw HTML is stripped, dangerous
  links are neutralized)
- Organize content with **categories** and **tags**
- Schedule posts, keep drafts, and moderate comments
- Switch themes and toggle plugins without restarting

## A code block

```rust
fn main() {
    println!("Hello, Polaris!");
}
```

## A table

| Feature | State |
|---------|-------|
| Multi-database | done |
| Themes | done |
| Plugins | done |

> Login to the admin panel at `/admin` to write your own posts.

Edit or delete this sample post to get started.
"#;

const SAMPLE_ABOUT: &str = r#"# About

This page is a sample **page** (as opposed to a blog post). Pages are great
for timeless content: an about page, links, projects, contact info.

You can edit this page in the admin panel under *Pages*.
"#;

/// Seed a fresh install with one post and one page, authored by the first user.
pub async fn seed_content(app: &App, author_id: i64) -> AppResult<()> {
    let (existing, _) = crate::repositories::posts::list(
        &app.db,
        &crate::repositories::posts::PostFilter {
            page: 1,
            per_page: 1,
            ..Default::default()
        },
    )
    .await?;
    if !existing.is_empty() {
        return Ok(());
    }

    let post = PostInput {
        title: "Hello, Polaris".into(),
        slug: Some("hello-polaris".into()),
        summary: "Your first Polaris post — a quick tour of what this engine can do.".into(),
        content_md: SAMPLE_POST.into(),
        status: PostStatus::Published,
        category: Some("General".into()),
        tags: vec!["welcome".into(), "polaris".into()],
        ..Default::default()
    };
    super::posts::create_post(app, author_id, post).await?;

    let page = PageInput {
        title: "About".into(),
        slug: Some("about".into()),
        summary: "About this site.".into(),
        content_md: SAMPLE_ABOUT.into(),
        status: PostStatus::Published,
        sort_order: 0,
    };
    super::posts::create_page(app, author_id, page).await?;
    tracing::info!("seeded sample content");
    Ok(())
}
