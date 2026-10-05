# Polaris — Development Guide

This document explains the architecture, the theme API, the plugin API and the
test suite. For running a blog in production, see [DEPLOYMENT.md](DEPLOYMENT.md).

## Building

```bash
cargo build                  # debug
cargo build --release        # optimized single binary
cargo test                   # unit + integration tests
cargo clippy --all-targets --all-features
```

Rust 2024 edition, stable toolchain. No build scripts, no codegen, no native
dependencies beyond what sqlx needs for the database drivers.

### Continuous integration

`.github/workflows/ci.yml` runs formatting, Clippy with warnings denied,
all-feature tests and a release build. A separate matrix runs
`tests/database_matrix.rs` against PostgreSQL 16 and MySQL 8.4. The same
contract runs against temporary SQLite by default:

```bash
cargo test --locked --test database_matrix
```

To run it against a server, set both `POLARIS_TEST_DATABASE_DRIVER` (`postgres`
or `mysql`) and `POLARIS_TEST_DATABASE_URL`. **Use a fresh, empty, dedicated
test database.** The test refuses databases containing tables and leaves its
test tables behind. It covers migration idempotence, rollback, full-text
visibility, concurrent administrator demotion, scheduler version fencing,
backup restore and post-restore insert IDs. It supplements the SQLite suite;
it does not establish all server failure/reconnection behavior. For constrained
machines use `cargo test --locked -j 2` to limit build parallelism.

SQLx Any represents MySQL `TEXT`/`MEDIUMTEXT` as byte values. Textual columns
use `db::text` / `db::optional_text` to decode these as UTF-8, while preserving
normal SQLite/PostgreSQL text handling. Invalid bytes remain errors. Use these
helpers when introducing another textual column read through the Any driver.

### Redis contracts

The `redis` CI job runs real Redis 7.4 tests. Locally, start a dedicated test
server and run:

```bash
POLARIS_TEST_REDIS_URL=redis://127.0.0.1:6379 \
  cargo test --locked --features redis --lib redis_live -- --ignored --nocapture
```

These three tests are explicitly ignored by the ordinary suite, which needs
no external server. They cover shared invalidation, namespace isolation,
logical clear, restart, metadata eviction and stale-fill fencing. Two tests
use local TCP proxies to disconnect only their own sockets, including an
unavailable-at-startup case; each exercises the real 30-second retry window.
They require local TCP binding permissions and a `redis://` TCP URL. Tests own
random site prefixes and clean those prefixes on success; they never use
`FLUSHDB`, server shutdown or server-wide client disconnection. A failed test
may leave its small metadata hash; use a disposable Redis service.

Cache invalidation and `begin_fill` are async: await invalidation after the
database mutation, and capture a fill **before** loading its source. Keep
`finish_fill` bound to that captured generation. Memory mode keeps in-process
counters; Redis reads shared random generations and uses a site-wide epoch
for clear/startup/recovery. Distributed guarantees and deployment limits are
described in the deployment guide.

## Architecture

Monolith, one process, one binary. Layering:

```
HTTP (axum handlers)
  ↓
Services        business rules + plugin hook dispatch
  ↓
Repositories    SQL, one module per aggregate
  ↓
Db              sqlx AnyPool wrapper + dialect translation
  ↓
SQLite / MySQL / PostgreSQL
```

Key decisions:

- **No ORM.** Repositories hand-write portable SQL; the `Db` layer translates
  `?` placeholders to `$n` (Postgres) and handles insert-id retrieval per
  dialect (`RETURNING id` for SQLite/Postgres, `LAST_INSERT_ID()` for MySQL).
- **No separate admin frontend.** The admin UI is server-rendered from
  templates embedded in the binary (`src/templates/admin/`), styled with one
  hand-written CSS file. No build step, no JS framework.
- **Themes live on disk** and are never compiled in. An embedded fallback theme
  guarantees the site renders even with an empty `themes/` directory.
- **Plugins are Rhai scripts.** Evaluation summary:
  - WASM (wasmtime): ~30 MB binary, slow instantiation — rejected.
  - Lua (mlua): vendored C, FFI surface — rejected.
  - JS (quickjs/boa): large runtime or C dependency — rejected.
  - External process + IPC: violates single-binary deployment — rejected.
  - **Rhai: pure Rust, `Send + Sync`, no unsafe, sandboxed by construction,
    microsecond-range call overhead — selected.**
- **Time handling without chrono.** `src/utils/time.rs` implements civil-time
  conversion (days-from-epoch algorithm) — enough for a blog, zero dependency.
