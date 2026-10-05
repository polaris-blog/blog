# Polaris — Deployment Guide

Polaris is a single static binary plus a working directory. It runs on any
Linux, macOS or Windows machine; SQLite mode needs no database server, no
Node.js, no Python, no Redis.

```
your-blog/
├── polaris            # the binary (target/release/polaris)
├── polaris.toml       # configuration
├── data/              # created automatically — SQLite database lives here
├── themes/            # at least themes/default/
└── plugins/           # optional
```

## 1. First-run checklist

```bash
./polaris user create <you> --role admin --password '<strong password>'
./polaris serve
```

Before going public:

1. **Retain the instance encryption key.** An empty `security.secret` generates
   and persists a random key on first startup. Alternatively configure a
   stable random value (`openssl rand -hex 32`). Keep a separate secure copy
   for portable restores; `CHANGE_ME` is rejected.
2. **Set `site.base_url`** to your final HTTPS URL (canonical links, RSS,
   sitemap all use it; leave empty only when serving from the Host header).
3. Bind to `127.0.0.1` and put a TLS-terminating reverse proxy in front
   (recommended), or terminate TLS directly if you prefer.

## 2. Running

### systemd (Linux)

`/etc/systemd/system/polaris.service`:

```ini
[Unit]
Description=Polaris blog
After=network.target

[Service]
User=polaris
Group=polaris
WorkingDirectory=/opt/polaris            # contains polaris, polaris.toml, themes/, plugins/, data/
ExecStart=/opt/polaris/polaris serve
Restart=on-failure
RestartSec=2
# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/opt/polaris/data
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --home /opt/polaris --shell /usr/sbin/nologin polaris
sudo systemctl daemon-reload
sudo systemctl enable --now polaris
journalctl -u polaris -f
```

### Docker

```bash
docker build -t polaris .
docker run -d --name blog \
  -p 3000:3000 \
  -v polaris-data:/app/data \
  -v $(pwd)/polaris.toml:/app/polaris.toml:ro \
  -v $(pwd)/themes:/app/themes:ro \
  polaris
```

The image is a distroless-style minimal runtime; the binary is fully static
against a musl target by default (see the Dockerfile).

### Manual / Windows / macOS

```bash
./polaris serve --host 127.0.0.1 --port 3000
```

`POLARIS_*` environment variables override the config file:
`POLARIS_SERVER_PORT=8080`, `POLARIS_DATABASE_URL=…`, etc.

### Optional shared Redis cache

Build with `cargo build --release --features redis`, then configure:

```toml
[cache]
driver = "redis"

[cache.redis]
url = "redis://127.0.0.1:6379"
namespace = "myblog-production"
```

`namespace` is required in Redis mode: 1–128 ASCII letters, digits, `.`, `_`
or `-`. All instances serving the same site's database must use the same
namespace. Every other site or environment sharing Redis must use a different
one. Do not derive it from a request Host header. Configure these nested
settings in TOML; the environment parser currently splits only the first
underscore (`POLARIS_CACHE_REDIS_NAMESPACE` is not a nested override).

Cache data and metadata use `polaris:v2:{namespace}:*`. Shared generations
make successful invalidations visible to subsequent reads on every instance;
each read consults Redis, with no pub/sub delivery or polling delay. Data loads
capture a generation before reading the database, so an older in-flight load
cannot refill the new generation. Startup and reconnection rotate the entire
site's epoch; adding or restarting an instance therefore makes the site's
cache cold. Random generation tokens prevent old values reappearing when
Redis loses or evicts generation metadata. Old `polaris:*` keys from previous
versions are unused and expire normally. Upgrade all instances together.

Admin clearing is a **logical clear** of this site's cache. Old data keys are
left to expire at their original TTL; memory is not reclaimed immediately.
The operation neither scans other sites nor deletes shared generation
metadata. Per-instance plugin caches stay local and are cleared only on the
instance receiving the admin request.

