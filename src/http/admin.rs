//! Admin UI — fully server-rendered, zero JavaScript.
//!
//! Auth: session cookie (`admin_auth_mw`), CSRF token per session, role
//! checks per handler. Login uses a double-submit CSRF cookie + per-IP
//! rate limiting.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use axum::extract::{ConnectInfo, Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use tera::Context;

use crate::auth::{self, AuthCtx};
use crate::error::{AppError, AppResult};
use crate::models::{CommentStatus, PostStatus, Role, TermKind};
use crate::repositories;
use crate::services;
use crate::state::App;
use crate::templates;
use crate::utils::{cookies, time};

pub fn router(app: App) -> Router<App> {
    // Routes reachable without a session.
    let open = Router::new()
        .route("/admin/login", get(login_form).post(login_submit))
        .route("/admin/setup", get(setup_form).post(setup_submit))
        .route(
            "/admin/static/style.css",
            get(crate::http::static_files::admin_css),
        )
        .route(
            "/admin/static/highlight.js",
            get(crate::http::static_files::admin_highlight_js),
        )
        .route(
            "/admin/static/media.js",
            get(crate::http::static_files::admin_media_js),
        )
        .route(
            "/admin/static/extensions.js",
            get(crate::http::static_files::admin_extensions_js),
        )
        .route(
            "/admin/static/admin.js",
            get(crate::http::static_files::admin_js),
        )
        .route(
            "/admin/static/navigation.js",
            get(crate::http::static_files::admin_navigation_js),
        )
        .route(
            "/admin/static/editor.js",
            get(crate::http::static_files::admin_editor_js),
        );

    let protected = Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/logout", post(logout))
        .route("/admin/posts", get(posts_list))
        .route("/admin/posts/new", get(post_new).post(post_create))
        .route("/admin/posts/{id}/edit", get(post_edit).post(post_update))
        .route("/admin/posts/{id}/delete", post(post_delete))
        .route("/admin/pages", get(pages_list))
        .route("/admin/pages/new", get(page_new).post(page_create))
        .route("/admin/pages/{id}/edit", get(page_edit).post(page_update))
        .route("/admin/pages/{id}/delete", post(page_delete))
        .route(
            "/admin/navigation",
            get(navigation_page).post(navigation_save),
        )
        .route("/admin/terms", get(terms_page))
        .route("/admin/terms/create", post(term_create))
        .route("/admin/terms/{id}/delete", post(term_delete))
        .route("/admin/comments", get(comments_list))
        .route("/admin/comments/{id}/status", post(comment_status))
        .route("/admin/comments/{id}/delete", post(comment_delete))
        .route("/admin/users", get(users_list))
        .route("/admin/users/new", get(user_new).post(user_create))
        .route("/admin/users/{id}/edit", get(user_edit).post(user_update))
        .route("/admin/users/{id}/delete", post(user_delete))
        .route("/admin/themes", get(themes_page))
        .route("/admin/themes/activate", post(theme_activate))
        .route(
            "/admin/themes/{name}/settings",
            get(theme_settings).post(theme_settings_save),
        )
        .route("/admin/plugins", get(plugins_page))
        .route("/admin/plugins/toggle", post(plugin_toggle))
        .route(
            "/admin/plugins/settings/{name}",
            get(plugin_settings).post(plugin_settings_save),
        )
        .route("/admin/settings", get(settings_page).post(settings_save))
        .route("/admin/profile", get(profile_page).post(profile_save))
        .route("/admin/profile/password", post(profile_password))
        .route("/admin/cache/clear", post(cache_clear))
        .route("/admin/preview", post(markdown_preview))
        .route("/admin/search", get(search_page))
        .route("/admin/search/rebuild", post(search_rebuild))
        .merge(super::admin_media::router(&app))
        .route("/admin/plugins/{*path}", get(plugin_admin_route))
        .route_layer(middleware::from_fn_with_state(
            app,
            crate::auth::admin_auth_mw,
        ));

    open.merge(protected)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Query parameters accepted by every admin page (flash + filters).
#[derive(Deserialize, Default)]
pub(crate) struct PageQuery {
    pub(crate) ok: Option<String>,
    pub(crate) err: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) page: Option<i64>,
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn redirect_ok(path: &str, msg: &str) -> Response {
    Redirect::to(&format!(
        "{path}?ok={}",
        urlencode(&crate::i18n::tr_or(msg))
    ))
    .into_response()
}

pub(crate) fn redirect_err(path: &str, msg: &str) -> Response {
    Redirect::to(&format!(
        "{path}?err={}",
        urlencode(&crate::i18n::tr_or(msg))
    ))
    .into_response()
}

pub(crate) fn require_role(auth: &AuthCtx, min: Role) -> AppResult<()> {
    if auth.role.at_least(min) {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "{} role required",
            min.as_str()
        )))
    }
}

/// May `auth` modify content owned by `owner_id`?
pub(crate) fn can_edit(auth: &AuthCtx, owner_id: i64) -> bool {
    auth.role.at_least(Role::Editor) || auth.user_id == owner_id
}

pub(crate) fn base_ctx(app: &App, auth: &AuthCtx, active: &str, q: &PageQuery) -> Context {
    let mut ctx = Context::new();
    ctx.insert("site_title", &app.site_title());
    ctx.insert(
        "user",
        &json!({
            "username": auth.username,
            "display_name": auth.display_name,
            "role": auth.role.as_str(),
        }),
    );
    ctx.insert("csrf", &auth.csrf);
    ctx.insert("active", active);
    ctx.insert("ok", &q.ok);
    ctx.insert("err", &q.err);
    ctx.insert("form_error", "");
    ctx.insert("version", env!("CARGO_PKG_VERSION"));
    let links: Vec<_> = app
        .plugins
        .admin_route_paths()
        .into_iter()
        .map(|(name, path)| json!({ "label": name, "url": format!("/admin{path}") }))
        .collect();
    ctx.insert("plugin_admin_links", &links);
    ctx
}

fn human_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

// ---------------------------------------------------------------------------
// Login / logout
// ---------------------------------------------------------------------------

async fn login_page(app: &App, error: &str) -> Response {
    // Fresh instance: send visitors to the setup wizard instead.
    if app.needs_setup()
        && repositories::users::count(&app.db).await.unwrap_or(0) == 0
    {
        return Redirect::to("/admin/setup").into_response();
    }
    let csrf = cookies::random_token(32);
    let mut ctx = Context::new();
    ctx.insert("csrf", &csrf);
    ctx.insert("error", &crate::i18n::tr_or(error));
    ctx.insert("next", "");
    let body = templates::render_admin("login.html", &ctx)
        .unwrap_or_else(|e| format!("template error: {e}"));
    let mut resp = Html(body).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        cookies::set_cookie(
            auth::LOGIN_CSRF_COOKIE,
            &csrf,
            1800,
            "/",
            true,
            auth::secure_cookies_for(app),
        ),
    );
    resp
}

async fn login_form(State(app): State<App>) -> AppResult<Response> {
    Ok(login_page(&app, "").await)
}

async fn login_submit(
    State(app): State<App>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<auth::LoginForm>,
) -> Response {    if !auth::login_csrf_ok(&headers, &form.csrf) {
        return login_page(&app, "Invalid or expired form token — please try again.").await;
    }
    let ip =
        crate::utils::client_ip::resolve(addr.ip(), &headers, &app.config.security.trusted_proxies);
    let key = format!("login:{ip}");
    if app.limiter.is_locked(&key) {
        return login_page(&app, "Too many failed attempts. Try again in 15 minutes.").await;
    }
    match services::users::authenticate(&app, &form.username, &form.password).await {
        Some(user) => {
            app.limiter.reset(&key);
            let ttl = app.config.security.session_ttl_hours * 3600;
            let token = app.sessions.create(user.id, ttl);
            let mut resp = Redirect::to(&auth::safe_next(&form.next)).into_response();
            let (name, value) = auth::session_cookie(&token, ttl, auth::secure_cookies_for(&app));
            resp.headers_mut().insert(name, value);
            resp
        }
        None => {
            app.limiter.record_failure(&key);
            login_page(&app, "Wrong username or password.").await
        }
    }
}

// ---------------------------------------------------------------------------
// First-run setup wizard
// ---------------------------------------------------------------------------

/// Serializes setup submissions so two concurrent requests cannot both pass
/// the "no user exists yet" check and each create an administrator — the
/// check-then-insert sequence in `create_user` is not atomic across requests.
static SETUP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Deserialize)]
struct SetupForm {
    csrf: String,
    #[serde(default)]
    step: String,
    // Step 1 — environment.
    #[serde(default)]
    db_driver: String,
    #[serde(default)]
    sqlite_path: String,
    #[serde(default)]
    db_host: String,
    #[serde(default)]
    db_port: String,
    #[serde(default)]
    db_user: String,
    #[serde(default)]
    db_password: String,
    #[serde(default)]
    db_name: String,
    #[serde(default)]
    cache_enabled: String,
    #[serde(default)]
    cache_driver: String,
    #[serde(default)]
    redis_url: String,
    #[serde(default)]
    redis_namespace: String,
    // Step 2 — administrator.
    #[serde(default)]
    site_title: String,
    #[serde(default)]
    site_locale: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password_confirm: String,
}

