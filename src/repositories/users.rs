use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::models::{Role, User};

pub struct NewUser {
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub role: Role,
    pub display_name: String,
}

pub async fn insert(db: &Db, u: &NewUser) -> AppResult<i64> {
    let now = crate::utils::time::now();
    db.insert(
        "INSERT INTO users (username, email, password_hash, role, display_name, bio, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, '', ?, ?)",
        &[
            Bind::S(u.username.clone()),
            Bind::S(u.email.clone()),
            Bind::S(u.password_hash.clone()),
            Bind::S(u.role.as_str().to_string()),
            Bind::S(u.display_name.clone()),
            Bind::I(now),
            Bind::I(now),
        ],
    )
    .await
}

pub async fn find_by_username(db: &Db, username: &str) -> AppResult<Option<User>> {
    let row = db
        .fetch_optional(
            "SELECT * FROM users WHERE username = ?",
            &[Bind::S(username.to_string())],
        )
        .await?;
    row.map(|r| User::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn find_by_id(db: &Db, id: i64) -> AppResult<Option<User>> {
    let row = db
        .fetch_optional("SELECT * FROM users WHERE id = ?", &[Bind::I(id)])
        .await?;
    row.map(|r| User::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn list(db: &Db) -> AppResult<Vec<User>> {
    let rows = db.fetch_all("SELECT * FROM users ORDER BY id", &[]).await?;
    rows.iter()
        .map(User::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)
}

pub async fn count(db: &Db) -> AppResult<i64> {
    let n = db
        .fetch_one("SELECT COUNT(*) AS total FROM users", &[])
        .await?
        .try_get::<i64, _>("total")
        .map_err(crate::error::AppError::Db)?;
    Ok(n)
}

pub async fn update_profile(
    db: &Db,
    id: i64,
    email: &str,
    display_name: &str,
    bio: &str,
) -> AppResult<()> {
    db.execute(
        "UPDATE users SET email = ?, display_name = ?, bio = ?, updated_at = ? WHERE id = ?",
        &[
            Bind::S(email.to_string()),
            Bind::S(display_name.to_string()),
            Bind::S(bio.to_string()),
            Bind::I(crate::utils::time::now()),
            Bind::I(id),
        ],
    )
    .await?;
    Ok(())
}

pub async fn update_password(db: &Db, id: i64, password_hash: &str) -> AppResult<()> {
    db.execute(
        "UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?",
        &[
            Bind::S(password_hash.to_string()),
            Bind::I(crate::utils::time::now()),
            Bind::I(id),
        ],
    )
    .await?;
    Ok(())
}

pub async fn update_role(db: &Db, id: i64, role: Role) -> AppResult<()> {
    let mut tx = db.pool().begin().await?;
    if role != Role::Admin {
        guard_last_admin(db, &mut tx, id).await?;
    }
    let sql = db.translate("UPDATE users SET role = ?, updated_at = ? WHERE id = ?");
    sqlx::query(&sql)
        .bind(role.as_str())
        .bind(crate::utils::time::now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> AppResult<bool> {
    let mut tx = db.pool().begin().await?;
    guard_last_admin(db, &mut tx, id).await?;
    let sql = db.translate("DELETE FROM users WHERE id = ?");
    let n = sqlx::query(&sql)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(n > 0)
}

async fn guard_last_admin(
    db: &Db,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: i64,
) -> AppResult<()> {
    // Acquire write locks before reading; concurrent demotions/deletions serialize.
    sqlx::query("UPDATE users SET role = role WHERE role = 'admin'")
        .execute(&mut **tx)
        .await?;
    let sql = db.translate("SELECT role FROM users WHERE id = ?");
    let target = sqlx::query(&sql).bind(id).fetch_optional(&mut **tx).await?;
    if target.is_some_and(|r| {
        r.try_get::<String, _>("role")
            .is_ok_and(|role| role == "admin")
    }) {
        let row = sqlx::query("SELECT COUNT(*) AS total FROM users WHERE role = 'admin'")
            .fetch_one(&mut **tx)
            .await?;
        if row.try_get::<i64, _>("total")? <= 1 {
            return Err(crate::error::AppError::BadRequest(
                "cannot remove the last admin".into(),
            ));
        }
    }
    Ok(())
}
