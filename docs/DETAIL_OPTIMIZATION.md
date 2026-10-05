# Polaris Detail Optimization

## Scope

This pass inspected the HTTP/admin routes, authentication, repositories and
dialect migrations, caching/search, media processing/storage, extension
installation, backup/restore, scheduler integration, and shared admin assets.
The changes address concrete defects without changing the service/repository
architecture, adding runtime dependencies, or removing existing features.
This is a targeted code and regression-test audit, not a certification that
every API, deployment mode, or security boundary is defect-free.

## Fixes

### Backend and Security

- Post/page edit GET handlers now enforce the same ownership rules as edits.
  Authors can no longer retrieve another owner's draft through these forms.
- The backup listing requires an administrator; its navigation link is hidden
  from other roles.
- Logout validates the existing session CSRF token.
- Non-JSON API routing/extractor errors use the existing `error.code/message`
  envelope and preserve HTTP status and response headers, including `Allow`.
- Administrator deletion/demotion is guarded inside a database transaction.
  A write lock precedes the administrator count so concurrent operations
  cannot both pass an obsolete count. The delete service also checks role.
- Redis URLs and credentials are no longer included in startup logs. Redis
  degradation logs report error kind rather than raw server error content.
- Plugin enabled-name reads use the existing poisoned-lock recovery helper.
- Plugin SQL migration work and its applied marker share one transaction
  where the database supports transactional DDL.
- `search` is reserved as an engine-owned top-level page slug.

### Correctness and Performance

- Post/comment/media page offsets use saturating multiplication; oversized
  page numbers produce empty pages rather than overflow or wraparound.
- Post/comment ordering has an ID tie-breaker. Media already had one.
- Public post page-size metadata matches the repository's 100-row cap.
  Media search no longer wraps an i64 page number into u32; search result
  pages have a 100-row hard ceiling.
- Media search resolves a page of IDs in one query and attaches tags in one
  query, replacing up to two metadata queries per result while preserving rank.
- Cache fills retain the generation observed before loading. Invalidation and
  full cache clearing cannot make an old fill current again. All production
  manual JSON cache-aside fills now use this mechanism.
- Page-cache single-flight includes filling the cache before releasing the
  lock. Requests with sessions, Authorization or query parameters bypass it;
  responses with Set-Cookie, Vary or private/no-store/no-cache do not enter it.
- Compression respects gzip quality values, HEAD and partial responses.
  Bodies over 4 MiB or of unknown size stream without buffering/compression;
  page caching also bypasses these bodies. Previously they became 400/500.
- Redis connection establishment has a 3-second overall budget, a 2-second
  connection timeout and a 500-ms response timeout. Failures retain the
  existing fallback/degradation behavior.
- Image dimensions are checked before full decoding. Processing runs through
  `spawn_blocking`, bounded to two concurrent image processors. Thumbnail
  resizing preserves aspect ratio.

### Media Reliability

- Every storage write uses a unique sibling temporary file and atomic rename.
  Writes to `same.jpg` and `same.png` no longer share `same.part`.
- Failed replacement never deletes the destination. Staged-file copy fallback
  and duplication also commit through a temporary sibling, avoiding partial
  destination visibility.
- Storage keys reject Windows drive/ADS syntax, reserved device names and
  ambiguous trailing dots/spaces, in addition to existing traversal checks.
- Reversed byte ranges do not underflow.
- Upload errors returning normally remove the staging file. Deduplication
  no longer deletes staging before confirming the existing row still exists.
- Deduplication is scoped to the uploader, preventing another user's metadata
  from being returned for identical content.
- Sanitized SVG size reflects stored bytes, keeping HTTP length/ranges correct.

### Admin UI

- Shared submission guard blocks duplicate native POST submission, preserves
  form fields/named submitters, and resets on back/forward restoration.
- Delete actions and extension uninstall use one confirmation handler;
  scheduler confirmation pages and typed restore confirmation are retained.
- Removed the artificial 150-ms navigation delay.
- Shared tables have keyboard-focusable horizontal scroll regions, column
  scopes, and no document-level horizontal overflow in tested viewports.
- Added skip-to-content, current navigation state, live success notices and
  alert error notices; normalized letter spacing.
- Mobile form actions wrap without squeezing Save/Cancel labels.
- Extension uploads reject duplicate submission while pending and report
  network, timeout and cancellation errors inline.
- Media upload batches are serialized; one batch cannot reload the page while
  another is running. Error messages stay visible rather than disappearing
  on an automatic reload. Existing successful-upload refresh is retained.
- Editor heading/list actions now apply their prefixes. Code blocks replace
  selected text without duplicating it. Preview requests are deduplicated and
  cancelled on mode switches so late responses cannot overwrite a newer one.
- Job pagination preserves type/limit/status and displays success messages.

## Changed Files

