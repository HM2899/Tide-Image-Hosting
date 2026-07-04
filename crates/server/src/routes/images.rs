use crate::app::AppState;
use crate::auth::{ApiAccess, CurrentUser};
use crate::error::{AppError, AppResult, empty_ok};
use crate::models::{ImageQuery, RandomQuery, TokenClaims};
use crate::services::images::{self, UploadActor};
use crate::services::quota;
use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post, put};
use jsonwebtoken::{DecodingKey, Validation, decode};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use tide_shared::{ImageSummary, Page, UploadResult};
use uuid::Uuid;

#[derive(Deserialize)]
struct TagQuery {
    q: Option<String>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/", get(list).post(upload_alias))
        .route("/upload", post(upload))
        .route("/{image_id}", get(detail).put(update).delete(trash))
        .route("/{image_id}/restore", post(restore))
        .route("/{image_id}/permanent", delete(permanent))
        .route("/{image_id}/links", get(links))
        .route("/{image_id}/tags", get(image_tags).post(add_image_tags))
        .route("/{image_id}/tags/{tag_id}", delete(delete_image_tag))
}

pub fn guest_router() -> axum::Router<AppState> {
    axum::Router::new().route("/upload", post(guest_upload))
}

pub fn public_image_router() -> axum::Router<AppState> {
    axum::Router::new().route("/images", get(public_images))
}

pub fn tag_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/", get(tags).post(create_tag))
        .route("/{tag_id}", put(update_tag).delete(delete_tag))
}

pub fn storage_router() -> axum::Router<AppState> {
    axum::Router::new().route("/proxy/{provider_id}/{*object_key}", get(storage_proxy))
}

async fn upload(
    State(state): State<AppState>,
    access: ApiAccess,
    multipart: Multipart,
) -> AppResult<Json<tide_shared::ApiResponse<UploadResult>>> {
    access.require("upload")?;
    let response = images::handle_upload(
        &state,
        UploadActor {
            user_id: access.user.id,
            role: access.user.role,
            is_guest: false,
            guest_ip: None,
            guest_user_agent: None,
            guest_fingerprint: None,
        },
        multipart,
    )
    .await?;
    Ok(Json(tide_shared::ok(response)))
}

async fn upload_alias(
    state: State<AppState>,
    access: ApiAccess,
    multipart: Multipart,
) -> AppResult<Json<tide_shared::ApiResponse<UploadResult>>> {
    upload(state, access, multipart).await
}

async fn guest_upload(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    multipart: Multipart,
) -> AppResult<Json<tide_shared::ApiResponse<UploadResult>>> {
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='site'")
            .fetch_one(&state.pool)
            .await?;
    if !settings
        .get("guest_upload_enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(AppError::Forbidden("guest upload is disabled".to_string()));
    }
    let guest_user = ensure_guest_user(&state).await?;
    let response = images::handle_upload(
        &state,
        UploadActor {
            user_id: guest_user,
            role: "guest_account".to_string(),
            is_guest: true,
            guest_ip: guest_ip(&headers, addr),
            guest_user_agent: headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string),
            guest_fingerprint: headers
                .get("x-client-fingerprint")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string),
        },
        multipart,
    )
    .await?;
    Ok(Json(tide_shared::ok(response)))
}

fn guest_ip(headers: &HeaderMap, addr: SocketAddr) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| Some(addr.ip().to_string()))
}

async fn ensure_guest_user(state: &AppState) -> AppResult<Uuid> {
    if let Some(id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE role='guest_account' AND email='guest@local.tide'",
    )
    .fetch_optional(&state.pool)
    .await?
    {
        return Ok(id);
    }
    let hash = crate::auth::hash_password(&crate::auth::random_token())?;
    let id = sqlx::query_scalar("INSERT INTO users (email,username,password_hash,role,status) VALUES ('guest@local.tide','guest', $1,'guest_account','active') RETURNING id")
        .bind(hash)
        .fetch_one(&state.pool)
        .await?;
    Ok(id)
}

