use std::collections::HashMap;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures_util::FutureExt;
use serde::Deserialize;
use tokio::{sync::Notify, task::JoinSet};
use tokio_util::sync::CancellationToken;

use super::{
    executor::JobHandler,
    job::{Action, Attempt, Job, JobRequest, JobStatus, token},
    repository::{JobFilter, JobPage, JobRepository},
    worker,
};
use crate::error::{AppError, AppResult};
use crate::utils::{lock, time};

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SchedulerConfig {
    pub enabled: bool,
    pub workers: usize,
    /// Seconds, matching Polaris' existing numeric duration configuration.
    pub poll_interval: u64,
    pub default_timeout: u64,
    pub max_retries: u32,
}
impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            workers: 4,
            poll_interval: 1,
            default_timeout: 300,
            max_retries: 3,
        }
    }
}
impl SchedulerConfig {
    pub fn validate(&self) -> AppResult<()> {
        if !(1..=64).contains(&self.workers)
            || !(1..=60).contains(&self.poll_interval)
            || !(1..=86400).contains(&self.default_timeout)
            || self.max_retries > 100
        {
            return Err(AppError::BadRequest(
                "invalid scheduler configuration".into(),
            ));
        }
        Ok(())
    }
}

pub trait HandlerProvider: Send + Sync {
    fn resolve(&self, job_type: &str) -> Option<Arc<dyn JobHandler>>;
}

pub struct Scheduler {
    pub(crate) repository: Arc<dyn JobRepository>,
    pub config: SchedulerConfig,
    handlers: RwLock<HashMap<String, Arc<dyn JobHandler>>>,
    provider: RwLock<Option<Arc<dyn HandlerProvider>>>,
    pub(crate) wake: Notify,
    started: AtomicBool,
    instance: String,
}

pub struct SchedulerHandle {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl SchedulerHandle {
    pub fn stop_token(&self) -> CancellationToken {
        self.stop.clone()
    }
    pub fn stop(&self) {
        self.stop.cancel();
    }
    pub async fn shutdown(self) {
        self.stop.cancel();
        if self.task.await.is_err() {
            tracing::error!("scheduler supervisor stopped unexpectedly");
        }
    }
}

impl Scheduler {
    pub fn new(
        repository: Arc<dyn JobRepository>,
        config: SchedulerConfig,
    ) -> AppResult<Arc<Self>> {
        config.validate()?;
        Ok(Arc::new(Self {
            repository,
            config,
            handlers: RwLock::new(HashMap::new()),
            provider: RwLock::new(None),
            wake: Notify::new(),
            started: AtomicBool::new(false),
            instance: token(),
        }))
    }

    pub fn register_job(&self, kind: &str, handler: Arc<dyn JobHandler>) -> AppResult<()> {
        if kind.is_empty()
            || kind.len() > 160
            || !kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(AppError::BadRequest("invalid job type".into()));
        }
        let mut handlers = lock::write(&self.handlers);
        if handlers.contains_key(kind) {
            return Err(AppError::Conflict("job type already registered".into()));
        }
        handlers.insert(kind.into(), handler);
        self.wake.notify_one();
        Ok(())
    }

    pub fn set_provider(&self, provider: Arc<dyn HandlerProvider>) {
        *lock::write(&self.provider) = Some(provider);
    }
    pub(crate) fn handler(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        lock::read(&self.handlers)
            .get(kind)
            .cloned()
            .or_else(|| lock::read(&self.provider).as_ref()?.resolve(kind))
    }

    pub async fn create(&self, request: JobRequest) -> AppResult<Job> {
        self.create_owned(request, None).await
    }
    pub async fn create_owned(&self, request: JobRequest, owner: Option<String>) -> AppResult<Job> {
        if let Some(ref owner) = owner
            && !request.job_type.starts_with(&format!("{owner}."))
        {
            return Err(AppError::Forbidden("plugin job namespace mismatch".into()));
        }
        if owner.is_none() && self.handler(&request.job_type).is_none() {
            return Err(AppError::BadRequest("job type is not registered".into()));
        }
        let job = Job::new(request, &self.config, owner, time::now())?;
        self.repository.insert(job.clone()).await?;
        self.wake.notify_one();
        Ok(job)
    }

