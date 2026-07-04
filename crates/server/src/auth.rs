use crate::app::AppState;
use crate::error::{AppError, AppResult};
use crate::models::{TokenClaims, UserRow};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::ConnectInfo;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::{HeaderMap, request::Parts};
use axum_extra::TypedHeader;
use axum_extra::headers::{Authorization, authorization::Bearer};
use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::rngs::OsRng;
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

#[derive(Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
    pub status: String,
}

#[derive(Clone)]
pub struct ApiAccess {
    pub user: CurrentUser,
    pub scopes: Vec<String>,
}

impl CurrentUser {
    pub fn is_admin(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "super_admin")
    }
}

impl ApiAccess {
    pub fn require(&self, scope: &str) -> AppResult<()> {
        if scopes_include(&self.scopes, scope) {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!("api scope {scope} required")))
        }
    }
}

pub fn scopes_include(scopes: &[String], scope: &str) -> bool {
    scopes.iter().any(|item| item == scope || item == "admin")
}

impl FromRequestParts<AppState> for ApiAccess {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Ok(TypedHeader(auth)) =
            TypedHeader::<Authorization<Bearer>>::from_request_parts(parts, state).await
        {
            let token = auth.token();
            if verify_jwt(token, &state.config.session_secret).is_err() {
                let ip = request_ip(
                    &parts.headers,
                    parts.extensions.get::<ConnectInfo<SocketAddr>>(),
                );
                let (user_id, scopes) = api_token_access(&state.pool, token, ip.as_deref()).await?;
                let user = load_user(&state.pool, user_id).await?;
                if user.status != "active" {
                    return Err(AppError::Forbidden("user is not active".to_string()));
                }
                crate::services::quota::ensure_api_allowed(&state.pool, user.id, &user.role)
                    .await?;
                crate::services::quota::increment_api(&state.pool, user.id).await?;
                return Ok(Self {
                    user: CurrentUser {
                        id: user.id,
                        email: user.email,
                        username: user.username,
                        role: user.role,
                        status: user.status,
                    },
                    scopes,
                });
            }
        }
        let user = CurrentUser::from_request_parts(parts, state).await?;
        Ok(Self {
            user,
            scopes: vec![
                "upload".to_string(),
                "read".to_string(),
                "delete".to_string(),
                "random".to_string(),
                "admin".to_string(),
            ],
        })
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user_id = if let Ok(TypedHeader(auth)) =
            TypedHeader::<Authorization<Bearer>>::from_request_parts(parts, state).await
        {
            let token = auth.token();
            if let Ok(claims) = verify_jwt(token, &state.config.session_secret) {
                Uuid::parse_str(&claims.sub)
                    .map_err(|_| AppError::Unauthorized("invalid token subject".to_string()))?
            } else {
                let ip = request_ip(
                    &parts.headers,
                    parts.extensions.get::<ConnectInfo<SocketAddr>>(),
                );
                let user_id = user_id_from_api_token(&state.pool, token, ip.as_deref()).await?;
                let user = load_user(&state.pool, user_id).await?;
                crate::services::quota::ensure_api_allowed(&state.pool, user.id, &user.role)
                    .await?;
                crate::services::quota::increment_api(&state.pool, user.id).await?;
                user.id
            }
        } else {
            let cookie = parts
                .headers
                .get("cookie")
                .and_then(|value| value.to_str().ok())
                .and_then(extract_session_cookie)
                .ok_or_else(|| AppError::Unauthorized("missing auth token".to_string()))?;
            crate::services::security::user_id_from_session(state, &cookie).await?
        };
        let user = load_user(&state.pool, user_id).await?;
        if user.status != "active" {
            return Err(AppError::Forbidden("user is not active".to_string()));
        }
        Ok(Self {
            id: user.id,
            email: user.email,
            username: user.username,
            role: user.role,
            status: user.status,
        })
    }
}

pub async fn user_id_from_api_token(
    pool: &PgPool,
    token: &str,
    request_ip: Option<&str>,
) -> AppResult<Uuid> {
    Ok(api_token_access(pool, token, request_ip).await?.0)
}

pub async fn api_token_access(
    pool: &PgPool,
    token: &str,
    request_ip: Option<&str>,
) -> AppResult<(Uuid, Vec<String>)> {
    let hash = hash_token(token);
    let row: (Uuid, serde_json::Value, serde_json::Value) = sqlx::query_as(
        "SELECT user_id,scopes_json,ip_whitelist_json FROM api_tokens WHERE token_hash=$1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("invalid api token".to_string()))?;
    ensure_ip_allowed(&row.2, request_ip)?;
    sqlx::query("UPDATE api_tokens SET last_used_at=now() WHERE token_hash=$1")
        .bind(hash_token(token))
        .execute(pool)
        .await?;
    let scopes = row
        .1
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok((row.0, scopes))
}

pub fn request_ip(
    headers: &HeaderMap,
    connect_info: Option<&ConnectInfo<SocketAddr>>,
) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| connect_info.map(|info| info.0.ip().to_string()))
}

