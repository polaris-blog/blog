//! Service layer: users, posts, pages, comments.

mod common;

use polaris::error::AppError;
use polaris::models::{CommentStatus, PostStatus, Role};
use polaris::services::{comments, posts, users};

fn post_input(title: &str) -> posts::PostInput {
    posts::PostInput {
        title: title.into(),
        summary: "summary text".into(),
        content_md: "# Hello\n\nWorld".into(),
        status: PostStatus::Published,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[tokio::test]
async fn user_lifecycle() {
    let (app, _dir) = common::init_app().await;
    let u = common::create_user(&app, "alice", "password123", Role::Admin).await;
    assert_eq!(u.username, "alice");
    assert_eq!(u.role, Role::Admin);

    // Duplicate username.
    let err = users::create_user(&app, "alice", "a@example.test", "password123", Role::Author)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));

    // Short password.
    let err = users::create_user(&app, "bob", "b@example.test", "short", Role::Author)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));

    // Invalid characters in username.
    let err = users::create_user(
        &app,
        "bad name!",
        "c@example.test",
        "password123",
        Role::Author,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));

    // Authentication.
    assert!(
        users::authenticate(&app, "alice", "password123")
            .await
            .is_some()
    );
    assert!(
        users::authenticate(&app, "alice", "wrong-pass")
            .await
            .is_none()
    );
    assert!(
        users::authenticate(&app, "ghost", "password123")
            .await
            .is_none()
    );

    // Password change invalidates the old credential.
    users::change_password(&app, u.id, "newpassword456")
        .await
        .unwrap();
    assert!(
        users::authenticate(&app, "alice", "password123")
            .await
            .is_none()
    );
    assert!(
        users::authenticate(&app, "alice", "newpassword456")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn last_admin_protection() {
    let (app, _dir) = common::init_app().await;
    let admin = common::create_user(&app, "solo", "password123", Role::Admin).await;

    // The only admin cannot delete themselves.
    let err = users::delete_user(&app, &admin, admin.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));

    // A second admin may be deleted; the last one may not.
    let second = common::create_user(&app, "deputy", "password123", Role::Admin).await;
    users::delete_user(&app, &admin, second.id).await.unwrap();
    let err = users::delete_user(&app, &admin, admin.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

// ---------------------------------------------------------------------------
// Posts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_lifecycle_and_visibility() {
    let (app, _dir) = common::init_app().await;
    let author = common::create_user(&app, "writer", "password123", Role::Author).await;

    // Draft: not publicly visible.
    let mut input = post_input("Draft Post");
    input.status = PostStatus::Draft;
    let draft = posts::create_post(&app, author.id, input).await.unwrap();
    assert_eq!(draft.slug, "draft-post");
    assert!(draft.published_at.is_none());
    assert!(
        posts::get_public_post(&app, "draft-post")
            .await
            .unwrap()
            .is_none()
    );

    // Publish: becomes visible.
    let pubd = posts::update_post(&app, draft.id, post_input("Draft Post"))
        .await
        .unwrap();
    assert!(pubd.published_at.is_some());
    assert!(
        posts::get_public_post(&app, "draft-post")
            .await
            .unwrap()
            .is_some()
    );

    // Content update persists.
    let mut input = post_input("Draft Post");
    input.content_md = "# Changed".into();
    let updated = posts::update_post(&app, draft.id, input).await.unwrap();
    assert_eq!(updated.content_md, "# Changed");

    // Scheduled post: hidden until its due time passes and promote_scheduled runs.
    let mut input = post_input("Future Post");
    input.status = PostStatus::Scheduled;
    input.publish_at = Some(polaris::utils::time::now() - 1);
    let sched = posts::create_post(&app, author.id, input).await.unwrap();
    assert!(
        posts::get_public_post(&app, &sched.slug)
            .await
            .unwrap()
            .is_none()
    );
    app.promote_scheduled().await.unwrap();
    assert!(
        posts::get_public_post(&app, &sched.slug)
            .await
            .unwrap()
            .is_some()
    );

    // A not-yet-due scheduled post stays hidden after promotion runs.
    let mut input = post_input("Way Later");
    input.status = PostStatus::Scheduled;
    input.publish_at = Some(polaris::utils::time::now() + 3600);
    let later = posts::create_post(&app, author.id, input).await.unwrap();
    app.promote_scheduled().await.unwrap();
    assert!(
        posts::get_public_post(&app, &later.slug)
            .await
            .unwrap()
            .is_none()
    );

    // list_public sees published posts only.
    let list = posts::list_public(&app, 1, None).await.unwrap();
    assert!(list.posts.iter().any(|p| p.slug == "draft-post"));
    assert!(list.posts.iter().all(|p| p.status == PostStatus::Published));

    // Delete: gone, second delete is NotFound.
    posts::delete_post(&app, draft.id).await.unwrap();
    assert!(
        posts::get_public_post(&app, "draft-post")
            .await
            .unwrap()
            .is_none()
    );
    let err = posts::delete_post(&app, draft.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn slug_uniqueness_and_reserved_urls() {
    let (app, _dir) = common::init_app().await;
    let author = common::create_user(&app, "sluggish", "password123", Role::Author).await;

    // Same title twice → suffix.
    let a = posts::create_post(&app, author.id, post_input("Same Title"))
        .await
        .unwrap();
    let b = posts::create_post(&app, author.id, post_input("Same Title"))
        .await
        .unwrap();
    assert_eq!(a.slug, "same-title");
    assert_eq!(b.slug, "same-title-2");

    // Pages cannot take engine-reserved slugs.
    let err = posts::create_page(
        &app,
        author.id,
        posts::PageInput {
            title: "Admin".into(),
            slug: Some("admin".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));

    // A normal page is fine.
    let page = posts::create_page(
        &app,
        author.id,
        posts::PageInput {
            title: "Projects".into(),
            slug: Some("projects".into()),
            content_md: "# Projects".into(),
            status: PostStatus::Published,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.slug, "projects");

    // Empty title is rejected.
    let err = posts::create_post(&app, author.id, post_input(""))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[tokio::test]
async fn categories_and_tags_attach() {
    let (app, _dir) = common::init_app().await;
    let author = common::create_user(&app, "taxonomist", "password123", Role::Author).await;

    let mut input = post_input("Tagged Post");
    input.category = Some("Rust".into());
    input.tags = vec!["systems".into(), "fast".into()];
    let post = posts::create_post(&app, author.id, input).await.unwrap();

    // Re-fetch: terms are attached.
    let fetched = posts::get_public_post(&app, &post.slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.category().map(|t| t.name.as_str()), Some("Rust"));
    let tag_names: Vec<&str> = fetched.tags().iter().map(|t| t.name.as_str()).collect();
    assert!(tag_names.contains(&"systems"));
    assert!(tag_names.contains(&"fast"));

    // Term listings expose counts.
    let cats = polaris::repositories::terms::list_with_counts(
        &app.db,
        polaris::models::TermKind::Category,
    )
    .await
    .unwrap();
    assert!(
        cats.iter()
            .any(|t| t.name == "Rust" && t.count.unwrap_or(0) >= 1)
    );
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

#[tokio::test]
async fn comment_moderation_flow() {
    let (app, _dir) = common::init_app().await;
    common::create_user(&app, "host", "password123", Role::Admin).await;
    let seeded = posts::get_public_post(&app, "hello-polaris")
        .await
        .unwrap()
        .expect("seeded post");

    let new = || comments::NewComment {
        post_id: seeded.id,
        parent_id: None,
        author_name: "Visitor".into(),
        author_email: "v@example.test".into(),
        author_url: String::new(),
        content: "Nice post!".into(),
    };

    // Moderated comment starts pending and is not shown publicly.
    let c = comments::create(&app, new(), true).await.unwrap();
    assert_eq!(c.status, CommentStatus::Pending);
    assert!(
        polaris::repositories::comments::list_approved_for_post(&app.db, seeded.id)
            .await
            .unwrap()
            .is_empty()
    );

    // Approve → visible.
    comments::moderate(&app, c.id, CommentStatus::Approved)
        .await
        .unwrap();
    let approved = polaris::repositories::comments::list_approved_for_post(&app.db, seeded.id)
        .await
        .unwrap();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].content, "Nice post!");

    // Unmoderated comment is approved immediately.
    let c2 = comments::create(&app, new(), false).await.unwrap();
    assert_eq!(c2.status, CommentStatus::Approved);

    // Unknown post → NotFound.
    let mut orphan = new();
    orphan.post_id = 999_999;
    let err = comments::create(&app, orphan, true).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    // Blank name → BadRequest.
    let mut nameless = new();
    nameless.author_name = "   ".into();
    let err = comments::create(&app, nameless, true).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));

    // Delete.
    comments::delete(&app, c.id).await.unwrap();
    let err = comments::delete(&app, c.id).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}
