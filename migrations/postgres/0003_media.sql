-- Media system v3 (PostgreSQL)
-- Metadata only: file bytes live in the configured StorageProvider.

CREATE TABLE IF NOT EXISTS media_folders (
    id         BIGSERIAL PRIMARY KEY,
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    created_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS media (
    id                BIGSERIAL PRIMARY KEY,
    uuid              TEXT NOT NULL UNIQUE,
    filename          TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    storage_key       TEXT NOT NULL,
    mime_type         TEXT NOT NULL,
    extension         TEXT NOT NULL,
    size              BIGINT NOT NULL,
    width             BIGINT,
    height            BIGINT,
    duration          BIGINT,
    hash              TEXT NOT NULL,
    title             TEXT NOT NULL DEFAULT '',
    description       TEXT NOT NULL DEFAULT '',
    alt               TEXT NOT NULL DEFAULT '',
    caption           TEXT NOT NULL DEFAULT '',
    thumbnails        TEXT NOT NULL DEFAULT '',
    folder_id         BIGINT REFERENCES media_folders (id) ON DELETE SET NULL,
    uploaded_by       BIGINT NOT NULL REFERENCES users (id),
    created_at        BIGINT NOT NULL,
    updated_at        BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_hash ON media (hash);
CREATE INDEX IF NOT EXISTS idx_media_mime ON media (mime_type);
CREATE INDEX IF NOT EXISTS idx_media_uploaded_by ON media (uploaded_by);
CREATE INDEX IF NOT EXISTS idx_media_folder ON media (folder_id);
CREATE INDEX IF NOT EXISTS idx_media_created ON media (created_at);

CREATE TABLE IF NOT EXISTS media_tags (
    media_id BIGINT NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    PRIMARY KEY (media_id, tag)
);
CREATE INDEX IF NOT EXISTS idx_media_tags_tag ON media_tags (tag);

CREATE TABLE IF NOT EXISTS media_references (
    media_id   BIGINT NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    ref_type   TEXT NOT NULL,
    ref_id     BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (media_id, ref_type, ref_id)
);
CREATE INDEX IF NOT EXISTS idx_media_references_ref ON media_references (ref_type, ref_id);