    pub async fn get(&self, id: &str) -> AppResult<Job> {
        self.repository.get(id).await
    }
    pub async fn list(&self, filter: JobFilter) -> AppResult<JobPage> {
        self.repository.list(filter).await
    }
    pub async fn history(&self, id: &str) -> AppResult<Vec<Attempt>> {
        self.repository.history(id, 100).await
    }

    pub async fn action(&self, id: &str, action: Action, owner: Option<&str>) -> AppResult<()> {
        for _ in 0..4 {
            let mut job = self.get(id).await?;
            if let Some(owner) = owner
                && job.owner.as_deref() != Some(owner)
            {
                return Err(AppError::Forbidden("job belongs to another owner".into()));
            }
            let now = time::now();
            job.updated_at = now;
            match action {
                Action::Run | Action::Retry => {
                    if job.status == JobStatus::Running
                        || (matches!(action, Action::Retry) && job.status != JobStatus::Failed)
                    {
                        return Err(AppError::Conflict(
                            "job cannot be run in its current state".into(),
                        ));
                    }
                    if self.handler(&job.job_type).is_none() {
                        return Err(AppError::Conflict("job handler is unavailable".into()));
                    }
                    job.status = JobStatus::Pending;
                    job.run_at = now;
                    job.paused = false;
                    job.cancel_requested = false;
                    job.retry_count = 0;
                    job.started_at = None;
                    job.finished_at = None;
                    job.last_error = None;
                    job.run_key = token();
                }
                Action::Pause | Action::Resume => {
                    if !matches!(job.status, JobStatus::Pending | JobStatus::Running) {
                        return Err(AppError::Conflict(
                            "only active schedules can be paused or resumed".into(),
                        ));
                    }
                    job.paused = matches!(action, Action::Pause);
                }
                Action::Cancel => {
                    if !matches!(
                        job.status,
                        JobStatus::Pending | JobStatus::Running | JobStatus::Cancelled
                    ) {
                        return Err(AppError::Conflict("job has already finished".into()));
                    }
                    job.cancel_requested = true;
                    if job.status != JobStatus::Running {
                        job.status = JobStatus::Cancelled;
                        job.finished_at = Some(now);
                    }
                }
                Action::Delete => {
                    if !matches!(
                        job.status,
                        JobStatus::Success | JobStatus::Failed | JobStatus::Cancelled
                    ) {
                        return Err(AppError::Conflict(
                            "cancel the job and wait for execution to stop before deleting".into(),
                        ));
                    }
                    if self.repository.delete(job).await? {
                        return Ok(());
                    }
                    continue;
                }
            }
            if self.repository.save(job, None).await? {
                tracing::info!(job_id = id, action = ?action, "job control updated");
                self.wake.notify_one();
                return Ok(());
            }
        }
        Err(AppError::Conflict(
            "job changed concurrently; try again".into(),
        ))
    }

