use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::models::{Role, User};
use crate::plugins;
use crate::repositories::{self, users};
use crate::state::App;
use crate::utils::slug;

const USERNAME_MIN: usize = 3;
const USERNAME_MAX: usize = 32;
const PASSWORD_MIN: usize = 8;

pub fn validate_username(name: &str) -> AppResult<()> {
    let n = name.trim();
    if n.len() < USERNAME_MIN || n.len() > USERNAME_MAX {
        return Err(AppError::BadRequest(format!(
            "username must be {USERNAME_MIN}-{USERNAME_MAX} characters"
        )));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::BadRequest(
            "username may only contain letters, digits, '-' and '_'".into(),
        ));
    }
    Ok(())
}

pub fn validate_password(pw: &str) -> AppResult<()> {
    if pw.chars().count() < PASSWORD_MIN {
        return Err(AppError::BadRequest(format!(
            "password must be at least {PASSWORD_MIN} characters"
        )));
    }
    Ok(())
}

pub async fn create_user(
    app: &App,
    username: &str,
    email: &str,
    password: &str,
    role: Role,
) -> AppResult<User> {
    let username = username.trim();
    validate_username(username)?;
    validate_password(password)?;
    if users::find_by_username(&app.db, username).await?.is_some() {
        return Err(AppError::Conflict("username already taken".into()));
    }
    let hash = crate::auth::hash_password(password)?;
    let display_name = username.to_string();
    let id = users::insert(
        &app.db,
        &users::NewUser {
            username: username.to_string(),
            email: email.trim().to_string(),
            password_hash: hash,
            role,
            display_name,
        },
    )
    .await?;
    let user = users::find_by_id(&app.db, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("user vanished after insert")))?;

    // First user ever: seed sample content so a fresh install is not empty,
    // and retire the setup wizard flag.
    if users::count(&app.db).await? <= 1 {
        app.set_setup_done();
        if let Err(e) = crate::services::seed::seed_content(app, user.id).await {
            tracing::warn!(error = %e, "seeding sample content failed");
        }
    }
    Ok(user)
}

/// One-time dummy Argon2id hash, verified against when the username does not
/// exist, so response timing cannot distinguish "no such user" from "wrong
/// password" (otherwise the missing Argon2 pass is a cheap user-enumeration
/// oracle). Built once at first use with the same parameters as real hashes.
fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| {
        crate::auth::hash_password(&crate::utils::cookies::random_token(16))
            .unwrap_or_else(|_| "$argon2id$v=19$m=19456,t=2,p=1$".to_string())
    })
}

pub async fn authenticate(app: &App, username: &str, password: &str) -> Option<User> {
    let user = repositories::users::find_by_username(&app.db, username.trim())
        .await
        .ok()
        .flatten();
    // Equal-cost verification whether or not the user exists.
    let verified = match &user {
        Some(u) => crate::auth::verify_password(&u.password_hash, password),
        None => crate::auth::verify_password(dummy_hash(), password),
    };
    if !verified {
        return None;
    }
    let user = user?;
    plugins::event_json(
        app,
        "user_login",
        &json!({ "id": user.id, "username": user.username, "role": user.role.as_str() }),
    );
    Some(user)
}

pub async fn change_password(app: &App, user_id: i64, new_password: &str) -> AppResult<()> {
    validate_password(new_password)?;
    let hash = crate::auth::hash_password(new_password)?;
    users::update_password(&app.db, user_id, &hash).await?;
    // Invalidate existing sessions after a password change.
    app.sessions.remove_user(user_id);
    Ok(())
}

pub async fn delete_user(app: &App, acting: &User, target_id: i64) -> AppResult<()> {
    if acting.role != Role::Admin {
        return Err(AppError::Forbidden("admin role required".into()));
    }
    if acting.id == target_id {
        return Err(AppError::BadRequest(
            "you cannot delete your own account".into(),
        ));
    }
    // Refuse to delete the last admin.
    let target = users::find_by_id(&app.db, target_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    if target.role == Role::Admin {
        let admins = users::list(&app.db)
            .await?
            .into_iter()
            .filter(|u| u.role == Role::Admin)
            .count();
        if admins <= 1 {
            return Err(AppError::BadRequest("cannot delete the last admin".into()));
        }
    }
    users::delete(&app.db, target_id).await?;
    app.sessions.remove_user(target_id);
    Ok(())
}

/// Guard: may `acting` edit content authored by `owner_id`?
pub fn can_edit_content(acting: &User, owner_id: i64) -> bool {
    acting.role.at_least(Role::Editor) || acting.id == owner_id
}

/// Ensure a slug-ish string is usable (used for username-ish checks).
pub fn normalized(name: &str) -> String {
    slug::slugify(name)
}