- **Static files** are served by a small handler with ETag + caching headers
  instead of pulling in tower-http's full static-service stack.

### Module map

| Path | Responsibility |
|---|---|
| `src/main.rs` | CLI: `serve`, `migrate`, `user create`, `theme …`, `plugin …`, `search …`, `media …` |
| `src/config.rs` | `polaris.toml` + `POLARIS_*` env + CLI overrides, layered |
| `src/config_schema.rs` | parses theme/plugin `config.schema.toml`; types, validation, `show_if` |
| `src/config_store.rs` | `ConfigManager`: layering, persistence, encryption, in-memory snapshots |
| `src/state.rs` | `AppState`: db, settings cache, markdown cache, session store, limiters |
| `src/error.rs` | `AppError` (BadRequest/NotFound/Conflict/Unauthorized/Internal/Db/…) |
| `src/db/mod.rs` | `Db` (AnyPool), bind types, dialect translation, `insert`/`execute`/`fetch_*` |
| `src/db/migrate.rs` | embedded SQL migrations per dialect + version table |
| `src/models/` | `Post`, `Page`, `Term`, `Comment`, `User`, `Media`, enums, JSON shapes |
| `src/repositories/` | `posts`, `pages`, `terms`, `comments`, `users`, `settings`, `media` |
| `src/services/` | `posts` (CRUD + rendering + terms sync), `users`, `comments`, `seed`, `media` |
| `src/auth/` | Argon2 hashing, `SessionStore`, CSRF, `LoginLimiter` |
| `src/http/` | `public`, `admin`, `admin_media`, `api`, `media_api`, `media`, `search`, `seo`, `comments`, `static_files`, `plugins_http` |
| `src/media/` | media subsystem: `storage` (provider abstraction), `validate` (magic bytes, SVG sanitizer), `image` (thumbnails, EXIF, re-encode) |
| `src/search/` | full-text search: service, providers, query parsing, highlighting |
| `src/themes/mod.rs` | `ThemeManager`: disk scan, Tera build, hot activation, traversal guard |
| `src/plugins/mod.rs` | `PluginManager`: Rhai engine, hook dispatch, routes, filters |
| `src/markdown.rs` | pulldown-cmark render, HTML stripping, safe-link checks, text extraction |
| `src/templates/` | embedded admin + fallback themes, shared Tera filters |
| `src/utils/` | `time`, `slug`, `xml`, `cookies`, `hash` |

### Request lifecycle (public page)

1. Axum routes to `http/public.rs`.
2. Handler loads data through a **service** (`services::posts::list_public`).
3. Services run plugin hooks (`before_*`/`after_*`) around repository calls.
4. Handler builds a `tera::Context` (plain JSON values) and renders the active
   theme template.
5. Response includes ETag; a matching `If-None-Match` short-circuits to 304.

### Performance notes

- Rendered Markdown is cached in memory keyed by `(kind, id, updated_at,
  plugin_generation)` — content edits or plugin reloads invalidate naturally.
- SQLite runs in WAL mode with sane pool defaults (`max_connections` from
  config, default 10).
- Admin templates and the fallback theme are compiled once (`OnceLock`).
- Theme Tera instances are rebuilt only on activation (hot switch), not per
  request.
- The release profile uses `lto = "thin"`, `codegen-units = 1`, `strip = true`.

## Configuration

`src/config.rs` loads `polaris.toml` (path overridable with `--config`), then
applies `POLARIS_*` environment variables (e.g. `POLARIS_DATABASE_URL`), then
CLI arguments. Runtime values changed in the admin UI are persisted to the
`settings` table and override the file on next boot.

Theme and plugin settings use a separate, schema-backed system — see the next
section.

## Theme & plugin configuration

Themes and plugins declare their settings in a `config.schema.toml`; Polaris
Core owns storage, validation and the admin UI. **The schema is the contract**:
adding a setting never requires touching core source code, and the admin
settings pages are generated from it.

```
themes/default/
├── theme.toml            metadata
├── config.schema.toml    optional settings schema
├── config.toml           optional file-level defaults
├── templates/
└── static/

plugins/example/
├── plugin.toml
├── config.schema.toml    optional settings schema
├── config.toml           optional file-level defaults
└── main.rhai
```

Admin locations: Appearance → Themes → *Settings* (per theme) and Plugins →
*Settings* (per plugin). A *Settings* link appears only when a schema with at
least one field is present; pages are admin-only.

### Schema

One table per setting (the table name is the setting id), plus a `groups`
table for admin page sections:

