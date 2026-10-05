use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::models::{Post, PostStatus};

const COLS: &str = "p.id, p.title, p.slug, p.summary, p.content_md, p.author_id, p.status, \
                    p.featured_image, p.published_at, p.created_at, p.updated_at, \
                    COALESCE(NULLIF(u.display_name, ''), u.username) AS author_name";

pub struct NewPost {
    pub title: String,
    pub slug: String,
    pub summary: String,
    pub content_md: String,
    pub author_id: i64,
    pub status: PostStatus,
    pub featured_image: Option<String>,
    pub published_at: Option<i64>,
}

#[derive(Default)]
pub struct PostFilter {
    /// Only posts visible on the public site.
    pub public: bool,
    pub status: Option<PostStatus>,
    pub author_id: Option<i64>,
    pub term_id: Option<i64>,
    pub page: i64,
    pub per_page: i64,
}

pub async fn insert(db: &Db, p: &NewPost) -> AppResult<i64> {
    let now = crate::utils::time::now();
    db.insert(
        "INSERT INTO posts (title, slug, summary, content_md, author_id, status, \
         featured_image, published_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Bind::S(p.title.clone()),
            Bind::S(p.slug.clone()),
            Bind::S(p.summary.clone()),
            Bind::S(p.content_md.clone()),
            Bind::I(p.author_id),
            Bind::S(p.status.as_str().to_string()),
            Bind::OptS(p.featured_image.clone()),
            Bind::OptI(p.published_at),
            Bind::I(now),
            Bind::I(now),
        ],
    )
    .await
}

pub async fn update(db: &Db, id: i64, p: &NewPost) -> AppResult<()> {
    let now = crate::utils::time::now();
    db.execute(
        "UPDATE posts SET title = ?, slug = ?, summary = ?, content_md = ?, status = ?, \
         featured_image = ?, published_at = ?, updated_at = ? WHERE id = ?",
        &[
            Bind::S(p.title.clone()),
            Bind::S(p.slug.clone()),
            Bind::S(p.summary.clone()),
            Bind::S(p.content_md.clone()),
            Bind::S(p.status.as_str().to_string()),
            Bind::OptS(p.featured_image.clone()),
            Bind::OptI(p.published_at),
            Bind::I(now),
            Bind::I(id),
        ],
    )
    .await?;
    Ok(())
}

pub async fn find_by_id(db: &Db, id: i64) -> AppResult<Option<Post>> {
    let sql =
        format!("SELECT {COLS} FROM posts p JOIN users u ON u.id = p.author_id WHERE p.id = ?");
    let row = db.fetch_optional(&sql, &[Bind::I(id)]).await?;
    row.map(|r| Post::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn find_by_slug(db: &Db, slug: &str) -> AppResult<Option<Post>> {
    let sql =
        format!("SELECT {COLS} FROM posts p JOIN users u ON u.id = p.author_id WHERE p.slug = ?");
    let row = db
        .fetch_optional(&sql, &[Bind::S(slug.to_string())])
        .await?;
    row.map(|r| Post::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn list(db: &Db, f: &PostFilter) -> AppResult<(Vec<Post>, i64)> {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();

    if f.public {
        where_clauses.push(
            "(p.status = 'published' AND p.published_at IS NOT NULL AND p.published_at <= ?)"
                .into(),
        );
        binds.push(Bind::I(crate::utils::time::now()));
    }
    if let Some(st) = f.status {
        where_clauses.push("p.status = ?".into());
        binds.push(Bind::S(st.as_str().to_string()));
    }
    if let Some(a) = f.author_id {
        where_clauses.push("p.author_id = ?".into());
        binds.push(Bind::I(a));
    }
    if let Some(t) = f.term_id {
        where_clauses.push(
            "EXISTS (SELECT 1 FROM post_terms pt WHERE pt.post_id = p.id AND pt.term_id = ?)"
                .into(),
        );
        binds.push(Bind::I(t));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let total = db
        .fetch_one(
            &format!("SELECT COUNT(*) AS total FROM posts p {where_sql}"),
            &binds,
        )
        .await?
        .try_get::<i64, _>("total")
        .map_err(crate::error::AppError::Db)?;

    let order = if f.public {
        "ORDER BY COALESCE(p.published_at, p.created_at) DESC, p.id DESC"
    } else {
        "ORDER BY p.updated_at DESC, p.id DESC"
    };
    let page = f.page.max(1);
    let per = f.per_page.clamp(1, 100);
    let offset = (page - 1).saturating_mul(per);
    let mut qbinds = binds.clone();
    qbinds.push(Bind::I(per));
    qbinds.push(Bind::I(offset));

    let rows = db
        .fetch_all(
            &format!(
                "SELECT {COLS} FROM posts p JOIN users u ON u.id = p.author_id \
                 {where_sql} {order} LIMIT ? OFFSET ?"
            ),
            &qbinds,
        )
        .await?;
    let posts = rows
        .iter()
        .map(Post::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)?;
    Ok((posts, total))
}

pub async fn delete(db: &Db, id: i64) -> AppResult<bool> {
    let n = db
        .execute("DELETE FROM posts WHERE id = ?", &[Bind::I(id)])
        .await?;
    Ok(n > 0)
}

pub async fn slug_taken(db: &Db, slug: &str, except_id: Option<i64>) -> AppResult<bool> {
    let (sql, binds) = match except_id {
        Some(id) => (
            "SELECT COUNT(*) AS total FROM posts WHERE slug = ? AND id <> ?".to_string(),
            vec![Bind::S(slug.to_string()), Bind::I(id)],
        ),
        None => (
            "SELECT COUNT(*) AS total FROM posts WHERE slug = ?".to_string(),
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

/// Promote scheduled posts whose publish time has arrived.
pub async fn promote_scheduled(db: &Db, now: i64) -> AppResult<u64> {
    db.execute(
        "UPDATE posts SET status = 'published' WHERE status = 'scheduled' AND published_at <= ?",
        &[Bind::I(now)],
    )
    .await
}

pub struct StatusCounts {
    pub draft: i64,
    pub scheduled: i64,
    pub published: i64,
}

pub async fn counts_by_status(db: &Db) -> AppResult<StatusCounts> {
    let rows = db
        .fetch_all(
            "SELECT status, COUNT(*) AS total FROM posts GROUP BY status",
            &[],
        )
        .await?;
    let mut c = StatusCounts {
        draft: 0,
        scheduled: 0,
        published: 0,
    };
    for r in rows {
        let status: String = r.try_get("status").map_err(crate::error::AppError::Db)?;
        let total: i64 = r.try_get("total").map_err(crate::error::AppError::Db)?;
        match status.as_str() {
            "draft" => c.draft = total,
            "scheduled" => c.scheduled = total,
            "published" => c.published = total,
            _ => {}
        }
    }
    Ok(c)
}

/// Latest published posts (feeds, dashboard).
pub async fn latest_published(db: &Db, limit: i64) -> AppResult<Vec<Post>> {
    let now = crate::utils::time::now();
    let rows = db
        .fetch_all(
            &format!(
                "SELECT {COLS} FROM posts p JOIN users u ON u.id = p.author_id \
                 WHERE p.status = 'published' AND p.published_at IS NOT NULL AND p.published_at <= ? \
                 ORDER BY p.published_at DESC LIMIT ?"
            ),
            &[Bind::I(now), Bind::I(limit)],
        )
        .await?;
    rows.iter()
        .map(Post::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}
