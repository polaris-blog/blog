# Polaris implementation checklist

Updated: 2026-09-25. Priorities target a reliable single-instance release.

## P0 — completed in this pass

- [x] Keep old theme/plugin directories until database restore commits; roll
  them back on replay failure, including newly introduced extensions.
- [x] Isolate same-name theme/plugin rollback paths. Stage replacement trees
  beside their destination before atomic rename, even when the configured
  temporary directory is on another filesystem.
- [x] Reload restored templates, scripts and configuration when extension
  names have not changed; also reload after extension-only restores.
- [x] Reject placeholder encryption keys; preserve automatically generated
  keys across restarts; explicitly reject wrong-key and malformed ciphertext.
- [x] Validate encrypted settings before restore mutates files or database.
  Document how to retain and reuse the original key on a fresh instance.
- [x] Add opt-in trusted proxy IPs for login/comment limits, right-to-left
  X-Forwarded-For processing, IPv4-mapped address normalization and tests.
- [x] Add CI for formatting, all-feature Clippy/tests, release builds and a
  PostgreSQL 16 / MySQL 8.4 database matrix.
- [x] Fix defects exposed by server tests: MySQL TEXT decoding through SQLx
  Any, a missing MySQL full-text query bind, and untranslated PostgreSQL
  placeholders when restoring the persisted instance key.

### Verification

- `cargo fmt --all -- --check`: passed.
- `cargo test --locked --offline --all-features -j 2`: 265 passed, 0 failed.
- `cargo clippy --locked --offline --all-targets --all-features -j 2 -- -D warnings`: passed.
- `tests/database_matrix.rs`: passed against temporary SQLite, PostgreSQL 16
  and MySQL 8.4 databases. Server tests used isolated local containers.
- CI YAML parsed successfully locally. Hosted GitHub Actions have not run in
  this workspace; the matrix commands were exercised locally instead.
- `cargo build --locked --offline --release -j 2`: passed (P0).

### Remaining boundaries

Restore still requires quiescent traffic/workers. Media merges are separate
from database commit; abrupt process death, uncertain commit outcomes,
concurrent restore/install and recovery after rollback I/O errors need further
work. The regression tests establish normal rollback behavior, not crash-safe
distributed transactions. Encryption key rotation is not implemented. The
server matrix is contract coverage, not exhaustive cross-database testing.

## P1 — in progress

- [x] Redis: configurable per-site namespace, restart-safe generations,
  cross-instance invalidation, isolated cache clearing and disconnect tests.
- [ ] Move scheduled publishing and automatic backups onto the persistent
  scheduler; add task-history retention and define queue backup behavior.
- [ ] Persist retryable search-index updates and media reconciliation work;
  test cancellation, failed database writes and orphan cleanup.
- [ ] Add liveness/readiness checks and operational metrics; measure startup,
  memory, latency and throughput with a reproducible dataset.
- [ ] Align README claims with implemented authentication, JavaScript usage
  and measured performance; add release metadata and license files, and keep
  browser regression scripts outside disposable build directories.

### Redis implementation and verification

- Explicit, validated `cache.redis.namespace` isolates sites/environments.
  Same-site instances read shared generations and await invalidation; fills
  retain the generation captured before loading their source.
- Startup/reconnection rotates a site epoch. Missing metadata receives random
  generations, preventing reuse of old data after metadata eviction. Clear is
  logical and site-scoped; old data expires at its existing TTL.
- Outages bypass caching and retry after 30 seconds, including startup
  failures. Recovery invalidates entries potentially made stale offline.
  Unsupported/invalid Redis configuration does not fall back to private memory.
- `cargo test --locked --offline --all-features -j 2`: 266 passed, 0 failed;
  3 real-server tests ignored by this ordinary suite.
- All 3 `redis_live` tests passed separately against temporary Redis 7.4:
  cross-instance invalidation, site isolation, restart, metadata eviction,
  stale fills, runtime disconnection and unavailable-at-startup recovery.
  A dedicated Redis service job now runs them in CI.
- Formatting, all-target/all-feature Clippy with warnings denied, and default
  all-target compilation passed. Example TOML and CI YAML parsed successfully.
  Hosted Actions have not run; this pass did not rebuild the release binary.

Remaining limits: database commit and Redis invalidation are not atomic.
Writer-only network partitions, process death or Redis rollback can leave old
entries on peers until TTL/recovery/clear. Startup makes same-site caches cold;
clearing does not reclaim Redis data immediately. Sessions, settings snapshots,
themes/plugins and markdown caches remain process-local, so this does not yet
establish complete multi-instance deployment support. Full deployment details
and the real-server test command are in DEPLOYMENT.md and DEVELOPMENT.md.

## P2 — product enhancements

- [ ] Draft autosave, unsaved-change warning, revision history and edit-conflict detection.
- [ ] Scoped, revocable API tokens and complete API documentation if external clients are needed.
- [ ] Markdown import/export, then WordPress migration and optional object storage.
