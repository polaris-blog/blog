use super::job::Attempt;
use super::repository::JobFilter;
use super::*;
use crate::{
    config::DatabaseConfig,
    db::{Db, migrate},
    repositories::jobs::SqlJobRepository,
    utils::time,
};
use futures_util::future::BoxFuture;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Handler {
    mode: u8,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
}
impl Handler {
    fn new(mode: u8) -> Arc<Self> {
        Arc::new(Self {
            mode,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }
}
impl JobHandler for Handler {
    fn execute(&self, context: JobContext) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let calls = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                1 if calls == 0 => return Err(JobError::retryable("temporary failure")),
                2 => return Err(JobError::permanent("permanent failure")),
                3 => {
                    context.cancellation.cancelled().await;
                    return Ok(());
                }
                4 => panic!("test panic"),
                6 => return Err(JobError::retryable("still unavailable")),
                _ => {}
            }
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            if self.mode == 5 {
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

async fn setup(workers: usize) -> (Arc<Scheduler>, Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::connect(&DatabaseConfig {
        url: dir.path().join("jobs.db").to_string_lossy().into_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    migrate::run(&db).await.unwrap();
    let scheduler = Scheduler::new(
        Arc::new(SqlJobRepository::new(db.clone())),
        SchedulerConfig {
            workers,
            ..Default::default()
        },
    )
    .unwrap();
    (scheduler, db, dir)
}
fn request(kind: &str) -> JobRequest {
    JobRequest {
        name: "test".into(),
        job_type: kind.into(),
        retry_interval: 1,
        max_backoff: 4,
        ..Default::default()
    }
}
async fn wait_status(scheduler: &Scheduler, id: &str, status: JobStatus) -> Job {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let job = scheduler.get(id).await.unwrap();
            if job.status == status {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("job reached expected status")
}

#[tokio::test]
async fn once_delay_immediate_pause_resume_cancel_and_delete() {
    let (scheduler, _, _dir) = setup(2).await;
    let handler = Handler::new(0);
    scheduler.register_job("test", handler.clone()).unwrap();
    let immediate = scheduler.create(request("test")).await.unwrap();
    let delayed = scheduler
        .create(JobRequest {
            delay: Some(3600),
            ..request("test")
        })
        .await
        .unwrap();
    scheduler
        .action(&delayed.id, Action::Pause, None)
        .await
        .unwrap();
    let handle = scheduler.start().unwrap().unwrap();
    wait_status(&scheduler, &immediate.id, JobStatus::Success).await;
    assert_eq!(
        scheduler.get(&delayed.id).await.unwrap().status,
        JobStatus::Pending
    );
    assert!(scheduler.get(&delayed.id).await.unwrap().paused);
    scheduler
        .action(&delayed.id, Action::Resume, None)
        .await
        .unwrap();
    scheduler
        .action(&delayed.id, Action::Run, None)
        .await
        .unwrap();
    wait_status(&scheduler, &delayed.id, JobStatus::Success).await;
    assert_eq!(handler.calls.load(Ordering::SeqCst), 2);
    let cancelled = scheduler
        .create(JobRequest {
            delay: Some(3600),
            ..request("test")
        })
        .await
        .unwrap();
    scheduler
        .action(&cancelled.id, Action::Cancel, None)
        .await
        .unwrap();
    assert_eq!(
        scheduler.get(&cancelled.id).await.unwrap().status,
        JobStatus::Cancelled
    );
    scheduler
        .action(&cancelled.id, Action::Delete, None)
        .await
        .unwrap();
    assert!(scheduler.get(&cancelled.id).await.is_err());
    handle.shutdown().await;
}

#[tokio::test]
async fn delayed_job_does_not_run_before_due() {
    let (scheduler, _, _dir) = setup(1).await;
    let handler = Handler::new(0);
    scheduler.register_job("test", handler.clone()).unwrap();
    let job = scheduler
        .create(JobRequest {
            delay: Some(2),
            ..request("test")
        })
        .await
        .unwrap();
    assert!(
        scheduler
            .repository
            .due(job.run_at - 1, 1)
            .await
            .unwrap()
            .is_empty()
    );
    let handle = scheduler.start().unwrap().unwrap();
    let done = wait_status(&scheduler, &job.id, JobStatus::Success).await;
    assert!(done.started_at.unwrap() >= job.run_at);
    handle.shutdown().await;
}

#[tokio::test]
async fn retries_are_explicit_bounded_and_keep_idempotency_key() {
    let (scheduler, _, _dir) = setup(3).await;
    for (kind, mode) in [("transient", 1), ("permanent", 2), ("exhausted", 6)] {
        scheduler.register_job(kind, Handler::new(mode)).unwrap();
    }
    let transient = scheduler.create(request("transient")).await.unwrap();
    let permanent = scheduler.create(request("permanent")).await.unwrap();
    let exhausted = scheduler
        .create(JobRequest {
            max_retries: Some(1),
            ..request("exhausted")
        })
        .await
        .unwrap();
    let handle = scheduler.start().unwrap().unwrap();
    let done = wait_status(&scheduler, &transient.id, JobStatus::Success).await;
    assert_eq!(done.retry_count, 1);
    let attempts = scheduler.history(&done.id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.iter().all(|a| a.run_key == transient.run_key));
    assert_eq!(
        wait_status(&scheduler, &permanent.id, JobStatus::Failed)
            .await
            .retry_count,
        0
    );
    assert_eq!(
        wait_status(&scheduler, &exhausted.id, JobStatus::Failed)
            .await
            .retry_count,
        1
    );
    assert_eq!(scheduler.history(&exhausted.id).await.unwrap().len(), 2);
    handle.shutdown().await;
}

#[test]
fn fixed_and_exponential_backoff_are_capped_without_overflow() {
    use retry::{RetryStrategy::*, delay};
    assert_eq!(delay(Fixed, 3, 20, 5), 3);
    assert_eq!(delay(Exponential, 3, 20, 2), 12);
    assert_eq!(delay(Exponential, 3, 20, 100), 20);
}

#[tokio::test]
async fn timeout_and_running_cancel_release_leases() {
    let (scheduler, _, _dir) = setup(2).await;
    scheduler.register_job("wait", Handler::new(3)).unwrap();
    let timeout = scheduler
        .create(JobRequest {
            timeout: Some(1),
            ..request("wait")
        })
        .await
        .unwrap();
    let cancel = scheduler.create(request("wait")).await.unwrap();
    let handle = scheduler.start().unwrap().unwrap();
    wait_status(&scheduler, &cancel.id, JobStatus::Running).await;
    assert!(
        scheduler
            .action(&cancel.id, Action::Run, None)
            .await
            .is_err()
    );
    assert!(
        scheduler
            .action(&cancel.id, Action::Delete, None)
            .await
            .is_err()
    );
    scheduler
        .action(&cancel.id, Action::Cancel, None)
        .await
        .unwrap();
    let cancelled = wait_status(&scheduler, &cancel.id, JobStatus::Cancelled).await;
    assert!(cancelled.locked_by.is_none());
    let failed = wait_status(&scheduler, &timeout.id, JobStatus::Failed).await;
    assert!(failed.lease_until.is_none());
    assert_eq!(failed.last_error.as_deref(), Some("task timeout"));
    assert_eq!(failed.retry_count, 0);
    handle.shutdown().await;
}

#[tokio::test]
async fn panic_and_error_do_not_kill_workers() {
    let (scheduler, _, _dir) = setup(1).await;
    for (kind, mode) in [("panic", 4), ("error", 2), ("ok", 0)] {
        scheduler.register_job(kind, Handler::new(mode)).unwrap();
    }
    let panic = scheduler
        .create(JobRequest {
            priority: 10,
            ..request("panic")
        })
        .await
        .unwrap();
    let error = scheduler.create(request("error")).await.unwrap();
    let success = scheduler.create(request("ok")).await.unwrap();
    let handle = scheduler.start().unwrap().unwrap();
    wait_status(&scheduler, &panic.id, JobStatus::Failed).await;
    wait_status(&scheduler, &error.id, JobStatus::Failed).await;
    wait_status(&scheduler, &success.id, JobStatus::Success).await;
    handle.shutdown().await;
}

#[tokio::test]
async fn priority_concurrency_and_graceful_shutdown() {
    let (scheduler, _, _dir) = setup(2).await;
    let handler = Handler::new(5);
    scheduler.register_job("test", handler.clone()).unwrap();
    let mut jobs = Vec::new();
    for priority in 0..8 {
        jobs.push(
            scheduler
                .create(JobRequest {
                    priority,
                    ..request("test")
                })
                .await
                .unwrap(),
        );
    }
    let due = scheduler.repository.due(time::now(), 8).await.unwrap();
    assert_eq!(due.first().unwrap().priority, 7);
    let handle = scheduler.start().unwrap().unwrap();
    for job in &jobs {
        wait_status(&scheduler, &job.id, JobStatus::Success).await;
    }
    assert_eq!(handler.peak.load(Ordering::SeqCst), 2);
    assert_eq!(handler.calls.load(Ordering::SeqCst), 8);
    let last = scheduler.create(request("test")).await.unwrap();
    wait_status(&scheduler, &last.id, JobStatus::Running).await;
    handle.shutdown().await;
    assert_eq!(
        scheduler.get(&last.id).await.unwrap().status,
        JobStatus::Success
    );
}

#[tokio::test]
async fn multiple_instances_cannot_claim_same_job_and_stale_owner_is_fenced() {
    let (first, db, _dir) = setup(2).await;
    first.register_job("test", Handler::new(0)).unwrap();
    let second = Scheduler::new(
        Arc::new(SqlJobRepository::new(db)),
        SchedulerConfig::default(),
    )
    .unwrap();
    let job = first.create(request("test")).await.unwrap();
    let (a, b) = tokio::join!(first.claim(job.clone()), second.claim(job));
    let claims: Vec<Job> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    let old = claims[0].clone();
    let mut expired = first.get(&old.id).await.unwrap();
    expired.lease_until = Some(time::now() - 1);
    first.repository.save(expired, None).await.unwrap();
    first.recover().await.unwrap();
    first.action(&old.id, Action::Run, None).await.unwrap();
    let new = second
        .claim(first.get(&old.id).await.unwrap())
        .await
        .unwrap()
        .unwrap();
    first
        .complete(&old, JobStatus::Success, None, false, false)
        .await
        .unwrap();
    assert_eq!(first.get(&old.id).await.unwrap().locked_by, new.locked_by);
}

#[tokio::test]
async fn recovery_keeps_live_leases_and_requires_explicit_retry_permission() {
    let (scheduler, db, _dir) = setup(1).await;
    scheduler.register_job("test", Handler::new(0)).unwrap();
    let job = scheduler
        .create(JobRequest {
            retry_interrupted: true,
            max_retries: Some(1),
            ..request("test")
        })
        .await
        .unwrap();
    let running = scheduler.claim(job).await.unwrap().unwrap();
    assert_eq!(scheduler.recover().await.unwrap(), 0);
    let mut expired = running;
    expired.lease_until = Some(time::now() - 1);
    scheduler
        .repository
        .save(expired.clone(), None)
        .await
        .unwrap();
    drop(scheduler);
    let restarted = Scheduler::new(
        Arc::new(SqlJobRepository::new(db)),
        SchedulerConfig::default(),
    )
    .unwrap();
    assert_eq!(restarted.recover().await.unwrap(), 1);
    let recovered = restarted.get(&expired.id).await.unwrap();
    assert_eq!(recovered.status, JobStatus::Pending);
    assert_eq!(recovered.retry_count, 1);
    assert_eq!(
        restarted.history(&expired.id).await.unwrap()[0].status,
        JobStatus::Failed
    );
    restarted.register_job("test", Handler::new(0)).unwrap();
    let handle = restarted.start().unwrap().unwrap();
    wait_status(&restarted, &expired.id, JobStatus::Success).await;
    handle.shutdown().await;
}

#[tokio::test]
async fn job_and_attempt_writes_rollback_together() {
    let (scheduler, _, _dir) = setup(1).await;
    scheduler.register_job("test", Handler::new(0)).unwrap();
    let job = scheduler.create(request("test")).await.unwrap();
    let mut changed = job.clone();
    changed.status = JobStatus::Success;
    let missing = Attempt {
        id: "missing".into(),
        job_id: job.id.clone(),
        run_key: job.run_key.clone(),
        status: JobStatus::Failed,
        started_at: time::now(),
        finished_at: Some(time::now()),
        error: None,
    };
    assert!(
        scheduler
            .repository
            .save(changed, Some(missing))
            .await
            .is_err()
    );
    let unchanged = scheduler.get(&job.id).await.unwrap();
    assert_eq!(unchanged.status, JobStatus::Pending);
    assert_eq!(unchanged.version, 0);
}

#[tokio::test]
async fn pagination_filters_do_not_skip_same_second_jobs() {
    let (scheduler, _, _dir) = setup(1).await;
    scheduler.register_job("test", Handler::new(0)).unwrap();
    for _ in 0..4 {
        scheduler.create(request("test")).await.unwrap();
    }
    let first = scheduler
        .list(JobFilter {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    let second = scheduler
        .list(JobFilter {
            limit: Some(2),
            before: first.next_cursor,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.jobs.len() + second.jobs.len(), 4);
    assert!(
        first
            .jobs
            .iter()
            .all(|a| second.jobs.iter().all(|b| a.id != b.id))
    );
    assert!(second.next_cursor.is_none());
    assert!(
        scheduler
            .list(JobFilter {
                status: Some(JobStatus::Failed),
                ..Default::default()
            })
            .await
            .unwrap()
            .jobs
            .is_empty()
    );
}

fn epoch(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp()
}
#[test]
fn cron_validates_standard_syntax_timezone_and_dst() {
    let next = cron::next("*/15 * * * *", "UTC", epoch("2026-01-01T00:01:00Z")).unwrap();
    assert_eq!(next, epoch("2026-01-01T00:15:00Z"));
    for expr in ["bad", "99 * * * *", "* * * * * *", "0 0 31 2 *"] {
        assert!(cron::next(expr, "UTC", time::now()).is_err(), "{expr}");
    }
    assert!(cron::next("* * * * *", "invalid/timezone", time::now()).is_err());
    // 02:30 does not exist on the spring transition day.
    assert_eq!(
        cron::next(
            "30 2 * * *",
            "America/New_York",
            epoch("2026-03-08T05:00:00Z")
        )
        .unwrap(),
        epoch("2026-03-09T06:30:00Z")
    );
    // Repeated 01:30 runs once at the earlier offset, never backwards in UTC.
    assert_eq!(
        cron::next(
            "30 1 * * *",
            "America/New_York",
            epoch("2026-11-01T04:00:00Z")
        )
        .unwrap(),
        epoch("2026-11-01T05:30:00Z")
    );
    assert_eq!(
        cron::next(
            "30 1 * * *",
            "America/New_York",
            epoch("2026-11-01T06:15:00Z")
        )
        .unwrap(),
        epoch("2026-11-02T06:30:00Z")
    );
    // Standard cron treats restricted day-of-month and day-of-week as OR.
    assert_eq!(
        cron::next("0 0 15 * MON", "UTC", epoch("2026-09-12T00:00:00Z")).unwrap(),
        epoch("2026-09-14T00:00:00Z")
    );
}

#[tokio::test]
async fn cron_reuses_job_row_and_preserves_execution_history() {
    let (scheduler, _, _dir) = setup(1).await;
    scheduler.register_job("test", Handler::new(0)).unwrap();
    let job = scheduler
        .create(JobRequest {
            cron: Some("* * * * *".into()),
            ..request("test")
        })
        .await
        .unwrap();
    scheduler.action(&job.id, Action::Run, None).await.unwrap();
    let running = scheduler
        .claim(scheduler.get(&job.id).await.unwrap())
        .await
        .unwrap()
        .unwrap();
    worker::execute(scheduler.clone(), running).await;
    let next = scheduler.get(&job.id).await.unwrap();
    assert_eq!(next.status, JobStatus::Pending);
    assert!(next.run_at > time::now());
    assert_eq!(
        scheduler
            .list(JobFilter::default())
            .await
            .unwrap()
            .jobs
            .len(),
        1
    );
    assert_eq!(
        scheduler.history(&job.id).await.unwrap()[0].status,
        JobStatus::Success
    );
}

#[test]
fn config_and_requests_have_bounded_resource_limits() {
    assert!(
        SchedulerConfig {
            workers: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Job::new(
            JobRequest {
                timeout: Some(0),
                ..request("test")
            },
            &SchedulerConfig::default(),
            None,
            time::now()
        )
        .is_err()
    );
}
