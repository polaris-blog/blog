use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::models::{Post, Term, TermKind};

const COLS: &str = "t.id, t.kind, t.name, t.slug";

/// Find or create a term, returning its id.
pub async fn ensure(db: &Db, kind: TermKind, name: &str, slug: &str) -> AppResult<i64> {
    if let Some(id) = find_id_by_slug(db, kind, slug).await? {
        return Ok(id);
    }
    let res = db
        .insert(
            "INSERT INTO terms (kind, name, slug) VALUES (?, ?, ?)",
            &[
                Bind::S(kind.as_str().to_string()),
                Bind::S(name.to_string()),
                Bind::S(slug.to_string()),
            ],
        )
        .await;
    match res {
        Ok(id) => Ok(id),
        // Lost a race against a concurrent insert — re-select.
        Err(_) => find_id_by_slug(db, kind, slug)
            .await?
            .ok_or_else(|| crate::error::AppError::Conflict("term creation failed".into())),
    }
}

pub async fn find_id_by_slug(db: &Db, kind: TermKind, slug: &str) -> AppResult<Option<i64>> {
    let row = db
        .fetch_optional(
            "SELECT id FROM terms WHERE kind = ? AND slug = ?",
            &[
                Bind::S(kind.as_str().to_string()),
                Bind::S(slug.to_string()),
            ],
        )
        .await?;
    Ok(row.map(|r| r.try_get::<i64, _>("id")).transpose()?)
}

pub async fn find_by_slug(db: &Db, kind: TermKind, slug: &str) -> AppResult<Option<Term>> {
    let sql = format!("SELECT {COLS} FROM terms t WHERE t.kind = ? AND t.slug = ?");
    let row = db
        .fetch_optional(
            &sql,
            &[
                Bind::S(kind.as_str().to_string()),
                Bind::S(slug.to_string()),
            ],
        )
        .await?;
    row.map(|r| Term::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn find_by_id(db: &Db, id: i64) -> AppResult<Option<Term>> {
    let sql = format!("SELECT {COLS} FROM terms t WHERE t.id = ?");
    let row = db.fetch_optional(&sql, &[Bind::I(id)]).await?;
    row.map(|r| Term::from_row(&r).map_err(Into::into))
        .transpose()
}

/// List terms of a kind with published-ish post counts.
pub async fn list_with_counts(db: &Db, kind: TermKind) -> AppResult<Vec<Term>> {
    let sql = format!(
        "SELECT {COLS}, COUNT(pt.post_id) AS cnt FROM terms t \
         LEFT JOIN post_terms pt ON pt.term_id = t.id \
         WHERE t.kind = ? GROUP BY t.id, t.kind, t.name, t.slug ORDER BY t.name"
    );
    let rows = db
        .fetch_all(&sql, &[Bind::S(kind.as_str().to_string())])
        .await?;
    rows.iter()
        .map(Term::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}

pub async fn list_for_post(db: &Db, post_id: i64) -> AppResult<Vec<Term>> {
    let sql = format!(
        "SELECT {COLS} FROM terms t JOIN post_terms pt ON pt.term_id = t.id \
         WHERE pt.post_id = ? ORDER BY t.kind, t.name"
    );
    let rows = db.fetch_all(&sql, &[Bind::I(post_id)]).await?;
    rows.iter()
        .map(Term::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}

/// Bulk-attach terms to a list of posts (used on index pages).
pub async fn attach(db: &Db, posts: &mut [Post]) -> AppResult<()> {
    if posts.is_empty() {
        return Ok(());
    }
    let ids: Vec<Bind> = posts.iter().map(|p| Bind::I(p.id)).collect();
    let placeholders = vec!["?"; posts.len()].join(",");
    let sql = format!(
        "SELECT pt.post_id, t.id, t.kind, t.name, t.slug FROM post_terms pt \
         JOIN terms t ON t.id = pt.term_id WHERE pt.post_id IN ({placeholders}) \
         ORDER BY t.kind, t.name"
    );
    let rows = db.fetch_all(&sql, &ids).await?;
    for r in &rows {
        let post_id: i64 = r.try_get("post_id").map_err(crate::error::AppError::Db)?;
        if let Ok(term) = Term::from_row(r)
            && let Some(p) = posts.iter_mut().find(|p| p.id == post_id)
        {
            p.terms.push(term);
        }
    }
    Ok(())
}

/// Replace the term set of a post (categories + tags) in one transaction.
pub async fn set_post_terms(db: &Db, post_id: i64, term_ids: &[i64]) -> AppResult<()> {
    let mut tx = db.pool().begin().await?;
    let d = db.dialect();
    let sql = d.translate("DELETE FROM post_terms WHERE post_id = ?");
    sqlx::query(sql.as_ref())
        .bind(post_id)
        .execute(&mut *tx)
        .await?;
    for tid in term_ids {
        let sql = d.translate("INSERT INTO post_terms (post_id, term_id) VALUES (?, ?)");
        sqlx::query(sql.as_ref())
            .bind(post_id)
            .bind(tid)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> AppResult<bool> {
    let n = db
        .execute("DELETE FROM terms WHERE id = ?", &[Bind::I(id)])
        .await?;
    Ok(n > 0)
}
