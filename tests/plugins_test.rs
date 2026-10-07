//! Plugin system: loading, hooks, filters, routes, lifecycle.

mod common;

use polaris::models::{PostStatus, Role};
use polaris::services::posts::{self, PostInput};

/// Write a test plugin ("greeter") into `<dir>/plugins/greeter/`.
fn write_greeter_plugin(dir: &std::path::Path) {
    let pdir = dir.join("plugins").join("greeter");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        r#"
name = "greeter"
version = "0.1.0"
author = "test"
description = "Integration test plugin"

[routes]
"/greet" = "greet"

[filters]
shout = "shout"
"#,
    )
    .unwrap();
    std::fs::write(
        pdir.join("main.rhai"),
        r#"
fn before_post_create(payload) {
    payload.summary = "intercepted: " + payload.summary;
    let tags = payload.tags;
    tags.push("plugged");
    payload.tags = tags;
    payload
}

fn markdown_before(md) {
    md + "\n\n*from plugin*"
}

fn shout(s) {
    s + "!"
}

fn greet(ctx) {
    #{ status: 201, body: "hello " + ctx.path, content_type: "text/plain; charset=utf-8" }
}

fn nav() {
    [#{ label: "Greet", url: "/greet" }]
}
"#,
    )
    .unwrap();
}

fn post_input(title: &str) -> PostInput {
    PostInput {
        title: title.into(),
        summary: "plain summary".into(),
        content_md: "# Body".into(),
        status: PostStatus::Published,
        ..Default::default()
    }
}

#[test]
fn manager_loads_and_dispatches() {
    let dir = tempfile::tempdir().unwrap();
    write_greeter_plugin(dir.path());
    let plugins_dir = dir.path().join("plugins");

    let mgr = polaris::plugins::PluginManager::new(
        &plugins_dir,
        &["greeter".to_string()],
        None,
        common::empty_config_manager(),
    );
    assert_eq!(mgr.enabled_names(), vec!["greeter".to_string()]);
    assert_eq!(mgr.generation(), 1);

    // Disk listing marks the plugin enabled.
    let list = mgr.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "greeter");
    assert!(list[0].2);

    // String hook chains through the plugin.
    assert_eq!(
        mgr.hook_str("markdown_before", "# T"),
        "# T\n\n*from plugin*"
    );
    // Registered filter callable.
    assert_eq!(mgr.call_filter("shout", "hi"), Some("hi!".to_string()));
    // Nav items contributed by the plugin.
    assert_eq!(
        mgr.nav_items(),
        vec![("Greet".to_string(), "/greet".to_string())]
    );

    // Reload bumps the generation.
    mgr.reload(&["greeter".to_string()]);
    assert_eq!(mgr.generation(), 2);

    // An empty enabled list disables everything.
    let off = polaris::plugins::PluginManager::new(
        &plugins_dir,
        &[],
        None,
        common::empty_config_manager(),
    );
    assert!(off.enabled_names().is_empty());
    assert_eq!(off.hook_str("markdown_before", "# T"), "# T");
    assert_eq!(off.call_filter("shout", "hi"), None);
}

/// Route keys may be registered as full URL paths or mount-relative;
/// `ctx.path` always receives the full request URL.
#[test]
fn route_keys_accept_full_and_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    let pdir = dir.path().join("plugins").join("paths");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        r#"
name = "paths"
version = "0.1.0"

[routes]
"/plugins/paths/full" = "h"
"/rel" = "h"

[admin_routes]
"/plugins/paths/admin" = "h"
"#,
    )
    .unwrap();
    std::fs::write(
        pdir.join("main.rhai"),
        r#"
fn h(ctx) {
    #{ status: 200, body: ctx.path, content_type: "text/plain" }
}
"#,
    )
    .unwrap();

    let mgr = polaris::plugins::PluginManager::new(
        &dir.path().join("plugins"),
        &["paths".to_string()],
        None,
        common::empty_config_manager(),
    );
    let q = rhai::Map::new();

    // Full-path registration.
    assert_eq!(
        mgr.route("/plugins/paths/full", &q).unwrap().body,
        "/plugins/paths/full"
    );
    // Mount-relative registration.
    assert_eq!(mgr.route("/plugins/rel", &q).unwrap().body, "/plugins/rel");

    // Admin route registered with a "/plugins/…" key but served under
    // "/admin/plugins/…" still resolves, with ctx.path = the real URL.
    let user = polaris::auth::AuthCtx {
        user_id: 1,
        username: "admin".into(),
        display_name: "Admin".into(),
        role: polaris::models::Role::Admin,
        csrf: "t".into(),
    };
    let r = mgr
        .admin_route("/admin/plugins/paths/admin", &q, &user)
        .unwrap();
    assert_eq!(r.body, "/admin/plugins/paths/admin");

    // Unknown path → no route.
    assert!(mgr.route("/plugins/nope", &q).is_none());
}

#[test]
fn broken_plugins_fail_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let pdir = dir.path().join("plugins").join("broken");
    std::fs::create_dir_all(&pdir).unwrap();
    // Valid metadata but the entry script is missing → load fails, no panic.
    std::fs::write(
        pdir.join("plugin.toml"),
        "name = \"broken\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mgr = polaris::plugins::PluginManager::new(
        &dir.path().join("plugins"),
        &["broken".to_string()],
        None,
        common::empty_config_manager(),
    );
    assert!(mgr.enabled_names().is_empty());

    // It still shows up in the disk listing (disabled).
    let list = mgr.list();
    assert_eq!(list.len(), 1);
    assert!(!list[0].2);
}

