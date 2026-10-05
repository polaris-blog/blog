//! Business adapters for the generic scheduler.
use crate::{
    error::AppResult,
    scheduler::{JobContext, JobError, JobHandler},
    state::{App, AppState},
};
use futures_util::future::BoxFuture;
use std::sync::{Arc, Weak};

#[derive(Clone, Copy)]
enum Builtin {
    Backup,
    Cleanup,
    SearchIndex,
    MediaCleanup,
}
struct Handler {
    app: Weak<AppState>,
    kind: Builtin,
}

pub fn register(app: &App) -> AppResult<()> {
    for (name, kind) in [
        ("backup", Builtin::Backup),
        ("cleanup", Builtin::Cleanup),
        ("search_index", Builtin::SearchIndex),
        ("media_cleanup", Builtin::MediaCleanup),
    ] {
        app.scheduler.register_job(
            name,
            Arc::new(Handler {
                app: Arc::downgrade(app),
                kind,
            }),
        )?;
    }
    Ok(())
}
impl JobHandler for Handler {
    fn execute(&self, context: JobContext) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let app = self
                .app
                .upgrade()
                .ok_or_else(|| JobError::permanent("application stopped"))?;
            let operation = async {
                match self.kind {
                    Builtin::Backup => {
                        let kind = context
                            .payload
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("database");
                        let kind = crate::backup::BackupKind::parse(kind).ok_or_else(|| {
                            crate::error::AppError::BadRequest("invalid backup kind".into())
                        })?;
                        app.backup().create(kind, "task scheduler").await?;
                    }
                    Builtin::Cleanup => {
                        app.backup().purge_stale_staging().await?;
                    }
                    Builtin::SearchIndex => {
                        app.search.rebuild(&app, |_, _| {}).await?;
                    }
                    Builtin::MediaCleanup => {
                        let apply = context
                            .payload
                            .get("apply")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        crate::services::media::cleanup_orphans(&app, apply).await?;
                    }
                }
                Ok::<_, crate::error::AppError>(())
            };
            tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => Err(JobError::permanent("job cancelled")),
                result = operation => result.map_err(|_| JobError::permanent("business job failed; inspect service logs")),
            }
        })
    }
}
