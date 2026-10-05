//! PostgreSQL provider — tsvector + GIN index.
//!
//! `search_vector` is a weighted concatenation built with
//! `setweight(to_tsvector(...))` at write time. The configured numeric
//! weights are mapped onto the four tsvector weight letters (A > B > C > D)
//! by relative rank, so `[search.weights]` reorders field importance
//! without a schema change. Queries use sanitized prefix lexemes through
//! `to_tsquery` and rank with `ts_rank_cd` (cover density).

use sqlx::Row;

use crate::config::SearchWeights;
use crate::db::{Bind, Db};
use crate::error::AppResult;

use super::{SearchContext, filter_clauses, order_clause, result_cols, row_to_search_row};
use crate::search::result::{IndexedDoc, SearchRow};

pub struct PostgresSearch {
    language: String,
    weights: SearchWeights,
}

/// Fields contributing to the vector, in expression order.
const VECTOR_FIELDS: [&str; 6] = ["title", "excerpt", "tags", "category", "author", "content"];

impl PostgresSearch {
    pub fn new(language: &str, weights: SearchWeights) -> Self {
        Self {
            language: language.to_string(),
            weights,
        }
    }

    /// tsvector weight letter (A > B > C > D) for one field, derived from
    /// the relative rank of its configured weight.
    fn letter_for_field(&self, field: &str) -> char {
        let w = match field {
            "title" => self.weights.title,
            "excerpt" => self.weights.excerpt,
            "tags" => self.weights.tags,
            "category" => self.weights.category,
            "author" => self.weights.author,
            _ => self.weights.content,
        };
        let mut values: Vec<f64> = [
            self.weights.title,
            self.weights.excerpt,
            self.weights.tags,
            self.weights.category,
            self.weights.author,
            self.weights.content,
        ]
        .into_iter()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
        values.sort_by(|x, y| y.partial_cmp(x).unwrap_or(std::cmp::Ordering::Equal));
        values.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
        values.truncate(4);
        if w <= 0.0 || !w.is_finite() {
            return 'D';
        }
        let letters = ['A', 'B', 'C', 'D'];
        match values.iter().position(|v| (w - *v).abs() < 1e-9) {
            Some(rank) => letters[rank.min(3)],
            None => 'D',
        }
    }

    /// `(sql, binds)` for the weighted tsvector of `doc`.
    fn vector_for(&self, doc: &IndexedDoc) -> (String, Vec<Bind>) {
        let mut parts = Vec::new();
        let mut binds = Vec::new();
        for field in VECTOR_FIELDS {
            let letter = self.letter_for_field(field);
            let value = match field {
                "title" => doc.title.clone(),
                "excerpt" => doc.excerpt.clone(),
                "tags" => doc.tags.clone(),
                "category" => doc.category.clone(),
                "author" => doc.author.clone(),
                _ => doc.content.clone(),
            };
            // `?::regconfig` is required: sqlx binds the language as text, and
            // PostgreSQL has no implicit text→regconfig cast, so an uncast
            // parameter fails with "function to_tsvector(text, text) does not
            // exist".
            parts.push(format!(
                "setweight(to_tsvector(?::regconfig, ?), '{letter}')"
            ));
            binds.push(Bind::S(self.language.clone()));
            binds.push(Bind::S(value));
        }
        (parts.join(" || "), binds)
    }

    pub async fn upsert(&self, db: &Db, doc: &IndexedDoc) -> AppResult<()> {
        let (vec_sql, vec_binds) = self.vector_for(doc);
        let updated = db
            .execute(
                &format!(
                    "UPDATE search_index SET title = ?, slug = ?, excerpt = ?, content = ?, \
                     author = ?, category = ?, tags = ?, visible = ?, published_at = ?, \
                     updated_at = ?, search_vector = {vec_sql} \
                     WHERE ref_type = ? AND ref_id = ?"
                ),
                &[
                    vec![
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
                    vec_binds.clone(),
                    vec![Bind::S(doc.ref_type.to_string()), Bind::I(doc.ref_id)],
                ]
                .concat(),
            )
            .await?;
        if updated == 0 {
            db.execute(
                &format!(
                    "INSERT INTO search_index (ref_type, ref_id, title, slug, excerpt, content, \
                     author, category, tags, visible, published_at, updated_at, search_vector) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {vec_sql})"
                ),
                &[
                    vec![
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
                    vec_binds,
                ]
                .concat(),
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
        let (clauses, mut fbinds) = filter_clauses(ctx.query);
        let filters = if clauses.is_empty() {
            String::new()
        } else {
            format!("AND {}", clauses.join(" AND "))
        };
        let base = format!(
            "FROM search_index si, to_tsquery(?::regconfig, ?) q \
             WHERE si.search_vector @@ q {filters}"
        );
        let mut count_binds = vec![
            Bind::S(self.language.clone()),
            Bind::S(ctx.parsed.pg_tsquery.clone()),
        ];
        count_binds.extend(fbinds.clone());
        let total = db
            .fetch_one(&format!("SELECT COUNT(*) AS total {base}"), &count_binds)
            .await?
            .try_get::<i64, _>("total")
            .map_err(crate::error::AppError::Db)?;

        let offset = (ctx.page.saturating_sub(1)).saturating_mul(ctx.per_page) as i64;
        let mut qbinds = vec![
            Bind::S(self.language.clone()),
            Bind::S(ctx.parsed.pg_tsquery.clone()),
        ];
        qbinds.append(&mut fbinds);
        qbinds.push(Bind::I(ctx.per_page as i64));
        qbinds.push(Bind::I(offset));
        let rows = db
            .fetch_all(
                &format!(
                    "SELECT {}, ts_rank_cd(si.search_vector, q) AS score {base} {} LIMIT ? OFFSET ?",
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
