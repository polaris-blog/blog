# Task Scheduler

Polaris stores jobs in the configured SQLite, MySQL or PostgreSQL database.
No queue server is required. The server starts the scheduler; other CLI commands
initialize its services without starting workers.

## Configuration

Use the existing `polaris.toml` configuration and `POLARIS_SCHEDULER_*` overrides.
Durations are integer seconds, as in the existing Polaris configuration.

```toml
[scheduler]
enabled = true
workers = 4
poll_interval = 1
default_timeout = 300
max_retries = 3
```

Workers must be 1..64, polling 1..60 seconds, timeout 1..86400 seconds,
and retries 0..100. Concurrency is per instance. Disabled schedulers still accept
durable jobs; enabling the server scheduler later executes due jobs.

## Architecture

`HTTP / Rhai -> Scheduler service -> Job domain -> JobRepository -> SqlJobRepository -> Db`

- `src/scheduler/job.rs`: state transitions and bounded request validation.
- `scheduler.rs`: registration, creation, controls, dispatch and recovery.
- `executor.rs` / `worker.rs`: handler contract, cancellation, panic isolation.
- `cron.rs` / `retry.rs`: calendar scheduling and bounded backoff.
- `repository.rs`: database-independent persistence interface.
- `src/repositories/jobs.rs`: transactional SQL implementation using existing Db.
- `src/services/jobs.rs`: business adapters, separate from the scheduling engine.

Dispatch uses a bounded JoinSet with at most `workers` active executions, never
one permanent task per scheduled job. Polling skips missed ticks; local mutations
notify the dispatcher immediately. Other instances see changes on their next poll.
Due jobs are selected in descending priority, then scheduled time and insertion
order. Expired leases are recovered in bounded batches. Running jobs check their
lease/cancellation once per second; lease queries cannot block their timeout.

## Persistence and State

Migration `0005_scheduler.sql` is embedded for all three databases. It adds:

- `scheduler_jobs`: complete serialized Job document and indexed projections
  for ID, status, schedule, priority, pause, type, owner, version and lease.
- `scheduler_attempts`: one durable record per attempt, including run identity,
  status, start/end timestamps and sanitized error. An explicit administrator
  deletion removes the job and its attempts transactionally through a foreign key.

All required task model fields are persisted in the Job document. Indexed fields
and the document are updated in the same transaction, avoiding dialect-specific
JSON operators. `sequence` provides stable pagination even when timestamps match.
Indexes cover status/due time, expired leases, type, creation time and history.

States are `pending`, `running`, `success`, `failed`, `cancelled`. Pause is a
separate persistent flag. Pausing a running task lets that attempt finish but
prevents the next cron/retry execution. Resume does not resurrect terminal jobs.
Run-now and manual retry create a new run identity and reset the retry budget.
Running jobs cannot be manually re-run or deleted. Cancellation stays requested
until execution has stopped; then it becomes terminal and releases the lease.

Each claim atomically compares the version, marks the job running, installs a
unique attempt token and inserts its history record. Only a committed claimant
executes. Every result is fenced by that token and a version comparison. Racing
administrative operations are re-read so pause/cancel cannot be overwritten.
Transactions start with a conditional write to avoid SQLite read-lock upgrades.

The lease expires at claim time + task timeout + 30 seconds. There is no periodic
lease renewal. Workers enforce the original deadline, allow up to five seconds
for cooperative cancellation, then drop the future. Completed handlers never
re-run just because persisting their result failed. Failed acknowledgments leave
the lease recoverable. Startup and normal polling recover expired running jobs;
live leases belonging to other instances are left alone.

## Scheduling and Retries

`run_at` is a UTC Unix timestamp. `delay` is a duration in seconds. Omit both for
immediate execution. They are mutually exclusive and cannot accompany `cron`.

Cron accepts the standard five fields, including lists, ranges, steps and weekday
names. Croner handles parsing and calendar matching; chrono-tz provides IANA
timezones. Restricted day-of-month/day-of-week follow standard OR semantics.
Nonexistent local times during spring transitions are skipped. Repeated wall
times run once using the earlier offset. UTC execution times always move forward.

Cron completion computes the next occurrence after completion on the same job
row. Missed occurrences are not replayed in a burst. A failure first uses any
explicitly permitted retries, then schedules the next cron occurrence. A cron
schedule intentionally recurs until cancelled; the retry budget applies to each
occurrence, not to the lifetime of the schedule. Attempt history preserves each
occurrence's outcome even though the job returns to `pending`.

Handlers return `JobError::retryable(...)` or `JobError::permanent(...)`.
Only the former retries, at most `max_retries` additional attempts. Retry options:
`retry_interval`, `retry_strategy` (`fixed` or `exponential`), `max_backoff`.
Timeouts and panics do not automatically retry. Interrupted process recovery
requires explicit `retry_interrupted = true`, uses the same finite retry budget,
and preserves the `run_key`. Otherwise the interrupted attempt fails. Administrators
may explicitly retry it after checking external side effects.

## Handlers

Implement `JobHandler::execute(JobContext) -> BoxFuture<Result<(), JobError>>`
and call `scheduler.register_job("my_type", Arc::new(handler))` before starting.
Context contains the job ID, stable run key, attempt token, JSON payload and a
cancellation token. Built-in adapters:

| Type | Behavior / Payload |
| --- | --- |
| `backup` | Existing backup service; `kind`: `database` (default), `full`, `media` |
| `cleanup` | Purge stale backup staging files |
| `search_index` | Rebuild the existing search index |
| `media_cleanup` | Existing orphan cleanup; `apply`: false by default, true to delete |
| `plugin_name.task` | Registered Rhai function |

