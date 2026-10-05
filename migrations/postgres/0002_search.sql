-- Search system v2 (PostgreSQL)
--
-- search_index holds the denormalized document text; search_vector is a
-- weighted tsvector maintained application-side (per-field setweight mapped
-- from [search.weights]) with a GIN index for fast matching.

CREATE TABLE IF NOT EXISTS search_index (
    id           BIGSERIAL PRIMARY KEY,
    ref_type     TEXT NOT NULL,
    ref_id       BIGINT NOT NULL,
    title        TEXT NOT NULL,
    slug         TEXT NOT NULL,
    excerpt      TEXT NOT NULL DEFAULT '',
    content      TEXT NOT NULL DEFAULT '',
    author       TEXT NOT NULL DEFAULT '',
    category     TEXT NOT NULL DEFAULT '',
    tags         TEXT NOT NULL DEFAULT '',
    visible      INTEGER NOT NULL DEFAULT 0,
    published_at BIGINT,
    updated_at   BIGINT NOT NULL,
    search_vector tsvector NOT NULL DEFAULT ''::tsvector,
    UNIQUE (ref_type, ref_id)
);
CREATE INDEX IF NOT EXISTS idx_search_visible ON search_index (visible, ref_type);
CREATE INDEX IF NOT EXISTS idx_search_vector ON search_index USING GIN (search_vector);

CREATE TABLE IF NOT EXISTS search_stats (
    query            TEXT PRIMARY KEY,
    hits             BIGINT NOT NULL DEFAULT 0,
    no_results       BIGINT NOT NULL DEFAULT 0,
    last_searched_at BIGINT NOT NULL DEFAULT 0
);
