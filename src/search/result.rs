//! Search result types and the document shape written to the index.

use serde::{Deserialize, Serialize};

use super::query::SearchSort;

/// A document to be written into the search index (denormalized from a
/// post, page or media item — the index is self-contained, searches never
/// join back to the content tables).
#[derive(Clone, Debug)]
pub struct IndexedDoc {
    /// "post", "page" or "media".
    pub ref_type: &'static str,
    pub ref_id: i64,
    pub title: String,
    pub slug: String,
    pub excerpt: String,
    pub content: String,
    pub author: String,
    pub category: String,
    /// Comma-joined tag names (text-searchable).
    pub tags: String,
    pub visible: bool,
    pub published_at: Option<i64>,
    pub updated_at: i64,
}

impl IndexedDoc {
    pub fn url(&self) -> String {
        if self.ref_type == "page" {
            format!("/{}", self.slug)
        } else {
            format!("/posts/{}", self.slug)
        }
    }
}

/// Raw provider row: like [`SearchResult`] but carries `content` so the
/// service layer can build a contextual snippet before exposing results.
#[derive(Clone, Debug)]
pub struct SearchRow {
    pub ref_type: String,
    pub ref_id: i64,
    pub title: String,
    pub slug: String,
    pub excerpt: String,
    pub content: String,
    pub author: String,
    pub category: String,
    pub tags: String,
    pub score: f64,
    pub published_at: Option<i64>,
    pub updated_at: i64,
}

/// A single search result as returned over HTTP / templates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub slug: String,
    pub excerpt: String,
    pub url: String,
    pub score: f32,
    /// Safe HTML: text is escaped, matches wrapped in `<mark>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<i64>,
    pub updated_at: i64,
    pub author: String,
    pub category: String,
    pub tags: Vec<String>,
}

/// A page of search results.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub sort: String,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
    pub pages: i64,
    pub results: Vec<SearchResult>,
}

impl SearchResponse {
    pub fn empty(query: &str, sort: SearchSort, page: u32, per_page: u32) -> Self {
        Self {
            query: query.to_string(),
            sort: sort.as_str().to_string(),
            page,
            per_page,
            total: 0,
            pages: 0,
            results: Vec::new(),
        }
    }
}

/// Index health snapshot (CLI `search status`, admin dashboard,
/// `GET /api/search/status`).
#[derive(Clone, Debug, Serialize)]
pub struct IndexStatus {
    pub provider: String,
    pub healthy: bool,
    pub indexed_posts: i64,
    pub indexed_pages: i64,
    pub indexed_media: i64,
    /// Unix seconds of the last full rebuild (None = never).
    pub last_rebuild: Option<i64>,
}

/// Outcome of `polaris search rebuild`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct RebuildStats {
    pub posts: i64,
    pub pages: i64,
    pub media: i64,
    pub verified: bool,
}
