//! MySQL provider — InnoDB FULLTEXT index.
//!
//! Matching runs in BOOLEAN MODE (`+term*` for AND + prefix semantics,
//! matching the behaviour of the other engines). Relevance is the natural
//! MATCH score plus LIKE-based boosts for title/tags/category (per-column
//! weights are not expressible in a single MySQL MATCH expression).
//! Column order in MATCH() must mirror the FULLTEXT index definition:
//! (title, excerpt, content, author, category, tags).

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;

use super::{SearchContext, filter_clauses, order_clause, result_cols, row_to_search_row, sqlf};
use crate::search::result::{IndexedDoc, SearchRow};

pub struct MySqlSearch;

/// Must match the FULLTEXT KEY column order in the migration.
const MATCH_COLS: &str = "si.title, si.excerpt, si.content, si.author, si.category, si.tags";

impl MySqlSearch {
    pub async fn upsert(&self, db: &Db, doc: &IndexedDoc) -> AppResult<()> {
        let binds = vec![
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
        ];
        let updated = db
            .execute(
                "UPDATE search_index SET title = ?, slug = ?, excerpt = ?, content = ?, \
                 author = ?, category = ?, tags = ?, visible = ?, published_at = ?, \
                 updated_at = ? WHERE ref_type = ? AND ref_id = ?",
                &[
                    binds[2].clone(),
                    binds[3].clone(),
                    binds[4].clone(),
                    binds[5].clone(),
                    binds[6].clone(),
                    binds[7].clone(),
                    binds[8].clone(),
                    binds[9].clone(),
                    binds[10].clone(),
                    binds[11].clone(),
                    binds[0].clone(),
                    binds[1].clone(),
                ],
            )
            .await?;
        if updated == 0 {
            db.execute(
                "INSERT INTO search_index (ref_type, ref_id, title, slug, excerpt, content, \
                 author, category, tags, visible, published_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                &binds,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn remove(&self, db: &Db, ref_type: &str, ref_id: i64) -> AppResult<()> {
        db.execute(
            "DELETE FROM search_index WHERE ref_type = ? AND ref_id = ?",
            &[Bind::S(ref_type.to_string()), Bind::I(ref_id)],
        )
        .await?;
        Ok(())
    }

    pub async fn clear(&self, db: &Db) -> AppResult<()> {
        db.execute("DELETE FROM search_index", &[]).await?;
        Ok(())
    }

    pub async fn search(
        &self,
        db: &Db,
        ctx: &SearchContext<'_>,
    ) -> AppResult<(Vec<SearchRow>, i64)> {
        let (rows, total) = self.fulltext_search(db, ctx).await?;
        if total > 0 {
            return Ok((rows, total));
        }
        // FULLTEXT ignores terms below `innodb_ft_min_token_size`
        // (default 3). Give short queries a bounded LIKE fallback on the
        // small denormalized index instead of failing empty.
        self.like_fallback(db, ctx).await
    }

    async fn fulltext_search(
        &self,
        db: &Db,
        ctx: &SearchContext<'_>,
    ) -> AppResult<(Vec<SearchRow>, i64)> {
        let w = ctx.weights;
        let terms = &ctx.parsed.terms;
        let mut boost_sql = String::new();
        let mut boost_binds: Vec<Bind> = Vec::new();
        // Like-based per-field boosts on the already-matched row set.
        for (field, weight) in [
            ("si.title", w.title),
            ("si.tags", w.tags),
            ("si.category", w.category),
            ("si.excerpt", w.excerpt),
        ] {
            if weight <= 0.0 {
                continue;
            }
            let mut cases = Vec::new();
            for t in terms {
                cases.push(format!("{field} LIKE ?"));
                boost_binds.push(Bind::S(format!("%{}%", t)));
            }
            if !cases.is_empty() {
                boost_sql.push_str(&format!(
                    " + (CASE WHEN {} THEN {} ELSE 0 END)",
                    cases.join(" OR "),
                    sqlf(weight)
                ));
            }
        }
        let score_expr = format!("MATCH({MATCH_COLS}) AGAINST (? IN BOOLEAN MODE){boost_sql}");
        let (clauses, mut fbinds) = filter_clauses(ctx.query);
        let filters = if clauses.is_empty() {
            String::new()
        } else {
            format!("AND {}", clauses.join(" AND "))
        };
        let base = format!(
            "FROM search_index si WHERE MATCH({MATCH_COLS}) AGAINST (? IN BOOLEAN MODE) \
             {filters}"
        );
        let mut count_binds = vec![Bind::S(ctx.parsed.mysql_boolean.clone())];
        count_binds.extend(fbinds.clone());
        let total = db
            .fetch_one(&format!("SELECT COUNT(*) AS total {base}"), &count_binds)
            .await?
            .try_get::<i64, _>("total")
            .map_err(crate::error::AppError::Db)?;

        let offset = (ctx.page.saturating_sub(1)).saturating_mul(ctx.per_page) as i64;
        let mut qbinds = vec![Bind::S(ctx.parsed.mysql_boolean.clone())];
        qbinds.append(&mut boost_binds);
        // The WHERE MATCH has its own placeholder, after the SELECT score
        // and field boosts. Each positional placeholder needs its own bind.
        qbinds.push(Bind::S(ctx.parsed.mysql_boolean.clone()));
        qbinds.append(&mut fbinds);
        qbinds.push(Bind::I(ctx.per_page as i64));
        qbinds.push(Bind::I(offset));
        let rows = db
            .fetch_all(
                &format!(
                    "SELECT {}, {} AS score {base} {} LIMIT ? OFFSET ?",
                    result_cols(),
                    score_expr,
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

    /// Bounded fallback for terms too short for FULLTEXT (e.g. 2-char).
    async fn like_fallback(
        &self,
        db: &Db,
        ctx: &SearchContext<'_>,
    ) -> AppResult<(Vec<SearchRow>, i64)> {
        let (clauses, mut fbinds) = filter_clauses(ctx.query);
        let filters = if clauses.is_empty() {
            String::new()
        } else {
            format!("AND {}", clauses.join(" AND "))
        };
        let mut like_conds: Vec<&str> = Vec::new();
        let mut like_binds: Vec<Bind> = Vec::new();
        for t in &ctx.parsed.terms {
            like_conds.push("(si.title LIKE ? OR si.tags LIKE ? OR si.excerpt LIKE ?)");
            for _ in 0..3 {
                like_binds.push(Bind::S(format!("%{}%", t)));
            }
        }
        let base = format!(
            "FROM search_index si WHERE ({}) {filters}",
            like_conds.join(" OR ")
        );
        let mut count_binds = like_binds.clone();
        count_binds.append(&mut fbinds.clone());
        let total = db
            .fetch_one(&format!("SELECT COUNT(*) AS total {base}"), &count_binds)
            .await?
            .try_get::<i64, _>("total")
            .map_err(crate::error::AppError::Db)?;
        let offset = (ctx.page.saturating_sub(1)).saturating_mul(ctx.per_page) as i64;
        let mut qbinds = like_binds;
        qbinds.append(&mut fbinds);
        qbinds.push(Bind::I(ctx.per_page as i64));
        qbinds.push(Bind::I(offset));
        let rows = db
            .fetch_all(
                &format!(
                    "SELECT {}, 0.0 AS score {base} {} LIMIT ? OFFSET ?",
                    result_cols(),
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
}