Redis errors bypass the cache for 30 seconds; the next operation then retries
with a 3-second connection/initialization bound and 500 ms command timeout.
Recovery rotates the site epoch before caching resumes, covering invalidations
missed during the outage. Redis unavailability at startup follows the same
retry path. Invalid Redis URLs or a binary built without Redis support disable
caching with a warning; they do not select a private memory cache. Normal
database operations continue during outages; explicit admin cache clearing
reports Redis failure. The Redis account needs `EVAL`, `HGET`, `HSET`, `GET`,
`SETEX`, `DEL` and `EXISTS` on its prefix, plus connection-handshake commands.

This is not a database/Redis transaction: a process crash between database
commit and invalidation, or a network partition affecting only the writer,
can leave other instances serving old entries until TTL, recovery or a manual
clear. Redis failover restoring older persisted state has the same limitation.
Redis cache sharing also does not synchronize sessions, settings snapshots,
loaded themes/plugins or markdown caches held in application memory. Treat it
as cache coordination, not complete multi-instance deployment support; use
coordinated restarts for runtime configuration/extension changes.

## 3. Reverse proxy

Configure the exact IP addresses of your proxies in `polaris.toml`:

```toml
[security]
trusted_proxies = ["127.0.0.1", "::1"]
```

The default is `[]`: forwarded headers are ignored. Login and comment rate
limits use `X-Forwarded-For` only when the connecting peer is trusted, walking
the chain from right to left until the first untrusted address. Every trusted
proxy must append the connecting peer or replace the header with a verified
client address. Do not list client networks; this setting accepts exact IPs,
not CIDRs. IPv4-mapped IPv6 addresses are normalized to IPv4. Malformed or
overlong chains fall back to the peer address. Environment equivalent:
`POLARIS_SECURITY_TRUSTED_PROXIES='["127.0.0.1","::1"]'`.

### nginx

