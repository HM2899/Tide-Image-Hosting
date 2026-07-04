use crate::app::AppState;
use crate::auth::{
    CurrentUser, hash_password, hash_token, map_user_unique_error, normalize_username, random_token,
};
use crate::error::{AppError, AppResult, empty_ok};
use crate::services::{quota, storage_registry};
use crate::storage::avatar_key;
use axum::Json;
use axum::body::to_bytes;
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::header;
use axum::routing::{delete, get, post, put};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/profile", get(profile).put(update_profile))
        .route("/password", put(update_password))
        .route("/avatar", post(avatar))
        .route("/quota", get(user_quota))
        .route("/api-tokens", get(api_tokens).post(create_token))
        .route("/api-tokens/{token_id}", delete(delete_token))
}

#[derive(Deserialize)]
struct ProfileRequest {
    username: Option<String>,
    display_name: Option<String>,
    bio: Option<String>,
}

#[derive(Deserialize)]
struct PasswordRequest {
    old_password: Option<String>,
    new_password: String,
}

#[derive(Deserialize)]
struct CreateTokenRequest {
    name: Option<String>,
    scopes: Option<Vec<String>>,
    ip_whitelist: Option<Vec<String>>,
    ip_whitelist_json: Option<Vec<String>>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn profile(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let profile: Option<(Option<String>, Option<String>, serde_json::Value)> =
        sqlx::query_as("SELECT display_name,bio,settings_json FROM user_profiles WHERE user_id=$1")
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?;
    let avatar_url: Option<String> = sqlx::query_scalar("SELECT avatar_url FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    let avatar_url = avatar_url.map(|url| {
        crate::services::images::normalize_public_url_for_base(&state.config.public_base_url, url)
    });
    Ok(Json(tide_shared::ok(
        json!({"id":user.id,"email":user.email,"username":user.username,"role":user.role,"status":user.status,"avatar_url":avatar_url,"profile":profile.map(|p| json!({"display_name":p.0,"bio":p.1,"settings":p.2}))}),
    )))
}

async fn update_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<ProfileRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    if let Some(username) = req.username {
        let username = normalize_username(&username)?;
        sqlx::query("UPDATE users SET username=$2, updated_at=now() WHERE id=$1")
            .bind(user.id)
            .bind(username)
            .execute(&state.pool)
            .await
            .map_err(map_user_unique_error)?;
    }
    sqlx::query("INSERT INTO user_profiles (user_id,display_name,bio) VALUES ($1,$2,$3) ON CONFLICT (user_id) DO UPDATE SET display_name=COALESCE($2,user_profiles.display_name), bio=COALESCE($3,user_profiles.bio), updated_at=now()")
        .bind(user.id)
        .bind(req.display_name)
        .bind(req.bio)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

async fn update_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<PasswordRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let existing: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    if let Some(old_password) = req.old_password
        && !crate::auth::verify_password(&old_password, &existing)?
    {
        return Err(AppError::Forbidden("old password is invalid".to_string()));
    }
    let hash = hash_password(&req.new_password)?;
    sqlx::query("UPDATE users SET password_hash=$2, updated_at=now() WHERE id=$1")
        .bind(user.id)
        .bind(hash)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

async fn avatar(
    State(state): State<AppState>,
    user: CurrentUser,
    request: Request,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("multipart/form-data"))
        .unwrap_or(false)
    {
        let avatar_url = upload_avatar_file(&state, &user, request).await?;
        return Ok(Json(tide_shared::ok(json!({"avatar_url":avatar_url}))));
    }
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    let body: serde_json::Value =
        serde_json::from_slice(&body).map_err(|err| AppError::BadRequest(err.to_string()))?;
    let avatar_url = body
        .get("avatar_url")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("avatar_url is required".to_string()))?;
    sqlx::query("UPDATE users SET avatar_url=$2, updated_at=now() WHERE id=$1")
        .bind(user.id)
        .bind(avatar_url)
        .execute(&state.pool)
        .await?;
    Ok(Json(tide_shared::ok(json!({"avatar_url":avatar_url}))))
}

async fn upload_avatar_file(
    state: &AppState,
    user: &CurrentUser,
    request: Request,
) -> AppResult<String> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    let mut bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?
    {
        if matches!(field.name(), Some("file" | "avatar" | "image")) {
            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|err| AppError::BadRequest(err.to_string()))?
                    .to_vec(),
            );
            break;
        }
    }
    let bytes = bytes.ok_or_else(|| AppError::BadRequest("avatar file is required".to_string()))?;
    if bytes.is_empty() {
        return Err(AppError::BadRequest("avatar file is required".to_string()));
    }
    let image =
        image::load_from_memory(&bytes).map_err(|err| AppError::BadRequest(err.to_string()))?;
    let resized = image.thumbnail(512, 512).to_rgba8();
    let webp = encode_avatar_webp(&state.pool, &resized).await?;
    let sha256 = hex::encode(Sha256::digest(&webp));
    let key = avatar_key(&user.id, &sha256);
    let provider_row = storage_registry::default_provider(state).await?;
    let provider = storage_registry::build_provider(state, &provider_row).await?;
    provider.health_check().await?;
    let stored = provider.put_object(&key, &webp, "image/webp").await?;
    let mut tx = state.pool.begin().await?;
    let file_object_id: Uuid = sqlx::query_scalar(
        "INSERT INTO file_objects (sha256,size,mime_type,width,height,orientation,aspect_ratio,ref_count) VALUES ($1,$2,'image/webp',$3,$4,'square','1:1',1) ON CONFLICT (sha256) DO UPDATE SET ref_count=file_objects.ref_count+1, updated_at=now() RETURNING id",
    )
    .bind(&sha256)
    .bind(webp.len() as i64)
    .bind(resized.width() as i32)
    .bind(resized.height() as i32)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO storage_objects (file_object_id,storage_provider_id,object_type,object_key,public_url,etag,size,status) VALUES ($1,$2,'avatar',$3,$4,$5,$6,'active') ON CONFLICT (storage_provider_id,object_key) DO UPDATE SET public_url=EXCLUDED.public_url, etag=EXCLUDED.etag, size=EXCLUDED.size, status='active', updated_at=now()")
        .bind(file_object_id)
        .bind(provider_row.id)
        .bind(stored.object_key)
        .bind(&stored.public_url)
        .bind(stored.etag)
        .bind(stored.size)
        .execute(&mut *tx)
        .await?;
    let avatar_url = stored
        .public_url
        .ok_or_else(|| AppError::External("avatar public url missing".to_string()))?;
    sqlx::query("UPDATE users SET avatar_url=$2, updated_at=now() WHERE id=$1")
        .bind(user.id)
        .bind(&avatar_url)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO user_profiles (user_id,avatar_file_object_id) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET avatar_file_object_id=$2, updated_at=now()")
        .bind(user.id)
        .bind(file_object_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(avatar_url)
}

