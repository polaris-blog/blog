mod common;

#[tokio::test]
async fn concurrent_admin_demotions_keep_one_admin() {
    let (app, _dir) = common::init_app().await;
    let first = common::create_user(&app, "first", "password123", Role::Admin).await;
    let second = common::create_user(&app, "second", "password123", Role::Admin).await;
    let (a, b) = tokio::join!(
        repositories::users::update_role(&app.db, first.id, Role::Author),
        repositories::users::update_role(&app.db, second.id, Role::Author)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        repositories::users::list(&app.db)
            .await
            .unwrap()
            .iter()
            .filter(|u| u.role == Role::Admin)
            .count(),
        1
    );
}

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use polaris::{
    models::{PostStatus, Role},
    repositories::{self, media, pages, posts},
};
use tower::ServiceExt;

#[tokio::test]
async fn extreme_pagination_is_empty_and_metadata_matches_limit() {
    let (app, _dir) = common::init_app().await;
    common::create_user(&app, "admin", "password123", Role::Admin).await;
    let (items, total) = posts::list(
        &app.db,
        &posts::PostFilter {
            page: i64::MAX,
            per_page: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(items.is_empty());
    assert!(total > 0);
    assert!(
        repositories::comments::list(&app.db, None, i64::MAX, 100)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert!(
        media::list(
            &app.db,
            &media::MediaFilter {
                page: i64::MAX,
                per_page: 200,
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .0
        .is_empty()
    );
    let list = polaris::services::posts::list_public_full(&app, 1, i64::MAX, None)
        .await
        .unwrap();
    assert_eq!(list.pages, 1);
}

#[tokio::test]
async fn authors_cannot_read_other_owners_edit_forms_or_backups() {
    let (app, _dir) = common::init_app().await;
    let owner = common::create_user(&app, "owner", "password123", Role::Admin).await;
    let author = common::create_user(&app, "author", "password123", Role::Author).await;
    let post_id = posts::insert(
        &app.db,
        &posts::NewPost {
            title: "private".into(),
            slug: "private".into(),
            summary: String::new(),
            content_md: "private draft content".into(),
            author_id: owner.id,
            status: PostStatus::Draft,
            featured_image: None,
            published_at: None,
        },
    )
    .await
    .unwrap();
    let page_id = pages::insert(
        &app.db,
        &pages::NewPage {
            title: "private".into(),
            slug: "private".into(),
            summary: String::new(),
            content_md: "private draft content".into(),
            author_id: owner.id,
            status: PostStatus::Draft,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    let router = polaris::http::router(app.clone());
    let token = app.sessions.create(author.id, 600);
    for uri in [
        format!("/admin/posts/{post_id}/edit"),
        format!("/admin/pages/{page_id}/edit"),
        "/admin/backups".into(),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", format!("polaris_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&bytes).contains("private draft content"));
    }
    let token = app.sessions.create(owner.id, 600);
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/admin/posts/{post_id}/edit"))
                .header("cookie", format!("polaris_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_api_parameters_have_json_errors_and_logout_requires_csrf() {
    let (app, router, _dir) = common::init_http().await;
    for uri in ["/api/posts/not-an-id", "/api/posts?page=invalid"] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        let status = response.status().as_u16();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], status);
    }
    let user = common::create_user(&app, "logout-user", "password123", Role::Author).await;
    let token = app.sessions.create(user.id, 600);
    let csrf = app.sessions.get(&token).unwrap().csrf;
    for (value, expected) in [
        ("wrong", StatusCode::FORBIDDEN),
        (csrf.as_str(), StatusCode::SEE_OTHER),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/logout")
                    .method("POST")
                    .header("cookie", format!("polaris_session={token}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={value}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert!(app.sessions.get(&token).is_none());
}

#[tokio::test]
async fn migration_marker_rolls_back_with_migration_work() {
    let (app, _dir) = common::init_app().await;
    let mut tx = app.db.pool().begin().await.unwrap();
    repositories::extensions::migration_record_in_transaction(&app.db, &mut tx, "test", "001.sql")
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(
        repositories::extensions::migrations_applied(&app.db, "test")
            .await
            .unwrap()
            .is_empty()
    );
}