```toml
[groups.appearance]            # group id -> label
label = "Appearance"

[accent_color]
type = "color"                 # required (see types below)
label = "Accent color"         # shown in the admin UI (defaults to the id)
description = "Primary color for links and buttons."
default = "#4F46E5"            # must itself pass validation
group = "appearance"           # section on the settings page

[posts_per_page]
type = "integer"
min = 1                        # range checks (integer/float)
max = 50
required = true                # checked against the effective value on save
permission = "editor"          # public | author | editor | admin (write access)
restart = true                 # admin sees a "restart required" notice

[tracking_id]
type = "string"
show_if = "dark_mode == true"  # hidden until another setting matches
                               # (`==` / `!=`, joined with `&&`)

[layout]
type = "select"                # options for select / radio / multiselect
options = [
    { value = "default", label = "Default" },
    { value = "wide", label = "Wide" },
]

[social_links]                 # repeatable rows of objects
type = "array"

[social_links.item.label]      # sub-fields of each row
type = "string"

[social_links.item.url]
type = "url"
```

Unknown keys inside a field table are rejected at load time — a typo like
`defualt` fails loudly instead of silently disabling validation.

Field types:

| Type | Admin input | Validation |
|---|---|---|
| `string` | text | — |
| `text` / `textarea` | multiline text | — |
| `integer` / `float` | number | `min` / `max` range |
| `boolean` | checkbox | — |
| `color` | color picker | `#RGB`, `#RRGGBB`, `#RRGGBBAA` |
| `url` | text | `http(s)://…` |
| `email` | text | `local@domain.tld` |
| `password`* | password field | sensitive — see below |
| `select` / `radio` | dropdown / list | one of `options` |
| `multiselect` | checkboxes | comma-separated, each in `options` |
| `image` / `file` | text (path/URL) | — |
| `array` | JSON rows | each row's sub-fields validated |

\* `password`, `api_key`, `secret`, `token` and `private_key` all map to the
sensitive password type.

### Value resolution

First match wins (later layers override earlier ones):

```
schema default  →  config.toml  →  database  →  environment
```

Environment overrides map to schema keys:
`POLARIS_THEME_DEFAULT_SITE_TITLE=…` sets `site_title` of theme `default`;
`POLARIS_PLUGIN_ANALYTICS_TRACKING_ID=…` sets `tracking_id` of plugin
`analytics`. Values are validated; invalid overrides are ignored with a
warning.

Database rows live in the existing `settings` table as
`theme.<name>.<key>` / `plugin.<name>.<key>` — no migration, portable across
SQLite/MySQL/PostgreSQL. An empty form field *removes* the override and the
value falls back to the default; an absent setting simply has no row.

Effective values are cached in memory (one `RwLock` snapshot per namespace) —
template rendering and Rhai calls are synchronous and never hit the database.

### Reading configuration

Templates (public pages of the active theme — sensitive values appear as
`null`):

```text
{{ theme.config.site_title }}
{% if theme.config.dark_mode %} … {% endif %}
{% for link in theme.config.social_links %}{{ link.label }}{% endfor %}
```

Plugin scripts — read-only, namespaced to the owning plugin:

```rhai
fn init(config) {              // effective values incl. this plugin's secrets
    print(config.tagline);
}

fn on_config_changed(ev) {     // {namespace, key, old_value, new_value}
    if ev.key == "tracking_id" { /* reload */ }
}

// host functions, always current (no restart needed):
config_get("tracking_id")      // string ("" when unset)
config_get_bool("enabled")     // bool   (false when unset)
config_get_int("posts")        // int    (0 when unset)
config_has("tracking_id")      // bool
```

Rust code uses the same `ConfigManager` (`app.configs`):
`get(ns, key)`, `get_string`, `get_bool`, `get_int`, `values_json(ns,
reveal_secrets)`.

### Sensitive values

`password` / `api_key` / `secret` / `token` / `private_key` fields are:

- encrypted at rest (AES-256-GCM, key derived from the server secret),
- masked in the admin form — empty input on save keeps the current value,
- excluded from template contexts (`null` instead of the value),
- admin-only by default (`permission` defaults to `admin` for sensitive
  fields, `editor` for everything else).

A plugin may read **its own** secrets (`init(config)`, `config_get`), but no
others: scripts have no write access to any configuration, and the save API
rejects keys outside the target namespace (`plugin.a` can never write
`plugin.b.*`, `theme.*` or `core.*`).

### Hot reload & cache interaction

Saving settings validates every field (types, ranges, options, `required`,
permissions), persists only changed rows, refreshes the in-memory snapshot,
invalidates the render/content caches (theme values affect every page) and
fires `on_config_changed` for each changed key. The next request renders with
the new values — no restart. Fields declared `restart = true` show a notice
instead.

