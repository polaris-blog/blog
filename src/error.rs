use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database error")]
    Db(#[from] sqlx::Error),
    #[error("template error")]
    Template(#[from] tera::Error),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Conflict(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        // Never leak internal error details to end users. Prefixes and any
        // static inner message with a translation key are localized; dynamic
        // inner text passes through `tr_or` unchanged when unlisted.
        use crate::i18n::{tr, tr_or};
        match self {
            Self::NotFound(m) => format!("{}: {}", tr("error.not_found", &[]), tr_or(m)),
            Self::Unauthorized => tr("error.auth_required", &[]),
            Self::Forbidden(m) => format!("{}: {}", tr("error.forbidden", &[]), tr_or(m)),
            Self::BadRequest(m) => format!("{}: {}", tr("error.bad_request", &[]), tr_or(m)),
            Self::PayloadTooLarge(m) => {
                format!("{}: {}", tr("error.too_large", &[]), tr_or(m))
            }
            Self::Conflict(m) => format!("{}: {}", tr("error.conflict", &[]), tr_or(m)),
            _ => tr("error.internal", &[]),
        }
    }
}

const ERROR_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{status} — Polaris</title>
<style>body{font-family:system-ui,sans-serif;display:flex;min-height:100vh;align-items:center;justify-content:center;margin:0;color:#333;background:#fafafa}
main{text-align:center}h1{font-size:4rem;margin:0;color:#555}p{color:#777}</style></head>
<body><main><h1>{status}</h1><p>{message}</p><p><a href="/">{back_home}</a></p></main></body></html>"#;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = ERROR_PAGE
            .replace("{status}", status.as_str())
            .replace("{message}", &html_escape_min(&self.message()))
            .replace("{back_home}", &crate::i18n::tr("error.back_home", &[]));
        let mut resp = Response::new(axum::body::Body::from(body));
        *resp.status_mut() = status;
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
        );
        resp
    }
}

/// JSON error shape for the REST API.
pub struct ApiError(pub AppError);

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.0.status();
        let body = serde_json::json!({ "error": { "code": status.as_u16(), "message": self.0.message() } });
        (status, axum::Json(body)).into_response()
    }
}

fn html_escape_min(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