#[tokio::test]
async fn plugin_hooks_modify_content_pipeline() {
    let (app, dir) = common::init_app().await;
    write_greeter_plugin(dir.path());
    app.set_plugins_enabled(&["greeter".to_string()])
        .await
        .unwrap();
    assert_eq!(app.plugins.enabled_names(), vec!["greeter".to_string()]);

    let author = common::create_user(&app, "writer", "password123", Role::Author).await;
    let post = posts::create_post(&app, author.id, post_input("Hooked"))
        .await
        .unwrap();

    // before_post_create rewrote the summary and appended a tag.
    assert_eq!(post.summary, "intercepted: plain summary");
    assert!(post.tags().iter().any(|t| t.name == "plugged"));

    // markdown_before appends content to the render pipeline.
    let html = posts::render_content(
        &app,
        posts::KIND_POST,
        post.id,
        post.updated_at,
        &post.content_md,
    );
    assert!(html.contains("<em>from plugin</em>"));

    // Render cache: identical key returns the same Arc (no re-render).
    let again = posts::render_content(
        &app,
        posts::KIND_POST,
        post.id,
        post.updated_at,
        &post.content_md,
    );
    assert!(std::sync::Arc::ptr_eq(&html, &again));
}

#[tokio::test]
async fn plugin_route_served_over_http() {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::StatusCode;
    use axum::response::Response;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (app, dir) = common::init_app().await;
    write_greeter_plugin(dir.path());
    app.set_plugins_enabled(&["greeter".to_string()])
        .await
        .unwrap();
    let router = polaris::http::router(app.clone());

    // Registered route returns the plugin's status/body/content-type.
    let resp: Response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins/greet")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "text/plain; charset=utf-8"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"hello /plugins/greet");

    // Unknown plugin path → 404.
    let resp: Response = router
        .oneshot(
            Request::builder()
                .uri("/plugins/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Regression: a runaway script (`loop {}`, unbounded recursion) must be
/// stopped by the engine's resource limits instead of hanging the worker
/// thread forever. Hooks run synchronously inside request paths, so an
/// unbounded plugin used to freeze the whole server. The script now fails
/// with an error (logged, hook skipped) and the pipeline continues.
#[test]
fn runaway_plugin_scripts_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let pdir = dir.path().join("plugins").join("runaway");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        "name = \"runaway\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        pdir.join("main.rhai"),
        r#"
fn markdown_before(md) {
    // Infinite loop: without operation limits this never returns.
    loop {
        let x = 1;
        x = x + 1;
    }
    md
}

fn shout(s) {
    // Unbounded recursion: without call-level limits this exhausts the stack.
    shout(s)
}
"#,
    )
    .unwrap();

    let mgr = polaris::plugins::PluginManager::new(
        &dir.path().join("plugins"),
        &["runaway".to_string()],
        None,
        common::empty_config_manager(),
    );
    // The plugin loads (compilation is fine) — only execution is unbounded.
    assert_eq!(mgr.enabled_names(), vec!["runaway".to_string()]);

    // Both calls must return (with the hook skipped), not hang.
    assert_eq!(mgr.hook_str("markdown_before", "# T"), "# T");
    assert_eq!(mgr.call_filter("shout", "hi"), None);
}

/// The generic HTTP/JSON host API follows the manifest permissions: a plugin
/// declaring `network.fetch` gets SSRF-guarded `http_*` functions, while for
/// others the functions are absent (the call throws and a script `catch`
/// handles it). `json_parse` / `json_stringify` are always available.
#[test]
fn network_api_follows_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join("plugins");

    for (name, permissions) in [
        ("withperm", "permissions = [\"network.fetch\"]\n"),
        ("noperm", ""),
    ] {
        let pdir = plugins_dir.join(name);
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(
            pdir.join("plugin.toml"),
            format!("name = \"{name}\"\nversion = \"0.1.0\"\n{permissions}"),
        )
        .unwrap();
        std::fs::write(
            pdir.join("main.rhai"),
            r#"
fn json_roundtrip(s) {
    json_parse(json_stringify(s))
}

fn probe(url) {
    // With the permission: an SSRF-guarded result map (status 0 for
    // non-public hosts, no I/O performed). Without it: the call throws
    // and the catch arm wins.
    let status = -1;
    try {
        let r = http_get(url);
        status = r.status;
    } catch (e) {
        status = -1;
    }
    status
}
"#,
        )
        .unwrap();
    }

    let mgr = polaris::plugins::PluginManager::new(
        &plugins_dir,
        &["withperm".to_string(), "noperm".to_string()],
        None,
        common::empty_config_manager(),
    );
    assert_eq!(
        mgr.enabled_names(),
        vec!["noperm".to_string(), "withperm".to_string()]
    );

    // JSON helpers: always registered.
    assert_eq!(
        mgr.call_plugin_str("withperm", "json_roundtrip", "hello"),
        Some("hello".to_string())
    );
    assert_eq!(
        mgr.call_plugin_str("noperm", "json_roundtrip", "hello"),
        Some("hello".to_string())
    );

    // http_get exists with the permission — loopback is blocked host-side
    // (status 0, no I/O).
    assert_eq!(
        mgr.call_plugin_str("withperm", "probe", "http://127.0.0.1:9/x"),
        Some("0".to_string())
    );
    // …and is absent without it (runtime error caught by the script).
    assert_eq!(
        mgr.call_plugin_str("noperm", "probe", "http://127.0.0.1:9/x"),
        Some("-1".to_string())
    );
}
