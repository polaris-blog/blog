//! Search suggestions (`GET /api/search/suggest?q=ru`).
//!
//! Sources, merged and de-duplicated: popular past searches, existing
//! tag/category names, indexed titles. Results are cached briefly (the
//! `search` cache namespace) so typing cannot hammer the database.

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;

use super::query::like_prefix;

/// Collect suggestions for `prefix` (already normalized by the caller).
pub async fn suggest(db: &Db, prefix: &str, limit: usize) -> AppResult<Vec<String>> {
    let pattern = like_prefix(&prefix.to_lowercase());
    let limit = limit.clamp(1, 20) as i64;
    let mut out: Vec<String> = Vec::new();
    let push = |v: String, out: &mut Vec<String>| {
        if !out.contains(&v) && out.len() < limit as usize {
            out.push(v);
        }
    };

    // Popular searches first — they are what users most likely mean.
    let rows = db
        .fetch_all(
            "SELECT query FROM search_stats WHERE LOWER(query) LIKE ? \
             ORDER BY hits DESC, query LIMIT ?",
            &[Bind::S(pattern.clone()), Bind::I(limit)],
        )
        .await?;
    for r in rows {
        if let Ok(q) = r.try_get::<String, _>("query") {
            push(q, &mut out);
        }
    }

    if out.len() < limit as usize {
        let rows = db
            .fetch_all(
                "SELECT name FROM terms WHERE LOWER(name) LIKE ? ORDER BY name LIMIT ?",
                &[Bind::S(pattern.clone()), Bind::I(limit)],
            )
            .await?;
        for r in rows {
            if let Ok(n) = r.try_get::<String, _>("name") {
                push(n, &mut out);
            }
        }
    }

    if out.len() < limit as usize {
        let rows = db
            .fetch_all(
                "SELECT title FROM search_index WHERE visible = 1 AND LOWER(title) LIKE ? \
                 ORDER BY COALESCE(published_at, updated_at) DESC LIMIT ?",
                &[Bind::S(pattern), Bind::I(limit)],
            )
            .await?;
        for r in rows {
            if let Ok(t) = r.try_get::<String, _>("title") {
                push(t, &mut out);
            }
        }
    }
    Ok(out)
}