    pub(crate) async fn claim(&self, mut job: Job) -> AppResult<Option<Job>> {
        let now = time::now();
        if job.status != JobStatus::Pending
            || job.paused
            || job.cancel_requested
            || job.run_at > now
        {
            return Ok(None);
        }
        job.status = JobStatus::Running;
        job.started_at = Some(now);
        job.finished_at = None;
        job.updated_at = now;
        job.locked_at = Some(now);
        job.locked_by = Some(format!("{}:{}", self.instance, token()));
        // A fixed hard deadline avoids heartbeat traffic. Grace covers cooperative
        // cancellation and commit; expired owners cannot publish results.
        job.lease_until = Some(now + job.timeout as i64 + 30);
        let attempt = Attempt {
            id: job.locked_by.clone().unwrap_or_default(),
            job_id: job.id.clone(),
            run_key: job.run_key.clone(),
            status: JobStatus::Running,
            started_at: now,
            finished_at: None,
            error: None,
        };
        if self.repository.save(job.clone(), Some(attempt)).await? {
            job.version += 1;
            Ok(Some(job))
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn complete(
        &self,
        original: &Job,
        status: JobStatus,
        error: Option<String>,
        retryable: bool,
        recovery: bool,
    ) -> AppResult<()> {
        for _ in 0..4 {
            let mut job = self.get(&original.id).await?;
            let now = time::now();
            if job.status != JobStatus::Running || job.locked_by != original.locked_by {
                return Ok(());
            }
            if !recovery && job.lease_until.unwrap_or(0) <= now {
                return Ok(());
            }
            let status = if job.cancel_requested {
                JobStatus::Cancelled
            } else {
                status
            };
            let attempt = Attempt {
                id: original.locked_by.clone().unwrap_or_default(),
                job_id: job.id.clone(),
                run_key: job.run_key.clone(),
                status,
                started_at: job.started_at.unwrap_or(now),
                finished_at: Some(now),
                error: error.clone(),
            };
            job.finish(status, error.clone(), retryable, now);
            let retry_count = job.retry_count;
            let retrying =
                status == JobStatus::Failed && job.status == JobStatus::Pending && retry_count > 0;
            if self.repository.save(job, Some(attempt)).await? {
                tracing::info!(job_id = %original.id, status = status.as_str(), retrying, retry_count, "job execution finished");
                return Ok(());
            }
        }
        Err(AppError::Conflict("job completion was contested".into()))
    }

    pub async fn recover(&self) -> AppResult<usize> {
        let expired = self
            .repository
            .expired(time::now(), self.config.workers as u32)
            .await?;
        let count = expired.len();
        for job in expired {
            self.complete(
                &job,
                JobStatus::Failed,
                Some("execution interrupted; lease expired".into()),
                job.retry_interrupted,
                true,
            )
            .await?;
        }
        Ok(count)
    }

    pub fn start(self: &Arc<Self>) -> AppResult<Option<SchedulerHandle>> {
        if !self.config.enabled {
            return Ok(None);
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(AppError::Conflict("scheduler already started".into()));
        }
        let scheduler = self.clone();
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let task = tokio::spawn(async move {
            tracing::info!(workers = scheduler.config.workers, "scheduler started");
            let mut workers = JoinSet::new();
            let mut tick =
                tokio::time::interval(Duration::from_secs(scheduler.config.poll_interval));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = signal.cancelled() => break,
                    _ = tick.tick() => {},
                    _ = scheduler.wake.notified() => {},
                    result = workers.join_next(), if !workers.is_empty() => {
                        if let Some(Err(_)) = result { tracing::error!("scheduler worker isolated after panic"); }
                    }
                }
                let result = std::panic::AssertUnwindSafe(async {
                    while let Some(result) = workers.try_join_next() {
                        if result.is_err() {
                            tracing::error!("scheduler worker isolated after panic");
                        }
                    }
                    scheduler.recover().await?;
                    let capacity = scheduler.config.workers.saturating_sub(workers.len());
                    if capacity > 0 {
                        for job in scheduler
                            .repository
                            .due(time::now(), capacity as u32)
                            .await?
                        {
                            if signal.is_cancelled() {
                                break;
                            }
                            if let Some(job) = scheduler.claim(job).await? {
                                let scheduler = scheduler.clone();
                                workers.spawn(async move {
                                    worker::execute(scheduler, job).await;
                                });
                            }
                        }
                    }
                    Ok::<_, AppError>(())
                })
                .catch_unwind()
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!("scheduler dispatch failed; backing off");
                    tokio::select! { _ = signal.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(scheduler.config.poll_interval)) => {} }
                }
            }
            // Existing attempts retain their timeout and lease while draining.
            while let Some(result) = workers.join_next().await {
                if result.is_err() {
                    tracing::error!("scheduler worker failed during shutdown");
                }
            }
            scheduler.started.store(false, Ordering::SeqCst);
            tracing::info!("scheduler stopped");
        });
        Ok(Some(SchedulerHandle { stop, task }))
    }
}
