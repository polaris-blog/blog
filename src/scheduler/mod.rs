//! Persistent scheduling. SQL and business handlers live outside this module.
pub mod cron;
pub mod executor;
pub mod job;
pub mod repository;
pub mod retry;
#[path = "scheduler.rs"]
pub mod service;
pub mod worker;

pub use executor::{JobContext, JobError, JobHandler};
pub use job::{Action, Job, JobRequest, JobStatus};
pub use service::{Scheduler, SchedulerConfig, SchedulerHandle};

#[cfg(test)]
mod tests;
