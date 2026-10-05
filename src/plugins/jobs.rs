//! Rhai scheduler bridge. Registrations are scoped to the plugin's directory
//! name; durable mutations return only after the database has committed.
use futures_util::future::BoxFuture;
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, RwLock, Weak},
};
use tokio_util::sync::CancellationToken;

use crate::scheduler::{Action, JobContext, JobError, JobHandler, JobRequest, Scheduler};
use crate::utils::lock;

thread_local! { static JOB_CANCEL: RefCell<Option<CancellationToken>> = const { RefCell::new(None) }; }

fn script_error(message: &str) -> Box<EvalAltResult> {
    message.into()
}

fn blocking<F: std::future::Future>(future: F) -> Result<F::Output, Box<EvalAltResult>> {
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| script_error("scheduler runtime unavailable"))?;
    if handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
        return Err(script_error(
            "plugin scheduler API requires the server runtime",
        ));
    }
    let cancellation = JOB_CANCEL
        .with(|cell| cell.borrow().clone())
        .unwrap_or_default();
    tokio::task::block_in_place(|| {
        handle.block_on(async {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(script_error("job cancelled")),
            result = tokio::time::timeout(std::time::Duration::from_secs(5), future) => result.map_err(|_| script_error("scheduler database timeout")),
        }
    })
    })
}

pub(super) fn register_api(
    engine: &mut Engine,
    owner: &str,
    scheduler: Option<Weak<Scheduler>>,
    registrations: Arc<RwLock<HashMap<String, String>>>,
) {
    engine.on_progress(|_| {
        JOB_CANCEL.with(|cell| {
            cell.borrow()
                .as_ref()
                .filter(|token| token.is_cancelled())
                .map(|_| Dynamic::from("job cancelled"))
        })
    });
    let namespace = format!("{owner}.");
    let map = registrations.clone();
    engine.register_fn(
        "register_job",
        move |kind: &str, function: &str| -> Result<(), Box<EvalAltResult>> {
            if !kind.starts_with(&namespace) || kind.len() > 160 {
                return Err(script_error("invalid job namespace"));
            }
            let mut map = lock::write(&map);
            if map.len() >= 100 {
                return Err(script_error("too many job registrations"));
            }
            map.insert(kind.into(), function.into());
            Ok(())
        },
    );
    let owner_create = owner.to_string();
    let link = scheduler.clone();
    engine.register_fn(
        "create_job",
        move |request: rhai::Map| -> Result<String, Box<EvalAltResult>> {
            let scheduler = link
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or_else(|| script_error("scheduler unavailable"))?;
            let request: JobRequest = serde_json::from_value(super::map_to_json(&request))
                .map_err(|_| script_error("invalid job request"))?;
            if !lock::read(&registrations).contains_key(&request.job_type) {
                return Err(script_error("job is not registered by this plugin"));
            }
            blocking(scheduler.create_owned(request, Some(owner_create.clone())))?
                .map(|job| job.id)
                .map_err(|_| script_error("could not create job"))
        },
    );
    let owner_cancel = owner.to_string();
    engine.register_fn(
        "cancel_job",
        move |id: &str| -> Result<(), Box<EvalAltResult>> {
            let scheduler = scheduler
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or_else(|| script_error("scheduler unavailable"))?;
            blocking(scheduler.action(id, Action::Cancel, Some(&owner_cancel)))?
                .map_err(|_| script_error("could not cancel owned job"))
        },
    );
}

pub struct PluginJobs(pub Weak<super::PluginManager>);
impl crate::scheduler::service::HandlerProvider for PluginJobs {
    fn resolve(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        let manager = self.0.upgrade()?;
        let inner = lock::read(&manager.inner);
        for plugin in inner.enabled.values() {
            if let Some(function) = lock::read(&plugin.jobs).get(kind).cloned() {
                return Some(Arc::new(ScriptJob {
                    engine: plugin.engine.clone(),
                    ast: plugin.ast.clone(),
                    function,
                }));
            }
        }
        None
    }
}

struct ScriptJob {
    engine: Arc<Engine>,
    ast: rhai::AST,
    function: String,
}
impl JobHandler for ScriptJob {
    fn execute(&self, context: JobContext) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let engine = self.engine.clone();
            let ast = self.ast.clone();
            let function = self.function.clone();
            let cancellation = context.cancellation.clone();
            let guard = cancellation.clone().drop_guard();
            let result = tokio::task::spawn_blocking(move || {
                struct Reset;
                impl Drop for Reset { fn drop(&mut self) { JOB_CANCEL.with(|cell| *cell.borrow_mut() = None); } }
                JOB_CANCEL.with(|cell| *cell.borrow_mut() = Some(cancellation));
                let _reset = Reset;
                let map = super::json_to_map(&serde_json::json!({ "id": context.id, "run_key": context.run_key, "attempt_id": context.attempt_id, "payload": context.payload }));
                let result = engine.call_fn::<Dynamic>(&mut Scope::new(), &ast, &function, (Dynamic::from(map),))
                    .map_err(|_| JobError::permanent("plugin task failed"))?;
                if let Some(map) = result.try_cast::<rhai::Map>() {
                    let value = super::map_to_json(&map);
                    if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                        return Err(JobError { message: "plugin reported task failure".into(), retryable: value.get("retryable").and_then(|v| v.as_bool()).unwrap_or(false) });
                    }
                }
                Ok(())
            }).await.map_err(|_| JobError::permanent("plugin task panicked"))?;
            drop(guard);
            result
        })
    }
}
