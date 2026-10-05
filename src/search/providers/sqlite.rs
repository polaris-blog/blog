//! SQLite provider — FTS5 external-content index.
//!
//! `search_fts` indexes the text columns of `search_index` (the content
//! table). Relevance is `bm25()` with the configured per-field weights,
//! passed as arguments in FTS column order (title, excerpt, author,
//! category, tags, content). The index is maintained application-side:
//! updates issue the FTS5 `'delete'` command for the old row before
//! writing the new one (required by external-content tables).

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;

use super::{SearchContext, filter_clauses, order_clause, result_cols, row_to_search_row, sqlf};
use crate::search::result::{IndexedDoc, SearchRow};

pub struct SqliteSearch;

const FTS_COLS: &str = "title, excerpt, author, category, tags, content";

impl SqliteSearch {
    pub async fn upsert(&self, db: &Db, doc: &IndexedDoc) -> AppResult<()> {
        let old = db
            .fetch_optional(
                &format!(
                    "SELECT id, {FTS_COLS} FROM search_index WHERE ref_type = ? AND ref_id = ?"
                ),
                &[Bind::S(doc.ref_type.to_string()), Bind::I(doc.ref_id)],
            )
            .await?;
        let doc_binds = |extra: Vec<Bind>| {
            [
                vec![
                    Bind::S(doc.title.clone()),
                    Bind::S(doc.excerpt.clone()),
                    Bind::S(doc.author.clone()),
                    Bind::S(doc.category.clone()),
                    Bind::S(doc.tags.clone()),
                    Bind::S(doc.content.clone()),
                ],
                extra,
            ]
            .concat()
        };
        match old {
            Some(row) => {
                let id: i64 = row.try_get("id").map_err(crate::error::AppError::Db)?;
                let old_text: Vec<Bind> =
                    ["title", "excerpt", "author", "category", "tags", "content"]
                        .iter()
                        .map(|c| Bind::S(row.try_get::<String, _>(c).unwrap_or_default()))
                        .collect();
                // FTS5 external content: retract the old tokens, then
                // rewrite the row and index the new tokens.
                let mut del = vec![Bind::I(id)];
                del.extend(old_text);
                db.execute(
                    &format!(
                        "INSERT INTO search_fts(search_fts, rowid, {FTS_COLS}) \
                         VALUES ('delete', ?, ?, ?, ?, ?, ?, ?)"
                    ),
                    &del,
                )
                .await?;
                db.execute(
                    "UPDATE search_index SET title = ?, slug = ?, excerpt = ?, content = ?, \
                     author = ?, category = ?, tags = ?, visible = ?, published_at = ?, \
                     updated_at = ? WHERE id = ?",
                    &[
                        Bind::S(doc.title.clone()),
                        Bind::S(doc.slug.clone()),
                        Bind::S(doc.excerpt.clone()),
                        Bind::S(doc.content.clone()),
                        Bind::S(doc.author.clone()),
                        Bind::S(doc.category.clone()),
                        Bind::S(doc.tags.clone()),
                        Bind::I(doc.visible as i64),
                        Bind::OptI(doc.published_at),
                        Bind::I(doc.updated_at),
                        Bind::I(id),
                    ],
                )
                .await?;
                let mut ins = vec![Bind::I(id)];
                ins.extend(doc_binds(Vec::new()));
                db.execute(
                    &format!(
                        "INSERT INTO search_fts(rowid, {FTS_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?)"
                    ),
                    &ins,
                )
                .await?;
            }
            None => {
                let id = db
                    .insert(
                        "INSERT INTO search_index (ref_type, ref_id, title, slug, excerpt, \
                         content, author, category, tags, visible, published_at, updated_at) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        &[
                            Bind::S(doc.ref_type.to_string()),
                            Bind::I(doc.ref_id),
                            Bind::S(doc.title.clone()),
                            Bind::S(doc.slug.clone()),
                            Bind::S(doc.excerpt.clone()),
                            Bind::S(doc.content.clone()),
                            Bind::S(doc.author.clone()),
                            Bind::S(doc.category.clone()),
                            Bind::S(doc.tags.clone()),
                            Bind::I(doc.visible as i64),
                            Bind::OptI(doc.published_at),
                            Bind::I(doc.updated_at),
                        ],
                    )
                    .await?;
                let mut ins = vec![Bind::I(id)];
                ins.extend(doc_binds(Vec::new()));
                db.execute(
                    &format!(
                        "INSERT INTO search_fts(rowid, {FTS_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?)"
                    ),
                    &ins,
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn remove(&self, db: &Db, ref_type: &str, ref_id: i64) -> AppResult<()> {
        let old = db
            .fetch_optional(
                &format!(
                    "SELECT id, {FTS_COLS} FROM search_index WHERE ref_type = ? AND ref_id = ?"
                ),
                &[Bind::S(ref_type.to_string()), Bind::I(ref_id)],
            )
            .await?;
        let Some(row) = old else { return Ok(()) };
        let id: i64 = row.try_get("id").map_err(crate::error::AppError::Db)?;
        let mut del = vec![Bind::I(id)];
        for c in ["title", "excerpt", "author", "category", "tags", "content"] {
            del.push(Bind::S(row.try_get::<String, _>(c).unwrap_or_default()));
        }
        db.execute(
            &format!(
                "INSERT INTO search_fts(search_fts, rowid, {FTS_COLS}) \
                 VALUES ('delete', ?, ?, ?, ?, ?, ?, ?)"
            ),
            &del,
        )
        .await?;
        db.execute("DELETE FROM search_index WHERE id = ?", &[Bind::I(id)])
            .await?;
        Ok(())
    }

    pub async fn clear(&self, db: &Db) -> AppResult<()> {
        db.execute(
            "INSERT INTO search_fts(search_fts) VALUES ('delete-all')",
            &[],
        )
        .await?;
        db.execute("DELETE FROM search_index", &[]).await?;
        Ok(())
    }

    pub async fn search(
        &self,
        db: &Db,
        ctx: &SearchContext<'_>,
    ) -> AppResult<(Vec<SearchRow>, i64)> {
        let w = ctx.weights;
        // bm25() column order matches the FTS table definition.
        let score = format!(
            "-bm25(search_fts, {}, {}, {}, {}, {}, {})",
            sqlf(w.title),
            sqlf(w.excerpt),
            sqlf(w.author),
            sqlf(w.category),
            sqlf(w.tags),
            sqlf(w.content)
        );
        let (clauses, mut fbinds) = filter_clauses(ctx.query);
        let filters = if clauses.is_empty() {
            String::new()
        } else {
            format!("AND {}", clauses.join(" AND "))
        };
        let base = format!(
            "FROM search_fts JOIN search_index si ON si.id = search_fts.rowid \
             WHERE search_fts MATCH ? {filters}"
        );
        let mut count_binds = vec![Bind::S(ctx.parsed.fts_match.clone())];
        count_binds.extend(fbinds.clone());
        let total = db
            .fetch_one(&format!("SELECT COUNT(*) AS total {base}"), &count_binds)
            .await?
            .try_get::<i64, _>("total")
            .map_err(crate::error::AppError::Db)?;

        let offset = (ctx.page.saturating_sub(1)).saturating_mul(ctx.per_page) as i64;
        let mut qbinds = vec![Bind::S(ctx.parsed.fts_match.clone())];
        qbinds.append(&mut fbinds);
        qbinds.push(Bind::I(ctx.per_page as i64));
        qbinds.push(Bind::I(offset));
        let rows = db
            .fetch_all(
                &format!(
                    "SELECT {}, {} AS score {base} {} LIMIT ? OFFSET ?",
                    result_cols(),
                    score,
                    order_clause(ctx.query.sort)
                ),
                &qbinds,
            )
            .await?;
        let out = rows
            .iter()
            .map(row_to_search_row)
            .collect::<sqlx::Result<Vec<_>>>()
            .map_err(crate::error::AppError::Db)?;
        Ok((out, total))
    }

    /// FTS5 integrity check — fails loudly when the index is corrupted.
    pub async fn probe(&self, db: &Db) -> bool {
        db.execute(
            "INSERT INTO search_fts(search_fts) VALUES ('integrity-check')",
            &[],
        )
        .await
        .is_ok()
    }
}
