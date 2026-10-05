//! HTTP dispatch for plugin-registered routes.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::state::App;

fn query_to_map(params: &HashMap<String, String>) -> rhai::Map {
    let mut map = rhai::Map::new();
    for (k, v) in params {
        map.insert(k.as_str().into(), rhai::Dynamic::from(v.clone()));
    }
    map
}

pub(crate) fn route_response(r: crate::plugins::RouteResult) -> Response {
    let ct = match header::HeaderValue::from_str(&r.content_type) {
        Ok(v) => v,
        Err(_) => header::HeaderValue::from_static("text/plain; charset=utf-8"),
    };
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::OK);
    (status, [(header::CONTENT_TYPE, ct)], r.body).into_response()
}

/// `/plugins/{path}` — public plugin routes.
pub async fn public_route(
    State(app): State<App>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let full = format!("/plugins/{path}");
    let qmap = query_to_map(&params);
    match app.plugins.route(&full, &qmap) {
        Some(r) => route_response(r),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
