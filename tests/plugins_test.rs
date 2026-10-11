//! Plugin system: loading, hooks, filters, routes, lifecycle.

mod common;

use std::path::Path;

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

fn write_gate_plugin(dir: &Path, with_permission: bool) {
    let root = dir.join("plugins").join("gate");
    std::fs::create_dir_all(&root).unwrap();
    let perms = if with_permission {
        "permissions = [\"request.guard\"]\n"
    } else {
        ""
    };
    std::fs::write(
        root.join("plugin.toml"),
        format!(
            "id = \"gate\"\nname = \"gate\"\nversion = \"1.0.0\"\nauthor = \"t\"\ndescription = \"t\"\nlicense = \"MIT\"\n{perms}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("main.rhai"),
        // Redirect every guarded request to the gate page.
        "fn request_guard(req) { #{redirect: \"/plugins/gate/verify\"} }",
    )
    .unwrap();
}

fn guard_map(path: &str, cookies: &[(&str, &str)]) -> rhai::Map {
    let mut m = rhai::Map::new();
    m.insert("method".into(), rhai::Dynamic::from("GET"));
    m.insert("path".into(), rhai::Dynamic::from(path.to_string()));
    m.insert("query".into(), rhai::Dynamic::from(""));
    let mut c = rhai::Map::new();
    for (k, v) in cookies {
        c.insert((*k).into(), rhai::Dynamic::from((*v).to_string()));
    }
    m.insert("cookies".into(), rhai::Dynamic::from(c));
    m.insert("ip".into(), rhai::Dynamic::from("203.0.113.9"));
    m.insert("user_agent".into(), rhai::Dynamic::from("test"));
    m.insert("has_session".into(), rhai::Dynamic::from(false));
    m
}

#[test]
fn request_guard_redirects_when_permission_declared() {
    let dir = tempfile::tempdir().unwrap();
    write_gate_plugin(dir.path(), true);
    let plugins_dir = dir.path().join("plugins");

    let mgr = polaris::plugins::PluginManager::new(
        &plugins_dir,
        &["gate".to_string()],
        None,
        common::empty_config_manager(),
    );

    // Content pages are redirected to the gate.
    match mgr.request_guard(guard_map("/posts/x", &[])) {
        polaris::plugins::GuardOutcome::Redirect(loc) => {
            assert_eq!(loc, "/plugins/gate/verify");
        }
        other => panic!("expected redirect, got {other:?}"),
    }

    // Machine routes are exempt at the HTTP layer; the guard itself still
    // answers when asked directly (the middleware never calls it for those).
}

#[test]
fn request_guard_without_permission_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    // Same plugin script, but the manifest does NOT declare request.guard.
    write_gate_plugin(dir.path(), false);
    let plugins_dir = dir.path().join("plugins");

    let mgr = polaris::plugins::PluginManager::new(
        &plugins_dir,
        &["gate".to_string()],
        None,
        common::empty_config_manager(),
    );

    assert!(matches!(
        mgr.request_guard(guard_map("/posts/x", &[])),
        polaris::plugins::GuardOutcome::Allow
    ));
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

// ---------------------------------------------------------------------------
// Turnstile gate plugin (lives outside the repository — gitignored like the
// gate plugin — so these tests are skipped when the files are absent, e.g.
// on CI or a fresh clone)
// ---------------------------------------------------------------------------

/// True when the turnstile plugin sources are present in the working tree.
fn turnstile_available() -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins/turnstile/main.rhai")
        .is_file()
}

/// Copy the turnstile plugin into the test plugins dir, with the given
/// file-level config defaults.
fn write_turnstile_plugin(dir: &Path, config_toml: &str) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/turnstile");
    let root = dir.join("plugins").join("turnstile");
    std::fs::create_dir_all(&root).unwrap();
    for file in ["plugin.toml", "main.rhai", "config.schema.toml"] {
        std::fs::write(
            root.join(file),
            std::fs::read_to_string(src.join(file)).unwrap(),
        )
        .unwrap();
    }
    if !config_toml.is_empty() {
        std::fs::write(root.join("config.toml"), config_toml).unwrap();
    }
}

/// PluginManager for the bundled turnstile plugin with its config namespace
/// (schema defaults + config.toml file defaults) loaded.
async fn turnstile_manager(dir: &Path, config_toml: &str) -> polaris::plugins::PluginManager {
    write_turnstile_plugin(dir, config_toml);
    let plugins_dir = dir.join("plugins");
    // A ConfigManager built on the current test runtime (the shared helper
    // spins its own runtime and cannot be used from #[tokio::test]).
    let cfg = common::test_config(dir);
    let db = polaris::db::Db::connect(&cfg.database)
        .await
        .expect("db connect");
    let configs = std::sync::Arc::new(polaris::config_store::ConfigManager::new(
        db,
        "test-secret".into(),
    ));
    configs
        .load_plugin(&plugins_dir, "turnstile")
        .await
        .expect("load turnstile config schema");
    polaris::plugins::PluginManager::new(
        &plugins_dir,
        &["turnstile".to_string()],
        Some(std::sync::Arc::new(
            polaris::cache::memory::MemoryCache::new(64),
        )),
        configs,
    )
}

