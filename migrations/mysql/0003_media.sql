-- Media system v3 (MySQL 8+)
-- Metadata only: file bytes live in the configured StorageProvider.

CREATE TABLE IF NOT EXISTS media_folders (
    id         BIGINT AUTO_INCREMENT PRIMARY KEY,
    name       VARCHAR(128) NOT NULL,
    slug       VARCHAR(128) NOT NULL UNIQUE,
    created_at BIGINT NOT NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE IF NOT EXISTS media (
    id                BIGINT AUTO_INCREMENT PRIMARY KEY,
    uuid              VARCHAR(32) NOT NULL UNIQUE,
    filename          VARCHAR(255) NOT NULL,
    original_filename VARCHAR(255) NOT NULL,
    storage_key       VARCHAR(512) NOT NULL,
    mime_type         VARCHAR(128) NOT NULL,
    extension         VARCHAR(16) NOT NULL,
    size              BIGINT NOT NULL,
    width             BIGINT NULL,
    height            BIGINT NULL,
    duration          BIGINT NULL,
    hash              VARCHAR(64) NOT NULL,
    title             VARCHAR(255) NOT NULL,
    description       TEXT NOT NULL,
    alt               TEXT NOT NULL,
    caption           TEXT NOT NULL,
    thumbnails        VARCHAR(255) NOT NULL,
    folder_id         BIGINT NULL,
    uploaded_by       BIGINT NOT NULL,
    created_at        BIGINT NOT NULL,
    updated_at        BIGINT NOT NULL,
    CONSTRAINT fk_media_folder FOREIGN KEY (folder_id) REFERENCES media_folders (id) ON DELETE SET NULL,
    CONSTRAINT fk_media_uploader FOREIGN KEY (uploaded_by) REFERENCES users (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_media_hash ON media (hash);
CREATE INDEX idx_media_mime ON media (mime_type);
CREATE INDEX idx_media_uploaded_by ON media (uploaded_by);
CREATE INDEX idx_media_folder ON media (folder_id);
CREATE INDEX idx_media_created ON media (created_at);

CREATE TABLE IF NOT EXISTS media_tags (
    media_id BIGINT NOT NULL,
    tag      VARCHAR(64) NOT NULL,
    PRIMARY KEY (media_id, tag),
    CONSTRAINT fk_media_tags_media FOREIGN KEY (media_id) REFERENCES media (id) ON DELETE CASCADE
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_media_tags_tag ON media_tags (tag);

CREATE TABLE IF NOT EXISTS media_references (
    media_id   BIGINT NOT NULL,
    ref_type   VARCHAR(16) NOT NULL,
    ref_id     BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (media_id, ref_type, ref_id),
    CONSTRAINT fk_media_references_media FOREIGN KEY (media_id) REFERENCES media (id) ON DELETE CASCADE
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
CREATE INDEX idx_media_references_ref ON media_references (ref_type, ref_id);
