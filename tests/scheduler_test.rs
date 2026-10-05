mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use polaris::{
    models::Role,
    scheduler::{Action, JobRequest, JobStatus, repository::JobFilter},
};
use tower::ServiceExt;

fn request(kind: &str) -> JobRequest {
    JobRequest {
        name: "Scheduled job".into(),
        job_type: kind.into(),
        delay: Some(3600),
        ..Default::default()
    }
}

#[tokio::test]
async fn admin_api_enforces_roles_csrf_confirmation_and_renders_pages() {
    let (app, dir) = common::init_app().await;
    let admin = common::create_user(&app, "admin", "password123", Role::Admin).await;
    let editor = common::create_user(&app, "editor", "password123", Role::Editor).await;
    let token = app.sessions.create(admin.id, 600);
    let editor_token = app.sessions.create(editor.id, 600);
    let csrf = app.sessions.get(&token).unwrap().csrf;
    let router = polaris::http::router(app.clone());
    let job = app.scheduler.create(request("cleanup")).await.unwrap();
    for uri in [
        "/api/admin/jobs".to_string(),
        format!("/api/admin/jobs/{}", job.id),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", format!("polaris_session={editor_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    for uri in [
        "/admin/jobs".to_string(),
        format!("/admin/jobs/{}", job.id),
        format!("/admin/jobs/{}/cancel", job.id),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", format!("polaris_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("Scheduled job"));
    }
    for action in ["run", "pause", "resume", "cancel", "retry"] {
        let uri = format!("/api/admin/jobs/{}/{action}", job.id);
        for (cookie, token, expected) in [
            (&editor_token, &csrf, StatusCode::FORBIDDEN),
            (&token, &String::new(), StatusCode::FORBIDDEN),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&uri)
                        .header("cookie", format!("polaris_session={cookie}"))
                        .header("x-csrf-token", token)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/admin/jobs/{}/cancel", job.id))
                .header("cookie", format!("polaris_session={token}"))
                .header("x-csrf-token", &csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    for confirm in [None, Some(job.id.as_str())] {
        let mut builder = Request::builder()
            .method("DELETE")
            .uri(format!("/api/admin/jobs/{}", job.id))
            .header("cookie", format!("polaris_session={token}"))
            .header("x-csrf-token", &csrf);
        if let Some(confirm) = confirm {
            builder = builder.header("x-confirm-job", confirm);
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if confirm.is_some() {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::BAD_REQUEST
            }
        );
    }
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/jobs")
                .header("cookie", format!("polaris_session={token}"))
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&request("cleanup")).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    drop(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_register_create_cron_cancel_ownership_and_unload_history() {
    let (app, _dir) = common::init_app().await;
    let plugin_dir = std::path::Path::new(&app.config.plugin.dir).join("jobs_demo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.toml"), "name = 'Jobs demo'\n").unwrap();
    std::fs::write(plugin_dir.join("main.rhai"), r#"
fn init(config) { register_job("jobs_demo.task", "task"); }
fn task(ctx) { cache_set("last_run", ctx.run_key, 60); #{ok: true} }
fn schedule(input) { create_job(#{name: "plugin cron", type: "jobs_demo.task", cron: "* * * * *", timezone: "UTC"}) }
fn cancel(id) { cancel_job(id); "cancelled" }
fn foreign(input) { create_job(#{name: "invalid", type: "cleanup"}) }
"#).unwrap();
    app.plugins.reload(&["jobs_demo".into()]);
    assert_eq!(app.plugins.enabled_names(), vec!["jobs_demo"]);
    assert!(
        app.plugins
            .call_plugin_str("jobs_demo", "foreign", "")
            .is_none()
    );
    let id = app
        .plugins
        .call_plugin_str("jobs_demo", "schedule", "")
        .expect("persisted plugin cron id");
    let job = app.scheduler.get(&id).await.unwrap();
    assert_eq!(job.owner.as_deref(), Some("jobs_demo"));
    assert!(job.cron.is_some());
    let core = app.scheduler.create(request("cleanup")).await.unwrap();
    assert!(
        app.plugins
            .call_plugin_str("jobs_demo", "cancel", &core.id)
            .is_none()
    );
    assert!(
        app.scheduler
            .action(&id, Action::Cancel, Some("other_plugin"))
            .await
            .is_err()
    );
    app.scheduler.action(&id, Action::Run, None).await.unwrap();
    let handle = app.scheduler.start().unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app
            .scheduler
            .history(&id)
            .await
            .unwrap()
            .first()
            .is_none_or(|a| a.status != JobStatus::Success)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        app.plugins
            .call_plugin_str("jobs_demo", "cancel", &id)
            .as_deref(),
        Some("cancelled")
    );
    app.plugins.reload(&[]);
    assert_eq!(
        app.scheduler.get(&id).await.unwrap().status,
        JobStatus::Cancelled
    );
    assert_eq!(app.scheduler.history(&id).await.unwrap().len(), 1);
    assert_eq!(
        app.scheduler
            .list(JobFilter::default())
            .await
            .unwrap()
            .jobs
            .len(),
        2
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn pending_jobs_survive_database_reconnect() {
    let (app, dir) = common::init_app().await;
    let job = app.scheduler.create(request("cleanup")).await.unwrap();
    drop(app);
    let reopened = polaris::state::AppState::init(common::test_config(dir.path()))
        .await
        .unwrap();
    assert_eq!(
        reopened.scheduler.get(&job.id).await.unwrap().status,
        JobStatus::Pending
    );
    reopened
        .scheduler
        .action(&job.id, Action::Run, None)
        .await
        .unwrap();
    let handle = reopened.scheduler.start().unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while reopened.scheduler.get(&job.id).await.unwrap().status != JobStatus::Success {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await;
}
