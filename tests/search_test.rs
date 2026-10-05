//! Search system: SQLite FTS5 provider, service layer, index maintenance,
//! caching, suggestions, analytics, rebuild and the HTTP endpoints.

mod common;

use polaris::models::{PostStatus, Role};
use polaris::search::{SearchKind, SearchQuery, SearchSort};
use polaris::services::posts::{self, PageInput, PostInput};

fn post_input(title: &str, body: &str) -> PostInput {
    PostInput {
        title: title.into(),
        summary: String::new(),
        content_md: body.into(),
        status: PostStatus::Published,
        ..Default::default()
    }
}

async fn setup() -> (polaris::state::App, tempfile::TempDir) {
    let (app, dir) = common::init_app().await;
    let author = common::create_user(&app, "writer", "password123", Role::Author).await;
    // Note: the first user seeds a "Hello, Polaris" post mentioning Rust and
    // an "About" page — tests must not assume an empty index.
    posts::create_post(
        &app,
        author.id,
        post_input(
            "Rust Web Performance",
            "Rust is a **fast** systems language for web servers",
        ),
    )
    .await
    .unwrap();
    posts::create_post(
        &app,
        author.id,
        PostInput {
            title: "Docker Basics".into(),
            summary: "Containers explained".into(),
            content_md: "Docker packaging for linux deployments".into(),
            status: PostStatus::Published,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    posts::create_post(
        &app,
        author.id,
        PostInput {
            title: "Draft Secret".into(),
            content_md: "hidden draft content rust".into(),
            status: PostStatus::Draft,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (app, dir)
}

fn query(q: &str) -> SearchQuery {
    SearchQuery {
        query: q.into(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Core search behavior (SQLite FTS5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn finds_published_posts_and_excludes_drafts() {
    let (app, _dir) = setup().await;
    let resp = app.search.search(&app, query("rust")).await.unwrap();
    assert!(resp.total >= 1, "should find the rust post");
    assert!(
        resp.results.iter().all(|r| r.title != "Draft Secret"),
        "drafts must never appear in search results"
    );
    let hit = resp
        .results
        .iter()
        .find(|r| r.title.contains("Rust"))
        .unwrap();
    assert_eq!(hit.url, format!("/posts/{}", hit.slug));
}

#[tokio::test]
async fn content_matches_are_found() {
    let (app, _dir) = setup().await;
    // "packaging" only appears in Docker post content, not the title.
    let resp = app.search.search(&app, query("packaging")).await.unwrap();
    assert_eq!(resp.total, 1);
    assert!(resp.results[0].title.contains("Docker"));
}

#[tokio::test]
async fn title_outranks_content_match() {
    let (app, _dir) = setup().await;
    // "rust" is in the "Rust Web Performance" title and in the seeded post's
    // content only — bm25 field weights must rank the title match first.
    let resp = app.search.search(&app, query("rust")).await.unwrap();
    assert!(
        resp.results[0].title.contains("Rust Web"),
        "title match should outrank content matches, got {:?}",
        resp.results
            .iter()
            .map(|r| r.title.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn highlight_is_safe_html() {
    let (app, _dir) = setup().await;
    let resp = app.search.search(&app, query("rust")).await.unwrap();
    let r = resp
        .results
        .iter()
        .find(|r| r.title.contains("Rust"))
        .unwrap();
    let hl = r
        .highlight
        .as_deref()
        .expect("highlight enabled by default");
    assert!(hl.contains("<mark>"), "match must be marked: {hl}");
    assert!(!hl.contains("<script"), "raw HTML must never pass through");
}

#[tokio::test]
async fn pagination_and_sorting() {
    let (app, _dir) = setup().await;
    // "rust" matches both the seeded post (content) and "Rust Web Performance"
    // (title) — two results to paginate through.
    let q = SearchQuery {
        query: "rust".into(),
        per_page: 1,
        page: 1,
        ..Default::default()
    };
    let resp = app.search.search(&app, q).await.unwrap();
    assert!(resp.total >= 2);
    let page2 = SearchQuery {
        query: "rust".into(),
        per_page: 1,
        page: 2,
        ..Default::default()
    };
    let resp2 = app.search.search(&app, page2).await.unwrap();
    assert_eq!(resp2.results.len(), 1);
    assert_ne!(resp.results[0].id, resp2.results[0].id);

    // All sorts are accepted; relevance is the default.
    for s in ["relevance", "date", "updated", "title"] {
        let q = SearchQuery {
            query: "rust".into(),
            sort: SearchSort::parse(s).unwrap(),
            ..Default::default()
        };
        let resp = app.search.search(&app, q).await.unwrap();
        assert_eq!(resp.sort, s);
    }
}

#[tokio::test]
async fn per_page_is_capped_by_config() {
    let (app, _dir) = setup().await;
    let q = SearchQuery {
        query: "rust".into(),
        per_page: 999_999,
        ..Default::default()
    };
    let resp = app.search.search(&app, q).await.unwrap();
    assert!(resp.per_page <= app.search.max_per_page());
    assert!(resp.results.len() as u32 <= resp.per_page);
}

#[tokio::test]
async fn invalid_queries_return_empty_not_error() {
    let (app, _dir) = setup().await;
    for bad in ["", "  ", "x", "\" OR 1=1; --", "!!! ??? ***"] {
        let resp = app.search.search(&app, query(bad)).await.unwrap();
        assert_eq!(resp.total, 0, "query {bad:?} must not error");
    }
}

#[tokio::test]
async fn kind_filter_returns_only_pages_or_posts() {
    let (app, _dir) = setup().await;
    let author = polaris::repositories::users::find_by_username(&app.db, "writer")
        .await
        .unwrap()
        .unwrap();
    posts::create_page(
        &app,
        author.id,
        PageInput {
            title: "About Rust".into(),
            summary: String::new(),
            content_md: "This site covers rust topics".into(),
            status: PostStatus::Published,
            sort_order: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let q = SearchQuery {
        query: "rust".into(),
        kind: Some(SearchKind::Page),
        ..Default::default()
    };
    let resp = app.search.search(&app, q).await.unwrap();
    assert!(resp.total >= 1);
    assert!(resp.results.iter().all(|r| r.kind == "page"));
    assert!(resp.results.iter().all(|r| r.url.starts_with('/')));
}

// ---------------------------------------------------------------------------
// Index maintenance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_and_delete_are_reflected() {
    let (app, _dir) = setup().await;
    let author = polaris::repositories::users::find_by_username(&app.db, "writer")
        .await
        .unwrap()
        .unwrap();
    let p = posts::create_post(
        &app,
        author.id,
        post_input("Kubernetes Intro", "k8s orchestration"),
    )
    .await
    .unwrap();
    assert_eq!(
        app.search
            .search(&app, query("kubernetes"))
            .await
            .unwrap()
            .total,
        1
    );

    // Update: title change is searchable.
    posts::update_post(
        &app,
        p.id,
        post_input("Nomad Intro", "hashicorp orchestration"),
    )
    .await
    .unwrap();
    app.cache.invalidate(&[polaris::cache::ns::SEARCH]).await;
    assert_eq!(
        app.search
            .search(&app, query("kubernetes"))
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        app.search.search(&app, query("nomad")).await.unwrap().total,
        1
    );

    // Delete: gone from the index.
    posts::delete_post(&app, p.id).await.unwrap();
    app.cache.invalidate(&[polaris::cache::ns::SEARCH]).await;
    assert_eq!(
        app.search.search(&app, query("nomad")).await.unwrap().total,
        0
    );
}

#[tokio::test]
async fn rebuild_reindexes_everything_and_verifies() {
    let (app, _dir) = setup().await;
    let stats = app.search.rebuild(&app, |_, _| {}).await.unwrap();
    assert!(stats.verified, "rebuild must verify post/page counts");
    assert!(stats.posts >= 1);
    let status = app.search.status(&app).await.unwrap();
    assert!(status.healthy);
    assert_eq!(status.indexed_posts, stats.posts);
    assert!(status.last_rebuild.is_some(), "rebuild stamps last_rebuild");
    // And searching still works.
    assert!(app.search.search(&app, query("rust")).await.unwrap().total >= 1);
}

// ---------------------------------------------------------------------------
// Cache integration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn results_are_cached_and_invalidated_on_content_change() {
    let (app, _dir) = setup().await;
    let author = polaris::repositories::users::find_by_username(&app.db, "writer")
        .await
        .unwrap()
        .unwrap();

    let first = app.search.search(&app, query("cassandra")).await.unwrap();
    assert_eq!(first.total, 0);
    // Same query again — served from cache (still empty).
    assert_eq!(
        app.search
            .search(&app, query("cassandra"))
            .await
            .unwrap()
            .total,
        0
    );

    // New content must be visible: creation bumps the content namespace.
    posts::create_post(
        &app,
        author.id,
        post_input("Cassandra Guide", "wide column store"),
    )
    .await
    .unwrap();
    let after = app.search.search(&app, query("cassandra")).await.unwrap();
    assert_eq!(
        after.total, 1,
        "cache must be invalidated by content changes"
    );
}

// ---------------------------------------------------------------------------
// Suggestions & analytics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suggest_finds_titles_and_tags() {
    let (app, _dir) = setup().await;
    let author = polaris::repositories::users::find_by_username(&app.db, "writer")
        .await
        .unwrap()
        .unwrap();
    let mut input = post_input("Zig Systems Language", "zig guide");
    input.tags = vec!["ziglang".into()];
    posts::create_post(&app, author.id, input).await.unwrap();

    let out = app.search.suggest(&app, "zig").await.unwrap();
    assert!(
        !out.is_empty(),
        "titles/tags matching the prefix should be suggested"
    );
    assert!(out.iter().any(|s| s.to_lowercase().contains("zig")));
    // Short prefix is rejected.
    assert!(app.search.suggest(&app, "z").await.unwrap().is_empty());
}

#[tokio::test]
async fn analytics_records_queries_privately() {
    // Analytics is off by default — enable it via config before init.
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::test_config(dir.path());
    cfg.search.analytics.enabled = true;
    let app = polaris::state::AppState::init(cfg).await.unwrap();
    let author = common::create_user(&app, "writer", "password123", Role::Author).await;
    posts::create_post(
        &app,
        author.id,
        post_input("Rust Web Performance", "rust content"),
    )
    .await
    .unwrap();

    let resp = app.search.search(&app, query("rust")).await.unwrap();
    assert!(resp.total > 0);
    let popular = app.search.popular_searches(&app, 10).await;
    assert!(popular.iter().any(|s| s.query == "rust"));

    // A no-result search is tracked for content ideas.
    app.search.search(&app, query("quantum")).await.unwrap();
    let none = app.search.no_result_searches(&app, 10).await;
    assert!(
        none.iter()
            .any(|s| s.query == "quantum" && s.no_results >= 1)
    );
}

// ---------------------------------------------------------------------------
// HTTP endpoints
// ---------------------------------------------------------------------------

use axum::body::Body;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn make_app_router(app: polaris::state::App) -> axum::Router {
    polaris::http::router(app)
}

async fn get_json(router: &axum::Router, uri: &str) -> (u16, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
    };
    (status, v)
}

#[tokio::test]
async fn api_search_returns_json_results() {
    let (app, _dir) = setup().await;
    let router = make_app_router(app.clone());
    let (status, v) = get_json(&router, "/api/search?q=rust").await;
    assert_eq!(status, 200);
    assert_eq!(v["query"], "rust");
    assert!(v["total"].as_i64().unwrap() >= 1);
    let results = v["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .all(|r| r["url"].is_string() && r["title"].is_string())
    );

    // Injection attempts are neutralized, never 500.
    let (status, _) = get_json(&router, "/api/search?q=%22%20OR%201%3D1%3B").await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn api_search_page_renders_html() {
    let (app, _dir) = setup().await;
    let router = make_app_router(app.clone());
    let resp = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/search?q=rust")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/html")
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("results for"));
    assert!(html.contains("Rust"), "matched post title should appear");
}

#[tokio::test]
async fn api_suggest_returns_array() {
    let (app, _dir) = setup().await;
    let router = make_app_router(app.clone());
    let (status, v) = get_json(&router, "/api/search/suggest?q=ru").await;
    assert_eq!(status, 200);
    assert!(v.is_array());
}

#[tokio::test]
async fn api_search_status_requires_auth() {
    let (app, _dir) = setup().await;
    let router = make_app_router(app.clone());
    let (status, _) = get_json(&router, "/api/search/status").await;
    assert!(
        status == 401 || status == 403,
        "status endpoint must be protected"
    );
}