fn config_file_path(app: &App) -> std::path::PathBuf {
    app.config
        .config_path
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("polaris.toml"))
}

/// Step 2 is offered once the environment step has been saved. The marker
/// lives in the config file's `[setup]` section — it survives switching to a
/// different database, unlike a settings-table row.
fn env_step_done(app: &App) -> bool {
    std::fs::read_to_string(config_file_path(app))
        .ok()
        .and_then(|raw| raw.parse::<toml::Value>().ok())
        .and_then(|v| {
            v.get("setup")
                .and_then(|s| s.get("env_done"))
                .and_then(|b| b.as_bool())
        })
        .unwrap_or(false)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn setup_page(
    app: &App,
    step: &str,
    error: &str,
    saved: bool,
    values: &HashMap<String, String>,
) -> Response {
    let csrf = cookies::random_token(32);
    let locale = crate::i18n::locale();
    let mut ctx = Context::new();
    ctx.insert("csrf", &csrf);
    ctx.insert("step", step);
    ctx.insert("error", &crate::i18n::tr_or(error));
    ctx.insert("saved", &saved);
    // Environment defaults come from the running configuration.
    let db = &app.config.database;
    let cache = &app.config.cache;
    let def = |key: &str, fallback: &str| {
        values
            .get(key)
            .cloned()
            .unwrap_or_else(|| fallback.to_owned())
    };
    ctx.insert("v_db_driver", &def("db_driver", &db.driver));
    let sqlite_default = if db.driver == "sqlite" {
        db.url.as_str()
    } else {
        "data/polaris.db"
    };
    ctx.insert("v_sqlite_path", &def("sqlite_path", sqlite_default));
    ctx.insert(
        "v_db_port",
        &def("db_port", if db.driver == "postgres" { "5432" } else { "3306" }),
    );
    ctx.insert("v_db_host", &def("db_host", "127.0.0.1"));
    ctx.insert("v_db_user", &def("db_user", "polaris"));
    ctx.insert("v_db_name", &def("db_name", "polaris"));
    ctx.insert(
        "v_cache_enabled",
        &values
            .get("cache_enabled")
            .cloned()
            .unwrap_or_else(|| if cache.enabled { "on".into() } else { String::new() }),
    );
    ctx.insert("v_cache_driver", &def("cache_driver", &cache.driver));
    ctx.insert("v_redis_url", &def("redis_url", &cache.redis.url));
    ctx.insert(
        "v_redis_namespace",
        &def("redis_namespace", &cache.redis.namespace),
    );
    ctx.insert("v_site_title", &def("site_title", ""));
    ctx.insert("v_username", &def("username", ""));
    ctx.insert("v_email", &def("email", ""));
    ctx.insert("v_locale", &def("site_locale", &locale));
    let body = templates::render_admin("setup.html", &ctx)
        .unwrap_or_else(|e| format!("template error: {e}"));
    let mut resp = Html(body).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        cookies::set_cookie(
            auth::LOGIN_CSRF_COOKIE,
            &csrf,
            1800,
            "/",
            true,
            auth::secure_cookies_for(app),
        ),
    );
    resp
}

/// Merge the environment choices into the config file's `[database]` and
/// `[cache]` sections, plus a `[setup] env_done` marker for the wizard.
fn write_env_config(
    app: &App,
    db_driver: &str,
    db_url: &str,
    cache_enabled: bool,
    cache_driver: &str,
    redis_url: &str,
    redis_namespace: &str,
) -> anyhow::Result<()> {
    use toml::value::{Table, Value as Tv};
    let path = config_file_path(app);
    let mut root: Table = match std::fs::read_to_string(&path) {
        Ok(raw) => match raw.parse::<Tv>() {
            Ok(Tv::Table(t)) => t,
            _ => Table::new(),
        },
        Err(_) => Table::new(),
    };

    let mut db = match root.remove("database") {
        Some(Tv::Table(t)) => t,
        _ => Table::new(),
    };
    db.insert("driver".into(), Tv::String(db_driver.to_owned()));
    db.insert("url".into(), Tv::String(db_url.to_owned()));
    root.insert("database".into(), Tv::Table(db));

    let mut cache = match root.remove("cache") {
        Some(Tv::Table(t)) => t,
        _ => Table::new(),
    };
    cache.insert("enabled".into(), Tv::Boolean(cache_enabled));
    cache.insert("driver".into(), Tv::String(cache_driver.to_owned()));
    if cache_driver == "redis" {
        let mut redis = match cache.remove("redis") {
            Some(Tv::Table(t)) => t,
            _ => Table::new(),
        };
        redis.insert("url".into(), Tv::String(redis_url.to_owned()));
        redis.insert("namespace".into(), Tv::String(redis_namespace.to_owned()));
        cache.insert("redis".into(), Tv::Table(redis));
    }
    root.insert("cache".into(), Tv::Table(cache));

    let mut setup = match root.remove("setup") {
        Some(Tv::Table(t)) => t,
        _ => Table::new(),
    };
    setup.insert("env_done".into(), Tv::Boolean(true));
    root.insert("setup".into(), Tv::Table(setup));

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(&root)?)?;
    Ok(())
}

async fn setup_form(State(app): State<App>) -> AppResult<Response> {
    // Guard: the wizard is only available before the first account exists.
    if repositories::users::count(&app.db).await.unwrap_or(0) > 0 {
        app.set_setup_done();
        return Ok(Redirect::to("/admin/login").into_response());
    }
    let step = if env_step_done(&app) { "admin" } else { "env" };
    Ok(setup_page(&app, step, "", false, &HashMap::new()))
}

async fn setup_submit(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<SetupForm>,
) -> AppResult<Response> {
    // Guard: refuse to re-run setup on an installed instance.
    if repositories::users::count(&app.db).await.unwrap_or(0) > 0 {
        app.set_setup_done();
        return Ok(Redirect::to("/admin/login").into_response());
    }
    // Re-check under the lock: the "no user exists yet" test above is not
    // atomic with account creation, so two concurrent submissions could
    // otherwise each create an administrator. Held through create_user below.
    let _setup_guard = SETUP_LOCK.lock().await;
    if repositories::users::count(&app.db).await.unwrap_or(0) > 0 {
        app.set_setup_done();
        return Ok(Redirect::to("/admin/login").into_response());
    }

    let tr = |key: &str| crate::i18n::translate(&crate::i18n::locale(), key);

    if !auth::login_csrf_ok(&headers, &form.csrf) {
        let msg = tr("setup.error.csrf");
        return Ok(setup_page(&app, "env", &msg, false, &HashMap::new()));
    }

    // ---- Step 1: environment (database + cache) → written to the config file.
    if form.step == "env" {
        let driver = form.db_driver.trim();
        let url = match driver {
            "sqlite" => {
                let p = form.sqlite_path.trim();
                if p.is_empty() {
                    return Ok(setup_page(
                        &app,
                        "env",
                        &tr("setup.error.db_fields"),
                        false,
                        &HashMap::new(),
                    ));
                }
                p.to_owned()
            }
            "mysql" | "postgres" => {
                if form.db_host.trim().is_empty()
                    || form.db_user.trim().is_empty()
                    || form.db_name.trim().is_empty()
                {
                    return Ok(setup_page(
                        &app,
                        "env",
                        &tr("setup.error.db_fields"),
                        false,
                        &HashMap::new(),
                    ));
                }
                let port = if form.db_port.trim().is_empty() {
                    if driver == "mysql" { "3306" } else { "5432" }
                } else {
                    form.db_port.trim()
                };
                format!(
                    "{driver}://{}:{}@{}:{}/{}",
                    percent_encode(form.db_user.trim()),
                    percent_encode(&form.db_password),
                    form.db_host.trim(),
                    port,
                    percent_encode(form.db_name.trim())
                )
            }
            _ => {
                return Ok(setup_page(
                    &app,
                    "env",
                    &tr("setup.error.db_driver"),
                    false,
                    &HashMap::new(),
                ));
            }
        };
        let cache_driver = form.cache_driver.trim();
        if cache_driver != "memory" && cache_driver != "redis" {
            return Ok(setup_page(
                &app,
                "env",
                &tr("setup.error.cache_driver"),
                false,
                &HashMap::new(),
            ));
        }
        if cache_driver == "redis" && form.redis_url.trim().is_empty() {
            return Ok(setup_page(
                &app,
                "env",
                &tr("setup.error.cache_driver"),
                false,
                &HashMap::new(),
            ));
        }
        let cache_enabled = form.cache_enabled == "on";
        match write_env_config(
            &app,
            driver,
            &url,
            cache_enabled,
            cache_driver,
            form.redis_url.trim(),
            if form.redis_namespace.trim().is_empty() {
                "polaris"
            } else {
                form.redis_namespace.trim()
            },
        ) {
            Ok(()) => {
                tracing::info!(
                    db = driver,
                    cache = cache_driver,
                    "environment configuration saved by the setup wizard"
                );
                return Ok(setup_page(&app, "env", "", true, &HashMap::new()));
            }
            Err(e) => {
                let msg = format!("{}: {e}", tr("setup.error.write"));
                return Ok(setup_page(&app, "env", &msg, false, &HashMap::new()));
            }
        }
    }

    // ---- Step 2: administrator account (only after the environment step).
    if !env_step_done(&app) {
        return Ok(setup_page(&app, "env", "", false, &HashMap::new()));
    }

    let mut values: HashMap<String, String> = HashMap::new();
    values.insert("site_title".into(), form.site_title.trim().to_string());
    values.insert("site_locale".into(), form.site_locale.trim().to_string());
    values.insert("username".into(), form.username.trim().to_string());
    values.insert("email".into(), form.email.trim().to_string());

    let title = form.site_title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        let msg = tr("setup.error.site_title");
        return Ok(setup_page(&app, "admin", &msg, false, &values));
    }
    if !form.username.trim().chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
    }) || form.username.trim().is_empty()
        || form.username.trim().chars().count() > 64
    {
        let msg = tr("setup.error.username");
        return Ok(setup_page(&app, "admin", &msg, false, &values));
    }
    if form.password.chars().count() < 8 {
        let msg = tr("setup.error.password_length");
        return Ok(setup_page(&app, "admin", &msg, false, &values));
    }
    if form.password != form.password_confirm {
        let msg = tr("setup.error.password_mismatch");
        return Ok(setup_page(&app, "admin", &msg, false, &values));
    }

    let locale = crate::i18n::normalize(&form.site_locale);
    match services::users::create_user(&app, &form.username, &form.email, &form.password, Role::Admin)
        .await
    {
        Ok(_) => {
            let setting_values: HashMap<String, String> = [
                ("site.title".to_string(), title.to_string()),
                ("site.locale".to_string(), locale.clone()),
            ]
            .into_iter()
            .collect();
            app.settings.set_many(&app.db, &setting_values).await?;
            crate::i18n::set_locale(&locale);
            app.invalidate_content().await;
            Ok(Redirect::to("/admin/login").into_response())
        }
        Err(e) => Ok(setup_page(&app, "admin", &e.message(), false, &values)),
    }
}