async fn list(
    State(state): State<AppState>,
    access: ApiAccess,
    Query(query): Query<ImageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<ImageSummary>>>> {
    access.require("read")?;
    Ok(Json(tide_shared::ok(
        images::list_images(&state, Some(&access.user), &query, false).await?,
    )))
}

async fn public_images(
    State(state): State<AppState>,
    Query(query): Query<ImageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<ImageSummary>>>> {
    Ok(Json(tide_shared::ok(
        images::list_public_images(&state, &query).await?,
    )))
}

async fn detail(
    State(state): State<AppState>,
    access: ApiAccess,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<ImageSummary>>> {
    access.require("read")?;
    Ok(Json(tide_shared::ok(
        images::get_image(&state, Some(&access.user), image_id, false).await?,
    )))
}

#[derive(Deserialize)]
struct ImageUpdateRequest {
    title: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
}

#[derive(Deserialize)]
struct ImageTagsRequest {
    tags: Vec<String>,
}

async fn update(
    State(state): State<AppState>,
    access: ApiAccess,
    Path(image_id): Path<Uuid>,
    Json(req): Json<ImageUpdateRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    access.require("delete")?;
    let _ = images::get_image(&state, Some(&access.user), image_id, false).await?;
    sqlx::query("UPDATE images SET title=COALESCE($2,title), description=COALESCE($3,description), visibility=COALESCE($4,visibility), updated_at=now() WHERE id=$1")
        .bind(image_id)
        .bind(req.title)
        .bind(req.description)
        .bind(req.visibility)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

async fn trash(
    State(state): State<AppState>,
    access: ApiAccess,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    access.require("delete")?;
    images::trash_image(&state, &access.user, image_id, false).await?;
    Ok(empty_ok())
}

async fn restore(
    State(state): State<AppState>,
    access: ApiAccess,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    access.require("delete")?;
    images::restore_image(&state, &access.user, image_id, false).await?;
    Ok(empty_ok())
}

async fn permanent(
    State(state): State<AppState>,
    access: ApiAccess,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    access.require("delete")?;
    images::permanent_delete(&state, &access.user, image_id, false).await?;
    Ok(empty_ok())
}

async fn links(
    State(state): State<AppState>,
    access: ApiAccess,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<tide_shared::ImageLinks>>> {
    access.require("read")?;
    let image = images::get_image(&state, Some(&access.user), image_id, false).await?;
    let file_object_id: Uuid = sqlx::query_scalar("SELECT file_object_id FROM images WHERE id=$1")
        .bind(image.id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(tide_shared::ok(
        images::links_for(
            &state,
            file_object_id,
            &image.original_name,
            images::LinkContext::Authorized { image_id: image.id },
        )
        .await?,
    )))
}

pub async fn random(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(mut query): Query<RandomQuery>,
) -> AppResult<Response> {
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='random'")
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| json!({"enabled":true}));
    let random_settings = RandomSettings::from_value(&settings);
    if !random_settings.enabled {
        return Err(AppError::Forbidden("random image is disabled".to_string()));
    }
    random_settings.validate_query(&query)?;
    if let Some(user) = random_actor(&state, &headers, Some(addr)).await?
        && random_settings.limit_enabled
    {
        quota::ensure_random_allowed(&state.pool, user.id, &user.role).await?;
        quota::increment_random(&state.pool, user.id).await?;
    }
    let image = images::random_image(&state, &query).await?;
    if query.image.is_none() {
        query.image = Some(random_settings.default_image);
    }
    match query.r#type.as_deref().unwrap_or("redirect") {
        "json" => Ok((StatusCode::OK, Json(tide_shared::ok(image))).into_response()),
        "markdown" => Ok((StatusCode::OK, image.markdown).into_response()),
        "html" => Ok((StatusCode::OK, image.html).into_response()),
        _ => Ok(
            Redirect::temporary(if query.image.as_deref() == Some("original") {
                &image.url
            } else {
                &image.preview_url
            })
            .into_response(),
        ),
    }
}

struct RandomSettings {
    enabled: bool,
    limit_enabled: bool,
    allow_tag_filter: bool,
    allow_orientation_filter: bool,
    allow_resolution_filter: bool,
    default_image: String,
    no_match_strategy: String,
}

impl RandomSettings {
    fn from_value(value: &serde_json::Value) -> Self {
        Self {
            enabled: value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            limit_enabled: value
                .get("limit_enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            allow_tag_filter: value
                .get("allow_tag_filter")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            allow_orientation_filter: value
                .get("allow_orientation_filter")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            allow_resolution_filter: value
                .get("allow_resolution_filter")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            default_image: match value
                .get("default_image")
                .and_then(serde_json::Value::as_str)
            {
                Some("original") => "original".to_string(),
                _ => "preview".to_string(),
            },
            no_match_strategy: value
                .get("no_match_strategy")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("not_found")
                .to_string(),
        }
    }

    fn validate_query(&self, query: &RandomQuery) -> AppResult<()> {
        if !self.allow_tag_filter && (query.tag.is_some() || query.tags.is_some()) {
            return Err(AppError::Forbidden(
                "random tag filter is disabled".to_string(),
            ));
        }
        if !self.allow_orientation_filter && query.orientation.is_some() {
            return Err(AppError::Forbidden(
                "random orientation filter is disabled".to_string(),
            ));
        }
        if !self.allow_resolution_filter
            && (query.min_width.is_some()
                || query.min_height.is_some()
                || query.width.is_some()
                || query.height.is_some()
                || query.ratio.is_some())
        {
            return Err(AppError::Forbidden(
                "random resolution filter is disabled".to_string(),
            ));
        }
        if !matches!(self.no_match_strategy.as_str(), "not_found") {
            return Err(AppError::BadRequest(
                "unsupported random no-match strategy".to_string(),
            ));
        }
        Ok(())
    }
}

async fn random_actor(
    state: &AppState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
) -> AppResult<Option<CurrentUser>> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Ok(None);
    };
    let Ok(value) = value.to_str() else {
        return Ok(None);
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Ok(None);
    };
    let (user_id, api_token_scopes) =
        match crate::auth::verify_jwt(token, &state.config.session_secret) {
            Ok(claims) => (
                Uuid::parse_str(&claims.sub)
                    .map_err(|_| AppError::Unauthorized("invalid token subject".to_string()))?,
                None,
            ),
            Err(_) => {
                let connect_info = addr.map(ConnectInfo);
                let ip = crate::auth::request_ip(headers, connect_info.as_ref());
                let (user_id, scopes) =
                    crate::auth::api_token_access(&state.pool, token, ip.as_deref()).await?;
                if !crate::auth::scopes_include(&scopes, "random") {
                    return Err(AppError::Forbidden("api scope random required".to_string()));
                }
                (user_id, Some(scopes))
            }
        };
    let user = crate::auth::load_user(&state.pool, user_id).await?;
    if api_token_scopes.is_some() {
        quota::ensure_api_allowed(&state.pool, user.id, &user.role).await?;
        quota::increment_api(&state.pool, user.id).await?;
    }
    Ok(Some(CurrentUser {
        id: user.id,
        email: user.email,
        username: user.username,
        role: user.role,
        status: user.status,
    }))
}

async fn tags(
    State(state): State<AppState>,
    Query(query): Query<TagQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<tide_shared::TagView>>>> {
    let q = tag_search_pattern(query.q.as_deref());
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, i32)>(
        "SELECT id,name,slug,status,usage_count FROM tags WHERE ($1::text IS NULL OR lower(name) LIKE $1 OR lower(slug) LIKE $1) ORDER BY usage_count DESC,name ASC",
    )
    .bind(q)
    .fetch_all(&state.pool)
    .await?;
    let tags = rows
        .into_iter()
        .map(|row| tide_shared::TagView {
            id: row.0,
            name: row.1,
            slug: row.2,
            status: row.3,
            usage_count: row.4,
        })
        .collect();
    Ok(Json(tide_shared::ok(tags)))
}

fn tag_search_pattern(q: Option<&str>) -> Option<String> {
    q.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_lowercase()))
}

