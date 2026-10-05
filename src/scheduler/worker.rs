use super::{Job, JobContext, JobError, JobStatus, Scheduler};
use futures_util::FutureExt;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(crate) async fn execute(scheduler: Arc<Scheduler>, job: Job) {
    let outcome = std::panic::AssertUnwindSafe(run(&scheduler, &job))
        .catch_unwind()
        .await;
    let (status, error, retryable) =
        outcome.unwrap_or((JobStatus::Failed, Some("worker panicked".into()), false));
    // A failed commit leaves the lease intact for recovery; never execute again
    // merely because acknowledging an already completed handler failed.
    for attempt in 0..3 {
        if scheduler
            .complete(&job, status, error.clone(), retryable, false)
            .await
            .is_ok()
        {
            return;
        }
        tracing::warn!(job_id = %job.id, "job result could not be committed");
        tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
    }
}

async fn run(scheduler: &Scheduler, job: &Job) -> (JobStatus, Option<String>, bool) {
    let remaining = job.timeout.saturating_sub(
        crate::utils::time::now().saturating_sub(job.started_at.unwrap_or(0)) as u64,
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(remaining);
    match tokio::time::timeout_at(deadline, scheduler.get(&job.id)).await {
        Ok(Ok(current)) if current.cancel_requested => return (JobStatus::Cancelled, None, false),
        Ok(Ok(current))
            if current.status == JobStatus::Running
                && current.locked_by == job.locked_by
                && remaining > 0 => {}
        _ => {
            return (
                JobStatus::Failed,
                Some("execution lease could not be verified".into()),
                false,
            );
        }
    }
    let Some(handler) = scheduler.handler(&job.job_type) else {
        return (
            JobStatus::Failed,
            Some("job handler is unavailable".into()),
            false,
        );
    };
    let cancellation = CancellationToken::new();
    let context = JobContext {
        id: job.id.clone(),
        run_key: job.run_key.clone(),
        attempt_id: job.locked_by.clone().unwrap_or_default(),
        payload: job.payload.clone(),
        cancellation: cancellation.clone(),
    };
    tracing::info!(job_id = %job.id, "job execution started");
    let future =
        std::panic::AssertUnwindSafe(async { handler.execute(context).await }).catch_unwind();
    tokio::pin!(future);
    let timeout = tokio::time::sleep_until(deadline);
    tokio::pin!(timeout);
    let monitor = async {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            match scheduler.get(&job.id).await {
                Ok(current) if current.cancel_requested => {
                    return (JobStatus::Cancelled, None, false);
                }
                Ok(current)
                    if current.locked_by == job.locked_by
                        && current.lease_until.unwrap_or(0) > crate::utils::time::now() => {}
                _ => {
                    return (
                        JobStatus::Failed,
                        Some("execution lease could not be verified".into()),
                        false,
                    );
                }
            }
        }
    };
    let stopped = {
        tokio::select! {
            biased;
            _ = &mut timeout => {
                tracing::warn!(job_id = %job.id, "job timed out");
                (JobStatus::Failed, Some("task timeout".into()), false)
            }
            result = &mut future => {
                return match result {
                    Ok(Ok(())) => (JobStatus::Success, None, false),
                    Ok(Err(JobError { message, retryable })) => (JobStatus::Failed, Some(message.chars().take(2000).collect()), retryable),
                    Err(_) => (JobStatus::Failed, Some("job handler panicked".into()), false),
                };
            }
            result = monitor => result,
        }
    };
    cancellation.cancel();
    // Cooperative handlers can await child work before acknowledging cancellation.
    // All handlers must remain safe to drop after this bounded grace period.
    let _ = tokio::time::timeout(Duration::from_secs(5), &mut future).await;
    stopped
}
