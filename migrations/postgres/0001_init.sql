-- Polaris schema v1 (PostgreSQL)
CREATE TABLE IF NOT EXISTS settings (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id            BIGSERIAL PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    email         TEXT NOT NULL DEFAULT '',
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'author',
    display_name  TEXT NOT NULL DEFAULT '',
    bio           TEXT NOT NULL DEFAULT '',
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS posts (
    id             BIGSERIAL PRIMARY KEY,
    title          TEXT NOT NULL,
    slug           TEXT NOT NULL UNIQUE,
    summary        TEXT NOT NULL DEFAULT '',
    content_md     TEXT NOT NULL DEFAULT '',
    author_id      BIGINT NOT NULL REFERENCES users (id),
    status         TEXT NOT NULL DEFAULT 'draft',
    featured_image TEXT,
    published_at   BIGINT,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_posts_status_published ON posts (status, published_at);
CREATE INDEX IF NOT EXISTS idx_posts_author ON posts (author_id);
CREATE INDEX IF NOT EXISTS idx_posts_updated ON posts (updated_at);

CREATE TABLE IF NOT EXISTS pages (
    id         BIGSERIAL PRIMARY KEY,
    title      TEXT NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    summary    TEXT NOT NULL DEFAULT '',
    content_md TEXT NOT NULL DEFAULT '',
    author_id  BIGINT NOT NULL REFERENCES users (id),
    status     TEXT NOT NULL DEFAULT 'draft',
    sort_order BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS terms (
    id   BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    UNIQUE (kind, slug)
);
CREATE INDEX IF NOT EXISTS idx_terms_kind ON terms (kind);

CREATE TABLE IF NOT EXISTS post_terms (
    post_id BIGINT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    term_id BIGINT NOT NULL REFERENCES terms (id) ON DELETE CASCADE,
    PRIMARY KEY (post_id, term_id)
);
CREATE INDEX IF NOT EXISTS idx_post_terms_term ON post_terms (term_id);

CREATE TABLE IF NOT EXISTS comments (
    id           BIGSERIAL PRIMARY KEY,
    post_id      BIGINT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    parent_id    BIGINT REFERENCES comments (id) ON DELETE CASCADE,
    author_name  TEXT NOT NULL,
    author_email TEXT NOT NULL DEFAULT '',
    author_url   TEXT NOT NULL DEFAULT '',
    content      TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    created_at   BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_comments_post ON comments (post_id, status);
CREATE INDEX IF NOT EXISTS idx_comments_status ON comments (status);
