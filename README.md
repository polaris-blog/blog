# Polaris

**Fast · Lightweight · Secure · Extensible — a blog engine that feels like a native Rust application, not a web framework wrapped in Rust.**

[![CI](https://github.com/polaris-blog/blog/actions/workflows/ci.yml/badge.svg)](https://github.com/polaris-blog/blog/actions/workflows/ci.yml)
[![Release](https://github.com/polaris-blog/blog/actions/workflows/release.yml/badge.svg)](https://github.com/polaris-blog/blog/releases)
[![Docker Image](https://img.shields.io/github/v/release/polaris-blog/blog?label=GHCR&logo=docker&logoColor=white)](https://github.com/polaris-blog/blog/pkgs/container/blog)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Polaris is a dynamic blog system shipped as a **single binary**. Drop it on a low-end VPS, a NAS, a home server — anywhere — and run it. No Node.js, no npm, no Redis, no database server (with SQLite).

```
$ ./polaris serve
✦ polaris listening on 0.0.0.0:3000 — startup 38ms
```

## Highlights

| | |
|---|---|
| **Single binary** | `polaris` is all you deploy; themes and plugins live on disk |
| **Multi database** | SQLite (WAL, zero-config), MySQL, PostgreSQL behind one repository layer |
| **Fast** | async I/O (tokio + axum), connection pooling, ETag/304, in-memory render cache; homepage P95 well under 20ms on modest hardware |
| **Lightweight** | tens of MB of RAM at idle, no external services, ~100ms cold start |
| **Themeable** | [Tera](https://tera.netlify.app) templates + static assets on disk, hot switching, embedded fallback theme; default theme uses **zero JavaScript** |
| **Plugin-based** | sandboxed [Rhai](https://rhai.rs) scripts — hooks, events, routes, template filters, admin pages. No unstable native ABI |
| **Secure** | Argon2 password hashing, CSRF tokens, session hardening, login rate limiting, HTML-sanitized Markdown, parameterized SQL everywhere, path-traversal guards |
| **Complete blog** | posts, pages, categories, tags, comments with moderation, scheduled publishing, draft workflow |
| **Media library** | upload with magic-byte validation, SHA-256 dedup, thumbnails, WebP conversion, folders/tags, reference tracking — served with immutable cache + ETag + Range |
| **SSR admin** | server-rendered admin at `/admin` — no React/Vue, minimal JS |
| **SEO built in** | `/sitemap.xml`, `/robots.txt`, `/rss.xml`, `/atom.xml`, canonical, Open Graph, Twitter Card, JSON-LD |
| **REST API** | `/api/posts`, `/api/pages`, `/api/categories`, `/api/tags`, `/api/comments` — session or token auth |

## Install

Prebuilt binaries are attached to every [release](https://github.com/polaris-blog/blog/releases)
for five targets — Linux (static musl, UPX-compressed) and Windows in x86_64
and arm64, plus macOS Apple Silicon. Binaries are attached raw (no archive
wrapper); `SHA256SUMS` covers the exact released files.

| Download | Target | Notes |
|---|---|---|
| `polaris-vX.Y.Z-x86_64-unknown-linux-musl` | Linux x86_64 | static, no runtime deps |
| `polaris-vX.Y.Z-aarch64-unknown-linux-musl` | Linux arm64 | static, no runtime deps |
| `polaris-vX.Y.Z-aarch64-apple-darwin` | macOS Apple Silicon | |
| `polaris-vX.Y.Z-x86_64-pc-windows-msvc.exe` | Windows x86_64 | UPX-compressed |
| `polaris-vX.Y.Z-aarch64-pc-windows-msvc.exe` | Windows arm64 | |

```bash
# Example: Linux x86_64 (adjust the version and target)
V=v1.0.0
curl -fsSL -o polaris \
  "https://github.com/polaris-blog/blog/releases/download/${V}/polaris-${V}-x86_64-unknown-linux-musl"
chmod +x polaris
sha256sum -c <(grep linux-musl SHA256SUMS)   # verify
./polaris serve
```

Or with [Docker](#docker) — no download step at all.

## Quickstart

```bash
# 1. build (or grab a release binary)
cargo build --release
cp target/release/polaris /somewhere/polaris

# 2. lay out the working directory
mkdir myblog && cd myblog
cp /path/to/polaris.toml.example polaris.toml   # edit at least security.secret
cp -r /path/to/{themes,plugins} .

# 3. create your first user
./polaris user create admin --role admin

# 4. run
./polaris serve
```

Then open:

- `http://localhost:3000/` — the blog (a sample post and page are seeded on first run)
- `http://localhost:3000/admin` — the admin panel

SQLite needs no setup: `data/polaris.db` is created automatically in WAL mode.

### MySQL / PostgreSQL

```toml
[database]
driver = "mysql"        # or "postgres"
url = "mysql://user:pass@host:3306/polaris"
```

Run `./polaris migrate` (or keep `auto_migrate = true`) to create the schema. Business logic never talks to a specific database — repositories emit portable SQL and the `Db` layer translates placeholders per dialect.

## CLI

```
polaris serve [--host H] [--port P] [--config FILE]   start the blog
polaris migrate                                        apply pending migrations
polaris user create <name> [--email E] [--role R] [--password P]
polaris theme list | install <zip|dir> | enable <name> | disable <name> | remove <name>
polaris plugin list | install <zip|dir> | enable <name> | disable <name> | remove <name> [--remove-data]
polaris extension verify | logs                        integrity check / install audit trail
polaris search status | rebuild                        index health / full reindex
polaris backup create|list|verify|restore|delete|cleanup|schedule
                                                       backup & restore (see Admin → Backups)
polaris media orphan | cleanup [--apply] | verify [--deep]
                                                       library maintenance
```

Configuration precedence: `polaris.toml` < `POLARIS_*` environment variables < CLI arguments. Example:

```bash
POLARIS_SERVER_PORT=8080 polaris serve --port 9090   # -> 9090 wins
```

See [polaris.toml.example](polaris.toml.example) for every option.

## Working directory

```
polaris            # the binary
polaris.toml       # configuration
data/
├── polaris.db     # SQLite database (default)
├── media/         # uploaded media (date-bucketed, content-hash named)
├── tmp/extensions/   # staging area for uploaded packages (auto-cleaned)
└── backups/extensions/  # version backups taken before updates/downgrades
themes/
└── default/       # the active theme lives on disk, never in the binary
plugins/
├── example/       # sample plugin (disabled by default)
└── turnstile/     # Cloudflare Turnstile entry gate (disabled by default)
```

## Extensions (theme & plugin packages)

Themes and plugins are both **extensions**: offline ZIP packages installed
through one pipeline — upload, validate, install, update, downgrade,
uninstall — shared by the admin UI and the CLI.

```
aurora.zip
└── aurora/
    ├── theme.toml      # id, name, version, author, description, license
    ├── templates/     # index, post, page, archive, 404, …
    └── assets/        # css, js, images
```

A plugin package is the same idea with `plugin.toml`, its entry script and an
optional `migrations/` directory (SQL applied transactionally on install and
update; tables survive uninstall unless you pick *Remove + data*).

**Install flow** (identical for admin upload, `POST /api/admin/extensions/upload`,
and `polaris theme|plugin install pkg.zip`):

```
stream to data/tmp/extensions/ → size/ZIP limits → path & symlink scan
→ manifest validation → id/version rules → Polaris compatibility range
→ dependency versions → extract to staging → re-validate → backup old
version → atomic rename → plugin migrations → registry + audit log
```

- **No marketplace, no internet** — a ZIP file is the only input.
- **Safe extraction**: zip-slip, traversal, symlink, zip-bomb and executable
  uploads are rejected before anything is written; extraction always lands in
  staging and only reaches `themes/`/`plugins/` after every check passes.
- **Nothing auto-activates**: a new theme installs inactive, a new plugin
  installs disabled — the admin activates/enables explicitly. Plugin
  permissions declared in the manifest are surfaced at install time.
- **Updates keep a backup** (`data/backups/extensions/<id>-<version>.zip`)
  and roll back automatically if a plugin migration fails.
- **Audit trail**: every install/update/downgrade/uninstall is recorded
  (actor, action, version, result) and shown in **Admin → Extension log**.
- **Limits are configurable**: `[extensions.upload] max_file_size`,
  `max_uncompressed_size`, `max_files`.

Uninstall rules: the active theme cannot be removed; plugin data is kept by
default. `polaris extension verify` re-checks manifests, the registry and
plugin entry files at any time.

## Themes

A theme is a directory under `themes/`:

```
themes/default/
├── theme.toml          # name, version, author, description
├── templates/          # index, post, page, category, tag, 404 (+ partials)
└── static/             # served at /static/ (css, js, images)
```

- Switch at runtime: **Admin → Themes → Activate**, or `polaris theme enable <name>` — no restart.
- Missing templates fall back to an embedded default, so partial themes work.
- Template context is plain data (site, seo, posts, pagination, …) — templates cannot touch the filesystem, database or OS.

The full template context and filter list is documented in `docs/DEVELOPMENT.md`.

## Media library

**Admin → Media** is a file-manager-style library: grid/list views, drag-and-drop
and paste uploads with progress, folders, tags, batch delete/move/tag, metadata
editing and reference checks before deletion.

- **Storage is abstracted** behind a provider interface (local disk by default;
  S3/R2/MinIO designed as optional build features). Files live in
  `data/media/YYYY/MM/{uuid}.{ext}` — never under their original name.
- **Metadata in the database, bytes in storage.** The database never stores
  binaries; `hash` is the SHA-256 of the stored bytes and drives dedup,
  ETags and `polaris media verify`.
- **Uploads are validated beyond the extension**: filename cleaning, extension
  whitelist, magic-byte sniffing, per-kind size limits enforced while streaming.
  `evil.php` renamed to `evil.jpg` is rejected. SVGs are sanitized with an
  allow-list (scripts, event handlers and external references stripped).
- **Images**: EXIF orientation baked in, sensitive EXIF stripped, thumbnails in
  configurable sizes (never upscaled), optional WebP/JPEG/PNG conversion.
- **Serving**: `/media/{uuid}.{ext}` (and `.{size}.{ext}` thumbnails) with
  `Cache-Control: public, max-age=31536000, immutable`, strong hash ETags and
  single-range `Range` support for audio/video seeking.
- **References**: posts and pages embedding a media URL are tracked — deleting
  an in-use item requires confirmation.
- **URLs are decoupled from storage keys**, so enabling a CDN later is a
  config-only change.

API: `POST /api/media/upload`, `GET/PUT/DELETE /api/media/:id`,
`GET /api/media?search=&type=&folder=&tag=`, plus copy/move/batch/folder
endpoints (session auth + CSRF). Authors only see their own uploads; editors
and admins manage everything.

## Plugins

Plugins are sandboxed Rhai scripts — no native code, no filesystem; outbound
HTTP only through the permission-gated `network.fetch` capability:

```
plugins/example/
├── plugin.toml     # metadata + permissions + [routes] / [admin_routes] / [filters]
├── main.rhai       # hooks & handlers
└── config.toml     # optional plugin configuration
```

Hook surface (excerpt):

```
before_post_create   after_post_create     markdown_before / markdown_after
before_post_update   after_post_update     before_comment_create
before_post_delete   after_post_delete     user_login (event)
nav()                init(config)
```

Host functions (excerpt): `log`, `now` / `now_iso`, `cache_get/set/del` (namespaced),
`config_get*` (read-only), `json_parse` / `json_stringify`, crypto & encoding
helpers (`sha256_hex`, `hmac_sha256_hex` for webhook signatures,
`base64_encode/decode`, `url_encode`, `sign_hex` for plugin-scoped signed
tokens), and — for plugins declaring `permissions = ["network.fetch"]` —
SSRF-guarded `http_get` / `http_post` / `http_request` returning
`{status, body, content_type, headers, error}`.

Plugins declaring `permissions = ["request.guard"]` can define a
`request_guard(req)` hook that runs on every content-page request (input:
method, path, query, cookies, ip, user agent, has-session) and may redirect
or respond directly, with page-scoped response **headers** that override the
security middleware's same-named defaults (a gate page whitelisting a widget
origin in its CSP, for example).

The bundled **Turnstile Gate** plugin uses this to put a
[Cloudflare Turnstile](https://developers.cloudflare.com/turnstile/) human
verification in front of the blog: visitors without a signed clearance cookie
get an embedded challenge, the token is verified server-side against
Turnstile's siteverify endpoint (requires `network.fetch`), and passed
visitors keep a configurable-lifetime clearance. Site/secret keys, exemptions
(signed-in users, path prefixes, crawler user agents) and the widget look are
configured under **Admin → Plugins → Turnstile Gate → Settings**;
**Admin → Plugins → Turnstile Gate** shows challenge/pass/failure counters and
a list of recent failed verifications (suspected bots).

Enable at runtime: **Admin → Plugins → Enable**, or `polaris plugin enable example`. Full API: `docs/DEVELOPMENT.md`.

## REST API

```
GET    /api/posts            GET  /api/posts/:id
POST   /api/posts            PUT  /api/posts/:id      (auth required)
DELETE /api/posts/:id        (auth required)
GET    /api/pages            GET  /api/categories
GET    /api/tags             GET  /api/comments
```

Reads are public; writes require an authenticated session (or `Authorization: Bearer <token>` where enabled). Plugins may expose additional endpoints under `/plugins/*`.

## Security notes

- **Passwords**: Argon2id via the `argon2` crate.
- **Sessions**: server-side store, random IDs, HttpOnly + SameSite cookies, expiry.
- **CSRF**: per-session token required on every admin/API mutation.
- **Brute force**: per-IP login rate limiting.
- **XSS**: Markdown output is sanitized (raw HTML stripped, `javascript:`/`data:` links neutralized); all template output is escaped unless explicitly trusted.
- **SQL injection**: parameterized queries only, across all three databases.
- **Path traversal**: static file serving and theme/plugin installers validate paths.
- Still: put Polaris behind HTTPS in production (see `docs/DEPLOYMENT.md`).

## Docker

Multi-arch images (`linux/amd64` + `linux/arm64`, Alpine-based, static musl binary,
non-root, health-checked) are published to GHCR on every release. The image ships
with default settings, so it works out of the box:

```bash
docker run -d --name polaris \
  -p 3000:3000 \
  -v polaris-data:/app/data \
  ghcr.io/polaris-blog/blog:latest
```

Or with Compose:

```yaml
services:
  polaris:
    image: ghcr.io/polaris-blog/blog:latest
    ports:
      - "3000:3000"
    volumes:
      - polaris-data:/app/data
    restart: unless-stopped

volumes:
  polaris-data:
```

**First run — the setup wizard:**

1. Open `http://localhost:3000/admin` (from v1.0.1 the homepage redirects there
   automatically while no account exists).
2. Step 1 keeps the bundled defaults (SQLite on the data volume) — just save.
3. Step 2 creates the administrator account; then sign in.

> **Do not mount `polaris.toml` on the first run.** The setup wizard writes the
> configuration inside the container, and a host-mounted file is not writable
> by the container user (`permission denied`). Site settings live in the
> database anyway — mount a config file only after setup, only if you need to
> pin the database/cache/secret, and then mount it read-only.

All persistent state (database, media, uploads) lives on the `polaris-data`
volume. Tags: `latest` and `X.Y.Z` track releases.

**Behind a CDN or reverse proxy:** Polaris speaks **plain HTTP** — it does not
terminate TLS. Point your CDN's back-to-origin at `http://<origin>:3000`
(**not** HTTPS — an HTTPS origin fetch against a plain-HTTP server fails with
"Origin Refused" style 502s), allow the CDN's back-to-origin IP range in your
firewall, and set **Settings → Site → Base URL** to your public
`https://domain` so canonical/RSS/sitemap URLs match what visitors use
(`X-Forwarded-Proto: https` is also honored when Base URL is empty).

Build the image locally with `docker build -t polaris .` — the multi-stage
Dockerfile compiles a static musl binary inside a Rust/Alpine builder and ships
it in a ~20 MB runtime image.

## Documentation

Developer docs live in the working tree and are not part of the public repository:

- `docs/DEVELOPMENT.md` — architecture, theme API, plugin API, testing
- `docs/DEPLOYMENT.md` — binary, systemd, Docker, reverse proxy, backups, hardening
- `docs/SCHEDULER.md`, `docs/DETAIL_OPTIMIZATION.md`, `docs/TODO.md` — internals, tuning, roadmap

## Development

```bash
cargo build                                  # debug build
cargo test                                   # unit + db + http + services + search + media + plugins + themes + extensions
cargo clippy --all-targets --all-features    # clean
cargo build --release                        # optimized single binary
```

Rust 2024 edition. The dependency set is deliberately small: tokio, axum, tower-http-less (hand-rolled middleware where lighter), sqlx, tera, rhai, argon2, pulldown-cmark, clap, tracing, thiserror/anyhow.

### Binary size

Release builds are tuned to stay small: `lto = "fat"`, one codegen unit, symbols
stripped at link time. Release binaries for Linux and Windows x86_64 are
additionally compressed with UPX (`--best --lzma`) in CI; macOS binaries are
not (compressed Mach-O images are killed by code signing on modern macOS).
The scheduler deliberately keeps unwinding enabled (`catch_unwind` isolates
job panics), so `panic = "abort"` is intentionally not used.

## Project layout

```
src/
├── main.rs               CLI (serve / migrate / user / theme / plugin / extension)
├── config.rs             TOML + env + CLI layered configuration
├── state.rs              AppState, settings cache, markdown cache
├── error.rs              typed application errors
├── markdown.rs           safe Markdown rendering + text extraction
├── db/                   SQLx AnyPool wrapper, dialects, migrations
├── models/               domain types (Post, Page, Term, Comment, User…)
├── repositories/         SQL, per aggregate
├── services/             business rules + plugin hook dispatch
├── http/                 public pages, admin SSR, REST API, SEO, static files
├── auth/                 passwords, sessions, CSRF, rate limiting
├── extension/            ZIP package pipeline (manifest, package, installer)
├── templates/            embedded admin + fallback theme templates
├── themes/               theme manager (disk loading, hot switching)
├── plugins/              Rhai plugin manager
└── utils/                time, slug, xml, cookies, hash
```

## License

MIT OR Apache-2.0.