#[derive(Deserialize)]
struct TagRequest {
    name: Option<String>,
    merge_into_tag_id: Option<Uuid>,
    status: Option<String>,
}

async fn create_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<TagRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let name = normalized_tag_name(&req)?;
    let policy = images::load_tag_policy(&state).await?;
    images::validate_tag_name(&name, &policy)?;
    let quota_row = quota::load_quota(&state.pool, user.id, &user.role).await?;
    if !quota_row.allow_tag_create && !user.is_admin() {
        return Err(AppError::Forbidden(
            "tag creation is disabled for this user group".to_string(),
        ));
    }
    let slug = images::slugify(&name);
    let status = if policy.tag_review_required() && !user.is_admin() {
        "pending"
    } else {
        "normal"
    };
    let existing_tag: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,status FROM tags WHERE slug=$1")
            .bind(&slug)
            .fetch_optional(&state.pool)
            .await?;
    let id = if let Some((id, existing_status)) = existing_tag {
        if matches!(existing_status.as_str(), "normal" | "pending") {
            id
        } else {
            return Err(AppError::BadRequest("tag is unavailable".to_string()));
        }
    } else {
        sqlx::query_scalar(
            "INSERT INTO tags (name,slug,created_by,status) VALUES ($1,$2,$3,$4) RETURNING id",
        )
        .bind(&name)
        .bind(slug)
        .bind(user.id)
        .bind(status)
        .fetch_one(&state.pool)
        .await?
    };
    Ok(Json(tide_shared::ok(json!({"id":id}))))
}

