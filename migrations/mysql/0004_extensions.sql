-- Extension registry: installed theme/plugin packages, install log and
-- per-plugin migration history. Files live on disk; this is metadata only.

CREATE TABLE extensions (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    ext_id VARCHAR(128) NOT NULL,
    kind VARCHAR(16) NOT NULL,
    version VARCHAR(64) NOT NULL,
    package_hash VARCHAR(128) NOT NULL DEFAULT '',
    permissions TEXT NOT NULL,
    installed_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE UNIQUE INDEX idx_extensions_kind_ext ON extensions (kind, ext_id);

CREATE TABLE extension_logs (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    ext_id VARCHAR(128) NOT NULL,
    kind VARCHAR(16) NOT NULL,
    action VARCHAR(32) NOT NULL,
    version VARCHAR(64) NOT NULL DEFAULT '',
    actor VARCHAR(128) NOT NULL,
    result VARCHAR(16) NOT NULL,
    detail TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX idx_extension_logs_kind ON extension_logs (kind, id);

CREATE TABLE extension_migrations (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    ext_id VARCHAR(128) NOT NULL,
    name VARCHAR(255) NOT NULL,
    applied_at BIGINT NOT NULL
);

CREATE UNIQUE INDEX idx_extension_migrations ON extension_migrations (ext_id, name);
