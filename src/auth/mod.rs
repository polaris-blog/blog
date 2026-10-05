use std::collections::HashMap;
use std::sync::RwLock;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::models::Role;
use crate::state::App;
use crate::utils::{cookies, time};

pub const SESSION_COOKIE: &str = "polaris_session";
pub const LOGIN_CSRF_COOKIE: &str = "polaris_csrf";
const MAX_ATTEMPTS: i64 = 5;
const LOCK_WINDOW_SECS: i64 = 15 * 60;

// ---------------------------------------------------------------------------
// Password hashing (Argon2id)
// ---------------------------------------------------------------------------

pub fn hash_password(password: &str) -> AppResult<String> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hash failed: {e}")))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ---------------------------------------------------------------------------
// Sessions (in-memory, secure random tokens)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: i64,
    pub csrf: String,
    pub expires_at: i64,
}

#[derive(Default)]
pub struct SessionStore {
    sessions: RwLock<HashMap<String, Session>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&self, user_id: i64, ttl_secs: i64) -> String {
        let token = cookies::random_token(32);
        let session = Session {
            user_id,
            csrf: cookies::random_token(32),
            expires_at: time::now() + ttl_secs,
        };
        let mut map = crate::utils::lock::write(&self.sessions);
        // Opportunistic cleanup so the map cannot grow unbounded.
        if map.len() > 10_000 {
            let now = time::now();
            map.retain(|_, s| s.expires_at > now);
        }
        map.insert(token.clone(), session);
        token
    }

    pub fn get(&self, token: &str) -> Option<Session> {
        let s = crate::utils::lock::read(&self.sessions)
            .get(token)
            .cloned()?;
        if s.expires_at <= time::now() {
            self.remove(token);
            return None;
        }
        Some(s)
    }

    pub fn remove(&self, token: &str) {
        crate::utils::lock::write(&self.sessions).remove(token);
    }

    pub fn remove_user(&self, user_id: i64) {
        crate::utils::lock::write(&self.sessions).retain(|_, s| s.user_id != user_id);
    }
}

// ---------------------------------------------------------------------------
// Brute-force protection
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct LoginLimiter {
    attempts: RwLock<HashMap<String, Vec<i64>>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_locked(&self, key: &str) -> bool {
        let now = time::now();
        crate::utils::lock::read(&self.attempts)
            .get(key)
            .map(|v| {
                v.iter().filter(|t| now - **t < LOCK_WINDOW_SECS).count() as i64 >= MAX_ATTEMPTS
            })
            .unwrap_or(false)
    }

    pub fn record_failure(&self, key: &str) {
        let now = time::now();
        let mut map = crate::utils::lock::write(&self.attempts);
        if map.len() > 10_000 {
            // Drop the entries that can no longer lock anything out, then
            // only clear as a last resort. Clearing wholesale would let an
            // attacker reset everyone's counter simply by spraying 10k
            // distinct keys at the login form.
            map.retain(|_, v| v.iter().any(|t| now - *t < LOCK_WINDOW_SECS));
            if map.len() > 10_000 {
                map.clear();
            }
        }
        let v = map.entry(key.to_string()).or_default();
        v.retain(|t| now - *t < LOCK_WINDOW_SECS);
        v.push(now);
    }

    pub fn reset(&self, key: &str) {
        crate::utils::lock::write(&self.attempts).remove(key);
    }
}

// ---------------------------------------------------------------------------
// Request auth context
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AuthCtx {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub csrf: String,
}

async fn load_auth(app: &App, headers: &HeaderMap) -> Option<AuthCtx> {
    let token = cookies::get_cookie(headers, SESSION_COOKIE)?;
    let session = app.sessions.get(&token)?;
    let user = crate::repositories::users::find_by_id(&app.db, session.user_id)
        .await
        .ok()
        .flatten()?;
    let display_name = user.display().to_string();
    Some(AuthCtx {
        user_id: user.id,
        username: user.username,
        display_name,
        role: user.role,
        csrf: session.csrf,
    })
}

/// Middleware protecting admin pages: redirects to the login page, or to the
/// first-run setup wizard while no user account exists.
pub async fn admin_auth_mw(State(app): State<App>, mut req: Request, next: Next) -> Response {
    match load_auth(&app, req.headers()).await {
        Some(ctx) => {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        None => {
            let target = if app.needs_setup() {
                "/admin/setup"
            } else {
                "/admin/login"
            };
            Redirect::to(target).into_response()
        }
    }
}

/// Middleware protecting write APIs: returns 401 JSON.
pub async fn api_auth_mw(State(app): State<App>, mut req: Request, next: Next) -> Response {
    match load_auth(&app, req.headers()).await {
        Some(ctx) => {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": { "code": 401, "message": "authentication required" }
            })),
        )
            .into_response(),
    }
}

/// CSRF token check for logged-in (session-bound) form posts.
pub fn ensure_csrf(auth: &AuthCtx, form_token: &str) -> AppResult<()> {
    if cookies::ct_eq(&auth.csrf, form_token) {
        Ok(())
    } else {
        Err(AppError::Forbidden("invalid CSRF token".into()))
    }
}

/// Double-submit CSRF check for the login form (pre-session).
pub fn login_csrf_ok(headers: &HeaderMap, form_token: &str) -> bool {
    match cookies::get_cookie(headers, LOGIN_CSRF_COOKIE) {
        Some(cookie) => cookies::ct_eq(&cookie, form_token),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Login form
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    pub csrf: String,
    pub next: Option<String>,
}

/// Only allow internal redirect targets after login.
///
/// Rejects protocol-relative targets (`//evil.example`) and anything a
/// browser may normalize into one: a leading `/\` is folded to `//`, and
/// encoded or control-character forms are refused outright.
pub fn safe_next(next: &Option<String>) -> String {
    let Some(n) = next else {
        return "/admin".to_string();
    };
    // Internal paths only: a single leading slash, never `//`.
    if !n.starts_with('/') || n.starts_with("//") || n.starts_with("/\\") {
        return "/admin".to_string();
    }
    // No control bytes (header injection) and no backslash in the first
    // segment — browsers fold `/\` into `//`, making it protocol-relative.
    if n.bytes().any(|b| b < 0x20 || b == 0x7f)
        || n.split('/').nth(1).is_some_and(|seg| seg.contains('\\'))
    {
        return "/admin".to_string();
    }
    n.clone()
}

pub fn session_cookie(
    value: &str,
    ttl_secs: i64,
    secure: bool,
) -> (header::HeaderName, axum::http::HeaderValue) {
    (
        header::SET_COOKIE,
        cookies::set_cookie(SESSION_COOKIE, value, ttl_secs, "/", true, secure),
    )
}

pub fn clear_session_cookie(secure: bool) -> (header::HeaderName, axum::http::HeaderValue) {
    (
        header::SET_COOKIE,
        cookies::set_cookie(SESSION_COOKIE, "", 0, "/", true, secure),
    )
}

/// Effective `Secure` flag: explicit config wins, otherwise derived from the
/// configured base URL (HTTPS deployments get `Secure` cookies).
pub fn secure_cookies_for(app: &crate::state::AppState) -> bool {
    app.config.security.secure_cookies.unwrap_or_else(|| {
        app.base_url()
            .trim()
            .to_ascii_lowercase()
            .starts_with("https://")
    })
}
