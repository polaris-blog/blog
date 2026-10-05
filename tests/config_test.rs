//! Theme & plugin configuration system: schema-generated settings pages,
//! validation, persistence, secret handling, cache invalidation and the Rhai
//! `config_get*` API (including live reads after an admin save).

mod common;

use std::path::Path;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use polaris::db::Bind;
use tower::ServiceExt;

use polaris::models::Role;
use polaris::state::{App, AppState};

const DEMO_PLUGIN_TOML: &str = r#"
name = "demo"
version = "1.0.0"
author = "Polaris"
description = "Scratch plugin for config integration tests"

[routes]
"/plugins/demo/cfg" = "route_cfg"
"#;

const DEMO_PLUGIN_RHAI: &str = r#"
fn init(config) {
    log("info", "demo plugin initialized");
}

fn on_config_changed(event) {
    cache_set("chg_" + event.key,
        event.old_value + "|" + event.new_value,
        60);
}

fn route_cfg(ctx) {
    #{
        status: 200,
        content_type: "text/plain; charset=utf-8",
        body: "tagline=" + config_get("tagline")
            + ";sig=" + config_get_bool("append_signature")
            + ";key=" + config_get("api_key")
            + ";tagline_chg=" + cache_get("chg_tagline"),
    }
}
"#;

const DEMO_PLUGIN_SCHEMA: &str = r#"
[groups.general]
label = "General"

[tagline]
type = "string"
label = "Tagline"
default = "Demo plugin"
group = "general"

[append_signature]
type = "boolean"
label = "Append signature"
default = true
group = "general"

[per_page]
type = "integer"
label = "Items per page"
default = 10
min = 1
max = 50
group = "general"

[show_details]
type = "boolean"
label = "Show details"
default = true
group = "general"

[tracking_id]
type = "string"
label = "Tracking ID"
show_if = "show_details == true"
group = "general"

[api_key]
type = "api_key"
label = "API key"
group = "general"
"#;

#[tokio::test]
async fn instance_keys_are_validated_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::test_config(dir.path());
    cfg.security.secret = "CHANGE_ME".into();
    assert!(AppState::init(cfg.clone()).await.is_err());
    assert!(!dir.path().join("test.db").exists());
    cfg.security.secret.clear();
    let app = AppState::init(cfg.clone()).await.unwrap();
    let secret = app.settings.get("security.secret").unwrap();
    assert_eq!(secret.len(), 64);
    let restarted = AppState::init(cfg).await.unwrap();
    assert_eq!(
        restarted.settings.get("security.secret").as_deref(),
        Some(secret.as_str())
    );
}

#[tokio::test]
async fn restart_with_wrong_key_fails_explicitly() {
    let (app, _, dir) = init_config_app().await;
    app.save_plugin_config(
        "demo",
        &std::collections::HashMap::from([("api_key".into(), "private-api-key".into())]),
        polaris::config_schema::Permission::Admin,
    )
    .await
    .unwrap();
    let mut cfg = common::test_config(dir.path());
    cfg.security.secret = "incorrect-key".into();
    let error = AppState::init(cfg)
        .await
        .err()
        .expect("wrong key must fail");
    assert!(error.to_string().contains("cannot decrypt"));
    assert!(!error.to_string().contains("private-api-key"));
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let ty = entry.file_type().unwrap();
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
        }
    }
}

/// App with the real shipped `themes/default` (templates + config schema)
/// copied into a temp dir, plus a scratch `demo` plugin that reports its own
/// configuration through a public route.
async fn init_config_app() -> (App, axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    copy_dir_all(
        Path::new("themes/default"),
        &root.join("themes").join("default"),
    );

    let plugin = root.join("plugins").join("demo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.toml"), DEMO_PLUGIN_TOML).unwrap();
    std::fs::write(plugin.join("main.rhai"), DEMO_PLUGIN_RHAI).unwrap();
    std::fs::write(plugin.join("config.schema.toml"), DEMO_PLUGIN_SCHEMA).unwrap();

    let mut cfg = common::test_config(root);
    cfg.theme.active = "default".into();
    let app = AppState::init(cfg).await.expect("app init");
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    app.set_plugins_enabled(&["demo".to_string()])
        .await
        .unwrap();
    let router = polaris::http::router(app.clone());
    (app, router, dir)
}

async fn send(router: &axum::Router, req: Request) -> Response {
    router.clone().oneshot(req).await.expect("oneshot")
}