async fn logout(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    auth::ensure_csrf(&auth, &form.csrf)?;
    if let Some(token) = cookies::get_cookie(&headers, auth::SESSION_COOKIE) {
        app.sessions.remove(&token);
    }
    let mut resp = Redirect::to("/admin/login").into_response();
    let (name, value) = auth::clear_session_cookie(auth::secure_cookies_for(&app));
    resp.headers_mut().insert(name, value);
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

async fn dashboard(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let pc = repositories::posts::counts_by_status(&app.db).await?;
    let cc = repositories::comments::counts_by_status(&app.db).await?;
    let users = repositories::users::count(&app.db).await?;
    let (recent, _) = repositories::posts::list(
        &app.db,
        &repositories::posts::PostFilter {
            page: 1,
            per_page: 5,
            ..Default::default()
        },
    )
    .await?;
    let recent_comments = repositories::comments::recent(&app.db, 5).await?;
    let theme = app.theme.current();

    let recent_json: Vec<_> = recent
        .iter()
        .map(|p| {
            json!({
                "id": p.id, "title": p.title, "status": p.status.as_str(),
                "updated": time::format(p.updated_at, "datetime"),
                "can_edit": can_edit(&auth, p.author_id),
            })
        })
        .collect();
    let comments_json: Vec<_> = recent_comments
        .iter()
        .map(|c| {
            json!({
                "id": c.id, "author": c.author_name, "status": c.status.as_str(),
                "excerpt": crate::markdown::truncate_chars(&c.content, 80),
                "post_title": c.post_title.clone().unwrap_or_default(),
            })
        })
        .collect();

    let mut ctx = base_ctx(&app, &auth, "dashboard", &q);
    ctx.insert("posts_draft", &pc.draft);
    ctx.insert("posts_scheduled", &pc.scheduled);
    ctx.insert("posts_published", &pc.published);
    ctx.insert("comments_pending", &cc.pending);
    ctx.insert("comments_approved", &cc.approved);
    ctx.insert("comments_spam", &cc.spam);
    ctx.insert("users_total", &users);
    ctx.insert("recent_posts", &recent_json);
    ctx.insert("recent_comments", &comments_json);
    ctx.insert("theme_name", theme.display_name());
    ctx.insert("db_dialect", app.db.dialect().name());
    ctx.insert("uptime", &human_duration(time::now() - app.started_at));

    // Cache panel.
    let stats = app.cache.stats();
    ctx.insert("cache_enabled", &stats.enabled);
    ctx.insert("cache_driver", &stats.driver);
    ctx.insert("cache_entries", &stats.entries);
    ctx.insert("cache_hits", &stats.hits);
    ctx.insert("cache_misses", &stats.misses);
    ctx.insert("cache_hit_rate", &format!("{:.1}", stats.hit_rate * 100.0));
    ctx.insert(
        "cache_memory",
        &stats
            .memory_bytes
            .map(|b| human_bytes(b).to_string())
            .unwrap_or_else(|| "—".into()),
    );

    let body = templates::render_admin("dashboard.html", &ctx)?;
    Ok(Html(body).into_response())
}

/// `POST /admin/cache/clear` — drop all cached entries (admin only).
async fn cache_clear(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    match app.cache.clear_all().await {
        Ok(()) => {
            tracing::info!(by = auth.username, "cache cleared via admin UI");
            Ok(redirect_ok("/admin", "Cache cleared."))
        }
        Err(e) => Ok(redirect_err(
            "/admin",
            &crate::i18n::tr("admin.flash.cache_clear_failed", &[("error", &e.to_string())]),
        )),
    }
}

/// `POST /admin/preview` — render Markdown through the same safe pipeline
/// the public site uses (raw HTML stripped, link schemes whitelisted).
/// Powers the editor's Preview tab. Session + CSRF required; the response
/// is JSON `{ "html": … }` and is never cached.
#[derive(Deserialize)]
struct PreviewForm {
    csrf: String,
    #[serde(default)]
    content: String,
}

async fn markdown_preview(
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<PreviewForm>,
) -> AppResult<Response> {
    auth::ensure_csrf(&auth, &form.csrf)?;
    // Cap the preview the same way the editor caps content (the storage
    // column is TEXT; a preview of megabytes serves no one).
    let content: String = form.content.chars().take(200_000).collect();
    let html = crate::markdown::to_html(&content);
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        axum::Json(json!({ "html": html })),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Search dashboard
// ---------------------------------------------------------------------------

/// `GET /admin/search` — provider, index status, statistics, rebuild action.
async fn search_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let status = match app.search.status(&app).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(redirect_err(
                "/admin",
                &crate::i18n::tr(
                    "admin.flash.search_unavailable",
                    &[("error", &e.message())],
                ),
            ));
        }
    };
    let popular: Vec<_> = app
        .search
        .popular_searches(&app, 10)
        .await
        .into_iter()
        .map(|s| {
            json!({
                "query": s.query,
                "hits": s.hits,
                "no_results": s.no_results,
            })
        })
        .collect();
    let no_results: Vec<_> = app
        .search
        .no_result_searches(&app, 10)
        .await
        .into_iter()
        .map(|s| json!({ "query": s.query, "hits": s.hits, "no_results": s.no_results }))
        .collect();

    let stats = app.cache.stats();
    let mut ctx = base_ctx(&app, &auth, "search", &q);
    ctx.insert("status", &status);
    ctx.insert(
        "status_label",
        if status.healthy {
            "Healthy"
        } else {
            "Needs rebuild"
        },
    );
    ctx.insert(
        "last_rebuild",
        &status
            .last_rebuild
            .map(|t| time::format(t, "datetime"))
            .unwrap_or_else(|| "never".into()),
    );
    ctx.insert("search_enabled", &app.search.enabled());
    ctx.insert("popular", &popular);
    ctx.insert("no_results_list", &no_results);
    ctx.insert("analytics_enabled", &app.config.search.analytics.enabled);
    ctx.insert("cache_hit_rate", &format!("{:.1}", stats.hit_rate * 100.0));
    let body = templates::render_admin("search.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct SearchActionForm {
    csrf: String,
}

