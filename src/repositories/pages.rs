use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::models::{Page, PostStatus};

const COLS: &str = "g.id, g.title, g.slug, g.summary, g.content_md, g.author_id, g.status, \
                    g.sort_order, g.created_at, g.updated_at, \
                    COALESCE(NULLIF(u.display_name, ''), u.username) AS author_name";

pub struct NewPage {
    pub title: String,
    pub slug: String,
    pub summary: String,
    pub content_md: String,
    pub author_id: i64,
    pub status: PostStatus,
    pub sort_order: i64,
}

pub async fn insert(db: &Db, p: &NewPage) -> AppResult<i64> {
    let now = crate::utils::time::now();
    db.insert(
        "INSERT INTO pages (title, slug, summary, content_md, author_id, status, sort_order, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Bind::S(p.title.clone()),
            Bind::S(p.slug.clone()),
            Bind::S(p.summary.clone()),
            Bind::S(p.content_md.clone()),
            Bind::I(p.author_id),
            Bind::S(p.status.as_str().to_string()),
            Bind::I(p.sort_order),
            Bind::I(now),
            Bind::I(now),
        ],
    )
    .await
}

pub async fn update(db: &Db, id: i64, p: &NewPage) -> AppResult<()> {
    let now = crate::utils::time::now();
    db.execute(
        "UPDATE pages SET title = ?, slug = ?, summary = ?, content_md = ?, status = ?, \
         sort_order = ?, updated_at = ? WHERE id = ?",
        &[
            Bind::S(p.title.clone()),
            Bind::S(p.slug.clone()),
            Bind::S(p.summary.clone()),
            Bind::S(p.content_md.clone()),
            Bind::S(p.status.as_str().to_string()),
            Bind::I(p.sort_order),
            Bind::I(now),
            Bind::I(id),
        ],
    )
    .await?;
    Ok(())
}

pub async fn find_by_id(db: &Db, id: i64) -> AppResult<Option<Page>> {
    let sql =
        format!("SELECT {COLS} FROM pages g JOIN users u ON u.id = g.author_id WHERE g.id = ?");
    let row = db.fetch_optional(&sql, &[Bind::I(id)]).await?;
    row.map(|r| Page::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn find_by_slug(db: &Db, slug: &str) -> AppResult<Option<Page>> {
    let sql =
        format!("SELECT {COLS} FROM pages g JOIN users u ON u.id = g.author_id WHERE g.slug = ?");
    let row = db
        .fetch_optional(&sql, &[Bind::S(slug.to_string())])
        .await?;
    row.map(|r| Page::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn list(db: &Db, public_only: bool) -> AppResult<Vec<Page>> {
    let cond = if public_only {
        "WHERE g.status = 'published'"
    } else {
        ""
    };
    let rows = db
        .fetch_all(
            &format!(
                "SELECT {COLS} FROM pages g JOIN users u ON u.id = g.author_id {cond} \
                 ORDER BY g.sort_order, g.title"
            ),
            &[],
        )
        .await?;
    rows.iter()
        .map(Page::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}

pub async fn delete(db: &Db, id: i64) -> AppResult<bool> {
    let n = db
        .execute("DELETE FROM pages WHERE id = ?", &[Bind::I(id)])
        .await?;
    Ok(n > 0)
}

pub async fn slug_taken(db: &Db, slug: &str, except_id: Option<i64>) -> AppResult<bool> {
    let (sql, binds) = match except_id {
        Some(id) => (
            "SELECT COUNT(*) AS total FROM pages WHERE slug = ? AND id <> ?".to_string(),
            vec![Bind::S(slug.to_string()), Bind::I(id)],
        ),
        None => (
            "SELECT COUNT(*) AS total FROM pages WHERE slug = ?".to_string(),
            vec![Bind::S(slug.to_string())],
        ),
    };
    let n = db
        .fetch_one(&sql, &binds)
        .await?
        .try_get::<i64, _>("total")
        .map_err(crate::error::AppError::Db)?;
    Ok(n > 0)
}