Schemas are re-read from disk whenever their settings page opens, so
iterating on a schema needs no restart either.

## Themes

A theme is `themes/<name>/`:

```
theme.toml            metadata
config.schema.toml    optional settings schema (see above)
config.toml           optional file-level defaults
templates/            base.html, index.html, post.html, page.html,
                      category.html, tag.html, 404.html (+ any partials)
static/               served at /static/<name>/… with far-future cache headers
```

`theme.toml`:

```toml
name = "Polaris Default"
version = "1.0.0"
author = "Polaris"
description = "Default Polaris theme"
```

### Template context

Every template extends `base.html` and receives:

| Variable | Shape | Notes |
|---|---|---|
| `site` | `{title, description, base_url, version}` | |
| `theme` | `{config: {…}}` | active theme's settings; sensitive values are `null` |
| `seo` | `{title, description, canonical, og_type, og_image}` | handlers fill per-page |
| `nav_pages` | `[{slug, title, …}]` | published pages |
| `plugin_nav` | `[{label, url}]` | contributed by plugins |
| `current_year` | string | |
| `json_ld` | string (pre-serialized) | structured data, on post pages |

Per-template:

- `index.html`, `category.html`, `tag.html`: `posts` (array of post cards:
  `url, title, date, datetime, author, reading_time, excerpt, category, tags,
  featured_image`), `term` (`{name, slug}` — category/tag pages),
  `pagination` (`{current, pages, prev, next, has_prev, has_next}`),
  `pagination_base`.
- `post.html`: `post` (full card + `content_html`), `comments_enabled`,
  `comments` (`[{id, parent_id, author, author_url, content, date}]`).
- `page.html`: `page` (`{title, date, content_html, …}`).
- `404.html`: base context only.

Custom filters available in themes:

- `date` — `{{ ts | date(format="rfc3339") }}`; presets: `date`, `datetime`,
  `year`, `rfc3339`, `rfc822`, `http`.
- `truncate_chars` — `{{ s | truncate_chars(n=160) }}` (adds `…`).
- Any filter registered by an enabled plugin (see below).

### Rules for themes

- **Zero JavaScript is a feature.** The default theme uses none.
- Templates are data-only: no filesystem, database or OS access. Tera is
  configured without `include`-style file access beyond theme templates.
- All output is HTML-escaped by default; only `content_html` (sanitized
  Markdown output) is marked `| safe`.
- Missing templates fall back to the embedded default — a theme may override
  just `post.html` if it wants.
- Hot switching: activating a theme rebuilds its Tera instance immediately
  (Admin → Themes, or `polaris theme enable <name>`). Path traversal
  (`../evil`) is rejected.

## Plugins

A plugin is `plugins/<name>/`:

```
plugin.toml          metadata + registration tables
main.rhai            script (entry point; overridable via `entry` in plugin.toml)
config.schema.toml   optional settings schema (see theme & plugin configuration)
config.toml          optional file-level defaults
```

`plugin.toml`:

```toml
name = "example"
version = "1.0.0"
author = "Polaris"
description = "…"

[routes]                       # public, served under /plugins/…
"/plugins/example/archive" = "route_archive"

[admin_routes]                 # require an admin session
"/plugins/example/status" = "route_status"

[filters]                      # Tera filters
"star" = "filter_star"
```

> Route keys may be written either as the full URL path
> (`"/plugins/example/archive"`) or mount-relative (`"/archive"`, served at
> `/plugins/<plugin>/archive`); `ctx.path` always receives the full request
> URL. Full paths are recommended — they are unambiguous and self-documenting.

### Script API (Rhai)

Lifecycle and hooks — define any of these functions; all are optional:

| Function | Signature | Effect |
|---|---|---|
| `init` | `(config_map) -> ()` | called on load with effective config values (schema defaults + `config.toml` + database + environment) |
| `on_config_changed` | `(event) -> ()` | fired per changed setting: `{namespace, key, old_value, new_value}` |
| `before_post_create` | `(payload) -> payload` | may modify `title`, `slug`, `summary`, `content`, `status`, `tags` |
| `before_post_update` | `(payload) -> payload` | same, plus `id` |
| `before_page_create` / `before_page_update` | `(payload) -> payload` | |
| `before_comment_create` | `(payload) -> payload` | may modify `author_name`, `content`, `status`, … |
| `after_post_create` / `after_post_update` / `after_post_delete` | `(obj) -> ()` | events |
| `after_page_create` / `after_page_update` / `after_page_delete` | `(obj) -> ()` | events |
| `after_comment_create` | `(obj) -> ()` | event |
| `markdown_before` / `markdown_after` | `(string) -> string` | Markdown pipeline transform |
| `nav` | `() -> [{label, url}]` | site navigation items |
| route handlers | `(ctx) -> string \| {status, body, content_type}` | registered via `plugin.toml` |
| filter handlers | `(string) -> string` | Tera filters |