/// `POST /admin/search/rebuild` — full index rebuild (admin only). For very
/// large sites prefer the CLI (`polaris search rebuild`, same code path).
async fn search_rebuild(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<SearchActionForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin/search", &e.message()));
    }
    tracing::info!(by = auth.username, "search rebuild started via admin UI");
    match app.search.rebuild(&app, |_, _| {}).await {
        Ok(stats) => {
            let msg = if stats.verified {
                crate::i18n::tr(
                    "admin.flash.index_rebuilt",
                    &[
                        ("posts", &stats.posts.to_string()),
                        ("pages", &stats.pages.to_string()),
                    ],
                )
            } else {
                crate::i18n::tr(
                    "admin.flash.index_rebuilt_warnings",
                    &[
                        ("posts", &stats.posts.to_string()),
                        ("pages", &stats.pages.to_string()),
                    ],
                )
            };
            Ok(redirect_ok("/admin/search", &msg))
        }
        Err(e) => Ok(redirect_err(
            "/admin/search",
            &crate::i18n::tr(
                "admin.flash.rebuild_failed",
                &[("error", &e.message())],
            ),
        )),
    }
}

fn human_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    let n = n as f64;
    if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

// ---------------------------------------------------------------------------
// Posts
// ---------------------------------------------------------------------------

async fn posts_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let status = q
        .status
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(PostStatus::parse);
    let page = q.page.unwrap_or(1).max(1);
    let per: i64 = 20;
    let (posts, total) = repositories::posts::list(
        &app.db,
        &repositories::posts::PostFilter {
            status,
            page,
            per_page: per,
            ..Default::default()
        },
    )
    .await?;
    let mut posts = posts;
    repositories::terms::attach(&app.db, &mut posts).await?;
    let counts = repositories::posts::counts_by_status(&app.db).await?;

    let rows: Vec<_> = posts
        .iter()
        .map(|p| {
            json!({
                "id": p.id, "title": p.title, "slug": p.slug, "status": p.status.as_str(),
                "author": p.author_name.clone().unwrap_or_default(),
                "category": p.category().map(|t| t.name.clone()).unwrap_or_default(),
                "updated": time::format(p.updated_at, "datetime"),
                "can_edit": can_edit(&auth, p.author_id),
            })
        })
        .collect();

    let mut ctx = base_ctx(&app, &auth, "posts", &q);
    ctx.insert("posts", &rows);
    ctx.insert("total", &total);
    ctx.insert("current_page", &page);
    ctx.insert("pages", &((total + per - 1) / per));
    ctx.insert("status_filter", q.status.as_deref().unwrap_or(""));
    ctx.insert("count_draft", &counts.draft);
    ctx.insert("count_scheduled", &counts.scheduled);
    ctx.insert("count_published", &counts.published);
    let body = templates::render_admin("posts.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct PostForm {
    csrf: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    featured_image: String,
    #[serde(default)]
    publish_at: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    tags: String,
}

fn post_form_json(
    post: Option<&crate::models::Post>,
    submitted: Option<&PostForm>,
) -> serde_json::Value {
    // Submitted values win (re-populate after an error), then the stored
    // post, then empty defaults.
    let g = |f: Option<&PostForm>, p: Option<&crate::models::Post>, key: &str| -> String {
        if let Some(f) = f {
            return match key {
                "title" => f.title.clone(),
                "slug" => f.slug.clone(),
                "summary" => f.summary.clone(),
                "content" => f.content.clone(),
                "status" => f.status.clone(),
                "featured_image" => f.featured_image.clone(),
                "publish_at" => f.publish_at.clone(),
                "category" => f.category.clone(),
                "tags" => f.tags.clone(),
                _ => String::new(),
            };
        }
        if let Some(p) = p {
            return match key {
                "title" => p.title.clone(),
                "slug" => p.slug.clone(),
                "summary" => p.summary.clone(),
                "content" => p.content_md.clone(),
                "status" => p.status.as_str().to_string(),
                "featured_image" => p.featured_image.clone().unwrap_or_default(),
                "publish_at" => {
                    if p.status == PostStatus::Scheduled {
                        time::format(p.published_at.unwrap_or_default(), "local")
                    } else {
                        String::new()
                    }
                }
                "category" => p.category().map(|t| t.name.clone()).unwrap_or_default(),
                "tags" => p
                    .tags()
                    .iter()
                    .map(|t| t.name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
        }
        String::new()
    };
    let mut status = g(submitted, post, "status");
    if status.is_empty() {
        status = "draft".to_string();
    }
    json!({
        "title": g(submitted, post, "title"),
        "slug": g(submitted, post, "slug"),
        "summary": g(submitted, post, "summary"),
        "content": g(submitted, post, "content"),
        "status": status,
        "featured_image": g(submitted, post, "featured_image"),
        "publish_at": g(submitted, post, "publish_at"),
        "category": g(submitted, post, "category"),
        "tags": g(submitted, post, "tags"),
    })
}

fn post_input(form: &PostForm) -> Result<services::posts::PostInput, String> {
    let title = form.title.trim().to_string();
    if title.is_empty() {
        return Err("Title must not be empty.".into());
    }
    let status = PostStatus::parse(form.status.trim()).ok_or("Invalid status.")?;
    let publish_at = match form.publish_at.trim() {
        "" => None,
        s => time::parse_datetime_local(s)
            .map(Some)
            .ok_or("Invalid publish date (expected YYYY-MM-DD HH:MM).")?,
    };
    let slug = form.slug.trim();
    let category = form.category.trim();
    let featured = form.featured_image.trim();
    let tags: Vec<String> = form
        .tags
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    Ok(services::posts::PostInput {
        title,
        slug: if slug.is_empty() {
            None
        } else {
            Some(slug.to_string())
        },
        summary: form.summary.trim().to_string(),
        content_md: form.content.clone(),
        status,
        featured_image: if featured.is_empty() {
            None
        } else {
            Some(featured.to_string())
        },
        publish_at,
        category: if category.is_empty() {
            None
        } else {
            Some(category.to_string())
        },
        tags,
    })
}

// SSR form-render glue: a flat parameter list reads better here than a
// one-off options struct.
#[allow(clippy::too_many_arguments)]
async fn post_form_response(
    app: &App,
    auth: &AuthCtx,
    q: &PageQuery,
    error: &str,
    post: Option<&crate::models::Post>,
    submitted: Option<&PostForm>,
    is_new: bool,
    post_id: i64,
) -> AppResult<Response> {
    let mut ctx = base_ctx(app, auth, "posts", q);
    ctx.insert("form_error", &crate::i18n::tr_or(error));
    ctx.insert("form", &post_form_json(post, submitted));
    ctx.insert("is_new", &is_new);
    ctx.insert("post_id", &post_id);
    let body = templates::render_admin("post_edit.html", &ctx)?;
    Ok(Html(body).into_response())
}

async fn post_new(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    post_form_response(&app, &auth, &q, "", None, None, true, 0).await
}

async fn post_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Form(form): Form<PostForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    match post_input(&form) {
        Ok(input) => match services::posts::create_post(&app, auth.user_id, input).await {
            Ok(post) => Ok(redirect_ok(
                &format!("/admin/posts/{}/edit", post.id),
                "Post created.",
            )),
            Err(e) => {
                post_form_response(&app, &auth, &q, &e.message(), None, Some(&form), true, 0).await
            }
        },
        Err(msg) => post_form_response(&app, &auth, &q, &msg, None, Some(&form), true, 0).await,
    }
}

async fn post_edit(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let Some(mut post) = repositories::posts::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/posts", "Post not found."));
    };
    if !can_edit(&auth, post.author_id) {
        return Err(AppError::Forbidden("not allowed to edit this post".into()));
    }
    post.terms = repositories::terms::list_for_post(&app.db, id).await?;
    post_form_response(&app, &auth, &q, "", Some(&post), None, false, id).await
}

async fn post_update(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Path(id): Path<i64>,
    Form(form): Form<PostForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let Some(existing) = repositories::posts::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/posts", "Post not found."));
    };
    if !can_edit(&auth, existing.author_id) {
        return Ok(redirect_err(
            "/admin/posts",
            "You may only edit your own posts.",
        ));
    }
    let back = format!("/admin/posts/{id}/edit");
    match post_input(&form) {
        Ok(input) => match services::posts::update_post(&app, id, input).await {
            Ok(_) => Ok(redirect_ok(&back, "Post saved.")),
            Err(e) => {
                let mut post = existing;
                post.terms = repositories::terms::list_for_post(&app.db, id).await?;
                post_form_response(
                    &app,
                    &auth,
                    &q,
                    &e.message(),
                    Some(&post),
                    Some(&form),
                    false,
                    id,
                )
                .await
            }
        },
        Err(msg) => {
            let mut post = existing;
            post.terms = repositories::terms::list_for_post(&app.db, id).await?;
            post_form_response(&app, &auth, &q, &msg, Some(&post), Some(&form), false, id).await
        }
    }
}

