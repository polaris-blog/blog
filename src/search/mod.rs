//! Full-text search subsystem.
//!
//! ```text
//! Search request → SearchService ─┬→ search cache (ns "search")
//!                                 ├→ query parser (normalize/validate/escape)
//!                                 └→ SearchProvider (enum dispatch)
//!                                    ├─ SQLite FTS5
//!                                    ├─ MySQL FULLTEXT
//!                                    └─ PostgreSQL tsvector/GIN
//! ```
//!
//! Principles: **database first** (the dialect's native engine, no extra
//! services), **index first** (every query hits a full-text index — no
//! default `LIKE '%kw%'` scans), **cache when useful** (versioned
//! namespace invalidated on content mutations), **plugin extensible**
//! (external engines slot in as additional `SearchProvider` variants),
//! **privacy friendly** (analytics off by default, aggregated queries
//! only).

pub mod analytics;
pub mod highlight;
pub mod providers;
pub mod query;
pub mod result;
pub mod service;
pub mod suggest;

pub use query::{SearchKind, SearchQuery, SearchSort};
pub use result::{IndexStatus, RebuildStats, SearchResponse, SearchResult};
pub use service::SearchService;