async fn update_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(tag_id): Path<Uuid>,
    Json(req): Json<TagRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    if !user.is_admin() {
        return Err(AppError::Forbidden("admin required".to_string()));
    }
    if let Some(target_id) = req.merge_into_tag_id {
        merge_tag(&state, &user, tag_id, target_id).await?;
        return Ok(empty_ok());
    }
    if let Some(status) = req.status.as_deref() {
        update_tag_status(&state, &user, tag_id, status).await?;
        return Ok(empty_ok());
    }
    let name = normalized_tag_name(&req)?;
    let policy = images::load_tag_policy(&state).await?;
    images::validate_tag_name(&name, &policy)?;
    let slug = images::slugify(&name);
    let updated = sqlx::query("UPDATE tags SET name=$2, slug=$3, updated_at=now() WHERE id=$1")
        .bind(tag_id)
        .bind(&name)
        .bind(slug)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(AppError::NotFound("tag not found".to_string()));
    }
    log_tag_admin_operation(&state, &user, "tag.update", tag_id, json!({"name": name})).await?;
    Ok(empty_ok())
}

async fn delete_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(tag_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    if !user.is_admin() {
        return Err(AppError::Forbidden("admin required".to_string()));
    }
    let updated = sqlx::query("UPDATE tags SET status='disabled', updated_at=now() WHERE id=$1")
        .bind(tag_id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(AppError::NotFound("tag not found".to_string()));
    }
    log_tag_admin_operation(&state, &user, "tag.disable", tag_id, json!({})).await?;
    Ok(empty_ok())
}

async fn update_tag_status(
    state: &AppState,
    user: &CurrentUser,
    tag_id: Uuid,
    status: &str,
) -> AppResult<()> {
    if !matches!(status, "normal" | "disabled" | "blocked" | "pending") {
        return Err(AppError::BadRequest("invalid tag status".to_string()));
    }
    let updated = sqlx::query("UPDATE tags SET status=$2, updated_at=now() WHERE id=$1")
        .bind(tag_id)
        .bind(status)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(AppError::NotFound("tag not found".to_string()));
    }
    log_tag_admin_operation(
        state,
        user,
        "tag.status.update",
        tag_id,
        json!({"status": status}),
    )
    .await?;
    Ok(())
}

fn normalized_tag_name(req: &TagRequest) -> AppResult<String> {
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::BadRequest("tag name is required".to_string()))?;
    Ok(name.to_string())
}

