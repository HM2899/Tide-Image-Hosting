use crate::app::AppConfig;
use crate::auth::{hash_password, normalize_username};
use crate::error::AppResult;
use crate::services::defaults;
use sqlx::PgPool;

pub async fn ensure_initial_state(pool: &PgPool, config: &AppConfig) -> AppResult<()> {
    defaults::ensure_default_settings(pool).await?;
    ensure_initial_admin(pool, config).await?;
    Ok(())
}

async fn ensure_initial_admin(pool: &PgPool, config: &AppConfig) -> AppResult<()> {
    let email = config.initial_admin_email.trim().to_lowercase();
    let username = normalize_username(&config.initial_admin_username)?;
    let existing: Option<(uuid::Uuid, String, String)> =
        sqlx::query_as("SELECT id,role,status FROM users WHERE email=$1")
            .bind(&email)
            .fetch_optional(pool)
            .await?;
    let admin_id = match existing {
        Some((id, role, status)) if admin_is_ready(&role, &status) => {
            sqlx::query("UPDATE users SET username=$2, locked_until=NULL, login_failed_count=0, updated_at=now() WHERE id=$1")
                .bind(id)
                .bind(&username)
                .execute(pool)
                .await?;
            id
        }
        Some((id, _, _)) => {
            let hash = hash_password(&config.initial_admin_password)?;
            sqlx::query("UPDATE users SET username=$2, password_hash=$3, role='super_admin', status='active', locked_until=NULL, login_failed_count=0, updated_at=now() WHERE id=$1")
                .bind(id)
                .bind(&username)
                .bind(hash)
                .execute(pool)
                .await?;
            id
        }
        None => {
            let hash = hash_password(&config.initial_admin_password)?;
            sqlx::query_scalar(
                "INSERT INTO users (email, username, password_hash, role, status) VALUES ($1,$2,$3,'super_admin','active') RETURNING id",
            )
            .bind(&email)
            .bind(&username)
            .bind(hash)
            .fetch_one(pool)
            .await?
        }
    };
    sqlx::query("INSERT INTO user_profiles (user_id, display_name) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET display_name=COALESCE(NULLIF(user_profiles.display_name,''), EXCLUDED.display_name), updated_at=now()")
        .bind(admin_id)
        .bind("系统管理员")
        .execute(pool)
        .await?;
    Ok(())
}

fn admin_is_ready(role: &str, status: &str) -> bool {
    role == "super_admin" && status == "active"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_ready_requires_super_admin_active() {
        assert!(admin_is_ready("super_admin", "active"));
        assert!(!admin_is_ready("admin", "active"));
        assert!(!admin_is_ready("super_admin", "banned"));
    }

    #[test]
    fn initial_admin_username_is_normalized() {
        assert_eq!(normalize_username(" admin ").unwrap(), "admin");
        assert!(normalize_username("admin@example.com").is_err());
    }
}