async fn encode_avatar_webp(pool: &sqlx::PgPool, image: &image::RgbaImage) -> AppResult<Vec<u8>> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='upload'")
            .fetch_optional(pool)
            .await?
            .unwrap_or_else(|| json!({}));
    let quality = value
        .get("webp_quality")
        .and_then(serde_json::Value::as_f64)
        .map(|value| value as f32)
        .map(normalize_webp_quality)
        .unwrap_or(75.0);
    let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
    Ok(encoder.encode(quality).to_vec())
}

fn normalize_webp_quality(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(1.0, 100.0)
    } else {
        75.0
    }
}

async fn user_quota(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<tide_shared::QuotaView>>> {
    let row = quota::load_quota(&state.pool, user.id, &user.role).await?;
    Ok(Json(tide_shared::ok(
        quota::view(&state.pool, user.id, &row).await?,
    )))
}

async fn api_tokens(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    let rows = sqlx::query_as::<_, (Uuid, String, serde_json::Value, serde_json::Value, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id,name,scopes_json,ip_whitelist_json,expires_at,last_used_at FROM api_tokens WHERE user_id=$1 AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let data = rows.into_iter().map(|row| json!({"id":row.0,"name":row.1,"scopes":row.2,"ip_whitelist":row.3,"expires_at":row.4,"last_used_at":row.5})).collect();
    Ok(Json(tide_shared::ok(data)))
}

async fn create_token(
    State(state): State<AppState>,
    user: CurrentUser,
    body: Option<Json<CreateTokenRequest>>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    quota::ensure_api_allowed(&state.pool, user.id, &user.role).await?;
    let token = random_token();
    let token_hash = hash_token(&token);
    let body = body.map(|Json(value)| value);
    let name = body
        .as_ref()
        .and_then(|value| value.name.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("默认 Token");
    let scopes = normalize_scopes(body.as_ref().and_then(|value| value.scopes.clone()))?;
    let expires_at = body.as_ref().and_then(|value| value.expires_at);
    let ip_whitelist = normalize_ip_whitelist(body.as_ref().and_then(|value| {
        value
            .ip_whitelist
            .clone()
            .or_else(|| value.ip_whitelist_json.clone())
    }))?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO api_tokens (user_id,name,token_hash,scopes_json,ip_whitelist_json,expires_at) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(user.id)
    .bind(name)
    .bind(token_hash)
    .bind(json!(scopes))
    .bind(json!(ip_whitelist))
    .bind(expires_at)
    .fetch_one(&state.pool)
    .await?;
    quota::increment_api(&state.pool, user.id).await?;
    Ok(Json(tide_shared::ok(
        json!({"id":id,"token":token,"name":name,"scopes":scopes,"ip_whitelist":ip_whitelist,"expires_at":expires_at}),
    )))
}

async fn delete_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    sqlx::query("UPDATE api_tokens SET revoked_at=now() WHERE id=$1 AND user_id=$2")
        .bind(token_id)
        .bind(user.id)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

fn normalize_scopes(scopes: Option<Vec<String>>) -> AppResult<Vec<String>> {
    let raw = scopes.unwrap_or_else(|| {
        vec![
            "upload".to_string(),
            "read".to_string(),
            "delete".to_string(),
            "random".to_string(),
        ]
    });
    let mut result = Vec::new();
    for scope in raw {
        let scope = scope.trim();
        if !matches!(scope, "upload" | "read" | "delete" | "random" | "ai") {
            return Err(AppError::BadRequest("invalid api token scope".to_string()));
        }
        if !result.iter().any(|item| item == scope) {
            result.push(scope.to_string());
        }
    }
    if result.is_empty() {
        return Err(AppError::BadRequest(
            "api token scope is required".to_string(),
        ));
    }
    Ok(result)
}

fn normalize_ip_whitelist(values: Option<Vec<String>>) -> AppResult<Vec<String>> {
    let mut result = Vec::new();
    for value in values.unwrap_or_default() {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if value != "*" && value.parse::<std::net::IpAddr>().is_err() {
            return Err(AppError::BadRequest(
                "invalid api token ip whitelist".to_string(),
            ));
        }
        if !result.iter().any(|item| item == value) {
            result.push(value.to_string());
        }
    }
    Ok(result)
}