async fn merge_tag(
    state: &AppState,
    user: &CurrentUser,
    source_id: Uuid,
    target_id: Uuid,
) -> AppResult<()> {
    validate_tag_merge(source_id, target_id)?;
    let mut tx = state.pool.begin().await?;
    let source_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM tags WHERE id=$1)")
            .bind(source_id)
            .fetch_one(&mut *tx)
            .await?;
    if !source_exists {
        return Err(AppError::NotFound("source tag not found".to_string()));
    }
    let target_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM tags WHERE id=$1 AND status='normal')",
    )
    .bind(target_id)
    .fetch_one(&mut *tx)
    .await?;
    if !target_exists {
        return Err(AppError::NotFound("target tag not found".to_string()));
    }
    sqlx::query("INSERT INTO image_tags (image_id,tag_id,created_by,created_at) SELECT image_id,$2,created_by,created_at FROM image_tags WHERE tag_id=$1 ON CONFLICT DO NOTHING")
        .bind(source_id)
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM image_tags WHERE tag_id=$1")
        .bind(source_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE tags SET status='disabled', usage_count=0, updated_at=now() WHERE id=$1")
        .bind(source_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE tags SET usage_count=(SELECT COUNT(*) FROM image_tags WHERE tag_id=$1), updated_at=now() WHERE id=$1")
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    log_tag_admin_operation(
        state,
        user,
        "tag.merge",
        source_id,
        json!({"merge_into_tag_id": target_id}),
    )
    .await?;
    Ok(())
}

fn validate_tag_merge(source_id: Uuid, target_id: Uuid) -> AppResult<()> {
    if source_id == target_id {
        return Err(AppError::BadRequest(
            "cannot merge a tag into itself".to_string(),
        ));
    }
    Ok(())
}

async fn log_tag_admin_operation(
    state: &AppState,
    user: &CurrentUser,
    action: &str,
    target_id: Uuid,
    detail_json: serde_json::Value,
) -> AppResult<()> {
    sqlx::query("INSERT INTO admin_operation_logs (admin_user_id,action,target_type,target_id,context_json,detail_json) VALUES ($1,$2,'tag',$3,$4,$4)")
        .bind(user.id)
        .bind(action)
        .bind(target_id)
        .bind(crate::services::security::redact_sensitive_json(detail_json))
        .execute(&state.pool)
        .await?;
    Ok(())
}

async fn image_tags(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<String>>>> {
    let _ = images::get_image(&state, Some(&user), image_id, false).await?;
    let tags = sqlx::query_scalar::<_, String>(
        "SELECT t.name FROM tags t JOIN image_tags it ON it.tag_id=t.id WHERE it.image_id=$1",
    )
    .bind(image_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(tags)))
}

async fn add_image_tags(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
    Json(req): Json<ImageTagsRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let _ = images::get_image(&state, Some(&user), image_id, false).await?;
    let quota_row = quota::load_quota(&state.pool, user.id, &user.role).await?;
    let policy = images::load_tag_policy(&state).await?;
    images::attach_tags(
        &state,
        image_id,
        user.id,
        &user.role,
        &req.tags,
        quota_row.allow_tag_create,
        &policy,
    )
    .await?;
    Ok(empty_ok())
}

async fn delete_image_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((image_id, tag_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let _ = images::get_image(&state, Some(&user), image_id, false).await?;
    sqlx::query("DELETE FROM image_tags WHERE image_id=$1 AND tag_id=$2")
        .bind(image_id)
        .bind(tag_id)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE tags SET usage_count=(SELECT COUNT(*) FROM image_tags WHERE tag_id=$1), updated_at=now() WHERE id=$1")
        .bind(tag_id)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

async fn storage_proxy(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<FileAccessQuery>,
    Path((provider_id, object_key)): Path<(Uuid, String)>,
) -> AppResult<Response> {
    let file = storage_proxy_object(&state, provider_id, &object_key).await?;
    serve_storage_file(&state, &headers, Some(addr), query.token.as_deref(), file).await
}

pub async fn local_file(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<FileAccessQuery>,
    Path(object_key): Path<String>,
) -> AppResult<Response> {
    let files = local_storage_objects(&state, &object_key).await?;
    serve_first_available_storage_file(&state, &headers, Some(addr), query.token.as_deref(), files)
        .await
}

#[derive(sqlx::FromRow)]
struct LocalFileRow {
    id: Uuid,
    file_object_id: Uuid,
    storage_provider_id: Uuid,
    object_key: String,
    object_type: String,
    mime_type: String,
}

#[derive(Deserialize)]
pub(crate) struct FileAccessQuery {
    token: Option<String>,
}

async fn local_storage_objects(state: &AppState, object_key: &str) -> AppResult<Vec<LocalFileRow>> {
    let object_key = object_key.trim_start_matches('/');
    let files_prefix = "files";
    let rows = sqlx::query_as::<_, LocalFileRow>(
        "SELECT so.id,so.file_object_id,so.storage_provider_id,so.object_key,so.object_type,fo.mime_type \
         FROM storage_objects so \
         JOIN storage_providers sp ON sp.id=so.storage_provider_id \
         JOIN file_objects fo ON fo.id=so.file_object_id \
         WHERE sp.provider_type='local' AND so.status='active' \
           AND btrim(COALESCE(NULLIF(sp.config_json->>'public_prefix',''), '/files'), '/')=$2 \
           AND (so.object_key=$1 OR (NULLIF(btrim(COALESCE(sp.config_json->>'path_prefix',''),'/'),'') IS NOT NULL \
             AND (btrim(COALESCE(sp.config_json->>'path_prefix',''),'/') || '/' || ltrim(so.object_key,'/'))=$1)) \
         ORDER BY so.updated_at DESC, so.created_at DESC",
    )
    .bind(object_key)
    .bind(files_prefix)
    .fetch_all(&state.pool)
    .await?;
    if rows.is_empty() {
        Err(AppError::NotFound("file not found".to_string()))
    } else {
        Ok(rows)
    }
}

async fn storage_proxy_object(
    state: &AppState,
    provider_id: Uuid,
    object_key: &str,
) -> AppResult<LocalFileRow> {
    sqlx::query_as::<_, LocalFileRow>(
        "SELECT so.id,so.file_object_id,so.storage_provider_id,so.object_key,so.object_type,fo.mime_type \
         FROM storage_objects so \
         JOIN file_objects fo ON fo.id=so.file_object_id \
         WHERE so.storage_provider_id=$1 AND so.object_key=$2 AND so.status='active' \
         ORDER BY so.updated_at DESC, so.created_at DESC LIMIT 1",
    )
    .bind(provider_id)
    .bind(object_key.trim_start_matches('/'))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("file not found".to_string()))
}

async fn serve_first_available_storage_file(
    state: &AppState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
    token: Option<&str>,
    files: Vec<LocalFileRow>,
) -> AppResult<Response> {
    let mut last_error = None;
    for file in files {
        match serve_storage_file(state, headers, addr, token, file).await {
            Ok(response) => return Ok(response),
            Err(AppError::NotFound(message)) => last_error = Some(message),
            Err(error) => return Err(error),
        }
    }
    Err(AppError::NotFound(
        last_error.unwrap_or_else(|| "file not found".to_string()),
    ))
}

async fn serve_storage_file(
    state: &AppState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
    token: Option<&str>,
    file: LocalFileRow,
) -> AppResult<Response> {
    if !local_file_allowed(
        state,
        headers,
        addr,
        file.file_object_id,
        &file.object_type,
        token,
    )
    .await?
    {
        return Err(AppError::Forbidden("file is not public".to_string()));
    }
    let row =
        crate::services::storage_registry::provider_by_id(state, file.storage_provider_id).await?;
    let provider = crate::services::storage_registry::build_provider(state, &row).await?;
    match provider.head_object(&file.object_key).await {
        Ok(true) => {}
        Ok(false) => {
            mark_storage_object_failed(state, file.id).await?;
            return Err(AppError::NotFound("file object is missing".to_string()));
        }
        Err(error) if storage_object_missing_error(&error) => {
            mark_storage_object_failed(state, file.id).await?;
            return Err(AppError::NotFound("file object is missing".to_string()));
        }
        Err(error) => return Err(error),
    }
    let bytes = match provider.get_object(&file.object_key).await {
        Ok(bytes) => bytes,
        Err(error) if storage_object_missing_error(&error) => {
            mark_storage_object_failed(state, file.id).await?;
            return Err(AppError::NotFound("file object is missing".to_string()));
        }
        Err(error) => return Err(error),
    };
    let content_type = content_type_for(&file);
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .map_err(|err| AppError::BadRequest(err.to_string()))?,
    );
    Ok(response)
}

async fn mark_storage_object_failed(state: &AppState, object_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE storage_objects SET status='failed', updated_at=now() WHERE id=$1")
        .bind(object_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

fn storage_object_missing_error(error: &AppError) -> bool {
    match error {
        AppError::Io(err) => err.kind() == std::io::ErrorKind::NotFound,
        AppError::External(message) => message.contains("404") || message.contains("NotFound"),
        _ => false,
    }
}

async fn local_file_allowed(
    state: &AppState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
    file_object_id: Uuid,
    object_type: &str,
    token: Option<&str>,
) -> AppResult<bool> {
    match object_type {
        "preview" => Ok(true),
        "original" => {
            if file_token_allows(state, token, file_object_id).await? {
                return Ok(true);
            }
            if has_public_image_reference(state, file_object_id).await? {
                return Ok(true);
            }
            let Some(user) = user_from_file_request(state, headers, addr).await? else {
                return Ok(false);
            };
            if user.is_admin() {
                return Ok(true);
            }
            user_owns_file_reference(state, file_object_id, user.id).await
        }
        "avatar" => Ok(true),
        "backup" => Ok(user_from_file_request(state, headers, addr)
            .await?
            .map(|user| user.is_admin())
            .unwrap_or(false)),
        _ => Ok(false),
    }
}

async fn file_token_allows(
    state: &AppState,
    token: Option<&str>,
    file_object_id: Uuid,
) -> AppResult<bool> {
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return Ok(false);
    };
    let claims = decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(state.config.session_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Forbidden("invalid file token".to_string()))?
    .claims;
    if claims.role != "file_read" {
        return Ok(false);
    }
    let Ok(image_id) = Uuid::parse_str(&claims.sub) else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM images WHERE id=$1 AND file_object_id=$2 AND status <> 'deleted')",
    )
    .bind(image_id)
    .bind(file_object_id)
    .fetch_one(&state.pool)
    .await?)
}

