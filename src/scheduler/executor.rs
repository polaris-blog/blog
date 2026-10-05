use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct JobContext {
    pub id: String,
    pub run_key: String,
    pub attempt_id: String,
    pub payload: Value,
    pub cancellation: CancellationToken,
}

/// Only `retryable` errors may automatically retry. Messages must be safe for
/// administrators: never include credentials or raw payloads.
#[derive(Debug)]
pub struct JobError {
    pub message: String,
    pub retryable: bool,
}
impl JobError {
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
}

/// Handlers must be cancellation-safe, async and cooperative. Do not detach
/// work: returning/dropping the future must end all side effects for this attempt.
pub trait JobHandler: Send + Sync {
    fn execute(&self, context: JobContext) -> BoxFuture<'_, Result<(), JobError>>;
}