fn ensure_ip_allowed(whitelist: &serde_json::Value, request_ip: Option<&str>) -> AppResult<()> {
    let Some(items) = whitelist.as_array() else {
        return Ok(());
    };
    let allowed = items
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if allowed.is_empty() {
        return Ok(());
    }
    let Some(request_ip) = request_ip.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(AppError::Forbidden(
            "api token ip is not allowed".to_string(),
        ));
    };
    if allowed
        .iter()
        .any(|item| *item == "*" || item.eq_ignore_ascii_case(request_ip))
    {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "api token ip is not allowed".to_string(),
        ))
    }
}

fn extract_session_cookie(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == "tide_session").then(|| value.to_string())
    })
}

impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

pub async fn load_user(pool: &PgPool, id: Uuid) -> AppResult<UserRow> {
    sqlx::query_as::<_, UserRow>(
        "SELECT id,email,username,password_hash,role,status,login_failed_count,locked_until FROM users WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("user not found".to_string()))
}

pub async fn load_user_by_identifier(pool: &PgPool, identifier: &str) -> AppResult<UserRow> {
    let identifier = identifier.trim().to_lowercase();
    if identifier.is_empty() {
        return Err(AppError::Unauthorized(
            "invalid username/email or password".to_string(),
        ));
    }
    let query = if identifier.contains('@') {
        "SELECT id,email,username,password_hash,role,status,login_failed_count,locked_until FROM users WHERE lower(email)=$1"
    } else {
        "SELECT id,email,username,password_hash,role,status,login_failed_count,locked_until FROM users WHERE lower(username)=$1"
    };
    sqlx::query_as::<_, UserRow>(query)
        .bind(identifier)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid username/email or password".to_string()))
}

pub fn normalize_username(username: &str) -> AppResult<String> {
    let value = username.trim();
    if value.is_empty() {
        return Err(AppError::BadRequest("username is required".to_string()));
    }
    if value.contains('@') {
        return Err(AppError::BadRequest(
            "username cannot contain @".to_string(),
        ));
    }
    if value.chars().count() > 64 {
        return Err(AppError::BadRequest("username is too long".to_string()));
    }
    Ok(value.to_string())
}

pub fn map_user_unique_error(err: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(database) = &err
        && database.is_unique_violation()
    {
        let message = match database.constraint() {
            Some("users_email_key") => "email already exists",
            Some("idx_users_username_lower_unique") => "username already exists",
            _ => "user already exists",
        };
        return AppError::Conflict(message.to_string());
    }
    AppError::from(err)
}

pub fn hash_password(password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|err| AppError::BadRequest(err.to_string()))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, hash: &str) -> AppResult<bool> {
    let parsed = PasswordHash::new(hash).map_err(|err| AppError::BadRequest(err.to_string()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub fn create_jwt(user: &UserRow, secret: &str) -> AppResult<String> {
    let exp = Utc::now()
        .checked_add_signed(Duration::days(30))
        .ok_or_else(|| AppError::BadRequest("invalid expiration".to_string()))?
        .timestamp() as usize;
    let claims = TokenClaims {
        sub: user.id.to_string(),
        role: user.role.clone(),
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|err| AppError::Unauthorized(err.to_string()))
}

pub fn verify_jwt(token: &str, secret: &str) -> AppResult<TokenClaims> {
    decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map(|data| data.claims)
    .map_err(|_| AppError::Unauthorized("invalid bearer token".to_string()))
}

pub fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_verifies_and_rejects_wrong_password() {
        let hash = hash_password("ChangeMe123!").expect("hash password");

        assert!(verify_password("ChangeMe123!", &hash).expect("verify password"));
        assert!(!verify_password("wrong-password", &hash).expect("verify wrong password"));
        assert_ne!(hash, "ChangeMe123!");
    }

    #[test]
    fn token_hash_is_stable_sha256_hex() {
        let first = hash_token("token-value");
        let second = hash_token("token-value");

        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert_ne!(first, "token-value");
    }

    #[test]
    fn username_normalization_rejects_ambiguous_values() {
        assert_eq!(normalize_username(" admin ").unwrap(), "admin");
        assert!(normalize_username("").is_err());
        assert!(normalize_username("admin@example.com").is_err());
        assert!(normalize_username(&"a".repeat(65)).is_err());
    }

    #[test]
    fn api_token_ip_whitelist_allows_empty_star_and_exact_match() {
        assert!(ensure_ip_allowed(&serde_json::json!([]), None).is_ok());
        assert!(ensure_ip_allowed(&serde_json::json!(["*"]), Some("203.0.113.7")).is_ok());
        assert!(
            ensure_ip_allowed(&serde_json::json!(["203.0.113.7"]), Some("203.0.113.7")).is_ok()
        );
        assert!(matches!(
            ensure_ip_allowed(&serde_json::json!(["203.0.113.7"]), Some("203.0.113.8")),
            Err(AppError::Forbidden(_))
        ));
        assert!(matches!(
            ensure_ip_allowed(&serde_json::json!(["203.0.113.7"]), None),
            Err(AppError::Forbidden(_))
        ));
    }
}