#[derive(Deserialize)]
struct ConfirmForm {
    csrf: String,
}

async fn post_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let Some(existing) = repositories::posts::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/posts", "Post not found."));
    };
    if !can_edit(&auth, existing.author_id) {
        return Ok(redirect_err(
            "/admin/posts",
            "You may only delete your own posts.",
        ));
    }
    services::posts::delete_post(&app, id).await?;
    Ok(redirect_ok("/admin/posts", "Post deleted."))
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

async fn pages_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let pages = repositories::pages::list(&app.db, false).await?;
    let rows: Vec<_> = pages
        .iter()
        .map(|p| {
            json!({
                "id": p.id, "title": p.title, "slug": p.slug, "status": p.status.as_str(),
                "sort_order": p.sort_order,
                "updated": time::format(p.updated_at, "datetime"),
                "can_edit": can_edit(&auth, p.author_id),
            })
        })
        .collect();
    let mut ctx = base_ctx(&app, &auth, "pages", &q);
    ctx.insert("pages", &rows);
    let body = templates::render_admin("pages.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct PageForm {
    csrf: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    sort_order: i64,
}

fn page_form_json(
    page: Option<&crate::models::Page>,
    submitted: Option<&PageForm>,
) -> serde_json::Value {
    let empty = PageForm {
        csrf: String::new(),
        title: String::new(),
        slug: String::new(),
        summary: String::new(),
        content: String::new(),
        status: "draft".into(),
        sort_order: 0,
    };
    let f = submitted.unwrap_or(&empty);
    json!({
        "title": if submitted.is_some() { f.title.clone() } else { page.map(|p| p.title.clone()).unwrap_or_default() },
        "slug": if submitted.is_some() { f.slug.clone() } else { page.map(|p| p.slug.clone()).unwrap_or_default() },
        "summary": if submitted.is_some() { f.summary.clone() } else { page.map(|p| p.summary.clone()).unwrap_or_default() },
        "content": if submitted.is_some() { f.content.clone() } else { page.map(|p| p.content_md.clone()).unwrap_or_default() },
        "status": if submitted.is_some() {
            if f.status.is_empty() { "draft".to_string() } else { f.status.clone() }
        } else {
            page.map(|p| p.status.as_str().to_string()).unwrap_or_else(|| "draft".into())
        },
        "sort_order": if submitted.is_some() { f.sort_order } else { page.map(|p| p.sort_order).unwrap_or(0) },
    })
}

fn page_input(form: &PageForm) -> Result<services::posts::PageInput, String> {
    let title = form.title.trim().to_string();
    if title.is_empty() {
        return Err("Title must not be empty.".into());
    }
    let status = PostStatus::parse(form.status.trim()).ok_or("Invalid status.")?;
    let slug = form.slug.trim();
    Ok(services::posts::PageInput {
        title,
        slug: if slug.is_empty() {
            None
        } else {
            Some(slug.to_string())
        },
        summary: form.summary.trim().to_string(),
        content_md: form.content.clone(),
        status,
        sort_order: form.sort_order,
    })
}

#[allow(clippy::too_many_arguments)]
async fn page_form_response(
    app: &App,
    auth: &AuthCtx,
    q: &PageQuery,
    error: &str,
    page: Option<&crate::models::Page>,
    submitted: Option<&PageForm>,
    is_new: bool,
    page_id: i64,
) -> AppResult<Response> {
    let mut ctx = base_ctx(app, auth, "pages", q);
    ctx.insert("form_error", &crate::i18n::tr_or(error));
    ctx.insert("form", &page_form_json(page, submitted));
    ctx.insert("is_new", &is_new);
    ctx.insert("page_id", &page_id);
    let body = templates::render_admin("page_edit.html", &ctx)?;
    Ok(Html(body).into_response())
}

async fn page_new(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    page_form_response(&app, &auth, &q, "", None, None, true, 0).await
}

async fn page_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Form(form): Form<PageForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    match page_input(&form) {
        Ok(input) => match services::posts::create_page(&app, auth.user_id, input).await {
            Ok(page) => Ok(redirect_ok(
                &format!("/admin/pages/{}/edit", page.id),
                "Page created.",
            )),
            Err(e) => {
                page_form_response(&app, &auth, &q, &e.message(), None, Some(&form), true, 0).await
            }
        },
        Err(msg) => page_form_response(&app, &auth, &q, &msg, None, Some(&form), true, 0).await,
    }
}

async fn page_edit(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let Some(page) = repositories::pages::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/pages", "Page not found."));
    };
    if !can_edit(&auth, page.author_id) {
        return Err(AppError::Forbidden("not allowed to edit this page".into()));
    }
    page_form_response(&app, &auth, &q, "", Some(&page), None, false, id).await
}

async fn page_update(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Path(id): Path<i64>,
    Form(form): Form<PageForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let Some(existing) = repositories::pages::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/pages", "Page not found."));
    };
    if !can_edit(&auth, existing.author_id) {
        return Ok(redirect_err(
            "/admin/pages",
            "You may only edit your own pages.",
        ));
    }
    let back = format!("/admin/pages/{id}/edit");
    match page_input(&form) {
        Ok(input) => match services::posts::update_page(&app, id, input).await {
            Ok(_) => Ok(redirect_ok(&back, "Page saved.")),
            Err(e) => {
                page_form_response(
                    &app,
                    &auth,
                    &q,
                    &e.message(),
                    Some(&existing),
                    Some(&form),
                    false,
                    id,
                )
                .await
            }
        },
        Err(msg) => {
            page_form_response(
                &app,
                &auth,
                &q,
                &msg,
                Some(&existing),
                Some(&form),
                false,
                id,
            )
            .await
        }
    }
}

async fn page_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let Some(existing) = repositories::pages::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/pages", "Page not found."));
    };
    if !can_edit(&auth, existing.author_id) {
        return Ok(redirect_err(
            "/admin/pages",
            "You may only delete your own pages.",
        ));
    }
    services::posts::delete_page(&app, id).await?;
    Ok(redirect_ok("/admin/pages", "Page deleted."))
}

// ---------------------------------------------------------------------------
// Terms (categories & tags)
// ---------------------------------------------------------------------------

async fn terms_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let cats = repositories::terms::list_with_counts(&app.db, TermKind::Category).await?;
    let tags = repositories::terms::list_with_counts(&app.db, TermKind::Tag).await?;
    let to_json = |t: &crate::models::Term| json!({ "id": t.id, "name": t.name, "slug": t.slug, "count": t.count.unwrap_or(0) });
    let mut ctx = base_ctx(&app, &auth, "terms", &q);
    ctx.insert("categories", &cats.iter().map(to_json).collect::<Vec<_>>());
    ctx.insert("tags", &tags.iter().map(to_json).collect::<Vec<_>>());
    ctx.insert("new_kind", q.status.as_deref().unwrap_or("category"));
    let body = templates::render_admin("terms.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct TermForm {
    csrf: String,
    kind: String,
    name: String,
}

async fn term_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<TermForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Editor) {
        return Ok(redirect_err("/admin/terms", &e.message()));
    }
    let name = form.name.trim();
    let slug = crate::utils::slug::slugify(name);
    if name.is_empty() || slug.is_empty() {
        return Ok(redirect_err("/admin/terms", "Name must not be empty."));
    }
    let kind = match TermKind::parse(form.kind.trim()) {
        Some(k) => k,
        None => {
            return Ok(redirect_err(
                "/admin/terms",
                "Kind must be category or tag.",
            ));
        }
    };
    repositories::terms::ensure(&app.db, kind, name, &slug).await?;
    Ok(redirect_ok("/admin/terms", "Term created."))
}

async fn term_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Editor) {
        return Ok(redirect_err("/admin/terms", &e.message()));
    }
    if !repositories::terms::delete(&app.db, id).await? {
        return Ok(redirect_err("/admin/terms", "Term not found."));
    }
    app.invalidate_content().await;
    Ok(redirect_ok("/admin/terms", "Term deleted."))
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

async fn comments_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Editor) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let status = q.status.as_deref().and_then(CommentStatus::parse);
    let page = q.page.unwrap_or(1).max(1);
    let per: i64 = 20;
    let (comments, total) = repositories::comments::list(&app.db, status, page, per).await?;
    let rows: Vec<_> = comments
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "author": c.author_name,
                "email": c.author_email,
                "content": c.content,
                "status": c.status.as_str(),
                "created": time::format(c.created_at, "datetime"),
                "post_title": c.post_title.clone().unwrap_or_default(),
                "post_slug": c.post_slug.clone().unwrap_or_default(),
            })
        })
        .collect();
    let counts = repositories::comments::counts_by_status(&app.db).await?;
    let mut ctx = base_ctx(&app, &auth, "comments", &q);
    ctx.insert("comments", &rows);
    ctx.insert("current_page", &page);
    ctx.insert("pages", &((total + per - 1) / per));
    ctx.insert("status_filter", q.status.as_deref().unwrap_or(""));
    ctx.insert("count_pending", &counts.pending);
    ctx.insert("count_approved", &counts.approved);
    ctx.insert("count_spam", &counts.spam);
    let body = templates::render_admin("comments.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct CommentStatusForm {
    csrf: String,
    status: String,
}

