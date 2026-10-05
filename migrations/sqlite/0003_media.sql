-- Media system v3 (SQLite)
--
-- The database stores metadata only; file bytes live in the configured
-- StorageProvider (local `data/media` by default). `storage_key` is the
-- provider-relative key (e.g. `2026/08/8c7d2f91.webp`) and is decoupled
-- from the public URL (`/media/{uuid}.{ext}`). `hash` is the SHA-256 of
-- the stored bytes and drives deduplication.

CREATE TABLE IF NOT EXISTS media_folders (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS media (
    id                INTEGER PRIMARY KEY,
    uuid              TEXT NOT NULL UNIQUE,
    filename          TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    storage_key       TEXT NOT NULL,
    mime_type         TEXT NOT NULL,
    extension         TEXT NOT NULL,
    size              INTEGER NOT NULL,
    width             INTEGER,
    height            INTEGER,
    duration          INTEGER,
    hash              TEXT NOT NULL,
    title             TEXT NOT NULL DEFAULT '',
    description       TEXT NOT NULL DEFAULT '',
    alt               TEXT NOT NULL DEFAULT '',
    caption           TEXT NOT NULL DEFAULT '',
    thumbnails        TEXT NOT NULL DEFAULT '',
    folder_id         INTEGER REFERENCES media_folders (id) ON DELETE SET NULL,
    uploaded_by       INTEGER NOT NULL REFERENCES users (id),
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_hash ON media (hash);
CREATE INDEX IF NOT EXISTS idx_media_mime ON media (mime_type);
CREATE INDEX IF NOT EXISTS idx_media_uploaded_by ON media (uploaded_by);
CREATE INDEX IF NOT EXISTS idx_media_folder ON media (folder_id);
CREATE INDEX IF NOT EXISTS idx_media_created ON media (created_at);

CREATE TABLE IF NOT EXISTS media_tags (
    media_id INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    PRIMARY KEY (media_id, tag)
);
CREATE INDEX IF NOT EXISTS idx_media_tags_tag ON media_tags (tag);

-- Which posts/pages embed a media item (scanned from content on save).
CREATE TABLE IF NOT EXISTS media_references (
    media_id   INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    ref_type   TEXT NOT NULL,
    ref_id     INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (media_id, ref_type, ref_id)
);
CREATE INDEX IF NOT EXISTS idx_media_references_ref ON media_references (ref_type, ref_id);