Webhook/email or other native integrations implement the same handler interface;
no network delivery logic is hardcoded into the scheduler. Existing automatic
backup and scheduled-post maintenance are preserved.

Handlers must be async, cooperative and safe to cancel/drop. They must not detach
threads, processes, blocking work or Tokio tasks that can keep producing side
effects after the handler ends. Rhai executions use bounded sandbox operations
and cancellation-aware progress checks; workers await their completion.

Lease ownership prevents ordinary concurrent execution, but database transactions
cannot guarantee exactly-once external effects across crashes or suspended hosts.
Use `run_key` as an idempotency key at the external service, and/or enforce the
attempt token at the destination. Keep instance clocks synchronized. Do not enable
interrupted retries for non-idempotent work. Arbitrary native blocking code cannot
be forcibly terminated inside a Rust process; run such work in a separately
controlled process if hard termination is required.

Logs contain identifiers, status, retry count and lifecycle events, never payloads.
Native handler error messages must already be safe for administrators; at most
2,000 characters are persisted. Rhai errors are generic so thrown payload content
cannot leak into history or scheduler logs.

## Plugin API

```rhai
fn init(config) {
    register_job("my_plugin.task", "process_task");
}

fn process_task(ctx) {
    // ctx.id, ctx.run_key, ctx.attempt_id, ctx.payload
    #{ok: true}
    // Explicit transient failure: #{ok: false, retryable: true}
}

fn schedule(ctx) {
    create_job(#{
        name: "Periodic plugin task",
        type: "my_plugin.task",
        cron: "0 3 * * *",
        timezone: "Asia/Shanghai",
        payload: #{},
        max_retries: 2,
        retry_interval: 30
    })
}

fn stop_task(id) { cancel_job(id); }
```

`create_job` also accepts `delay` or `run_at`, and returns the persisted ID only
after commit. An alternative to `register_job` is `[jobs]` in `plugin.toml`, mapping
`"my_plugin.task" = "process_task"`. Names must use the plugin directory prefix.
Plugins can create only their registered types and cancel only their own jobs.
The synchronous Rhai bridge requires Polaris' multi-thread Tokio server runtime.
Plugin unload removes handler availability without deleting jobs or execution
history. An already running attempt retains its bounded handler snapshot.

## Administration

Open `/admin/jobs`. Listing uses status filters and cursor pagination. Details show
payload, errors, timings, next run and the latest 100 attempts. Actions use a
separate confirmation page, administrator sessions and CSRF validation.

| Method | Endpoint |
| --- | --- |
| GET / POST | `/api/admin/jobs` (list / create) |
| GET / DELETE | `/api/admin/jobs/{id}` |
| POST | `/api/admin/jobs/{id}/run` |
| POST | `/api/admin/jobs/{id}/pause` |
| POST | `/api/admin/jobs/{id}/resume` |
| POST | `/api/admin/jobs/{id}/cancel` |
| POST | `/api/admin/jobs/{id}/retry` |

All endpoints require an administrator session. Mutations require
`X-CSRF-Token`; DELETE also requires `X-Confirm-Job` equal to the ID. Creation
accepts a JSON JobRequest. Listing accepts `status`, `type`, `before`, `limit`
(maximum 100). Control endpoints return 204, creation returns 201.

## Operational Boundaries

- Graceful shutdown stops claiming work, then waits for active attempts and their
  configured timeout/cancellation grace. A forced stop relies on lease recovery.
- Restore database snapshots with scheduler instances stopped. The existing
  portable blog-content backup format does not include scheduler tables; back up
  the database itself when task queues and execution history must also be archived.
- No automatic history deletion is performed. Administrators can delete terminal
  tasks explicitly; a retention policy can be added through another handler.
- Scheduler tables are protected from extension `uninstall_tables` declarations,
  including when an administrator chooses to remove plugin-owned data.
- SQLite integration tests exercise locking, rollback and reconnect recovery.
  MySQL/PostgreSQL SQL and migrations need validation against deployed server
  versions; SQLite tests do not establish behavior of those servers.

## Changed Files

- Core: `src/scheduler/{mod,scheduler,job,executor,worker,retry,cron,repository,tests}.rs`.
- Infrastructure: `src/repositories/jobs.rs`, `src/repositories/mod.rs`,
  `src/db/migrate.rs`, `migrations/{sqlite,mysql,postgres}/0005_scheduler.sql`.
- Integration: `src/services/jobs.rs`, `src/services/mod.rs`, `src/plugins/jobs.rs`,
  `src/plugins/mod.rs`, `src/state.rs`, `src/main.rs`, `src/lib.rs`.
- Configuration/dependencies: `src/config.rs`, `polaris.toml.example`,
  `Cargo.toml`, `Cargo.lock`.
- Admin: `src/http/admin_jobs.rs`, `src/http/mod.rs`, `src/templates/mod.rs`,
  `src/templates/admin/{jobs,job_detail,job_confirm,base}.html`,
  `src/templates/admin/admin.css`.
- Uninstall protection: `src/extension/installer.rs`.
- Tests: `tests/scheduler_test.rs`, `tests/db_test.rs`, `tests/extensions_test.rs`.
- Documentation: `docs/SCHEDULER.md`.

Local browser verification scripts, screenshots and the isolated preview database
are under ignored `target/scheduler-ui` and `target/scheduler-preview` directories.