async fn comment_status(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<CommentStatusForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Editor) {
        return Ok(redirect_err("/admin/comments", &e.message()));
    }
    let Some(status) = CommentStatus::parse(form.status.trim()) else {
        return Ok(redirect_err("/admin/comments", "Invalid status."));
    };
    match services::comments::moderate(&app, id, status).await {
        Ok(()) => Ok(redirect_ok("/admin/comments", "Comment updated.")),
        Err(e) => Ok(redirect_err("/admin/comments", &e.message())),
    }
}

async fn comment_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Editor) {
        return Ok(redirect_err("/admin/comments", &e.message()));
    }
    services::comments::delete(&app, id).await?;
    Ok(redirect_ok("/admin/comments", "Comment deleted."))
}

// ---------------------------------------------------------------------------
// Users (admin only)
// ---------------------------------------------------------------------------

async fn users_list(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let users = repositories::users::list(&app.db).await?;
    let rows: Vec<_> = users
        .iter()
        .map(|u| {
            json!({
                "id": u.id, "username": u.username, "email": u.email,
                "role": u.role.as_str(), "display_name": u.display(),
                "created": time::format(u.created_at, "date"),
                "is_self": u.id == auth.user_id,
            })
        })
        .collect();
    let mut ctx = base_ctx(&app, &auth, "users", &q);
    ctx.insert("users", &rows);
    let body = templates::render_admin("users.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct UserForm {
    csrf: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    display_name: String,
}

fn user_form_json(
    submitted: Option<&UserForm>,
    user: Option<&crate::models::User>,
) -> serde_json::Value {
    let g = |k: &str| -> String {
        if let Some(f) = submitted {
            return match k {
                "username" => f.username.clone(),
                "email" => f.email.clone(),
                "password" => f.password.clone(),
                "role" => f.role.clone(),
                "display_name" => f.display_name.clone(),
                _ => String::new(),
            };
        }
        if let Some(u) = user {
            return match k {
                "username" => u.username.clone(),
                "email" => u.email.clone(),
                "password" => String::new(),
                "role" => u.role.as_str().to_string(),
                "display_name" => u.display_name.clone(),
                _ => String::new(),
            };
        }
        String::new()
    };
    let mut role = g("role");
    if role.is_empty() {
        role = "author".to_string();
    }
    json!({
        "username": g("username"),
        "email": g("email"),
        "password": g("password"),
        "role": role,
        "display_name": g("display_name"),
    })
}

#[allow(clippy::too_many_arguments)]
async fn user_form_response(
    app: &App,
    auth: &AuthCtx,
    q: &PageQuery,
    error: &str,
    user: Option<&crate::models::User>,
    submitted: Option<&UserForm>,
    is_new: bool,
    user_id: i64,
) -> AppResult<Response> {
    let mut ctx = base_ctx(app, auth, "users", q);
    ctx.insert("form_error", &crate::i18n::tr_or(error));
    ctx.insert("form", &user_form_json(submitted, user));
    ctx.insert("is_new", &is_new);
    ctx.insert("user_id", &user_id);
    let body = templates::render_admin("user_edit.html", &ctx)?;
    Ok(Html(body).into_response())
}

async fn user_new(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    user_form_response(&app, &auth, &q, "", None, None, true, 0).await
}

async fn user_create(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Form(form): Form<UserForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let role = Role::parse(form.role.trim()).unwrap_or(Role::Author);
    match services::users::create_user(
        &app,
        form.username.trim(),
        form.email.trim(),
        form.password.trim(),
        role,
    )
    .await
    {
        Ok(_) => Ok(redirect_ok("/admin/users", "User created.")),
        Err(e) => {
            user_form_response(&app, &auth, &q, &e.message(), None, Some(&form), true, 0).await
        }
    }
}

async fn user_edit(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let Some(user) = repositories::users::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/users", "User not found."));
    };
    user_form_response(&app, &auth, &q, "", Some(&user), None, false, id).await
}

async fn user_update(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<UserForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let Some(target) = repositories::users::find_by_id(&app.db, id).await? else {
        return Ok(redirect_err("/admin/users", "User not found."));
    };
    // An invalid or missing role must fail loudly — never silently demote.
    let Some(new_role) = Role::parse(form.role.trim()) else {
        return Ok(redirect_err(
            &format!("/admin/users/{id}/edit"),
            "Invalid role.",
        ));
    };
    // Guard: never demote the last admin.
    if target.role == Role::Admin && new_role != Role::Admin {
        let admins = repositories::users::list(&app.db)
            .await?
            .into_iter()
            .filter(|u| u.role == Role::Admin)
            .count();
        if admins <= 1 {
            return Ok(redirect_err(
                "/admin/users",
                "Cannot demote the last admin.",
            ));
        }
    }
    repositories::users::update_profile(
        &app.db,
        id,
        form.email.trim(),
        form.display_name.trim(),
        &target.bio,
    )
    .await?;
    repositories::users::update_role(&app.db, id, new_role).await?;
    app.invalidate_content().await;
    Ok(redirect_ok(
        &format!("/admin/users/{id}/edit"),
        "User saved.",
    ))
}

async fn user_delete(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let Some(acting) = repositories::users::find_by_id(&app.db, auth.user_id).await? else {
        return Ok(redirect_err("/admin/users", "Account not found."));
    };
    match services::users::delete_user(&app, &acting, id).await {
        Ok(()) => Ok(redirect_ok("/admin/users", "User deleted.")),
        Err(e) => Ok(redirect_err("/admin/users", &e.message())),
    }
}

// ---------------------------------------------------------------------------
// Themes (admin only)
// ---------------------------------------------------------------------------

async fn themes_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let themes = services::extensions::status(&app, crate::extension::ExtensionKind::Theme).await?;
    let current = app.theme.current();
    let mut ctx = base_ctx(&app, &auth, "themes", &q);
    ctx.insert("themes", &themes);
    ctx.insert("current_theme", current.display_name());
    ctx.insert("theme_is_fallback", &current.is_fallback);
    let body = templates::render_admin("themes.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct ThemeActivateForm {
    csrf: String,
    name: String,
}

async fn theme_activate(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<ThemeActivateForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    match app.set_active_theme(form.name.trim()).await {
        Ok(()) => Ok(redirect_ok("/admin/themes", "Theme activated.")),
        Err(e) => Ok(redirect_err("/admin/themes", &e.message())),
    }
}

// ---------------------------------------------------------------------------
// Plugins (admin only)
// ---------------------------------------------------------------------------

async fn plugins_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let plugins =
        services::extensions::status(&app, crate::extension::ExtensionKind::Plugin).await?;
    let mut ctx = base_ctx(&app, &auth, "plugins", &q);
    ctx.insert("plugins", &plugins);
    let body = templates::render_admin("plugins.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct PluginToggleForm {
    csrf: String,
    name: String,
    action: String,
}

