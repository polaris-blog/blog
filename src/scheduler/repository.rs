use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::job::{Attempt, Job, JobStatus};
use crate::error::AppResult;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct JobFilter {
    pub status: Option<JobStatus>,
    #[serde(rename = "type")]
    pub job_type: Option<String>,
    pub before: Option<i64>,
    pub limit: Option<u32>,
}

#[derive(Serialize)]
pub struct JobPage {
    pub jobs: Vec<Job>,
    pub next_cursor: Option<i64>,
}

/// Implementations must atomically compare `version`, write the job and its
/// attempt, and commit before reporting success. No handler runs before commit.
pub trait JobRepository: Send + Sync {
    fn insert(&self, job: Job) -> BoxFuture<'_, AppResult<()>>;
    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, AppResult<Job>>;
    fn list(&self, filter: JobFilter) -> BoxFuture<'_, AppResult<JobPage>>;
    fn due(&self, now: i64, limit: u32) -> BoxFuture<'_, AppResult<Vec<Job>>>;
    fn expired(&self, now: i64, limit: u32) -> BoxFuture<'_, AppResult<Vec<Job>>>;
    fn save(&self, job: Job, attempt: Option<Attempt>) -> BoxFuture<'_, AppResult<bool>>;
    fn delete(&self, job: Job) -> BoxFuture<'_, AppResult<bool>>;
    fn history<'a>(&'a self, id: &'a str, limit: u32) -> BoxFuture<'a, AppResult<Vec<Attempt>>>;
}