`ctx` for route handlers contains `path` (full URL), query parameters as
string keys, and on admin routes `user` = `{username, role}`.

Host functions available to scripts:

- `log(level, message)`, `now()`
- plugin cache (always namespaced to the owning plugin, TTL-capped):
  `cache_get(key)`, `cache_set(key, value, ttl_secs)`, `cache_del(key)`
- plugin configuration (read-only, this plugin's namespace only — see the
  theme & plugin configuration section): `config_get(key)`,
  `config_get_bool(key)`, `config_get_int(key)`, `config_has(key)`

**Nothing else** — no file, network or process access. The engine is rebuilt
per load; plugins are compiled to AST once and called by name, so hook
dispatch stays in the microsecond range.

### Lifecycle

- `polaris plugin enable <name>` (or Admin → Plugins) loads and `init`s the
  script, bumps the plugin generation (invalidating the Markdown cache) and
  re-registers plugin Tera filters on the active theme.
- Disable takes effect immediately.
- A broken plugin (missing entry file, compile error) is skipped with a
  warning — the blog keeps running.
- Scripts are plain source: there is no native ABI to break. The hook surface
  above is the compatibility contract; new hooks are added, never repurposed.

## Extension packages

Themes and plugins share one installation pipeline — `src/extension/`. Both
are distributed as offline ZIP packages with a manifest that decides the kind
(`theme.toml` → theme, `plugin.toml` → plugin).

### Package layout

```
<package>.zip
└── <id>/                    # single root directory named like the id
    ├── theme.toml | plugin.toml
    ├── (theme) templates/ assets/ static/
    ├── (plugin) main.rhai migrations/*.sql
    └── README.md
```

Manifest contract (`manifest.rs`): required `id`, `name`, `version`,
`author`, `description`, `license`; optional `homepage`, `repository`,
`minimum_polaris_version` / `maximum_polaris_version`, `[dependencies]`
(id → semver requirement), `permissions` (plugins), `entry` (plugins,
default `main.rhai`), `uninstall_tables` (plugins, used by *Remove + data*).
Ids are `[a-z0-9-_]{1,64}` — they become directory names, so traversal-safe
by construction.

### Install pipeline (`installer.rs`)

```
stage ZIP (streaming, size-capped)      data/tmp/extensions/upload-*.zip
→ package::inspect                      entry scan: count, uncompressed
                                        size, symlink bits, blocked
                                        extensions (exe/dll/so/bat/…),
                                        path validation (no .., no \, no
                                        absolute, no Windows device names),
                                        single-root or flat layout
→ manifest::parse                        strict field validation
→ Polaris compatibility                 min/max version (force-overridable)
→ dependency check                      declared ids must be installed at
                                        matching versions
→ version comparison                    older needs allow_downgrade
→ package::extract → staging dir       write budget enforced even if the
                                        archive lies about sizes
→ re-validate on disk                    manifest + plugin entry file
→ backup old version                    data/backups/extensions/<id>-<v>.zip
→ atomic rename into themes/ | plugins/
→ plugin migrations                     each *.sql in name order, one
                                        transaction per file, recorded in
                                        extension_migrations (delta on update)
→ registry upsert + audit log
```

Any failure before the rename leaves no trace; a migration failure rolls the
previous version back into place. Staging directories are always cleaned up.

At startup extensions that pre-date the registry (shipped with the initial
deployment, restored from backup) are registered automatically — pure
bookkeeping, nothing is activated or enabled.

### Uninstall rules

- The **active theme** cannot be uninstalled (activate another first).
- An **enabled plugin** is disabled first.
- Plugin database tables survive by default; *Remove + data* drops the tables
  listed in `uninstall_tables` (validated identifiers, translated per
  dialect) and clears the plugin's migration history.

### Admin & CLI surfaces

- Admin: Themes/Plugins pages show name, version, author, status
  (active/enabled/disabled/broken), compatibility, permissions, and an
  upload panel (drag-drop + progress via `extensions.js`, plain form post
  without JS). The extension log page shows the audit trail.
- REST: `POST /api/admin/extensions/upload` (multipart, CSRF-checked,
  body-limited), `GET /api/admin/extensions?kind=`, `DELETE
  /api/admin/extensions/{kind}/{id}?remove_data=1`, `POST
  /api/admin/extensions/verify`.