/// Signing key the engine derives for this plugin when the instance secret
/// is empty (tests construct PluginManager without `with_secret`).
fn turnstile_signer() -> String {
    polaris::plugins::crypto::hmac_sha256_hex(b"", "plugin-signing:turnstile")
}

fn signed(payload: &str) -> String {
    let key = turnstile_signer();
    format!(
        "{payload}.{}",
        polaris::plugins::crypto::hmac_sha256_hex(key.as_bytes(), payload)
    )
}

/// A valid clearance payload expiring in an hour; `ip` is already normalized
/// (empty = not IP-bound).
fn clearance(exp_in_secs: i64, ip: &str) -> String {
    signed(&format!(
        "v1|{}|{}",
        polaris::utils::time::now() + exp_in_secs,
        ip
    ))
}

fn expired_clearance(ip: &str) -> String {
    signed(&format!("v1|{}|{}", polaris::utils::time::now() - 10, ip))
}

/// Enabled gate with a key pair configured (the gate refuses to run without
/// both keys — see `gate_ready` in the plugin script).
const TEST_GATE_CONFIG: &str = "enabled = true\nsite_key = \"0x4AAAAAAA_test_site\"\nsecret_key = \"0x4AAAAAAA_test_secret\"\n";

#[tokio::test]
async fn turnstile_gate_is_off_until_configured() {
    if !turnstile_available() {
        eprintln!("skipping: plugins/turnstile is not present locally");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Schema defaults only → enabled = false.
    let mgr = turnstile_manager(dir.path(), "").await;

    // The guard lets everything through while disabled.
    assert!(matches!(
        mgr.request_guard(guard_map("/posts/x", &[])),
        polaris::plugins::GuardOutcome::Allow
    ));

    // The verify endpoint reports the disabled state as 404.
    let r = mgr
        .route("/plugins/turnstile/verify", &rhai::Map::new())
        .unwrap();
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn turnstile_gate_challenges_and_clears() {
    if !turnstile_available() {
        eprintln!("skipping: plugins/turnstile is not present locally");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mgr = turnstile_manager(dir.path(), TEST_GATE_CONFIG).await;

    // Without a clearance: a 403 challenge page whitelisting the widget.
    match mgr.request_guard(guard_map("/posts/hello", &[])) {
        polaris::plugins::GuardOutcome::Respond {
            status,
            body,
            headers,
        } => {
            assert_eq!(status, 403);
            assert!(body.contains("challenges.cloudflare.com/turnstile"));
            assert!(body.contains("/plugins/turnstile/verify"));
            assert!(headers.iter().any(|(n, v)| n == "Content-Security-Policy"
                && v.contains("https://challenges.cloudflare.com")));
        }
        other => panic!("expected challenge page, got {other:?}"),
    }

    // A valid, unbound clearance lets the visitor through.
    let value = clearance(3600, "");
    let ok = vec![("ts_cleared", value.as_str())];
    assert!(matches!(
        mgr.request_guard(guard_map("/posts/hello", &ok)),
        polaris::plugins::GuardOutcome::Allow
    ));

    // A tampered signature is challenged again.
    let mut tampered = clearance(3600, "");
    tampered.push_str("beef");
    let bad = vec![("ts_cleared", tampered.as_str())];
    assert!(matches!(
        mgr.request_guard(guard_map("/posts/hello", &bad)),
        polaris::plugins::GuardOutcome::Respond { .. }
    ));

    // An expired clearance is challenged again.
    let value = expired_clearance("");
    let old = vec![("ts_cleared", value.as_str())];
    assert!(matches!(
        mgr.request_guard(guard_map("/posts/hello", &old)),
        polaris::plugins::GuardOutcome::Respond { .. }
    ));

    // Signed-in users are exempt by default.
    let mut m = guard_map("/posts/hello", &[]);
    m.insert("has_session".into(), rhai::Dynamic::from(true));
    assert!(matches!(
        mgr.request_guard(m),
        polaris::plugins::GuardOutcome::Allow
    ));

    // Exempt path prefixes are never challenged.
    let dir2 = tempfile::tempdir().unwrap();
    let mgr2 = turnstile_manager(
        dir2.path(),
        &format!("{TEST_GATE_CONFIG}exempt_paths = \"/api,/health\"\n"),
    )
    .await;
    assert!(matches!(
        mgr2.request_guard(guard_map("/api/posts", &[])),
        polaris::plugins::GuardOutcome::Allow
    ));
    assert!(matches!(
        mgr2.request_guard(guard_map("/api/posts/3", &[])),
        polaris::plugins::GuardOutcome::Allow
    ));
    assert!(matches!(
        mgr2.request_guard(guard_map("/posts/3", &[])),
        polaris::plugins::GuardOutcome::Respond { .. }
    ));
}

#[tokio::test]
async fn turnstile_gate_ip_binding() {
    if !turnstile_available() {
        eprintln!("skipping: plugins/turnstile is not present locally");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mgr = turnstile_manager(
        dir.path(),
        "enabled = true\nbind_ip = true\nsite_key = \"0x4AAAAAAA_test_site\"\nsecret_key = \"0x4AAAAAAA_test_secret\"\n",
    )
    .await;

    // guard_map hardcodes ip 203.0.113.9 — a clearance bound to another
    // address is rejected…
    let value = clearance(3600, "1-2-3-4");
    let other = vec![("ts_cleared", value.as_str())];
    assert!(matches!(
        mgr.request_guard(guard_map("/posts/hello", &other)),
        polaris::plugins::GuardOutcome::Respond { .. }
    ));

    // …while one bound to the requesting IP passes.
    let value = clearance(3600, "203-0-113-9");
    let own = vec![("ts_cleared", value.as_str())];
    assert!(matches!(
        mgr.request_guard(guard_map("/posts/hello", &own)),
        polaris::plugins::GuardOutcome::Allow
    ));
}

#[tokio::test]
async fn turnstile_verify_rejects_bad_input() {
    if !turnstile_available() {
        eprintln!("skipping: plugins/turnstile is not present locally");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mgr = turnstile_manager(dir.path(), TEST_GATE_CONFIG).await;

    // No token, no ticket → 400 JSON.
    let r = mgr
        .route("/plugins/turnstile/verify", &rhai::Map::new())
        .unwrap();
    assert_eq!(r.status, 400);
    assert!(r.content_type.starts_with("application/json"));
    assert!(r.body.contains("bad_request"));

    // A signed but expired ticket is rejected the same way (the clearance
    // payload signature path is covered by the guard tests above).
    let mut ctx = rhai::Map::new();
    ctx.insert("token".into(), rhai::Dynamic::from("dummy-token"));
    ctx.insert(
        "ticket".into(),
        rhai::Dynamic::from(signed(&format!(
            "t|{}|203-0-113-9",
            polaris::utils::time::now() - 10
        ))),
    );
    let r = mgr.route("/plugins/turnstile/verify", &ctx).unwrap();
    assert_eq!(r.status, 400);
}

#[tokio::test]
async fn turnstile_stats_page_renders() {
    if !turnstile_available() {
        eprintln!("skipping: plugins/turnstile is not present locally");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mgr = turnstile_manager(dir.path(), TEST_GATE_CONFIG).await;
    let user = polaris::auth::AuthCtx {
        user_id: 1,
        username: "admin".into(),
        display_name: "Admin".into(),
        role: polaris::models::Role::Admin,
        csrf: "t".into(),
    };
    let r = mgr
        .admin_route("/admin/plugins/turnstile/stats", &rhai::Map::new(), &user)
        .unwrap();
    assert_eq!(r.status, 200);
    assert!(r.content_type.starts_with("text/html"));
    assert!(r.body.contains("Turnstile"));
}

/// End-to-end regression: the guard's page-scoped CSP must reach the client.
/// The site-wide security headers middleware used to overwrite it
/// unconditionally, which blocked the Turnstile widget script entirely.
#[tokio::test]
async fn turnstile_challenge_page_keeps_its_csp() {
    if !turnstile_available() {
        eprintln!("skipping: plugins/turnstile is not present locally");
        return;
    }
    use axum::body::Body;
    use axum::extract::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (app, dir) = common::init_app().await;
    write_turnstile_plugin(dir.path(), TEST_GATE_CONFIG);
    app.set_plugins_enabled(&["turnstile".to_string()])
        .await
        .unwrap();
    let router = polaris::http::router(app.clone());

    let resp: axum::response::Response = router
        .oneshot(
            Request::builder()
                .uri("/posts/anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);

    // The page must actually render as HTML — a text/plain default would
    // suppress every script on the page under `nosniff`.
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        ct.starts_with("text/html"),
        "guard challenge page must be served as HTML, got: {ct}"
    );

    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        csp.contains("https://challenges.cloudflare.com"),
        "guard CSP must survive the security middleware, got: {csp}"
    );

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&bytes).contains("challenges.cloudflare.com/turnstile"));
}
