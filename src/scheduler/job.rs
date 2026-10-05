use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    SchedulerConfig, cron,
    retry::{self, RetryStrategy},
};
use crate::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Success,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub job_type: String,
    pub payload: Value,
    pub priority: i32,
    pub run_at: Option<i64>,
    pub delay: Option<u64>,
    pub cron: Option<String>,
    pub timezone: String,
    pub max_retries: Option<u32>,
    pub timeout: Option<u64>,
    pub retry_interval: u64,
    pub retry_strategy: RetryStrategy,
    pub max_backoff: u64,
    /// Explicit permission to repeat an interrupted attempt after lease expiry.
    pub retry_interrupted: bool,
}

impl Default for JobRequest {
    fn default() -> Self {
        Self {
            name: String::new(),
            job_type: String::new(),
            payload: Value::Null,
            priority: 0,
            run_at: None,
            delay: None,
            cron: None,
            timezone: "UTC".into(),
            max_retries: None,
            timeout: None,
            retry_interval: 30,
            retry_strategy: RetryStrategy::Exponential,
            max_backoff: 3600,
            retry_interrupted: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub job_type: String,
    pub payload: Value,
    pub status: JobStatus,
    pub priority: i32,
    pub run_at: i64,
    pub cron: Option<String>,
    pub timezone: String,
    pub retry_count: u32,
    pub max_retries: u32,
    pub timeout: u64,
    pub retry_interval: u64,
    pub retry_strategy: RetryStrategy,
    pub max_backoff: u64,
    pub retry_interrupted: bool,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub paused: bool,
    pub cancel_requested: bool,
    pub owner: Option<String>,
    pub locked_at: Option<i64>,
    pub locked_by: Option<String>,
    pub lease_until: Option<i64>,
    pub version: i64,
    /// Stable across automatic retries; changes for each cron/manual execution.
    pub run_key: String,
}

pub fn token() -> String {
    crate::utils::cookies::random_token(24)
}

impl Job {
    pub fn new(
        request: JobRequest,
        config: &SchedulerConfig,
        owner: Option<String>,
        now: i64,
    ) -> AppResult<Self> {
        let timeout = request.timeout.unwrap_or(config.default_timeout);
        let max_retries = request.max_retries.unwrap_or(config.max_retries);
        if request.name.trim().is_empty()
            || request.name.len() > 200
            || request.job_type.is_empty()
            || request.job_type.len() > 160
            || !request
                .job_type
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || timeout == 0
            || timeout > 86400
            || max_retries > 100
            || request.retry_interval == 0
            || request.max_backoff < request.retry_interval
            || request.max_backoff > 604800
            || request.payload.to_string().len() > 65536
            || request.delay.unwrap_or(0) > 31536000
            || (request.run_at.is_some() && request.delay.is_some())
            || (request.cron.is_some() && (request.run_at.is_some() || request.delay.is_some()))
        {
            return Err(AppError::BadRequest("invalid job parameters".into()));
        }
        let run_at = if let Some(ref expression) = request.cron {
            cron::next(expression, &request.timezone, now)?
        } else {
            request
                .run_at
                .unwrap_or(now + request.delay.unwrap_or(0) as i64)
        };
        if !(0..=253402300799).contains(&run_at) {
            return Err(AppError::BadRequest("invalid run_at".into()));
        }
        Ok(Self {
            id: token(),
            name: request.name,
            job_type: request.job_type,
            payload: request.payload,
            status: JobStatus::Pending,
            priority: request.priority,
            run_at,
            cron: request.cron,
            timezone: request.timezone,
            retry_count: 0,
            max_retries,
            timeout,
            retry_interval: request.retry_interval,
            retry_strategy: request.retry_strategy,
            max_backoff: request.max_backoff,
            retry_interrupted: request.retry_interrupted,
            last_error: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
            paused: false,
            cancel_requested: false,
            owner,
            locked_at: None,
            locked_by: None,
            lease_until: None,
            version: 0,
            run_key: token(),
        })
    }

    pub fn finish(&mut self, status: JobStatus, error: Option<String>, retryable: bool, now: i64) {
        self.status = status;
        self.last_error = error;
        self.finished_at = Some(now);
        self.updated_at = now;
        self.locked_by = None;
        self.locked_at = None;
        self.lease_until = None;
        if self.cancel_requested || status == JobStatus::Cancelled {
            self.status = JobStatus::Cancelled;
        } else if status == JobStatus::Failed && retryable && self.retry_count < self.max_retries {
            self.run_at = now
                + retry::delay(
                    self.retry_strategy,
                    self.retry_interval,
                    self.max_backoff,
                    self.retry_count,
                ) as i64;
            self.retry_count += 1;
            self.status = JobStatus::Pending;
        } else if let Some(ref expression) = self.cron {
            match cron::next(expression, &self.timezone, now) {
                Ok(next) => {
                    self.run_at = next;
                    self.status = JobStatus::Pending;
                    self.retry_count = 0;
                    self.run_key = token();
                }
                Err(_) => {
                    self.status = JobStatus::Failed;
                    self.last_error = Some("cron has no future occurrence".into());
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Run,
    Pause,
    Resume,
    Cancel,
    Retry,
    Delete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub job_id: String,
    pub run_key: String,
    pub status: JobStatus,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}
