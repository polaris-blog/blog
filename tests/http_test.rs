//! HTTP layer: public pages, SEO endpoints, REST API, admin login flow,
//! comment submission.

mod common;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use tower::ServiceExt;

use polaris::models::{CommentStatus, Role};
use polaris::state::App;

/// App + router + kept-alive temp dir (admin user + seeded content).
async fn init_http() -> (App, axum::Router, tempfile::TempDir) {
    let (app, dir) = common::init_app().await;
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    let router = polaris::http::router(app.clone());
    (app, router, dir)
}

#[tokio::test]
async fn trusted_proxy_clients_have_separate_login_and_comment_limits() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::test_config(dir.path());
    cfg.security.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let app = polaris::state::AppState::init(cfg).await.unwrap();
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    let router = polaris::http::router(app.clone());
    let csrf = set_cookie(&get(&router, "/admin/login").await, "polaris_csrf").unwrap();
    // One client is locked; another client using the same proxy can log in.
    for _ in 0..5 {
        app.limiter.record_failure("login:198.51.100.1");
    }
    for (client, expected) in [
        ("198.51.100.1", StatusCode::OK),
        ("198.51.100.2", StatusCode::SEE_OTHER),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, format!("polaris_csrf={csrf}"))
            .header("x-forwarded-for", format!("192.0.2.99, {client}"))
            .extension(local_addr(40100))
            .body(Body::from(format!(
                "username=admin&password=password123&csrf={csrf}"
            )))
            .unwrap();
        assert_eq!(send(&router, req).await.status(), expected);
    }
    let post = polaris::repositories::posts::find_by_slug(&app.db, "hello-polaris")
        .await
        .unwrap()
        .unwrap();
    for _ in 0..5 {
        app.comment_limiter.record_failure("comment:198.51.100.1");
    }
    for (client, expected) in [
        ("198.51.100.1", StatusCode::TOO_MANY_REQUESTS),
        ("198.51.100.2", StatusCode::SEE_OTHER),
    ] {
        let req = Request::builder().method("POST").uri("/comments")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-forwarded-for", client)
            .extension(local_addr(40101))
            .body(Body::from(format!("post_id={}&post_slug=hello-polaris&name=Reader&email=reader%40example.test&content=Hello", post.id))).unwrap();
        assert_eq!(send(&router, req).await.status(), expected);
    }
    assert!(!app.comment_limiter.is_locked("comment:127.0.0.1"));
}

async fn send(router: &axum::Router, req: Request) -> Response {
    router.clone().oneshot(req).await.expect("oneshot")
}

