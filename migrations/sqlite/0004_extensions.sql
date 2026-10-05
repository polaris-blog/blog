-- Extension registry: installed theme/plugin packages, install log and
-- per-plugin migration history. Files live on disk; this is metadata only.

CREATE TABLE extensions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ext_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    version TEXT NOT NULL,
    package_hash TEXT NOT NULL DEFAULT '',
    permissions TEXT NOT NULL DEFAULT '[]',
    installed_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE UNIQUE INDEX idx_extensions_kind_ext ON extensions (kind, ext_id);

CREATE TABLE extension_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ext_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    action TEXT NOT NULL,
    version TEXT NOT NULL DEFAULT '',
    actor TEXT NOT NULL,
    result TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL
);

CREATE INDEX idx_extension_logs_kind ON extension_logs (kind, id);

CREATE TABLE extension_migrations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ext_id TEXT NOT NULL,
    name TEXT NOT NULL,
    applied_at BIGINT NOT NULL
);

CREATE UNIQUE INDEX idx_extension_migrations ON extension_migrations (ext_id, name);
