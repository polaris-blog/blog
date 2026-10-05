use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::models::{Comment, CommentStatus};
use crate::plugins;
use crate::repositories::comments;
use crate::state::App;

const NAME_MAX: usize = 64;
const CONTENT_MAX: usize = 4000;

pub struct NewComment {
    pub post_id: i64,
    pub parent_id: Option<i64>,
    pub author_name: String,
    pub author_email: String,
    pub author_url: String,
    pub content: String,
}

pub async fn create(app: &App, input: NewComment, moderate: bool) -> AppResult<Comment> {
    let name = input.author_name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err(AppError::BadRequest("name must be 1-64 characters".into()));
    }
    let content = input.content.trim();
    if content.is_empty() || content.len() > CONTENT_MAX {
        return Err(AppError::BadRequest(format!(
            "comment must be 1-{CONTENT_MAX} characters"
        )));
    }
    // Verify the post exists (also protects against orphan comments).
    let post = crate::repositories::posts::find_by_id(&app.db, input.post_id)
        .await?
        .ok_or_else(|| AppError::NotFound("post not found".into()))?;

    // Replies must target an existing approved comment on the same post, and
    // nesting is capped (root = depth 0; mirrors the rendering cap in the
    // post template). Walking up the parent chain also bounds pathological
    // cycles instead of looping forever.
    const MAX_REPLY_DEPTH: usize = 4;
    if let Some(pid) = input.parent_id {
        let parent = comments::find_by_id(&app.db, pid)
            .await?
            .filter(|p| p.post_id == input.post_id && p.status == CommentStatus::Approved)
            .ok_or_else(|| {
                AppError::BadRequest("the comment you replied to is no longer available".into())
            })?;
        let mut parent_depth = 0usize;
        let mut cursor = parent.parent_id;
        while let Some(ancestor_id) = cursor {
            parent_depth += 1;
            if parent_depth + 1 > MAX_REPLY_DEPTH {
                return Err(AppError::BadRequest("maximum reply depth reached".into()));
            }
            cursor = comments::find_by_id(&app.db, ancestor_id)
                .await?
                .map(|ancestor| ancestor.parent_id)
                .ok_or_else(|| {
                    AppError::BadRequest("the comment you replied to is no longer available".into())
                })?;
        }
    }

    let status = if moderate {
        CommentStatus::Pending
    } else {
        CommentStatus::Approved
    };

    let mut payload = json!({
        "post_id": input.post_id,
        "parent_id": input.parent_id,
        "author_name": name,
        "author_email": input.author_email.trim(),
        "author_url": input.author_url.trim(),
        "content": content,
        "status": status.as_str(),
        "post_title": post.title,
    });
    plugins::hook_json(app, "before_comment_create", &mut payload);

    let status = payload
        .get("status")
        .and_then(|s| s.as_str())
        .and_then(CommentStatus::parse)
        .unwrap_or(status);

    let new = comments::NewComment {
        post_id: input.post_id,
        parent_id: input.parent_id,
        author_name: payload
            .get("author_name")
            .and_then(|s| s.as_str())
            .unwrap_or(name)
            .to_string(),
        author_email: payload
            .get("author_email")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        author_url: payload
            .get("author_url")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        content: payload
            .get("content")
            .and_then(|s| s.as_str())
            .unwrap_or(content)
            .to_string(),
        status,
    };
    let id = comments::insert(&app.db, &new).await?;
    plugins::event_json(app, "after_comment_create", &payload);
    app.bump_content().await;

    // Read back the full row (id is authoritative; avoid ordering tricks).
    comments::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("comment vanished")))
}

pub async fn moderate(app: &App, id: i64, status: CommentStatus) -> AppResult<()> {
    if !comments::set_status(&app.db, id, status).await? {
        return Err(AppError::NotFound("comment not found".into()));
    }
    app.bump_content().await;
    Ok(())
}

pub async fn delete(app: &App, id: i64) -> AppResult<()> {
    if !comments::delete(&app.db, id).await? {
        return Err(AppError::NotFound("comment not found".into()));
    }
    app.bump_content().await;
    Ok(())
}