- CLI: `polaris theme|plugin install <zip|dir> [--force]
  [--allow-downgrade]`, `remove [--remove-data]`, `polaris extension
  verify|logs`.

### Post-install behavior

Nothing auto-activates: themes install inactive, plugins install disabled.
Installing a new version of an *active* theme hot-reloads its templates; a
new version of an *enabled* plugin reloads its script. Config schemas are
(re)loaded for the new version.

Configuration: `[extensions]` in `polaris.toml` — `upload.max_file_size`
(default 20MB), `upload.max_uncompressed_size` (100MB), `upload.max_files`
(5000), `security.require_signature` (reserved, default false — signature
verification is deliberately not core), `tmp_dir`, `backup_dir`.

## Database

Migrations live in `migrations/{sqlite,mysql,postgres}/NNNN_name.sql` and are
embedded in the binary. `polaris migrate` (or `auto_migrate = true`) applies
pending ones and records versions in a `_migrations` table.

Schema (v1):

```
users(id, username, email, password_hash, role, display_name, bio,
      created_at, updated_at)
posts(id, title, slug, summary, content_md, author_id, status,
      featured_image, published_at, created_at, updated_at)
pages(id, title, slug, summary, content_md, author_id, status,
      sort_order, created_at, updated_at)
terms(id, kind, name, slug)                  -- kind: category | tag
post_terms(post_id, term_id)
comments(id, post_id, parent_id, author_name, author_email, author_url,
         content, status, created_at)        -- status: pending|approved|spam
settings(key, value)      -- core runtime settings + theme.<name>.* /
                           -- plugin.<name>.* configuration overrides
                           -- (sensitive values stored AES-256-GCM encrypted)
_migrations(version)
```

Schema (v2, search):

```
search_index(id, ref_type, ref_id, title, slug, excerpt, content, author,
             category, tags, visible, published_at, updated_at
             [, search_vector tsvector on PostgreSQL])
             -- ref_type: post | page; UNIQUE(ref_type, ref_id)
search_fts               -- SQLite only: FTS5 external-content table over
                           -- search_index (unicode61 tokenizer)
search_stats(query, hits, no_results, last_searched_at)  -- optional
```

Schema (v3, media):

```
media(id, uuid UNIQUE, filename, original_filename, storage_key, mime_type,
      extension, size, width, height, duration, hash UNIQUE, title,
      description, alt, caption, thumbnails, folder_id, uploaded_by,
      created_at, updated_at)
      -- uuid: short random id used in public URLs (immutable)
      -- storage_key: provider-relative key (YYYY/MM/uuid.ext)
      -- hash: SHA-256 of the stored bytes (dedup + ETag source)
      -- thumbnails: comma-separated generated size names
      -- indexes: hash (unique), mime_type, uploaded_by, folder_id,
      --          created_at, updated_at
media_folders(id, name, slug UNIQUE, created_at)     -- virtual, no filesystem tie
media_tags(media_id, tag)                            -- PK (media_id, tag)
media_references(media_id, ref_type, ref_id)         -- ref_type: post | page
                                                      -- PK (media_id, ref_type, ref_id)
```

Schema (v4, extensions):

```
extensions(id, ext_id, kind, version, package_hash, permissions,
           installed_at, updated_at)
           -- kind: theme | plugin; UNIQUE(kind, ext_id)
           -- package_hash: SHA-256 of the installed ZIP (audit + dedup)
           -- permissions: JSON array of declared permission names
extension_logs(id, ext_id, kind, action, version, actor, result, detail,
               created_at)
               -- action: install | update | downgrade | uninstall
               -- result: success | failed; actor: username or "cli"
extension_migrations(id, ext_id, name, applied_at)
               -- per-plugin SQL migration history (PK ext_id + name)
```

All queries are parameterized. See `tests/db_test.rs` for the injection
regression check.

## Search

Full-text search lives in `src/search/` and follows the same layering rule as
the rest of the codebase: business code only touches `SearchService`, never a
specific engine.

```
HTTP /search, /api/search*
        ↓
SearchService            normalize → cache → provider → snippet/highlight
        ↓
SearchProvider (enum)    SQLite FTS5 | MySQL FULLTEXT | PostgreSQL tsvector
```

- **Database first.** The default provider is `"auto"`: SQLite → FTS5
  (`search_fts` external-content table over `search_index`), MySQL →
  `FULLTEXT` index, PostgreSQL → weighted `tsvector` column + GIN index. No
  external service is required or started.
