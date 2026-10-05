use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::models::{Comment, CommentStatus};

pub struct NewComment {
    pub post_id: i64,
    pub parent_id: Option<i64>,
    pub author_name: String,
    pub author_email: String,
    pub author_url: String,
    pub content: String,
    pub status: CommentStatus,
}

const COLS: &str = "c.id, c.post_id, c.parent_id, c.author_name, c.author_email, c.author_url, \
                    c.content, c.status, c.created_at";

pub async fn insert(db: &Db, c: &NewComment) -> AppResult<i64> {
    db.insert(
        "INSERT INTO comments (post_id, parent_id, author_name, author_email, author_url, \
         content, status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Bind::I(c.post_id),
            Bind::OptI(c.parent_id),
            Bind::S(c.author_name.clone()),
            Bind::S(c.author_email.clone()),
            Bind::S(c.author_url.clone()),
            Bind::S(c.content.clone()),
            Bind::S(c.status.as_str().to_string()),
            Bind::I(crate::utils::time::now()),
        ],
    )
    .await
}

pub async fn find_by_id(db: &Db, id: i64) -> AppResult<Option<Comment>> {
    let sql = format!("SELECT {COLS} FROM comments c WHERE c.id = ?");
    let row = db.fetch_optional(&sql, &[Bind::I(id)]).await?;
    row.map(|r| Comment::from_row(&r))
        .transpose()
        .map_err(crate::error::AppError::Db)
}

pub async fn list(
    db: &Db,
    status: Option<CommentStatus>,
    page: i64,
    per_page: i64,
) -> AppResult<(Vec<Comment>, i64)> {
    let (cond, mut binds) = match status {
        Some(s) => ("WHERE c.status = ?", vec![Bind::S(s.as_str().to_string())]),
        None => ("", vec![]),
    };
    let total = db
        .fetch_one(
            &format!("SELECT COUNT(*) AS total FROM comments c {cond}"),
            &binds,
        )
        .await?
        .try_get::<i64, _>("total")
        .map_err(crate::error::AppError::Db)?;

    let page = page.max(1);
    let per = per_page.clamp(1, 100);
    binds.push(Bind::I(per));
    binds.push(Bind::I((page - 1).saturating_mul(per)));
    let rows = db
        .fetch_all(
            &format!(
                "SELECT {COLS}, p.title AS post_title, p.slug AS post_slug \
                 FROM comments c JOIN posts p ON p.id = c.post_id {cond} \
                 ORDER BY c.created_at DESC, c.id DESC LIMIT ? OFFSET ?"
            ),
            &binds,
        )
        .await?;
    let comments = rows
        .iter()
        .map(Comment::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)?;
    Ok((comments, total))
}

pub async fn list_approved_for_post(db: &Db, post_id: i64) -> AppResult<Vec<Comment>> {
    let sql = format!(
        "SELECT {COLS} FROM comments c WHERE c.post_id = ? AND c.status = 'approved' \
         ORDER BY c.created_at ASC"
    );
    let rows = db.fetch_all(&sql, &[Bind::I(post_id)]).await?;
    rows.iter()
        .map(Comment::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}

pub struct CommentCounts {
    pub pending: i64,
    pub approved: i64,
    pub spam: i64,
}

pub async fn counts_by_status(db: &Db) -> AppResult<CommentCounts> {
    let rows = db
        .fetch_all(
            "SELECT status, COUNT(*) AS total FROM comments GROUP BY status",
            &[],
        )
        .await?;
    let mut c = CommentCounts {
        pending: 0,
        approved: 0,
        spam: 0,
    };
    for r in rows {
        let status: String = r.try_get("status").map_err(crate::error::AppError::Db)?;
        let total: i64 = r.try_get("total").map_err(crate::error::AppError::Db)?;
        match status.as_str() {
            "pending" => c.pending = total,
            "approved" => c.approved = total,
            "spam" => c.spam = total,
            _ => {}
        }
    }
    Ok(c)
}

pub async fn set_status(db: &Db, id: i64, status: CommentStatus) -> AppResult<bool> {
    let n = db
        .execute(
            "UPDATE comments SET status = ? WHERE id = ?",
            &[Bind::S(status.as_str().to_string()), Bind::I(id)],
        )
        .await?;
    Ok(n > 0)
}

pub async fn delete(db: &Db, id: i64) -> AppResult<bool> {
    let n = db
        .execute("DELETE FROM comments WHERE id = ?", &[Bind::I(id)])
        .await?;
    Ok(n > 0)
}

pub async fn recent(db: &Db, limit: i64) -> AppResult<Vec<Comment>> {
    let rows = db
        .fetch_all(
            &format!(
                "SELECT {COLS}, p.title AS post_title, p.slug AS post_slug \
                 FROM comments c JOIN posts p ON p.id = c.post_id \
                 ORDER BY c.created_at DESC LIMIT ?"
            ),
            &[Bind::I(limit)],
        )
        .await?;
    rows.iter()
        .map(Comment::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}