async fn has_public_image_reference(state: &AppState, file_object_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM images WHERE file_object_id=$1 AND status='active' AND visibility IN ('public','unlisted'))",
    )
    .bind(file_object_id)
    .fetch_one(&state.pool)
    .await?)
}

async fn user_owns_file_reference(
    state: &AppState,
    file_object_id: Uuid,
    user_id: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM images WHERE file_object_id=$1 AND user_id=$2 AND status <> 'deleted')",
    )
    .bind(file_object_id)
    .bind(user_id)
    .fetch_one(&state.pool)
    .await?)
}

async fn user_from_file_request(
    state: &AppState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
) -> AppResult<Option<CurrentUser>> {
    let user_id = if let Some(token) = bearer_token(headers) {
        if let Ok(claims) = crate::auth::verify_jwt(token, &state.config.session_secret) {
            Uuid::parse_str(&claims.sub)
                .map_err(|_| AppError::Unauthorized("invalid token subject".to_string()))?
        } else {
            let connect_info = addr.map(ConnectInfo);
            let ip = crate::auth::request_ip(headers, connect_info.as_ref());
            let (user_id, scopes) =
                crate::auth::api_token_access(&state.pool, token, ip.as_deref()).await?;
            if !crate::auth::scopes_include(&scopes, "read") {
                return Err(AppError::Forbidden("api scope read required".to_string()));
            }
            user_id
        }
    } else if let Some(session) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(extract_session_cookie)
    {
        crate::services::security::user_id_from_session(state, &session).await?
    } else {
        return Ok(None);
    };
    let user = crate::auth::load_user(&state.pool, user_id).await?;
    if user.status != "active" {
        return Err(AppError::Forbidden("user is not active".to_string()));
    }
    Ok(Some(CurrentUser {
        id: user.id,
        email: user.email,
        username: user.username,
        role: user.role,
        status: user.status,
    }))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn extract_session_cookie(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == "tide_session").then(|| value.to_string())
    })
}

