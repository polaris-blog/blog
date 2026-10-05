//! SQLx implementation of the scheduler port. The JSON document is the complete
//! versioned job model; indexed columns are updated in the same transaction.
use futures_util::future::BoxFuture;
use sqlx::Row;

use crate::db::{Bind, Db, bind_all};
use crate::error::{AppError, AppResult};
use crate::scheduler::job::{Attempt, Job, JobStatus};
use crate::scheduler::repository::{JobFilter, JobPage, JobRepository};

pub struct SqlJobRepository {
    db: Db,
}
impl SqlJobRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    async fn candidates(&self, sql: &str, now: i64, limit: u32) -> AppResult<Vec<Job>> {
        self.db
            .fetch_all(
                sql,
                &[Bind::I(now), Bind::I(i64::from(limit.clamp(1, 128)))],
            )
            .await?
            .iter()
            .map(decode_job)
            .collect()
    }
}

fn encode<T: serde::Serialize>(value: &T) -> AppResult<String> {
    serde_json::to_string(value).map_err(|e| AppError::Internal(e.into()))
}
fn decode_job(row: &sqlx::any::AnyRow) -> AppResult<Job> {
    serde_json::from_str(&crate::db::text(row, "document")?)
        .map_err(|e| AppError::Internal(e.into()))
}

fn binds(job: &Job) -> AppResult<Vec<Bind>> {
    Ok(vec![
        Bind::S(job.status.as_str().into()),
        Bind::I(job.run_at),
        Bind::S(job.job_type.clone()),
        Bind::I(job.created_at),
        Bind::I(i64::from(job.priority)),
        Bind::I(i64::from(job.paused)),
        Bind::OptI(job.lease_until),
        Bind::OptS(job.locked_by.clone()),
        Bind::OptI(job.locked_at),
        Bind::OptS(job.owner.clone()),
        Bind::S(encode(job)?),
        Bind::I(job.version),
        Bind::S(job.id.clone()),
    ])
}

impl JobRepository for SqlJobRepository {
    fn insert(&self, job: Job) -> BoxFuture<'_, AppResult<()>> {
        Box::pin(async move {
            let mut tx = self.db.pool().begin().await?;
            let sql = self.db.translate("INSERT INTO scheduler_jobs (status, run_at, job_type, created_at, priority, paused, lease_until, locked_by, locked_at, owner, document, version, id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)");
            bind_all(sqlx::query(&sql), &binds(&job)?)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, AppResult<Job>> {
        Box::pin(async move {
            let row = self
                .db
                .fetch_optional(
                    "SELECT document FROM scheduler_jobs WHERE id = ?",
                    &[Bind::S(id.into())],
                )
                .await?
                .ok_or_else(|| AppError::NotFound("job".into()))?;
            decode_job(&row)
        })
    }

    fn list(&self, filter: JobFilter) -> BoxFuture<'_, AppResult<JobPage>> {
        Box::pin(async move {
            let limit = filter.limit.unwrap_or(25).clamp(1, 100);
            let mut sql =
                "SELECT sequence, document FROM scheduler_jobs WHERE sequence < ?".to_string();
            let mut params = vec![Bind::I(filter.before.unwrap_or(i64::MAX))];
            if let Some(status) = filter.status {
                sql.push_str(" AND status = ?");
                params.push(Bind::S(status.as_str().into()));
            }
            if let Some(kind) = filter.job_type {
                sql.push_str(" AND job_type = ?");
                params.push(Bind::S(kind));
            }
            sql.push_str(" ORDER BY sequence DESC LIMIT ?");
            params.push(Bind::I(i64::from(limit) + 1));
            let rows = self.db.fetch_all(&sql, &params).await?;
            let next_cursor = if rows.len() > limit as usize {
                Some(rows[limit as usize - 1].try_get("sequence")?)
            } else {
                None
            };
            let jobs = rows
                .iter()
                .take(limit as usize)
                .map(decode_job)
                .collect::<AppResult<_>>()?;
            Ok(JobPage { jobs, next_cursor })
        })
    }

    fn due(&self, now: i64, limit: u32) -> BoxFuture<'_, AppResult<Vec<Job>>> {
        Box::pin(self.candidates("SELECT document FROM scheduler_jobs WHERE status = 'pending' AND paused = 0 AND run_at <= ? ORDER BY priority DESC, run_at, sequence LIMIT ?", now, limit))
    }

    fn expired(&self, now: i64, limit: u32) -> BoxFuture<'_, AppResult<Vec<Job>>> {
        Box::pin(self.candidates("SELECT document FROM scheduler_jobs WHERE status = 'running' AND lease_until <= ? ORDER BY lease_until LIMIT ?", now, limit))
    }

    fn save(&self, mut job: Job, attempt: Option<Attempt>) -> BoxFuture<'_, AppResult<bool>> {
        Box::pin(async move {
            let expected = job.version;
            job.version += 1;
            let mut params = binds(&job)?;
            params.push(Bind::I(expected));
            let mut tx = self.db.pool().begin().await?;
            // Write first: SQLite never has to upgrade a stale read transaction.
            let sql = self.db.translate("UPDATE scheduler_jobs SET status = ?, run_at = ?, job_type = ?, created_at = ?, priority = ?, paused = ?, lease_until = ?, locked_by = ?, locked_at = ?, owner = ?, document = ?, version = ? WHERE id = ? AND version = ?");
            if bind_all(sqlx::query(&sql), &params)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                != 1
            {
                tx.rollback().await?;
                return Ok(false);
            }
            if let Some(attempt) = attempt {
                if attempt.status == JobStatus::Running {
                    let sql = self.db.translate("INSERT INTO scheduler_attempts (id, job_id, started_at, document) VALUES (?, ?, ?, ?)");
                    sqlx::query(&sql)
                        .bind(&attempt.id)
                        .bind(&attempt.job_id)
                        .bind(attempt.started_at)
                        .bind(encode(&attempt)?)
                        .execute(&mut *tx)
                        .await?;
                } else {
                    let sql = self.db.translate(
                        "UPDATE scheduler_attempts SET document = ? WHERE id = ? AND job_id = ?",
                    );
                    let changed = sqlx::query(&sql)
                        .bind(encode(&attempt)?)
                        .bind(&attempt.id)
                        .bind(&job.id)
                        .execute(&mut *tx)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(AppError::Conflict("execution history missing".into()));
                    }
                }
            }
            tx.commit().await?;
            Ok(true)
        })
    }

    fn delete(&self, job: Job) -> BoxFuture<'_, AppResult<bool>> {
        Box::pin(async move {
            let mut tx = self.db.pool().begin().await?;
            let sql = self.db.translate("DELETE FROM scheduler_jobs WHERE id = ? AND version = ? AND status IN ('success', 'failed', 'cancelled')");
            let changed = sqlx::query(&sql)
                .bind(&job.id)
                .bind(job.version)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            tx.commit().await?;
            Ok(changed == 1)
        })
    }

    fn history<'a>(&'a self, id: &'a str, limit: u32) -> BoxFuture<'a, AppResult<Vec<Attempt>>> {
        Box::pin(async move {
            self.db.fetch_all("SELECT document FROM scheduler_attempts WHERE job_id = ? ORDER BY started_at DESC, id DESC LIMIT ?", &[Bind::S(id.into()), Bind::I(i64::from(limit.clamp(1, 100)))])
                .await?.iter().map(|row| {
                    serde_json::from_str(&crate::db::text(row, "document")?).map_err(|e| AppError::Internal(e.into()))
                }).collect()
        })
    }
}