async fn get(router: &axum::Router, uri: &str) -> Response {
    send(
        router,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

async fn body(resp: Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Extract a named cookie from Set-Cookie headers.
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

// ---------------------------------------------------------------------------
// Public pages
// ---------------------------------------------------------------------------

#[tokio::test]
async fn home_page_renders_with_etag() {
    let (_app, router, _dir) = init_http().await;

    let resp = get(&router, "/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers().get(header::ETAG).cloned().expect("etag");
    let html = body(resp).await;
    assert!(
        html.contains("Hello, Polaris"),
        "home lists the seeded post"
    );

    // Conditional request → 304 Not Modified.
    let req = Request::builder()
        .uri("/")
        .header(header::IF_NONE_MATCH, etag)
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn post_page_renders_markdown() {
    let (_app, router, _dir) = init_http().await;

    let resp = get(&router, "/posts/hello-polaris").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(html.contains("Hello, Polaris"));
    assert!(
        html.contains("<h1>Welcome to Polaris</h1>"),
        "markdown rendered"
    );
}

#[tokio::test]
async fn unknown_paths_are_404() {
    let (_app, router, _dir) = init_http().await;
    for uri in ["/definitely-not-a-page", "/posts/no-such-post"] {
        let resp = get(&router, uri).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "GET {uri}");
    }
}

#[tokio::test]
async fn security_headers_present() {
    let (_app, router, _dir) = init_http().await;
    let resp = get(&router, "/").await;
    let h = resp.headers();
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert!(h.get("content-security-policy").is_some());
}

// ---------------------------------------------------------------------------
// SEO endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn seo_feeds_and_sitemap() {
    let (_app, router, _dir) = init_http().await;

    let resp = get(&router, "/rss.xml").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let xml = body(resp).await;
    assert!(xml.contains("<rss"), "rss root element");
    assert!(xml.contains("hello-polaris"), "rss lists the seeded post");

    let resp = get(&router, "/atom.xml").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let xml = body(resp).await;
    assert!(xml.contains("<feed"), "atom root element");
    assert!(xml.contains("hello-polaris"));

    let resp = get(&router, "/sitemap.xml").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let xml = body(resp).await;
    assert!(xml.contains("<urlset"), "sitemap root element");
    assert!(xml.contains("hello-polaris"));
    assert!(xml.contains("/about"), "sitemap lists pages");

    let resp = get(&router, "/robots.txt").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let txt = body(resp).await;
    assert!(txt.contains("Disallow: /admin"));
    assert!(txt.contains("Sitemap:"));
}

// ---------------------------------------------------------------------------
// REST API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_read_list_and_auth_gate() {
    let (_app, router, _dir) = init_http().await;

    let resp = get(&router, "/api/posts").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body(resp).await;
    assert!(json.contains("\"data\""));
    assert!(json.contains("hello-polaris"));

    // Write endpoints require a session.
    let req = Request::builder()
        .method("POST")
        .uri("/api/posts")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"title":"x"}"#))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = get(&router, "/api/comments").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Admin login flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_login_flow() {
    let (_app, router, _dir) = init_http().await;

    // Unauthenticated dashboard access redirects to login.
    let resp = get(&router, "/admin").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("/admin/login")
    );

    // Login page sets the CSRF cookie.
    let resp = get(&router, "/admin/login").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let csrf = set_cookie(&resp, "polaris_csrf").expect("csrf cookie");
    let html = body(resp).await;
    assert!(
        html.contains("csrf") || html.contains("CSRF"),
        "form carries the token"
    );

    // Wrong password: no session, login page re-rendered with an error.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .extension(local_addr(40010))
        .body(Body::from(format!(
            "username=admin&password=wrong-pass&csrf={csrf}"
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(set_cookie(&resp, "polaris_session").is_none());

    // Missing CSRF cookie → rejected.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .extension(local_addr(40011))
        .body(Body::from(format!(
            "username=admin&password=password123&csrf={csrf}"
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::OK); // login page with error
    assert!(set_cookie(&resp, "polaris_session").is_none());

    // Correct credentials → session cookie + redirect to /admin.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .extension(local_addr(40012))
        .body(Body::from(format!(
            "username=admin&password=password123&csrf={csrf}"
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let session = set_cookie(&resp, "polaris_session").expect("session cookie");

    // Authenticated dashboard renders.
    let resp = get_with_session(&router, "/admin", &session).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(html.contains("Dashboard"));
}

async fn get_with_session(router: &axum::Router, uri: &str, session: &str) -> Response {
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

/// Log in as the seeded admin; returns the session cookie value.
async fn login_admin(router: &axum::Router, port: u16) -> String {
    let resp = get(router, "/admin/login").await;
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

// ---------------------------------------------------------------------------
// Admin navigation page
// ---------------------------------------------------------------------------

/// Extract the public top-bar navigation section from a rendered page.
fn navbar(html: &str) -> &str {
    let start = html.find("<nav").unwrap_or(0);
    let end = html.find("</nav>").unwrap_or(html.len());
    &html[start..end]
}

#[tokio::test]
async fn navigation_page_lists_system_entries_and_saves_custom_links() {
    let (app, router, _dir) = init_http().await;
    let session = login_admin(&router, 40140).await;

    // System entries are editable (show/hide + label) above the custom editor.
    let resp = get_with_session(&router, "/admin/navigation", &session).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(html.contains("System navigation"), "system section present");
    assert!(html.contains("Custom links"), "custom section present");
    assert!(
        html.contains("sys_enabled"),
        "visibility checkboxes present"
    );
    assert!(html.contains("sys_url"), "system rows carry their URL key");
    assert!(html.contains("nav-badge-page"), "published pages listed");
    assert!(html.contains("nav-edit-list"), "list-style editor present");

    let token = html
        .split("name=\"csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf token in form")
        .to_string();

    // Save one custom link (empty second row is dropped).
    let req = Request::builder()
        .method("POST")
        .uri("/admin/navigation")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_session={session}"))
        .extension(local_addr(40141))
        .body(Body::from(format!(
            "csrf={token}&label=Docs&url=/docs&label=&url="
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    let stored = app.settings.get("site.navigation").expect("stored value");
    assert!(
        stored.contains("\"label\":\"Docs\""),
        "label stored: {stored}"
    );
    assert!(stored.contains("\"url\":\"/docs\""), "url stored: {stored}");

    let html = body(get_with_session(&router, "/admin/navigation", &session).await).await;
    assert!(html.contains("value=\"Docs\""), "custom link re-rendered");
}

#[tokio::test]
async fn navigation_system_overrides_apply_to_public_nav() {
    let (_app, router, _dir) = init_http().await;
    let session = login_admin(&router, 40143).await;

    let html = body(get_with_session(&router, "/admin/navigation", &session).await).await;
    let token = html
        .split("name=\"csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf token in form")
        .to_string();

    // Rename Home, keep the About page at its default, hide the RSS link.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/navigation")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_session={session}"))
        .extension(local_addr(40144))
        .body(Body::from(format!(
            "csrf={token}&sys_url=/&sys_enabled=/&sys_label=Home+Sweet+Home&sys_url=/about&sys_enabled=/about&sys_label=&sys_url=/rss.xml&sys_label=&label=&url="
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // Public top bar reflects the overrides.
    let html = body(get(&router, "/").await).await;
    let nav = navbar(&html);
    assert!(nav.contains("Home Sweet Home"), "renamed home entry");
    assert!(nav.contains("About"), "page entry keeps default label");
    assert!(!nav.contains("rss.xml"), "hidden rss entry");
}

#[tokio::test]
async fn navigation_rss_entry_follows_theme_config() {
    let (app, router, dir) = init_http().await;
    let session = login_admin(&router, 40142).await;

    // A theme whose schema defaults the RSS link to on.
    let theme = dir.path().join("themes").join("rssy");
    std::fs::create_dir_all(theme.join("templates")).unwrap();
    std::fs::write(
        theme.join("theme.toml"),
        "name = \"Rssy\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        theme.join("config.schema.toml"),
        "[groups.appearance]\nlabel = \"Appearance\"\n\n[show_rss_link]\ntype = \"boolean\"\nlabel = \"RSS link in navigation\"\ndefault = true\ngroup = \"appearance\"\n",
    )
    .unwrap();
    app.set_active_theme("rssy").await.unwrap();

    let html = body(get_with_session(&router, "/admin/navigation", &session).await).await;
    assert!(html.contains("rss.xml"), "rss entry listed");
    assert!(
        html.contains("nav-badge-theme"),
        "rss marked as theme-driven"
    );
}

// ---------------------------------------------------------------------------
// Comment submission
// ---------------------------------------------------------------------------

#[tokio::test]
async fn comment_submission_and_honeypot() {
    let (app, router, _dir) = init_http().await;
    let seeded = polaris::services::posts::get_public_post(&app, "hello-polaris")
        .await
        .unwrap()
        .expect("seeded post");

    let form = format!(
        "post_id={}&post_slug=hello-polaris&name=Visitor&email=&url=&content=Great+post&website=",
        seeded.id
    );
    let req = Request::builder()
        .method("POST")
        .uri("/comments")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .extension(local_addr(40020))
        .body(Body::from(form))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // Stored as pending (moderation on by default).
    let (all, _) = polaris::repositories::comments::list(&app.db, None, 1, 10)
        .await
        .unwrap();
    assert!(
        all.iter()
            .any(|c| c.content == "Great post" && c.status == CommentStatus::Pending)
    );

    // Honeypot: pretend success, store nothing.
    let before = all.len();
    let form = format!(
        "post_id={}&post_slug=hello-polaris&name=Bot&email=&url=&content=spam&website=http://spam.example",
        seeded.id
    );
    let req = Request::builder()
        .method("POST")
        .uri("/comments")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .extension(local_addr(40021))
        .body(Body::from(form))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let (after, _) = polaris::repositories::comments::list(&app.db, None, 1, 10)
        .await
        .unwrap();
    assert_eq!(after.len(), before, "honeypot submissions are not stored");
}

// ---------------------------------------------------------------------------
// Nested comments
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nested_comments_validate_parent_and_render_depth() {
    let (app, router, _dir) = init_http().await;
    let seeded = polaris::services::posts::get_public_post(&app, "hello-polaris")
        .await
        .unwrap()
        .expect("seeded post");

    let submit = |router: &axum::Router, body: String, port: u16| {
        let router = router.clone();
        async move {
            send(
                &router,
                Request::builder()
                    .method("POST")
                    .uri("/comments")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .extension(local_addr(port))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
        }
    };
    let approve_latest = |app: &polaris::state::App, content: &str| {
        let app = app.clone();
        let content = content.to_string();
        async move {
            let (all, _) = polaris::repositories::comments::list(&app.db, None, 1, 50)
                .await
                .unwrap();
            let comment = all
                .iter()
                .find(|c| c.content == content)
                .expect("comment stored")
                .clone();
            polaris::services::comments::moderate(&app, comment.id, CommentStatus::Approved)
                .await
                .unwrap();
            comment.id
        }
    };

    // Top-level comment, then approve it.
    let resp = submit(
        &router,
        format!(
            "post_id={}&post_slug=hello-polaris&name=Ann&email=&url=&content=Top+level",
            seeded.id
        ),
        40050,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let top_id = approve_latest(&app, "Top level").await;

    // Replying to a non-approved parent is rejected.
    let resp = submit(&router, format!("post_id={}&post_slug=hello-polaris&name=Bob&email=&url=&content=Too+early&parent_id={}", seeded.id, top_id + 9999), 40051).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Reply to the approved top-level comment, approve, reply again (depth 2).
    let resp = submit(&router, format!("post_id={}&post_slug=hello-polaris&name=Bob&email=&url=&content=First+reply&parent_id={}", seeded.id, top_id), 40052).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let reply_id = approve_latest(&app, "First reply").await;

    let resp = submit(&router, format!("post_id={}&post_slug=hello-polaris&name=Cid&email=&url=&content=Nested+reply&parent_id={}", seeded.id, reply_id), 40053).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    approve_latest(&app, "Nested reply").await;

    // The post renders the flattened tree with increasing depth.
    let html = body(get(&router, "/posts/hello-polaris").await).await;
    assert!(html.contains("--depth: 0"), "top-level depth rendered");
    assert!(html.contains("--depth: 1"), "reply depth rendered");
    assert!(html.contains("--depth: 2"), "nested reply depth rendered");
    assert!(html.contains("data-reply="), "reply links rendered");
}

// ---------------------------------------------------------------------------
// First-run setup wizard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn setup_wizard_creates_first_admin() {
    // Fresh instance without any user account.
    let (app, dir) = common::init_app().await;
    let router = polaris::http::router(app.clone());

    // Unauthenticated /admin redirects to the setup wizard.
    let resp = get(&router, "/admin").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("/admin/setup"),
        "setup redirect"
    );

    // Setup page renders with the CSRF cookie (step 1: environment).
    let resp = get(&router, "/admin/setup").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let csrf = set_cookie(&resp, "polaris_csrf").expect("csrf cookie");
    let html = body(resp).await;
    assert!(html.contains("Step 1"), "environment step first");

    // Step 1: saving the environment writes the config file.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/setup")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .body(Body::from(
            format!("csrf={csrf}&step=env&db_driver=sqlite&sqlite_path=data/polaris.db&cache_enabled=on&cache_driver=memory"),
        ))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(
        html.contains("Environment configuration saved"),
        "saved banner"
    );

    let raw = std::fs::read_to_string(dir.path().join("polaris.toml")).expect("config written");
    assert!(
        raw.contains("[setup]") && raw.contains("env_done = true"),
        "marker written"
    );
    assert!(raw.contains("sqlite"), "database driver written");

    // Step 2 now renders (administrator account).
    let resp = get(&router, "/admin/setup").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(html.contains("Step 2"), "administrator step second");

    // Mismatched passwords re-render the page with an error.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/setup")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .body(Body::from(
            format!("csrf={csrf}&step=admin&site_title=Blog&site_locale=en&username=owner&email=&password=password123&password_confirm=other"),
        ))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body(resp).await;
    assert!(
        html.contains("Passwords do not match"),
        "mismatch error shown"
    );

    // Successful install: creates the admin, seeds content, redirects to login.
    let req = Request::builder()
        .method("POST")
        .uri("/admin/setup")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .body(Body::from(
            format!("csrf={csrf}&step=admin&site_title=My+Blog&site_locale=en&username=owner&email=&password=password123&password_confirm=password123"),
        ))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(
        resp.headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("/admin/login"),
        "redirect to login after install"
    );

    // Setup is closed once installed.
    let resp = get(&router, "/admin/setup").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let req = Request::builder()
        .method("POST")
        .uri("/admin/setup")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(
            "csrf=&site_title=X&username=hax&password=password123&password_confirm=password123",
        ))
        .unwrap();
    assert_eq!(send(&router, req).await.status(), StatusCode::SEE_OTHER);

    // The created admin can log in.
    let resp = get(&router, "/admin/login").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let csrf = set_cookie(&resp, "polaris_csrf").expect("csrf cookie");
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .extension(local_addr(40150))
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .body(Body::from(format!(
            "username=owner&password=password123&csrf={csrf}"
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let session = set_cookie(&resp, "polaris_session").expect("session cookie");
    let html = body(get_with_session(&router, "/admin", &session).await).await;
    assert!(html.contains("My Blog"), "site title applied");
}

#[tokio::test]
async fn i18n_locale_switches_public_strings() {
    let (app, router, _dir) = init_http().await;

    // English by default (test app uses the embedded fallback theme).
    let html = body(get(&router, "/").await).await;
    assert!(html.contains("Read more"), "english default");

    // Switch the site locale (as Admin → Settings does).
    app.settings
        .set(&app.db, "site.locale", "zh-CN")
        .await
        .unwrap();
    polaris::i18n::set_locale("zh-CN");

    let html = body(get(&router, "/").await).await;
    assert!(html.contains("阅读全文"), "localized read-more");
    assert!(html.contains("分钟阅读"), "localized reading time");

    // Unknown locales fall back to English.
    polaris::i18n::set_locale("fr-FR");
    let html = body(get(&router, "/").await).await;
    assert!(html.contains("Read more"), "fallback to english");
}

#[tokio::test]
async fn theme_static_files_are_served_streamed() {
    let (app, router, dir) = init_http().await;

    // A disk theme with one static asset and a minimal template.
    let theme = dir.path().join("themes").join("default");
    std::fs::create_dir_all(theme.join("templates")).unwrap();
    std::fs::create_dir_all(theme.join("static")).unwrap();
    std::fs::write(
        theme.join("theme.toml"),
        "name = \"Test\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        theme.join("templates").join("index.html"),
        "<!doctype html><title>{{ site.title }}</title>",
    )
    .unwrap();
    std::fs::write(theme.join("static").join("asset.txt"), "hello asset").unwrap();
    app.set_active_theme("default").await.unwrap();

    // Asset is served with content + cache headers.
    let resp = get(&router, "/static/asset.txt").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body(resp).await, "hello asset");
    let resp = get(&router, "/static/asset.txt").await;
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=86400"
    );
    assert!(resp.headers().get(header::ETAG).is_some());

    // Missing theme script falls back to the embedded one.
    let resp = get(&router, "/static/js/theme.js").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );

    // Traversal never escapes the static directory.
    let resp = get(&router, "/static/../theme.toml").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = get(&router, "/static/..%2ftheme.toml").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// CSRF on the body-less POST /api/cache/clear
// ---------------------------------------------------------------------------

/// A body-less POST can be forged cross-site (no CORS preflight), unlike
/// JSON endpoints. The session CSRF token is therefore required in the
/// `X-CSRF-Token` header.
#[tokio::test]
async fn api_cache_clear_requires_csrf_header() {
    let (_app, router, _dir) = init_http().await;

    // Anonymous → 401.
    let req = Request::builder()
        .method("POST")
        .uri("/api/cache/clear")
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Log in and read the CSRF token from the dashboard form.
    let resp = get(&router, "/admin/login").await;
    let csrf = set_cookie(&resp, "polaris_csrf").expect("csrf cookie");
    let req = Request::builder()
        .method("POST")
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("polaris_csrf={csrf}"))
        .extension(local_addr(40030))
        .body(Body::from(format!(
            "username=admin&password=password123&csrf={csrf}"
        )))
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let session = set_cookie(&resp, "polaris_session").expect("session cookie");

    let html = body(get_with_session(&router, "/admin/settings", &session).await).await;
    let token = html
        .split("name=\"csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf token in settings html")
        .to_string();
    assert!(!token.is_empty());

    // Session without the CSRF header → 403 (the forged request stops here).
    let req = Request::builder()
        .method("POST")
        .uri("/api/cache/clear")
        .header(header::COOKIE, format!("polaris_session={session}"))
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Session + X-CSRF-Token → cleared.
    let req = Request::builder()
        .method("POST")
        .uri("/api/cache/clear")
        .header(header::COOKIE, format!("polaris_session={session}"))
        .header("x-csrf-token", &token)
        .body(Body::empty())
        .unwrap();
    let resp = send(&router, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