async fn plugin_toggle(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<PluginToggleForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let name = form.name.trim().to_string();
    let known = app.plugins.list().iter().any(|(dir, _, _)| *dir == name);
    if !known {
        return Ok(redirect_err("/admin/plugins", "Plugin not found."));
    }
    let mut names = app.settings.plugins_enabled();
    match form.action.trim() {
        "enable" => {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        "disable" => names.retain(|n| n != &name),
        _ => return Ok(redirect_err("/admin/plugins", "Invalid action.")),
    }
    app.set_plugins_enabled(&names).await?;
    Ok(redirect_ok("/admin/plugins", "Plugin updated."))
}

// ---------------------------------------------------------------------------
// Schema-generated settings pages (themes & plugins, admin only)
// ---------------------------------------------------------------------------

/// One form field as JSON for `config_form.html`.
fn config_field_json(
    entry: &crate::config_store::NamespaceEntry,
    f: &crate::config_schema::FieldDef,
) -> serde_json::Value {
    use crate::config_schema::{ConfigValue, FieldType};

    let current = entry.effective.get(&f.key);
    let secret = f.sensitive();
    // Secrets never echo back; the form shows a placeholder instead.
    let value = match current {
        Some(ConfigValue::Array(_)) => {
            let s = current.map(|v| v.to_store()).unwrap_or_default();
            // pretty-print arrays for editing (compact storage form is valid too)
            serde_json::from_str::<serde_json::Value>(&s)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or(s)
        }
        Some(v) => v.to_cmp_string(),
        None => String::new(),
    };
    let display = if secret { String::new() } else { value };
    let options: Vec<_> = f
        .options
        .iter()
        .map(|(v, l)| {
            let selected = match current {
                Some(ConfigValue::Str(s)) => {
                    if f.ty == FieldType::Multiselect {
                        s.split(',').map(str::trim).any(|p| p == v)
                    } else {
                        s == v
                    }
                }
                _ => false,
            };
            json!({ "value": v, "label": l, "selected": selected })
        })
        .collect();
    json!({
        "key": f.key,
        "label": f.label,
        "description": f.description,
        "type": f.ty.as_str(),
        "input": f.ty.input_kind(),
        "value": display,
        "has_value": current.is_some(),
        "is_secret": secret,
        "required": f.required,
        "restart": f.restart,
        "permission": f.permission.as_str(),
        "visible": entry.show_if_visible(&f.key),
        "checked": matches!(current, Some(ConfigValue::Bool(true))),
        "options": options,
        "min": f.min,
        "max": f.max,
        "item_fields": f.item.iter().map(|i| i.key.clone()).collect::<Vec<_>>(),
    })
}

/// The whole settings form (groups → fields) as JSON for `config_form.html`.
fn config_form_json(entry: &crate::config_store::NamespaceEntry) -> serde_json::Value {
    let groups: Vec<_> = entry
        .schema
        .display_groups()
        .into_iter()
        .map(|(id, label, fields)| {
            json!({
                "id": id,
                "label": label,
                "fields": fields.iter().map(|f| config_field_json(entry, f)).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "groups": groups })
}

/// Merge raw form pairs (order-preserving) into the input map for
/// `ConfigManager::save`. Checkbox pairs arrive as `key=false` followed by
/// `key=true` when checked (last wins); multiselect values are joined with
/// commas; `__multi_<key>` markers let an emptied multiselect clear its value.
fn aggregate_config_form(
    entry: &crate::config_store::NamespaceEntry,
    pairs: &[(String, String)],
) -> HashMap<String, String> {
    use crate::config_schema::FieldType;

    let mut out: HashMap<String, String> = HashMap::new();
    let mut multi_present: Vec<String> = Vec::new();
    for (k, v) in pairs {
        if k == "csrf" {
            continue;
        }
        if let Some(key) = k.strip_prefix("__multi_") {
            multi_present.push(key.to_string());
            continue;
        }
        let is_multi = entry
            .schema
            .field(k)
            .map(|f| f.ty == FieldType::Multiselect)
            .unwrap_or(false);
        let merged = if is_multi {
            match out.get(k) {
                Some(prev) if !prev.is_empty() => format!("{prev},{v}"),
                _ => v.clone(),
            }
        } else {
            v.clone()
        };
        out.insert(k.clone(), merged);
    }
    for key in multi_present {
        out.entry(key).or_default();
    }
    out
}

/// CSRF token from a dynamic (untyped) form body.
fn csrf_from_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .find(|(k, _)| k == "csrf")
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

async fn theme_settings(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(name): Path<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let known = app.theme.list().iter().any(|(dir, _, _)| *dir == name);
    if !known {
        return Ok(redirect_err("/admin/themes", "Theme not found."));
    }
    let entry = match app
        .configs
        .load_theme(std::path::Path::new(&app.config.theme.dir), &name)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            return Ok(redirect_err(
                "/admin/themes",
                &crate::i18n::tr("admin.flash.invalid_config_schema", &[("error", &e)]),
            ));
        }
    };
    if entry.schema.fields.is_empty() {
        return Ok(redirect_err(
            "/admin/themes",
            "This theme declares no settings.",
        ));
    }
    let mut ctx = base_ctx(&app, &auth, "themes", &q);
    let title = format!("Theme settings — {name}");
    ctx.insert("cfg_title", &title);
    ctx.insert("back_url", "/admin/themes");
    ctx.insert("form", &config_form_json(&entry));
    let body = templates::render_admin("config_form.html", &ctx)?;
    Ok(Html(body).into_response())
}

async fn theme_settings_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &csrf_from_pairs(&pairs)) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let back = format!("/admin/themes/{name}/settings");
    let known = app.theme.list().iter().any(|(dir, _, _)| *dir == name);
    if !known {
        return Ok(redirect_err("/admin/themes", "Theme not found."));
    }
    // Reload from disk first so aggregation sees the current schema.
    let entry = app
        .configs
        .load_theme(std::path::Path::new(&app.config.theme.dir), &name)
        .await
        .map_err(AppError::BadRequest)?;
    let input = aggregate_config_form(&entry, &pairs);
    match app
        .save_theme_config(&name, &input, crate::config_schema::Permission::Admin)
        .await
    {
        Ok(out) if out.restart_required => Ok(redirect_ok(
            &back,
            "Saved. Some changes take effect after a restart.",
        )),
        Ok(_) => Ok(redirect_ok(&back, "Settings saved.")),
        Err(e) => Ok(redirect_err(&back, &e)),
    }
}

async fn plugin_settings(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(name): Path<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let known = app.plugins.list().iter().any(|(dir, _, _)| *dir == name);
    if !known {
        return Ok(redirect_err("/admin/plugins", "Plugin not found."));
    }
    let entry = match app
        .configs
        .load_plugin(std::path::Path::new(&app.config.plugin.dir), &name)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            return Ok(redirect_err(
                "/admin/plugins",
                &crate::i18n::tr("admin.flash.invalid_config_schema", &[("error", &e)]),
            ));
        }
    };
    if entry.schema.fields.is_empty() {
        return Ok(redirect_err(
            "/admin/plugins",
            "This plugin declares no settings.",
        ));
    }
    let mut ctx = base_ctx(&app, &auth, "plugins", &q);
    let title = format!("Plugin settings — {name}");
    ctx.insert("cfg_title", &title);
    ctx.insert("back_url", "/admin/plugins");
    ctx.insert("form", &config_form_json(&entry));
    let body = templates::render_admin("config_form.html", &ctx)?;
    Ok(Html(body).into_response())
}

async fn plugin_settings_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &csrf_from_pairs(&pairs)) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let back = format!("/admin/plugins/settings/{name}");
    let known = app.plugins.list().iter().any(|(dir, _, _)| *dir == name);
    if !known {
        return Ok(redirect_err("/admin/plugins", "Plugin not found."));
    }
    let entry = app
        .configs
        .load_plugin(std::path::Path::new(&app.config.plugin.dir), &name)
        .await
        .map_err(AppError::BadRequest)?;
    let input = aggregate_config_form(&entry, &pairs);
    match app
        .save_plugin_config(&name, &input, crate::config_schema::Permission::Admin)
        .await
    {
        Ok(out) if out.restart_required => Ok(redirect_ok(
            &back,
            "Saved. Some changes take effect after a restart.",
        )),
        Ok(_) => Ok(redirect_ok(&back, "Settings saved.")),
        Err(e) => Ok(redirect_err(&back, &e)),
    }
}

// ---------------------------------------------------------------------------
// Settings (admin only)
// ---------------------------------------------------------------------------

async fn navigation_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    // System-provided entries that the active theme renders in the public top
    // bar ahead of the custom links: the hardcoded Home link, published
    // pages, plugin-contributed items and the theme-controlled RSS link.
    // Shown read-only — they are managed by their own admin sections.
    let mut system_nav = vec![json!({
        "label": "Home",
        "url": "/",
        "source": "system",
    })];
    if let Ok(pages) = repositories::pages::list(&app.db, true).await {
        for page in pages {
            system_nav.push(json!({
                "label": page.title,
                "url": format!("/{}", page.slug),
                "source": "page",
            }));
        }
    }
    for (label, url) in app.plugins.nav_items() {
        system_nav.push(json!({
            "label": label,
            "url": url,
            "source": "plugin",
        }));
    }
    if app
        .theme_config_json()
        .get("show_rss_link")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        system_nav.push(json!({
            "label": "RSS",
            "url": "/rss.xml",
            "source": "theme",
        }));
    }
    // Merge the saved overrides (hidden / label) into the system entries so
    // the editor renders their current state.
    let sys_overrides: serde_json::Map<String, serde_json::Value> = app
        .settings
        .get("site.navigation.system")
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    for item in system_nav.iter_mut() {
        let url = item["url"].as_str().unwrap_or_default().to_owned();
        let ov = sys_overrides.get(&url).cloned().unwrap_or_default();
        item["hidden"] = ov
            .get("hidden")
            .cloned()
            .unwrap_or(json!(false));
        item["label_override"] = ov.get("label").cloned().unwrap_or(json!(""));
    }
    let rows: Vec<serde_json::Value> = app
        .settings
        .get("site.navigation")
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mut ctx = base_ctx(&app, &auth, "navigation", &q);
    ctx.insert("nav_rows", &rows);
    ctx.insert("system_nav", &system_nav);
    let body = templates::render_admin("navigation.html", &ctx)?;
    Ok(Html(body).into_response())
}

