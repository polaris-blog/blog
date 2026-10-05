-- Search system v2 (SQLite)
--
-- search_index is the source rows for the FTS index (external-content
-- table): text lives here once, tokens live in search_fts. The index is
-- maintained application-side (create/update/delete go through the search
-- service) and can be rebuilt wholesale with the FTS5 'rebuild' command.

CREATE TABLE IF NOT EXISTS search_index (
    id           INTEGER PRIMARY KEY,
    ref_type     TEXT NOT NULL,
    ref_id       INTEGER NOT NULL,
    title        TEXT NOT NULL,
    slug         TEXT NOT NULL,
    excerpt      TEXT NOT NULL DEFAULT '',
    content      TEXT NOT NULL DEFAULT '',
    author       TEXT NOT NULL DEFAULT '',
    category     TEXT NOT NULL DEFAULT '',
    tags         TEXT NOT NULL DEFAULT '',
    visible      INTEGER NOT NULL DEFAULT 0,
    published_at INTEGER,
    updated_at   INTEGER NOT NULL,
    UNIQUE (ref_type, ref_id)
);
CREATE INDEX IF NOT EXISTS idx_search_visible ON search_index (visible, ref_type);

CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
    title, excerpt, author, category, tags, content,
    content='search_index', content_rowid='id', tokenize='unicode61'
);

-- Aggregated search analytics (privacy-friendly: normalized query text and
-- counters only — no IPs, no user agents, no identities).
CREATE TABLE IF NOT EXISTS search_stats (
    query            TEXT PRIMARY KEY,
    hits             INTEGER NOT NULL DEFAULT 0,
    no_results       INTEGER NOT NULL DEFAULT 0,
    last_searched_at INTEGER NOT NULL DEFAULT 0
);