fn content_type_for(file: &LocalFileRow) -> String {
    if file.object_type == "preview" || file.object_type == "avatar" {
        "image/webp".to_string()
    } else {
        file.mime_type.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_request_requires_non_empty_name() {
        let missing = TagRequest {
            name: None,
            merge_into_tag_id: None,
            status: None,
        };
        assert!(matches!(
            normalized_tag_name(&missing),
            Err(AppError::BadRequest(_))
        ));

        let blank = TagRequest {
            name: Some("  ".to_string()),
            merge_into_tag_id: None,
            status: None,
        };
        assert!(matches!(
            normalized_tag_name(&blank),
            Err(AppError::BadRequest(_))
        ));

        let named = TagRequest {
            name: Some(" 风景 ".to_string()),
            merge_into_tag_id: None,
            status: None,
        };
        assert_eq!(normalized_tag_name(&named).unwrap(), "风景");
    }

    #[test]
    fn tag_merge_rejects_self_target() {
        let tag_id = Uuid::from_u128(42);
        assert!(matches!(
            validate_tag_merge(tag_id, tag_id),
            Err(AppError::BadRequest(_))
        ));
        assert!(validate_tag_merge(tag_id, Uuid::from_u128(43)).is_ok());
    }

    #[test]
    fn tag_search_pattern_normalizes_input() {
        assert_eq!(
            tag_search_pattern(Some("  Smoke ")),
            Some("%smoke%".to_string())
        );
        assert_eq!(tag_search_pattern(Some("  ")), None);
        assert_eq!(tag_search_pattern(None), None);
    }

    #[test]
    fn random_settings_apply_filter_switches() {
        let settings = RandomSettings::from_value(&json!({
            "default_image": "original",
            "allow_tag_filter": false,
            "allow_orientation_filter": false,
            "allow_resolution_filter": false
        }));
        assert_eq!(settings.default_image, "original");

        let mut query = RandomQuery {
            tag: Some("壁纸".to_string()),
            tags: None,
            mode: None,
            orientation: None,
            min_width: None,
            min_height: None,
            width: None,
            height: None,
            ratio: None,
            r#match: None,
            tolerance: None,
            r#type: None,
            image: None,
        };
        assert!(matches!(
            settings.validate_query(&query),
            Err(AppError::Forbidden(_))
        ));

        query.tag = None;
        query.orientation = Some("landscape".to_string());
        assert!(matches!(
            settings.validate_query(&query),
            Err(AppError::Forbidden(_))
        ));

        query.orientation = None;
        query.width = Some(1920);
        assert!(matches!(
            settings.validate_query(&query),
            Err(AppError::Forbidden(_))
        ));
    }

    #[test]
    fn random_settings_reject_unsupported_no_match_strategy() {
        let settings = RandomSettings::from_value(&json!({"no_match_strategy":"default_image"}));
        let query = RandomQuery {
            tag: None,
            tags: None,
            mode: None,
            orientation: None,
            min_width: None,
            min_height: None,
            width: None,
            height: None,
            ratio: None,
            r#match: None,
            tolerance: None,
            r#type: None,
            image: None,
        };
        assert!(matches!(
            settings.validate_query(&query),
            Err(AppError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn preview_files_are_publicly_readable() {
        let config = crate::app::AppConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "http://localhost:8080".to_string(),
            session_secret: "secret".to_string(),
            encryption_key: "encryption".to_string(),
            local_storage_root: "/tmp".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://127.0.0.1:8080".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "password".to_string(),
            ..Default::default()
        };
        let pool = sqlx::PgPool::connect_lazy("postgres://example").expect("lazy pool");
        let state = crate::app::AppState::new(pool, config);

        assert!(
            local_file_allowed(
                &state,
                &HeaderMap::new(),
                None,
                Uuid::nil(),
                "preview",
                None,
            )
            .await
            .unwrap()
        );
    }
}