- `src/cache/mod.rs`, `src/cache/redis.rs`
- `src/http/mod.rs`, `src/http/admin.rs`, `src/http/admin_backup.rs`, `src/http/admin_jobs.rs`
- `src/repositories/posts.rs`, `src/repositories/comments.rs`, `src/repositories/media.rs`
- `src/repositories/users.rs`, `src/repositories/extensions.rs`
- `src/services/posts.rs`, `src/services/media.rs`, `src/services/users.rs`
- `src/search/service.rs`
- `src/media/mod.rs`, `src/media/image.rs`, `src/media/storage.rs`
- `src/plugins/mod.rs`, `src/extension/installer.rs`
- `src/templates/admin/admin.css`, `src/templates/admin/admin.js`, `src/templates/admin/base.html`
- `src/templates/admin/editor.js`, `src/templates/admin/extensions.js`, `src/templates/admin/media.js`
- `src/templates/admin/jobs.html`
- `tests/detail_regressions.rs`, `tests/media_test.rs`
- `docs/DETAIL_OPTIMIZATION.md`

No schema migration or dependency change was needed. SQLite/MySQL/PostgreSQL
migrations 0001-0005 remain unchanged. The workspace has no `.git` directory,
so this is an edit inventory, not a Git-generated diff.

## Verification

| Check | Result |
| --- | --- |
| `cargo fmt` | Passed |
| `cargo check` | Passed |
| `cargo test` | 253 passed, 0 failed, 0 ignored |
| `cargo clippy` | Passed |
| `cargo clippy --all-targets --all-features -- -D warnings` | Passed on final source |
| `cargo build --release` | Passed, including final mobile CSS fix |
| `cargo test --test media_test` | 20 passed after adding cross-user dedup, SVG length and staging-cleanup assertions |
| JavaScript syntax checks | All four changed scripts passed `node --check` |
| Browser audit | 96 successful page/viewport/color-scheme combinations; no page JS errors or document overflow |
| Browser interactions | Delete cancellation, duplicate submit, form reset, editor formatting/preview, upload rejection and keyboard skip link passed |
| Mobile action-label follow-up | Passed; Save/Cancel remain on one line and the hint wraps separately |

The 96-case browser audit was repeated successfully against the final release
binary. The preview runs at `http://127.0.0.1:3011` from a separate copy of the
release executable under `target/scheduler-preview`, avoiding Windows build
file locks. It uses the existing isolated preview account/database, not
`data/polaris.db`.

Browser audit used Chrome via Playwright, isolated preview data, 16 admin
pages, widths 1440/768/390, and light/dark modes. Screenshots and detailed
results are in `target/scheduler-ui/audit-*`; reproducible scripts are
`detail-audit.cjs` and `mobile-actions.cjs` in that directory. Playwright is
installed only under `target`, not added as an application dependency.

The Rust suite includes existing backup/restore, extension install/update/
uninstall, plugin error isolation, search invalidation, media validation,
scheduler locks/recovery and SQL transaction tests. New regressions exercise
cache fill races, large/streaming responses, invalid API parameters, ownership,
extreme pagination, concurrent admin demotion, migration-marker rollback,
storage replacement/concurrency and thumbnail aspect ratio.

One intermediate Clippy run rejected redundant borrows/branches; these were
fixed before the passing checks. A supplemental test build initially failed
because the running Windows debug executable was locked. Moving the preview
to a separate executable copy resolved it; the rerun passed. The final CSS
adjustment was followed by a release rebuild and browser rerun; it did not
change Rust behavior.

Performance claims above describe removed work/bounded resource use. No
production throughput, latency, memory, startup-time or benchmark figures
were measured.

## Remaining Limitations / Unverified

- MySQL and PostgreSQL migrations, queries and concurrent lock behavior were
  reviewed but **not verified against live servers**. Tests use isolated SQLite.
  MySQL implicitly commits DDL; arbitrary plugin DDL cannot be made atomic
  simply by wrapping it in a transaction.
- Redis code compiles with the feature enabled. Live-server disconnect,
  black-hole networking, reconnection and fallback timing are **unverified**.
- Cache namespace versions are process-local. Cross-instance invalidation
  and Redis key isolation across restarts/sites remain unresolved design
  limitations; this pass fixes in-process fill races only.
- Filesystem and database commits are separate. A DB failure after storing
  media, a cancelled upload future or process crash can leave orphan files.
  Existing verify/orphan cleanup remains necessary; crash/fault injection of
  these paths is **unverified**. Deduplication is not a uniqueness guarantee
  for simultaneously uploaded identical files.
- Path validation does not prevent a privileged local process from replacing
  storage directories with symlinks/reparse points during access. Such races
  and remote object-storage failures are **unverified**; this build ships local
  storage only.
- Search index updates remain best-effort relative to the authoritative DB.
  Recovery after DB/search failure mid-mutation is **unverified**; the existing
  rebuild command remains the repair mechanism.
- Scheduler tables are still excluded from blog backup format, as documented
  in `SCHEDULER.md`.
- Native-form duplicate protection is client-side; it is not a general
  server-side idempotency protocol. Admin delete confirmation needs JS except
  for the existing explicit scheduler/restore confirmation pages.
- Normal task/restart/error paths are tested; exhaustive API permission/CSRF
  enumeration, full penetration testing, sustained high load, maximum-size
  uploads, screen readers and non-Chromium browsers are **unverified**.