async fn navigation_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &csrf_from_pairs(&pairs)) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }

    let labels: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "label")
        .map(|(_, value)| value.trim())
        .collect();
    let urls: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "url")
        .map(|(_, value)| value.trim())
        .collect();

    // System navigation overrides: per-entry visibility (`sys_enabled`
    // checkboxes carry the visible URLs) and label renames. Entries left at
    // their defaults are not stored.
    let sys_urls: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "sys_url")
        .map(|(_, value)| value.trim())
        .collect();
    let sys_labels: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "sys_label")
        .map(|(_, value)| value.trim())
        .collect();
    let sys_visible: std::collections::HashSet<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "sys_enabled")
        .map(|(_, value)| value.trim())
        .collect();
    let mut sys_overrides = serde_json::Map::new();
    for (index, url) in sys_urls.iter().enumerate() {
        if url.is_empty() || url.len() > 500 {
            continue;
        }
        let label = sys_labels.get(index).copied().unwrap_or("");
        if label.chars().count() > 80 {
            return Ok(redirect_err(
                "/admin/navigation",
                &crate::i18n::tr("admin.flash.nav_label_too_long", &[("url", url)]),
            ));
        }
        let hidden = !sys_visible.contains(url);
        if !hidden && label.is_empty() {
            continue;
        }
        let mut entry = serde_json::Map::new();
        entry.insert("hidden".to_owned(), json!(hidden));
        if !label.is_empty() {
            entry.insert("label".to_owned(), json!(label));
        }
        sys_overrides.insert(url.to_string(), json!(entry));
    }
    let mut links = Vec::new();
    for index in 0..labels.len().max(urls.len()) {
        let label = labels.get(index).copied().unwrap_or("");
        let url = urls.get(index).copied().unwrap_or("");
        if label.is_empty() && url.is_empty() {
            continue;
        }
        if label.is_empty() || url.is_empty() {
            let n = (index + 1).to_string();
            return Ok(redirect_err(
                "/admin/navigation",
                &crate::i18n::tr("admin.flash.nav_item_incomplete", &[("n", &n)]),
            ));
        }
        if label.chars().count() > 80 || url.len() > 500 {
            let n = (index + 1).to_string();
            return Ok(redirect_err(
                "/admin/navigation",
                &crate::i18n::tr("admin.flash.nav_item_too_long", &[("n", &n)]),
            ));
        }
        let relative = url.starts_with('/') && !url.starts_with("//");
        let absolute = url.starts_with("http://") || url.starts_with("https://");
        if !crate::markdown::url_is_safe(url) || (!relative && !absolute) {
            let n = (index + 1).to_string();
            return Ok(redirect_err(
                "/admin/navigation",
                &crate::i18n::tr("admin.flash.nav_item_invalid_url", &[("n", &n)]),
            ));
        }
        links.push(json!({ "label": label, "url": url }));
    }
    let value = serde_json::to_string(&links)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("navigation serialization failed: {e}")))?;
    app.settings.set(&app.db, "site.navigation", &value).await?;
    let sys_value = serde_json::to_string(&sys_overrides).map_err(|e| {
        AppError::Internal(anyhow::anyhow!("navigation overrides serialization failed: {e}"))
    })?;
    app.settings
        .set(&app.db, "site.navigation.system", &sys_value)
        .await?;
    app.invalidate_content().await;
    Ok(redirect_ok("/admin/navigation", "Navigation saved."))
}

async fn settings_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let mut ctx = base_ctx(&app, &auth, "settings", &q);
    ctx.insert("site_title", &app.site_title());
    ctx.insert("site_description", &app.site_description());
    ctx.insert("site_base_url", &app.base_url());
    ctx.insert("posts_per_page", &app.posts_per_page());
    ctx.insert("comments_enabled", &app.comments_enabled());
    ctx.insert("comments_moderate", &app.comments_moderate());
    ctx.insert("site_locale", &crate::i18n::locale());
    let body = templates::render_admin("settings.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct SettingsForm {
    csrf: String,
    #[serde(default)]
    site_title: String,
    #[serde(default)]
    site_description: String,
    #[serde(default)]
    site_base_url: String,
    #[serde(default)]
    posts_per_page: String,
    #[serde(default)]
    comments_enabled: String,
    #[serde(default)]
    comments_moderate: String,
    #[serde(default)]
    site_locale: String,
}

async fn settings_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<SettingsForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    if let Err(e) = require_role(&auth, Role::Admin) {
        return Ok(redirect_err("/admin", &e.message()));
    }
    let per_page: usize = match form.posts_per_page.trim().parse::<usize>() {
        Ok(n) if (1..=100).contains(&n) => n,
        _ => {
            return Ok(redirect_err(
                "/admin/settings",
                "Posts per page must be 1-100.",
            ));
        }
    };
    let base_url = form.site_base_url.trim().trim_end_matches('/').to_string();
    if !base_url.is_empty() && !base_url.starts_with("http://") && !base_url.starts_with("https://")
    {
        return Ok(redirect_err(
            "/admin/settings",
            "Base URL must start with http:// or https://.",
        ));
    }
    let values: HashMap<String, String> = [
        ("site.title".to_string(), form.site_title.trim().to_string()),
        (
            "site.description".to_string(),
            form.site_description.trim().to_string(),
        ),
        ("site.base_url".to_string(), base_url),
        ("site.posts_per_page".to_string(), per_page.to_string()),
        (
            "comments.enabled".to_string(),
            (form.comments_enabled == "on").to_string(),
        ),
        (
            "comments.moderate".to_string(),
            (form.comments_moderate == "on").to_string(),
        ),
        ("site.locale".to_string(), crate::i18n::normalize(&form.site_locale)),
    ]
    .into_iter()
    .collect();
    app.settings.set_many(&app.db, &values).await?;
    crate::i18n::set_locale(&form.site_locale);
    app.invalidate_content().await;
    Ok(redirect_ok("/admin/settings", "Settings saved."))
}

// ---------------------------------------------------------------------------
// Profile (any logged-in user)
// ---------------------------------------------------------------------------

async fn profile_page(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    let Some(user) = repositories::users::find_by_id(&app.db, auth.user_id).await? else {
        return Ok(redirect_err("/admin/login", "Account not found."));
    };
    let mut ctx = base_ctx(&app, &auth, "profile", &q);
    ctx.insert("p_username", &user.username);
    ctx.insert("p_display_name", &user.display_name);
    ctx.insert("p_email", &user.email);
    ctx.insert("p_bio", &user.bio);
    let body = templates::render_admin("profile.html", &ctx)?;
    Ok(Html(body).into_response())
}

#[derive(Deserialize)]
struct ProfileForm {
    csrf: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    bio: String,
}

async fn profile_save(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<ProfileForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    repositories::users::update_profile(
        &app.db,
        auth.user_id,
        form.email.trim(),
        form.display_name.trim(),
        form.bio.trim(),
    )
    .await?;
    app.invalidate_content().await;
    Ok(redirect_ok("/admin/profile", "Profile saved."))
}

#[derive(Deserialize)]
struct PasswordForm {
    csrf: String,
    #[serde(default)]
    current: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm: String,
}

async fn profile_password(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Form(form): Form<PasswordForm>,
) -> AppResult<Response> {
    if let Err(e) = auth::ensure_csrf(&auth, &form.csrf) {
        return Ok(e.into_response());
    }
    let Some(user) = repositories::users::find_by_id(&app.db, auth.user_id).await? else {
        return Ok(redirect_err("/admin/profile", "Account not found."));
    };
    if !auth::verify_password(&user.password_hash, &form.current) {
        return Ok(redirect_err("/admin/profile", "Current password is wrong."));
    }
    if form.password != form.confirm {
        return Ok(redirect_err(
            "/admin/profile",
            "New passwords do not match.",
        ));
    }
    match services::users::change_password(&app, auth.user_id, form.password.trim()).await {
        Ok(()) => {
            // change_password invalidates all sessions — back to login.
            Ok(redirect_ok(
                "/admin/login",
                "Password changed. Please sign in again.",
            ))
        }
        Err(e) => Ok(redirect_err("/admin/profile", &e.message())),
    }
}

// ---------------------------------------------------------------------------
// Plugin-provided admin pages
// ---------------------------------------------------------------------------

async fn plugin_admin_route(
    State(app): State<App>,
    Extension(auth): Extension<AuthCtx>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let full = format!("/admin/plugins/{path}");
    let mut qmap = rhai::Map::new();
    for (k, v) in &params {
        qmap.insert(k.as_str().into(), rhai::Dynamic::from(v.clone()));
    }
    match app.plugins.admin_route(&full, &qmap, &auth) {
        Some(r) => crate::http::plugins_http::route_response(r),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Html(format!(
                "<h1>404</h1><p>{}</p>",
                crate::i18n::tr_or("Plugin page not found.")
            )),
        )
            .into_response(),
    }
}
