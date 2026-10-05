//! Privacy-friendly search analytics.
//!
//! Only the normalized query text and aggregate counters are stored —
//! never IPs, user agents, session or user identities. Disabled by
//! default (`[search.analytics] enabled = false`).

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::utils::time;

const MAX_QUERY_LEN: usize = 100;

/// Record one executed search. Errors are swallowed by the caller.
pub async fn record(db: &Db, normalized: &str, found_results: bool) -> AppResult<()> {
    let q: String = normalized.chars().take(MAX_QUERY_LEN).collect();
    if q.is_empty() {
        return Ok(());
    }
    let now = time::now();
    let updated = db
        .execute(
            "UPDATE search_stats SET hits = hits + 1, no_results = no_results + ?, \
             last_searched_at = ? WHERE query = ?",
            &[
                Bind::I(i64::from(!found_results)),
                Bind::I(now),
                Bind::S(q.clone()),
            ],
        )
        .await?;
    if updated == 0 {
        db.execute(
            "INSERT INTO search_stats (query, hits, no_results, last_searched_at) \
             VALUES (?, 1, ?, ?)",
            &[Bind::S(q), Bind::I(i64::from(!found_results)), Bind::I(now)],
        )
        .await?;
    }
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SearchStat {
    pub query: String,
    pub hits: i64,
    pub no_results: i64,
}

/// Most searched queries.
pub async fn popular(db: &Db, limit: i64) -> AppResult<Vec<SearchStat>> {
    let rows = db
        .fetch_all(
            "SELECT query, hits, no_results FROM search_stats \
             WHERE hits > 0 ORDER BY hits DESC, query LIMIT ?",
            &[Bind::I(limit)],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| SearchStat {
            query: r.try_get("query").unwrap_or_default(),
            hits: r.try_get("hits").unwrap_or(0),
            no_results: r.try_get("no_results").unwrap_or(0),
        })
        .collect())
}

/// Searches that returned nothing — content ideas for the admin.
pub async fn top_no_results(db: &Db, limit: i64) -> AppResult<Vec<SearchStat>> {
    let rows = db
        .fetch_all(
            "SELECT query, hits, no_results FROM search_stats \
             WHERE no_results > 0 ORDER BY no_results DESC, query LIMIT ?",
            &[Bind::I(limit)],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| SearchStat {
            query: r.try_get("query").unwrap_or_default(),
            hits: r.try_get("hits").unwrap_or(0),
            no_results: r.try_get("no_results").unwrap_or(0),
        })
        .collect())
}