```nginx
server {
    listen 443 ssl http2;
    server_name blog.example.com;

    ssl_certificate     /etc/letsencrypt/live/blog.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/blog.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

### Caddy

```caddy
blog.example.com {
    reverse_proxy 127.0.0.1:3000
}
```

Polaris already sends `X-Content-Type-Options`, `Referrer-Policy` and friends;
add HSTS at the proxy if desired.

## 4. Databases

### SQLite (default)

- File at `data/polaris.db`, created automatically in **WAL mode**.
- Backups: use the SQLite online backup or simply
  `sqlite3 data/polaris.db ".backup '/backup/polaris-$(date +%F).db'"`.
  (Or stop the service and copy the file — WAL makes hot copies risky.)
- Sizing: a personal blog is a few hundred KB. Keep `max_connections` modest
  (default 10 is fine).

### MySQL / PostgreSQL

```toml
[database]
driver = "postgres"     # or "mysql"
url = "postgres://user:pass@db-host:5432/polaris?sslmode=require"
auto_migrate = true     # or run `./polaris migrate` manually during deploys
```

Recommendations: dedicated database and user; TLS to the DB host; run
`polaris migrate` as a release step in CI rather than `auto_migrate` if you
prefer explicit migrations. Backups via `pg_dump` / `mysqldump` as usual.

## 5. Updates

```bash
# build the new version, then:
systemctl stop polaris
mv polaris polaris.old && cp /path/to/new/polaris .
systemctl start polaris
```

- Database schema migrations run on boot when `auto_migrate = true`
  (additive by design; take a backup first).
- `data/`, `themes/` and `plugins/` are untouched by binary upgrades.
- Roll back by restoring the previous binary (and the DB backup if a
  migration ran).

## 6. Backups

Minimum viable backup: the `data/` directory (or DB dumps) plus any edits to
`themes/`. Everything else is reproducible from the binary and config.

```bash
# nightly SQLite backup
0 3 * * * sqlite3 /opt/polaris/data/polaris.db ".backup '/backups/polaris-$(date +\%F).db'"
```

`data/media/` holds the uploaded media (the database stores metadata only) —
include it in backups, or mirror it to object storage/CDN. After a restore or
storage migration, `polaris media verify` checks that every record still
matches its object, and `polaris media orphan` lists stray files either way.

### Encryption keys and portable restores

`security.secret = ""` generates a random key on first startup and persists
it in the database's `settings` table as `security.secret`. Alternatively,
configure a stable random key in the file or `POLARIS_SECURITY_SECRET`.
The placeholder `CHANGE_ME` is rejected. Keep a secure copy of the original
key separately from portable backup ZIPs: those deliberately exclude it.
For an automatically generated key, retrieve that settings row using a
trusted database administration tool and store it in your secret manager.
Do not put it in logs, source control or support tickets.

To move a backup containing encrypted theme/plugin settings to a fresh
instance, configure the original key **before starting the target instance**,
then restore the archive. A wrong key or malformed encrypted value causes
restore to fail before any snapshot, file replacement or database mutation.
Startup also rejects stored encrypted settings that the configured key cannot
decrypt. Changing a key is not key rotation: decrypt-and-reencrypt migration
is not implemented. Existing deployments using `CHANGE_ME` must migrate or
remove and re-enter their encrypted settings before changing the key.

Restore while normal traffic and scheduler workers are stopped. Old theme
and plugin directories are retained until database replay commits; ordinary
replay errors roll back those directory changes, including newly introduced
extensions. Rollback failures retain recovery files and log their location.
This does not provide a distributed transaction or crash recovery: media
files still merge separately, and forced process termination or an uncertain
database commit requires manual recovery. Same-name restored extensions and
their configuration are reloaded after a successful restore.

## 7. Security hardening

Already built in:

- Argon2id password hashing, per-session CSRF tokens, HttpOnly/SameSite
  session cookies, server-side session store with expiry.
- Per-IP login rate limiting; honeypot + rate limiting on public comments.
- Markdown rendered with raw HTML stripped and dangerous link schemes
  neutralized; template output escaped by default.
- Parameterized SQL on all three databases; path traversal blocked in static
  file serving and theme/plugin installation.
- Plugin scripts sandboxed: no filesystem, network or process access.

Operational recommendations:

- Run as a dedicated unprivileged user (see the systemd unit).
- Serve over HTTPS only; redirect HTTP → HTTPS at the proxy.
- Keep `security.secret` out of version control.
- Limit exposure of `/api/*` writes to authenticated users (the default) and
  put an IP allow-list or basic auth in front of `/admin` if you want a second
  factor at the proxy level.
- Watch logs: `journalctl -u polaris` — failed logins, plugin errors and
  scheduled publishing are all logged.

## 8. Troubleshooting

| Symptom | Check |
|---|---|
| `error: ... failed to open database` | `data/` exists and is writable by the service user |
| Placeholder `security.secret` rejected | supply a random key, or leave empty for automatic generation; preserve the original key for existing encrypted settings |
| Site renders unstyled | `themes/<name>/static/` missing or wrong file permissions |
| 404 on every page | theme missing → embedded fallback should render; check logs for theme load errors |
| Admin login loop | cookies blocked; ensure HTTPS or `localhost` origin, and correct `site.base_url` |
| Slow first page after deploy | cold render cache; subsequent requests are warm |
| Logs too quiet / noisy | `RUST_LOG=polaris=debug` (or `sqlx=warn`) |

## 9. Performance expectations

On a modest 1-vCPU VPS with SQLite:

- Cold start: ~100ms (logged as `startup_ms` at boot).
- Idle RSS: well under 50MB.
- Homepage P95: single-digit to low-double-digit milliseconds warm.

If you need more headroom: raise `site.posts_per_page` only as needed, keep
plugins few (each enabled plugin adds hook dispatch cost), and prefer SQLite
WAL on local disk over a network filesystem.