- **One index table.** `search_index` denormalizes title/excerpt/content/
  author/category/tags per post *and* page with a `visible` flag; searches
  never join back to content tables. `search_stats` holds optional query
  counters.
- **Index maintenance is synchronous** in the service layer: post/page create,
  update and delete upsert/remove the index row (failures log a warning —
  `polaris search rebuild` reconciles drift). Scheduled posts flip `visible`
  in one bulk statement when promoted.
- **User input never reaches SQL verbatim.** `ParsedQuery::parse` normalizes
  (trim/case/whitespace, ≤100 chars), strips FTS/tsquery/boolean operators,
  caps at 8 terms, then emits dialect-safe forms: quoted-prefix phrases for
  FTS5 (`"rust"*`), sanitized prefix lexemes for tsquery (`rust:* & web:*`),
  `+term*` for MySQL boolean mode. Filters are parameterized binds.
- **Ranking** uses each engine's relevance function with field weights from
  `[search.weights]` (title 5, excerpt 3, tags 3, category 2, content 1).
  PostgreSQL maps the numeric weights onto tsvector weight letters (A–D) by
  relative rank, so weights are configurable without a schema change.
- **Snippets & highlighting** are built in Rust: source text is HTML-escaped
  and matches wrapped in `<mark>` (never the reverse), with word-boundary
  matching so `rust` does not highlight inside `rustlang`. Templates render
  it with `{{ result.highlight | safe }}` — the HTML is generated, not user
  content.
- **Caching** reuses the cache system's `search` namespace. Result keys hash
  the full query shape (terms, page, per_page, sort, filters); suggestions
  are cached per prefix. The namespace is part of `ns::CONTENT`, so every
  content mutation invalidates search results in O(1) via the versioned-key
  mechanism — no full flush, no TTL reliance.
- **Suggestions** (`GET /api/search/suggest?q=ru`) merge popular past
  searches, tag/category names and indexed titles, prefix-matched via
  parameterized `LIKE` on short strings (indexed by `LOWER()`-friendly
  prefix scans; `minimum_query_length` guards against 1-char hammering).
- **Analytics** (`[search.analytics]`, off by default) stores only the
  normalized query text plus hit/no-result counters — no IPs, user agents or
  identities. The admin dashboard surfaces "popular" and "no results" lists
  as content ideas.
- **Ops**: `polaris search status` (provider/health/counts/last rebuild),
  `polaris search rebuild` (drop → scan → re-index with a progress bar and
  count verification), `GET /api/search/status` (admin API), and the
  `/admin/search` dashboard with a rebuild button.
- **HTML page**: `/search?q=…&sort=…&page=…` renders the theme's
  `search.html` (shipped in the default theme and the embedded fallback).
  The shared anonymous page cache skips `/search` deliberately — its key
  ignores query strings; per-query caching happens inside `SearchService`.

Future external providers (Meilisearch, Typesense, …) plug in as new
`SearchProvider` variants or via a plugin — core stays database-only.

## Media

The media library (`src/media/`, `src/services/media.rs`,
`src/repositories/media.rs`, `src/http/media*.rs`) follows the same layering as
the rest of the codebase — with one extra rule: **metadata in the database,
bytes in storage**. The database never stores binaries.

```
HTTP /media/* (public serving)   /api/media* (REST)   /admin/media (SSR)
        ↓                                   ↓
        MediaService (services/media.rs) — upload pipeline, dedup, RBAC,
        references, URL building, maintenance
        ↓                ↓                  ↓
   Validator        Image processor    Repository (metadata only)
   (validate.rs)    (image.rs)
        ↓
   Storage (storage.rs)  →  LocalStorage (data/media/YYYY/MM/{uuid}.{ext})
                            (S3/R2/MinIO: optional build features, by design)
```

Key decisions:

- **StorageProvider abstraction.** `Storage` is an enum (`Local` today,
  `S3`/`R2`/`MinIO` as future optional variants) — enum dispatch like
  `CacheBackend` and `SearchProvider`, so calls stay monomorphic and the
  object-storage crates stay out of the default build. Selecting an unbuilt
  provider fails at startup instead of silently falling back.
- **Streaming first.** Multipart fields stream through SHA-256 into a staging
  file on the storage filesystem (`data/media/.tmp/`), committed by rename.
  Only raster images are ever buffered whole — and they are size-capped.
  Size limits abort the stream the moment the cap is crossed, before any
  validation or storage write.
- **Content hash.** The stored bytes' SHA-256 drives dedup (identical uploads
  reuse the existing record), strong ETags and `polaris media verify`.