async fn body(resp: Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn set_cookie(resp: &Response, name: &str) -> Option<String> {
    for v in resp.headers().get_all(header::SET_COOKIE) {
        let s = v.to_str().ok()?;
        if let Some(rest) = s.strip_prefix(name)
            && let Some(val) = rest.strip_prefix('=')
        {
            return Some(val.split(';').next().unwrap_or("").to_string());
        }
    }
    None
}

fn local_addr(port: u16) -> ConnectInfo<std::net::SocketAddr> {
    ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
}

/// Log in as admin; returns the session cookie value.
async fn login(router: &axum::Router, port: u16) -> String {
    let resp = send(
        router,
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let csrf = set_cookie(&resp, "polaris_csrf").expect("csrf cookie");
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .extension(local_addr(port))
        .body(Body::from(format!(
            "username=admin&password=password123&csrf={csrf}"
        )))
        .unwrap();
    let resp = send(router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    set_cookie(&resp, "polaris_session").expect("session cookie")
}

async fn get_with(router: &axum::Router, uri: &str, session: &str) -> Response {
    send(
        router,
        Request::builder()
            .uri(uri)
            .header(header::COOKIE, format!("polaris_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post_form(
    router: &axum::Router,
    uri: &str,
    form: &str,
    session: &str,
    port: u16,
) -> Response {
    send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, format!("polaris_session={session}"))
            .extension(local_addr(port))
            .body(Body::from(form.to_string()))
            .unwrap(),
    )
    .await
}

// ---------------------------------------------------------------------------
// Theme settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn theme_settings_page_generated_from_schema() {
    let (app, router, _dir) = init_config_app().await;
    let session = login(&router, 41001).await;
    let csrf = app.sessions.get(&session).unwrap().csrf;

    // The themes list links to the settings page.
    let resp = get_with(&router, "/admin/themes", &session).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(html.contains("/admin/themes/default/settings"));

    // The settings page renders fields grouped by the schema.
    let resp = get_with(&router, "/admin/themes/default/settings", &session).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(html.contains("Appearance"));
    assert!(html.contains("Accent color"));
    assert!(html.contains("#0071e3"), "schema default is pre-filled");
    assert!(html.contains("Social links"));
    assert!(html.contains("name=\"accent_color\""));

    // Saving applies immediately: the public page renders the new value
    // (response caches are invalidated by the save).
    let form = format!("csrf={csrf}&accent_color=%23ff0000&footer_text=Made+with+Polaris");
    let resp = post_form(
        &router,
        "/admin/themes/default/settings",
        &form,
        &session,
        41002,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("ok=")
    );

    let resp = send(
        &router,
        Request::builder().uri("/").body(Body::empty()).unwrap(),
    )
    .await;
    let html = body(resp).await;
    assert!(html.contains("--accent: #ff0000"), "accent color applied");
    assert!(html.contains("Made with Polaris"), "footer note applied");

    // Unknown theme → redirect with an error.
    let resp = get_with(&router, "/admin/themes/nope/settings", &session).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("err=")
    );
}

#[tokio::test]
async fn theme_settings_validation_rejects_bad_input() {
    let (app, router, _dir) = init_config_app().await;
    let session = login(&router, 41011).await;
    let csrf = app.sessions.get(&session).unwrap().csrf;

    // Not a color.
    let form = format!("csrf={csrf}&accent_color=notacolor");
    let resp = post_form(
        &router,
        "/admin/themes/default/settings",
        &form,
        &session,
        41012,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(loc.contains("err="), "rejected: {loc}");

    // The value is unchanged (still the schema default).
    assert_eq!(
        app.configs
            .get_string("theme.default", "accent_color")
            .as_deref(),
        Some("#0071e3")
    );

    // URL fields are format-checked too.
    let form = format!(
        "csrf={csrf}&social_links=%5B%7B%22label%22%3A%22X%22%2C%22url%22%3A%22notaurl%22%7D%5D"
    );
    let resp = post_form(
        &router,
        "/admin/themes/default/settings",
        &form,
        &session,
        41013,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("err=")
    );
}

// ---------------------------------------------------------------------------
// Plugin settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plugin_settings_save_and_live_rhai_reads() {
    let (app, router, _dir) = init_config_app().await;
    let session = login(&router, 41021).await;
    let csrf = app.sessions.get(&session).unwrap().csrf;

    // The plugin's route reports the schema default before any save.
    let resp = send(
        &router,
        Request::builder()
            .uri("/plugins/demo/cfg")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body(resp).await.contains("tagline=Demo plugin"));

    // Out-of-range integer → rejected.
    let form = format!("csrf={csrf}&per_page=500");
    let resp = post_form(
        &router,
        "/admin/plugins/settings/demo",
        &form,
        &session,
        41022,
    )
    .await;
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(loc.contains("err="), "range enforced: {loc}");

    // Valid save (tagline + unchecked checkbox: hidden `false` only).
    let form = format!("csrf={csrf}&tagline=Hello+World&append_signature=false&per_page=25");
    let resp = post_form(
        &router,
        "/admin/plugins/settings/demo",
        &form,
        &session,
        41023,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("ok=")
    );

    // config_get* read the new values live — no plugin reload involved.
    let resp = send(
        &router,
        Request::builder()
            .uri("/plugins/demo/cfg")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let text = body(resp).await;
    assert!(text.contains("tagline=Hello World"), "{text}");
    assert!(
        text.contains("sig=false"),
        "checkbox false round-trips: {text}"
    );

    // The on_config_changed hook fired for the changed key.
    assert!(
        text.contains("tagline_chg=Demo plugin|Hello World"),
        "{text}"
    );

    // Typed reads through the manager agree.
    assert_eq!(
        app.configs.get_string("plugin.demo", "tagline").as_deref(),
        Some("Hello World")
    );
    assert_eq!(app.configs.get_int("plugin.demo", "per_page"), Some(25));
}

#[tokio::test]
async fn plugin_secrets_are_encrypted_and_masked() {
    let (app, router, _dir) = init_config_app().await;
    let session = login(&router, 41031).await;
    let csrf = app.sessions.get(&session).unwrap().csrf;

    let form = format!("csrf={csrf}&api_key=hunter2");
    let resp = post_form(
        &router,
        "/admin/plugins/settings/demo",
        &form,
        &session,
        41032,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("ok=")
    );

    // Encrypted at rest.
    let row = app
        .db
        .fetch_optional(
            "SELECT value FROM settings WHERE name = ?",
            &[Bind::S("plugin.demo.api_key".into())],
        )
        .await
        .unwrap()
        .expect("row");
    let stored: String = sqlx::Row::try_get(&row, "value").unwrap();
    assert!(stored.starts_with("enc:v1:"), "encrypted: {stored}");
    assert!(!stored.contains("hunter2"));

    // Never echoed back to the settings page.
    let resp = get_with(&router, "/admin/plugins/settings/demo", &session).await;
    let html = body(resp).await;
    assert!(!html.contains("hunter2"), "secret masked in the form");
    assert!(html.contains("API key"), "field still rendered");

    // But the plugin itself can read its own secret.
    let resp = send(
        &router,
        Request::builder()
            .uri("/plugins/demo/cfg")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(body(resp).await.contains("key=hunter2"));

    // An empty secret input keeps the current value.
    let form = format!("csrf={csrf}&api_key=");
    post_form(
        &router,
        "/admin/plugins/settings/demo",
        &form,
        &session,
        41033,
    )
    .await;
    assert_eq!(
        app.configs.get_string("plugin.demo", "api_key").as_deref(),
        Some("hunter2")
    );
}

#[tokio::test]
async fn show_if_hides_fields_server_side() {
    let (app, router, _dir) = init_config_app().await;
    let session = login(&router, 41041).await;
    let csrf = app.sessions.get(&session).unwrap().csrf;

    // show_details defaults to true → tracking_id is rendered.
    let resp = get_with(&router, "/admin/plugins/settings/demo", &session).await;
    let html = body(resp).await;
    assert!(html.contains("name=\"tracking_id\""));

    // Turn show_details off (hidden `false` + unchecked box).
    let form = format!("csrf={csrf}&show_details=false");
    post_form(
        &router,
        "/admin/plugins/settings/demo",
        &form,
        &session,
        41042,
    )
    .await;

    // tracking_id is no longer rendered (server-side show_if).
    let resp = get_with(&router, "/admin/plugins/settings/demo", &session).await;
    let html = body(resp).await;
    assert!(!html.contains("name=\"tracking_id\""));

    // And back on.
    let form = format!("csrf={csrf}&show_details=true");
    post_form(
        &router,
        "/admin/plugins/settings/demo",
        &form,
        &session,
        41043,
    )
    .await;
    let resp = get_with(&router, "/admin/plugins/settings/demo", &session).await;
    assert!(body(resp).await.contains("name=\"tracking_id\""));
}

#[tokio::test]
async fn settings_pages_require_admin_session() {
    let (_app, router, _dir) = init_config_app().await;

    for uri in [
        "/admin/themes/default/settings",
        "/admin/plugins/settings/demo",
    ] {
        let resp = send(
            &router,
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "GET {uri}");
        assert!(
            resp.headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("/admin/login")
        );
    }
}