- **URL ↔ key decoupling.** Public URLs are `/media/{uuid}.{ext}` (+
  `.{size}.{ext}` thumbnails); storage keys are `YYYY/MM/{uuid}.{ext}`. Moving
  to a CDN (`[media.cdn]`) or object storage never rewrites database rows.
  The serving handler always resolves the key from the record — a hostile URL
  cannot steer toward a filesystem path (and the storage layer re-validates
  every key against traversal).
- **Validation beyond extensions.** Filename cleaning (directory components,
  control bytes, Windows reserved names), extension whitelist, magic-byte
  sniffing cross-checked against the extension, per-kind size limits.
  SVGs additionally pass an allow-list sanitizer (scripts, event handlers,
  foreignObject and external references stripped; unsanitizable markup is
  rejected). Served SVGs/MIME-mismatches are `Content-Disposition: attachment`.
- **Image pipeline.** EXIF orientation is baked into pixels, sensitive EXIF
  stripped, thumbnails generated only for configured sizes and only when the
  source is larger (never upscaled). GIFs are never re-encoded (animation);
  AVIF passes through raw (decode support would be an optional feature).
  Decompression bombs are capped by pixel count.
- **Caching.** `/media` responses carry
  `Cache-Control: public, max-age=31536000, immutable` + hash ETags (uuid URLs
  are immutable per upload) and honor single-range `Range` requests. Media
  *metadata* is cached (namespace `media`) with cache-aside on the database —
  never the binaries.
- **RBAC.** Authors see and manage only their own uploads (server-side
  scoping; foreign items answer 404, not 403, so they cannot be enumerated).
  Editors and admins manage everything. Rules live in `services::media` —
  core decides, not themes or plugins.
- **References.** Saving posts/pages scans markdown for `/media/{uuid}` and
  records `media_references` rows; deleting referenced media requires
  `force`. Deleting media removes the original, every thumbnail, the database
  row and the search index entry.
- **Search.** Media metadata is indexed in the same `search_index` table
  (`ref_type = 'media'`, never mixed into public site search) and reachable
  through `GET /api/media?search=…`.
- **Maintenance.** `polaris media orphan` (both directions: records without
  objects, objects without records), `polaris media cleanup` (dry run unless
  `--apply`), `polaris media verify [--deep]` (size, deep SHA-256, thumbnail
  presence).

Configuration lives under `[media]` in `polaris.toml` — see
[polaris.toml.example](../polaris.toml.example) for storage provider, upload
limits per kind, image sizes/quality/format, EXIF stripping, SVG policy and
CDN base URL.

## Testing

```
tests/db_test.rs        migrations, settings upsert, parameter binding
tests/search_test.rs    FTS5 search, drafts excluded, title ranking,
                        safe highlight, pagination/sort/caps, index
                        maintenance, cache invalidation, suggest,
                        analytics, rebuild verification, HTTP endpoints
tests/config_test.rs    schema-generated settings pages (auth gate, form
                        rendering, validation errors, show_if, secret
                        encryption/masking, live Rhai reads after save)
tests/services_test.rs  users, posts (lifecycle, visibility, scheduling,
                        slugs), terms, comments moderation
tests/http_test.rs      homepage + ETag/304, post rendering, 404, security
                        headers, SEO feeds, API auth gate, admin login flow,
                        comment submission + honeypot
tests/media_test.rs     end-to-end media: upload pipeline (validation, SHA-256,
                        dedup, thumbnails, EXIF), public serving (immutable
                        cache, ETag/304, Range, traversal), REST API
                        lifecycle, RBAC scoping, folders/tags/copy,
                        references, search, orphan/cleanup/verify
tests/plugins_test.rs   loading, hook dispatch, content pipeline mutation,
                        HTTP routes, graceful failure
tests/theme_test.rs     default theme loads/renders, embedded fallback,
                        hot switching, path traversal
```

Unit tests live alongside the modules (markdown sanitizing, slug, time, xml,
config precedence, schema parsing/validation, config layering/encryption,
template parsing).

Conventions:

- Integration tests build a full `AppState` against a tempdir SQLite database
  (`tests/common/mod.rs`) — no mocks, the real stack runs.
- `cargo test -j 2` keeps peak memory low on small dev machines; the default
  job count is fine elsewhere.

## Adding a feature (checklist)

1. Can it be done with **less**? Polaris prefers removing a feature over
   growing the binary.
2. Does it belong in a plugin? Content transforms, integrations and extra
   pages usually do.
3. If core: add model → repository → service (with hooks) → handler →
   template. Add a test. Run `cargo clippy`.
