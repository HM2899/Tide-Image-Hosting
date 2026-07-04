use crate::app::AppState;
use crate::auth::{CurrentUser, map_user_unique_error, normalize_username};
use crate::error::{AppError, AppResult, empty_ok};
use crate::models::{ImageQuery, StorageRouteRow};
use crate::services::{audit, images, security, storage_registry, tasks};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tide_shared::{ImageSummary, Page, StorageProviderView};
use tokio::time::{Duration as TokioDuration, timeout};
use uuid::Uuid;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/dashboard", get(dashboard))
        .route("/users", get(users).post(create_user))
        .route("/users/page", get(users_page))
        .route(
            "/users/{user_id}",
            get(user_detail).put(update_user).delete(delete_user),
        )
        .route("/users/{user_id}/ban", post(ban_user))
        .route("/users/{user_id}/unban", post(unban_user))
        .route("/users/{user_id}/group", put(update_group))
        .route("/users/{user_id}/quota", put(update_quota))
        .route("/images", get(admin_images))
        .route(
            "/images/{image_id}",
            get(admin_image_detail).delete(admin_trash_image),
        )
        .route("/images/{image_id}/restore", post(admin_restore_image))
        .route(
            "/images/{image_id}/permanent",
            delete(admin_permanent_image),
        )
        .route("/images/{image_id}/approve", post(approve_image))
        .route("/images/{image_id}/reject", post(reject_image))
        .route("/images/{image_id}/block", post(block_image))
        .route("/user-groups", get(user_groups).post(create_group))
        .route(
            "/user-groups/{group_id}",
            put(update_group_detail).delete(delete_group),
        )
        .route("/quota-rules", get(quota_rules))
        .route("/quota-rules/{group_id}", put(update_quota_rule))
        .route("/quota-usage", get(quota_usage))
        .route("/audit/tasks", get(audit_tasks))
        .route("/audit/tasks/page", get(audit_tasks_page))
        .route("/audit/tasks/{task_id}", get(audit_task_detail))
        .route("/audit/tasks/{task_id}/approve", post(approve_task))
        .route("/audit/tasks/{task_id}/reject", post(reject_task))
        .route("/audit/tasks/{task_id}/retry", post(retry_task))
        .route(
            "/audit/settings",
            get(audit_settings).put(update_audit_settings),
        )
        .route("/audit/logs", get(audit_logs))
        .route("/audit/logs/page", get(audit_logs_page))
        .route(
            "/storage/providers",
            get(storage_providers).post(create_storage_provider),
        )
        .route("/storage/providers/page", get(storage_providers_page))
        .route(
            "/storage/providers/bulk-delete",
            post(bulk_delete_storage_providers),
        )
        .route(
            "/storage/providers/{provider_id}",
            get(storage_provider_detail)
                .put(update_storage_provider)
                .delete(delete_storage_provider),
        )
        .route(
            "/storage/providers/{provider_id}/test-connection",
            post(test_connection),
        )
        .route(
            "/storage/providers/{provider_id}/test-upload",
            post(test_upload),
        )
        .route(
            "/storage/providers/{provider_id}/test-delete",
            post(test_delete),
        )
        .route(
            "/storage/providers/{provider_id}/disable",
            post(disable_storage_provider),
        )
        .route(
            "/storage/providers/{provider_id}/set-default",
            post(set_default_storage),
        )
        .route(
            "/storage/routes",
            get(storage_routes).post(create_storage_route),
        )
        .route("/storage/routes/page", get(storage_routes_page))
        .route("/storage/routes/summary", get(storage_routes_summary))
        .route(
            "/storage/routes/{route_id}",
            get(storage_route_detail)
                .put(update_storage_route)
                .delete(delete_storage_route),
        )
        .route(
            "/storage/routes/{route_id}/enable",
            post(enable_storage_route),
        )
        .route(
            "/storage/routes/{route_id}/disable",
            post(disable_storage_route),
        )
        .route("/storage/health", get(storage_health))
        .route("/storage/health/page", get(storage_health_page))
        .route("/migrations", get(migrations).post(create_migration))
        .route("/migrations/page", get(migrations_page))
        .route("/migrations/{task_id}", get(migration_detail))
        .route("/migrations/{task_id}/pause", post(task_pause))
        .route("/migrations/{task_id}/resume", post(task_resume))
        .route("/migrations/{task_id}/cancel", post(task_cancel))
        .route("/migrations/{task_id}/retry-failed", post(task_retry))
        .route("/migrations/{task_id}/items", get(migration_items))
        .route(
            "/migrations/{task_id}/items/page",
            get(migration_items_page),
        )
        .route("/backups", get(backups).post(create_backup))
        .route("/backups/page", get(backups_page))
        .route("/backups/{backup_id}", get(backup_detail))
        .route("/backups/{backup_id}/download", get(backup_download))
        .route("/restores", post(create_restore))
        .route("/restores/{restore_id}", get(restore_detail))
        .route(
            "/backup-settings",
            get(backup_settings).put(update_backup_settings),
        )
        .route("/settings/site", put(update_site_settings))
        .route("/settings/theme", put(update_theme_settings))
        .route("/settings/upload", put(update_upload_settings))
        .route("/settings/random", put(update_random_settings))
        .route(
            "/settings/smtp",
            get(smtp_settings).put(update_smtp_settings),
        )
        .route("/logs/system", get(system_logs))
        .route("/logs/system/page", get(system_logs_page))
        .route("/logs/operations", get(operation_logs))
        .route("/logs/operations/page", get(operation_logs_page))
}

fn ensure_admin(user: &CurrentUser) -> AppResult<()> {
    if user.is_admin() {
        Ok(())
    } else {
        Err(AppError::Forbidden("admin required".to_string()))
    }
}

#[derive(Clone, Debug, Deserialize)]
struct AdminPageQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    status: Option<String>,
    role: Option<String>,
    level: Option<String>,
}

impl AdminPageQuery {
    fn page_values(&self, default_size: i64) -> (i64, i64, i64) {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(default_size).clamp(1, 100);
        let offset = (page - 1) * page_size;
        (page, page_size, offset)
    }

    fn search_pattern(&self) -> Option<String> {
        self.q
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("%{}%", value.to_lowercase()))
    }

    fn status_value(&self) -> String {
        self.status
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_string()
    }

    fn role_value(&self) -> String {
        self.role
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_string()
    }

    fn level_value(&self) -> String {
        self.level
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_string()
    }
}

async fn dashboard(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let total_users: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE status <> 'deleted'")
            .fetch_one(&state.pool)
            .await?;
    let today_users: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE status <> 'deleted' AND created_at >= date_trunc('day', now())",
    )
    .fetch_one(&state.pool)
    .await?;
    let total_images: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM images WHERE status <> 'deleted'")
            .fetch_one(&state.pool)
            .await?;
    let today_images: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM images WHERE status <> 'deleted' AND created_at >= date_trunc('day', now())",
    )
    .fetch_one(&state.pool)
    .await?;
    let storage_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size),0)::bigint FROM file_objects WHERE ref_count > 0",
    )
    .fetch_one(&state.pool)
    .await?;
    let pending_audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM images WHERE status='pending_review'")
            .fetch_one(&state.pool)
            .await?;
    let ai_stats = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, COUNT(*) FROM audit_tasks WHERE audit_type IN ('ai','llm','third_party') GROUP BY status ORDER BY status",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| json!({"status": row.0, "count": row.1}))
    .collect::<Vec<_>>();
    let recent_images = images::list_images(
        &state,
        Some(&user),
        &ImageQuery {
            tag: None,
            status: None,
            orientation: None,
            min_width: None,
            min_height: None,
            storage_provider_id: None,
            user_id: None,
            is_guest_upload: None,
            page: Some(1),
            page_size: Some(8),
        },
        true,
    )
    .await?
    .items;
    let recent_bans = sqlx::query_as::<
        _,
        (Uuid, String, String, String, String, chrono::DateTime<chrono::Utc>),
    >(
        "SELECT id,email,username,role,status,updated_at FROM users WHERE status='banned' ORDER BY updated_at DESC LIMIT 8",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| {
        json!({
            "id": row.0,
            "email": row.1,
            "username": row.2,
            "role": row.3,
            "status": row.4,
            "updated_at": row.5,
        })
    })
    .collect::<Vec<_>>();
    let storage = sqlx::query_as::<_, (Uuid, String, String, bool, bool)>(
        "SELECT id,name,provider_type,is_default,enabled FROM storage_providers WHERE deleted_at IS NULL ORDER BY priority ASC, created_at DESC LIMIT 12",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| {
        json!({
            "id": row.0,
            "name": row.1,
            "provider_type": row.2,
            "is_default": row.3,
            "enabled": row.4,
            "healthy": row.4,
            "error": if row.4 { serde_json::Value::Null } else { json!("disabled") },
        })
    })
    .collect::<Vec<_>>();

    Ok(Json(tide_shared::ok(json!({
        "users_total": total_users,
        "users_today": today_users,
        "images_total": total_images,
        "images_today": today_images,
        "storage_bytes": storage_bytes,
        "pending_audit": pending_audit,
        "ai_stats": ai_stats,
        "recent_images": recent_images,
        "recent_bans": recent_bans,
        "storage": storage,
    }))))
}

async fn users(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, String, chrono::DateTime<chrono::Utc>)>(
        "SELECT id,email,username,role,status,created_at FROM users ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows.into_iter().map(|row| json!({"id":row.0,"email":row.1,"username":row.2,"role":row.3,"status":row.4,"created_at":row.5})).collect())))
}

async fn users_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let q = query.search_pattern();
    let role = query.role_value();
    let status = query.status_value();
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            String,
            String,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id,email,username,role,status,created_at
         FROM users
         WHERE ($1::text IS NULL OR lower(email) LIKE $1 OR lower(username) LIKE $1 OR lower(id::text) LIKE $1)
           AND ($2='' OR role=$2)
           AND ($3='' OR status=$3)
         ORDER BY created_at DESC
         LIMIT $4 OFFSET $5",
    )
    .bind(&q)
    .bind(&role)
    .bind(&status)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM users
         WHERE ($1::text IS NULL OR lower(email) LIKE $1 OR lower(username) LIKE $1 OR lower(id::text) LIKE $1)
           AND ($2='' OR role=$2)
           AND ($3='' OR status=$3)",
    )
    .bind(&q)
    .bind(&role)
    .bind(&status)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows
            .into_iter()
            .map(|row| {
                json!({
                    "id": row.0,
                    "email": row.1,
                    "username": row.2,
                    "role": row.3,
                    "status": row.4,
                    "created_at": row.5
                })
            })
            .collect(),
        page,
        page_size,
        total,
    })))
}

#[derive(Deserialize)]
struct AdminCreateUserRequest {
    email: String,
    username: String,
    password: String,
    role: Option<String>,
}

#[derive(Deserialize)]
struct AdminUpdateUserRequest {
    email: Option<String>,
    username: Option<String>,
    role: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
struct GroupRequest {
    name: String,
    code: String,
    description: Option<String>,
    is_default: Option<bool>,
}

#[derive(Deserialize)]
struct QuotaRuleRequest {
    daily_upload_count: Option<i32>,
    daily_upload_bytes: Option<i64>,
    max_file_size: Option<i64>,
    total_storage_bytes: Option<i64>,
    daily_api_calls: Option<i32>,
    daily_random_calls: Option<i32>,
    require_review: Option<bool>,
    require_captcha: Option<bool>,
    allow_batch_upload: Option<bool>,
    allow_tag_create: Option<bool>,
    default_storage_provider_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct StorageRouteRequest {
    name: Option<String>,
    scope_type: Option<String>,
    scope_value: Option<String>,
    storage_provider_id: Option<Uuid>,
    enabled: Option<bool>,
    priority: Option<i32>,
    note: Option<String>,
}

async fn create_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<AdminCreateUserRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let role = req.role.unwrap_or_else(|| "user".to_string());
    let username = normalize_username(&req.username)?;
    let hash = crate::auth::hash_password(&req.password)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email,username,password_hash,role,status) VALUES ($1,$2,$3,$4,'active') RETURNING id",
    )
    .bind(req.email.trim().to_lowercase())
    .bind(&username)
    .bind(hash)
    .bind(role)
    .fetch_one(&state.pool)
    .await
    .map_err(map_user_unique_error)?;
    sqlx::query("INSERT INTO user_profiles (user_id,display_name) VALUES ($1,$2)")
        .bind(id)
        .bind(username)
        .execute(&state.pool)
        .await?;
    Ok(Json(tide_shared::ok(json!({"id":id}))))
}

async fn user_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let row: (Uuid, String, String, String, String) =
        sqlx::query_as("SELECT id,email,username,role,status FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(
        json!({"id":row.0,"email":row.1,"username":row.2,"role":row.3,"status":row.4}),
    )))
}

async fn update_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
    Json(req): Json<AdminUpdateUserRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let username = req
        .username
        .as_deref()
        .map(normalize_username)
        .transpose()?;
    sqlx::query("UPDATE users SET email=COALESCE($2,email), username=COALESCE($3,username), role=COALESCE($4,role), status=COALESCE($5,status), updated_at=now() WHERE id=$1")
        .bind(user_id)
        .bind(req.email.map(|value| value.trim().to_lowercase()))
        .bind(username)
        .bind(req.role)
        .bind(req.status)
        .execute(&state.pool)
        .await
        .map_err(map_user_unique_error)?;
    Ok(empty_ok())
}

async fn delete_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE users SET status='deleted', updated_at=now() WHERE id=$1")
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "user.delete",
        "user",
        Some(user_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn ban_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE users SET status='banned', updated_at=now() WHERE id=$1")
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(&state, &user, "user.ban", "user", Some(user_id), json!({})).await?;
    Ok(empty_ok())
}

async fn unban_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE users SET status='active', updated_at=now() WHERE id=$1")
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "user.unban",
        "user",
        Some(user_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn update_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let role = body
        .get("role")
        .or_else(|| body.get("group"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("user");
    sqlx::query("UPDATE users SET role=$2, updated_at=now() WHERE id=$1")
        .bind(user_id)
        .bind(role)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "user.group.update",
        "user",
        Some(user_id),
        json!({"role":role}),
    )
    .await?;
    Ok(empty_ok())
}

async fn update_quota(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("INSERT INTO user_quota_overrides (user_id,quota_json,reason,created_by) VALUES ($1,$2,$3,$4)")
        .bind(user_id)
        .bind(body.get("quota").cloned().unwrap_or_else(|| body.clone()))
        .bind(body.get("reason").and_then(serde_json::Value::as_str).unwrap_or(""))
        .bind(user.id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "user.quota.update",
        "user",
        Some(user_id),
        json!({"updated_fields":json_object_keys(&body)}),
    )
    .await?;
    Ok(empty_ok())
}

async fn admin_images(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ImageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<ImageSummary>>>> {
    ensure_admin(&user)?;
    Ok(Json(tide_shared::ok(
        images::list_images(&state, Some(&user), &query, true).await?,
    )))
}

async fn admin_image_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    Ok(Json(tide_shared::ok(
        images::admin_image_detail(&state, Some(&user), image_id).await?,
    )))
}

async fn admin_trash_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    images::trash_image(&state, &user, image_id, true).await?;
    log_admin_operation(
        &state,
        &user,
        "image.trash",
        "image",
        Some(image_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn admin_restore_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    images::restore_image(&state, &user, image_id, true).await?;
    log_admin_operation(
        &state,
        &user,
        "image.restore",
        "image",
        Some(image_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn admin_permanent_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    images::permanent_delete(&state, &user, image_id, true).await?;
    log_admin_operation(
        &state,
        &user,
        "image.permanent_delete",
        "image",
        Some(image_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn approve_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE images SET status='active', updated_at=now() WHERE id=$1")
        .bind(image_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "image.approve",
        "image",
        Some(image_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn reject_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE audit_tasks SET status='rejected', finished_at=now() WHERE image_id=$1 AND status IN ('pending','running','manual_required')")
        .bind(image_id)
        .execute(&state.pool)
        .await?;
    images::permanent_delete_with_reason(&state, Some(&user), image_id, true, "管理员审核拒绝")
        .await?;
    log_admin_operation(
        &state,
        &user,
        "image.reject",
        "image",
        Some(image_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn block_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(image_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE images SET status='blocked', updated_at=now() WHERE id=$1")
        .bind(image_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "image.block",
        "image",
        Some(image_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn storage_providers(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<StorageProviderView>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<_, crate::models::StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority FROM storage_providers WHERE deleted_at IS NULL ORDER BY priority ASC, created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(
        rows.into_iter()
            .map(|row| StorageProviderView {
                id: row.id,
                name: row.name,
                provider_type: row.provider_type,
                config_json: security::redact_sensitive_json(row.config_json),
                is_default: row.is_default,
                enabled: row.enabled,
                priority: row.priority,
            })
            .collect(),
    )))
}

async fn storage_providers_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(20);
    let q = query.search_pattern();
    let provider_type = query.role_value();
    let status = query.status_value();
    let rows = sqlx::query_as::<_, crate::models::StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority
         FROM storage_providers
         WHERE deleted_at IS NULL
           AND ($1::text IS NULL OR lower(name) LIKE $1 OR lower(id::text) LIKE $1)
           AND ($2='' OR provider_type=$2)
           AND ($3='' OR ($3='enabled' AND enabled=true) OR ($3='disabled' AND enabled=false) OR ($3='default' AND is_default=true))
         ORDER BY priority ASC, created_at DESC
         LIMIT $4 OFFSET $5",
    )
    .bind(&q)
    .bind(&provider_type)
    .bind(&status)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM storage_providers
         WHERE deleted_at IS NULL
           AND ($1::text IS NULL OR lower(name) LIKE $1 OR lower(id::text) LIKE $1)
           AND ($2='' OR provider_type=$2)
           AND ($3='' OR ($3='enabled' AND enabled=true) OR ($3='disabled' AND enabled=false) OR ($3='default' AND is_default=true))",
    )
    .bind(&q)
    .bind(&provider_type)
    .bind(&status)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows.into_iter().map(storage_provider_json).collect(),
        page,
        page_size,
        total,
    })))
}

fn storage_provider_json(row: crate::models::StorageProviderRow) -> serde_json::Value {
    json!({
        "id": row.id,
        "name": row.name,
        "provider_type": row.provider_type,
        "config_json": security::redact_sensitive_json(row.config_json),
        "is_default": row.is_default,
        "enabled": row.enabled,
        "priority": row.priority,
    })
}

async fn storage_provider_json_by_id(
    state: &AppState,
    provider_id: Uuid,
) -> AppResult<serde_json::Value> {
    let row = storage_registry::active_provider_by_id(state, provider_id).await?;
    Ok(storage_provider_json(row))
}

fn storage_routes_select_sql(order: &str) -> String {
    format!(
        "SELECT sr.id,sr.name,sr.scope_type,sr.scope_value,sr.storage_provider_id,
                sp.name AS storage_provider_name,sp.provider_type AS storage_provider_type,
                sr.enabled,sr.priority,sr.note
         FROM storage_routes sr
         JOIN storage_providers sp ON sp.id=sr.storage_provider_id
         {order}"
    )
}

fn storage_route_json(row: StorageRouteRow) -> serde_json::Value {
    json!({
        "id": row.id,
        "name": row.name,
        "scope_type": row.scope_type,
        "scope_value": row.scope_value,
        "storage_provider_id": row.storage_provider_id,
        "storage_provider_name": row.storage_provider_name,
        "storage_provider_type": row.storage_provider_type,
        "enabled": row.enabled,
        "priority": row.priority,
        "note": row.note,
    })
}

async fn load_storage_route(state: &AppState, route_id: Uuid) -> AppResult<StorageRouteRow> {
    sqlx::query_as::<_, StorageRouteRow>(
        "SELECT sr.id,sr.name,sr.scope_type,sr.scope_value,sr.storage_provider_id,
                sp.name AS storage_provider_name,sp.provider_type AS storage_provider_type,
                sr.enabled,sr.priority,sr.note
         FROM storage_routes sr
         JOIN storage_providers sp ON sp.id=sr.storage_provider_id
         WHERE sr.id=$1",
    )
    .bind(route_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("storage route not found".to_string()))
}

async fn create_storage_provider(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let provider_type = normalize_storage_provider_type(
        body.get("provider_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("local"),
    );
    let raw_config = body
        .get("config_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let raw_config = normalize_storage_config(&provider_type, raw_config);
    validate_storage_config(&provider_type, &raw_config)?;
    let config_json = security::encrypt_sensitive_json(&state.config, raw_config)?;
    let has_default: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM storage_providers WHERE is_default=true)")
            .fetch_one(&state.pool)
            .await?;
    let enabled = body
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let priority = body
        .get("priority")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(100);
    let id: Uuid = sqlx::query_scalar("INSERT INTO storage_providers (name,provider_type,config_json,is_default,enabled,priority) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(body.get("name").and_then(serde_json::Value::as_str).unwrap_or("Storage"))
        .bind(&provider_type)
        .bind(config_json)
        .bind(!has_default)
        .bind(enabled)
        .bind(priority)
        .fetch_one(&state.pool)
        .await?;
    if !has_default {
        sqlx::query("UPDATE quota_rules SET default_storage_provider_id=$1, updated_at=now()")
            .bind(id)
            .execute(&state.pool)
            .await?;
    }
    log_admin_operation(
        &state,
        &user,
        "storage_provider.create",
        "storage_provider",
        Some(id),
        json!({"provider_type": provider_type}),
    )
    .await?;
    Ok(Json(tide_shared::ok(
        storage_provider_json_by_id(&state, id).await?,
    )))
}

async fn storage_provider_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<StorageProviderView>>> {
    ensure_admin(&user)?;
    let row = storage_registry::active_provider_by_id(&state, provider_id).await?;
    Ok(Json(tide_shared::ok(StorageProviderView {
        id: row.id,
        name: row.name,
        provider_type: row.provider_type,
        config_json: security::redact_sensitive_json(row.config_json),
        is_default: row.is_default,
        enabled: row.enabled,
        priority: row.priority,
    })))
}

async fn update_storage_provider(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    storage_registry::active_provider_by_id(&state, provider_id).await?;
    let provider_type_update = body
        .get("provider_type")
        .and_then(serde_json::Value::as_str)
        .map(normalize_storage_provider_type);
    let config_json = if let Some(value) = body.get("config_json").cloned() {
        let (existing_provider_type, existing): (String, serde_json::Value) =
            sqlx::query_as(
                "SELECT provider_type,config_json FROM storage_providers WHERE id=$1 AND deleted_at IS NULL",
            )
                .bind(provider_id)
                .fetch_one(&state.pool)
                .await?;
        let provider_type = provider_type_update
            .clone()
            .unwrap_or(existing_provider_type);
        let value = normalize_storage_config(&provider_type, value);
        let value = security::preserve_redacted_sensitive_json(value, existing);
        validate_storage_config(&provider_type, &value)?;
        Some(security::encrypt_sensitive_json(&state.config, value)?)
    } else {
        None
    };
    sqlx::query("UPDATE storage_providers SET name=COALESCE($2,name), config_json=COALESCE($3,config_json), enabled=COALESCE($4,enabled), priority=COALESCE($5,priority), provider_type=COALESCE($6,provider_type), updated_at=now() WHERE id=$1 AND deleted_at IS NULL")
        .bind(provider_id)
        .bind(body.get("name").and_then(serde_json::Value::as_str))
        .bind(config_json)
        .bind(body.get("enabled").and_then(serde_json::Value::as_bool))
        .bind(body.get("priority").and_then(serde_json::Value::as_i64).map(|value| value as i32))
        .bind(provider_type_update.as_deref())
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "storage_provider.update",
        "storage_provider",
        Some(provider_id),
        json!({"updated_fields":json_object_keys(&body)}),
    )
    .await?;
    Ok(Json(tide_shared::ok(
        storage_provider_json_by_id(&state, provider_id).await?,
    )))
}

async fn delete_storage_provider(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let mode = delete_storage_provider_by_id(&state, provider_id).await?;
    log_admin_operation(
        &state,
        &user,
        "storage_provider.delete",
        "storage_provider",
        Some(provider_id),
        json!({"mode": mode}),
    )
    .await?;
    Ok(Json(tide_shared::ok(json!({"mode": mode}))))
}

async fn disable_storage_provider(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let mode = delete_storage_provider_by_id(&state, provider_id).await?;
    log_admin_operation(
        &state,
        &user,
        "storage_provider.delete_compat",
        "storage_provider",
        Some(provider_id),
        json!({"mode": mode, "compat_action": "disable"}),
    )
    .await?;
    Ok(Json(tide_shared::ok(json!({"mode": mode}))))
}

async fn bulk_delete_storage_providers(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let ids = body
        .get("ids")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::BadRequest("ids is required".to_string()))?
        .iter()
        .filter_map(|value| value.as_str())
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| AppError::BadRequest("ids must contain valid uuid values".to_string()))
        })
        .collect::<AppResult<Vec<_>>>()?;
    if ids.is_empty() {
        return Err(AppError::BadRequest("ids is required".to_string()));
    }
    let mut deleted = Vec::new();
    let mut disabled = Vec::new();
    let mut failed = Vec::new();
    for id in ids {
        match delete_storage_provider_by_id(&state, id).await {
            Ok("deleted") => deleted.push(id),
            Ok(_) => disabled.push(id),
            Err(err) => failed.push(json!({"id": id, "error": err.to_string()})),
        }
    }
    log_admin_operation(
        &state,
        &user,
        "storage_provider.bulk_delete",
        "storage_provider",
        None,
        json!({"deleted": deleted, "disabled": disabled, "failed": failed}),
    )
    .await?;
    Ok(Json(tide_shared::ok(json!({
        "deleted": deleted,
        "disabled": disabled,
        "failed": failed
    }))))
}

async fn delete_storage_provider_by_id(
    state: &AppState,
    provider_id: Uuid,
) -> AppResult<&'static str> {
    let row = storage_registry::active_provider_by_id(state, provider_id).await?;
    let replacement = if row.is_default {
        replacement_storage_provider(state, provider_id).await?
    } else {
        None
    };
    let has_storage_objects: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM storage_objects WHERE storage_provider_id=$1)",
    )
    .bind(provider_id)
    .fetch_one(&state.pool)
    .await?;
    let has_migrations: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM migration_tasks WHERE source_storage_provider_id=$1 OR target_storage_provider_id=$1)",
    )
    .bind(provider_id)
    .fetch_one(&state.pool)
    .await?;
    let has_backups: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM backup_tasks WHERE target_storage_provider_id=$1)",
    )
    .bind(provider_id)
    .fetch_one(&state.pool)
    .await?;
    let has_backup_files: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM backup_files WHERE storage_provider_id=$1)",
    )
    .bind(provider_id)
    .fetch_one(&state.pool)
    .await?;
    let has_routes: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM storage_routes WHERE storage_provider_id=$1)",
    )
    .bind(provider_id)
    .fetch_one(&state.pool)
    .await?;
    if has_storage_objects || has_migrations || has_backups || has_backup_files {
        if row.is_default {
            let Some(replacement) = replacement else {
                return Err(AppError::Conflict(
                    "cannot delete or hide the only enabled storage provider while it is still referenced"
                        .to_string(),
                ));
            };
            promote_default_storage_provider(state, provider_id, replacement).await?;
        } else {
            sqlx::query("UPDATE quota_rules SET default_storage_provider_id=NULL, updated_at=now() WHERE default_storage_provider_id=$1")
                .bind(provider_id)
                .execute(&state.pool)
                .await?;
        }
        sqlx::query("UPDATE storage_routes SET enabled=false, updated_at=now() WHERE storage_provider_id=$1")
            .bind(provider_id)
            .execute(&state.pool)
            .await?;
        sqlx::query("UPDATE storage_providers SET enabled=false, is_default=false, deleted_at=now(), updated_at=now() WHERE id=$1")
            .bind(provider_id)
            .execute(&state.pool)
            .await?;
        return Ok("disabled");
    }
    let mut tx = state.pool.begin().await?;
    if let Some(replacement) = replacement {
        sqlx::query("UPDATE storage_providers SET is_default=false WHERE deleted_at IS NULL")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE storage_providers SET is_default=true WHERE id=$1")
            .bind(replacement)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE quota_rules SET default_storage_provider_id=$1, updated_at=now() WHERE default_storage_provider_id=$2")
            .bind(replacement)
            .bind(provider_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("UPDATE quota_rules SET default_storage_provider_id=NULL, updated_at=now() WHERE default_storage_provider_id=$1")
            .bind(provider_id)
            .execute(&mut *tx)
            .await?;
    }
    if has_routes {
        sqlx::query("DELETE FROM storage_routes WHERE storage_provider_id=$1")
            .bind(provider_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM storage_providers WHERE id=$1")
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok("deleted")
}

async fn replacement_storage_provider(
    state: &AppState,
    provider_id: Uuid,
) -> AppResult<Option<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM storage_providers WHERE id <> $1 AND deleted_at IS NULL AND enabled=true ORDER BY priority ASC, created_at DESC LIMIT 1",
    )
    .bind(provider_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::from)
}

async fn promote_default_storage_provider(
    state: &AppState,
    provider_id: Uuid,
    replacement: Uuid,
) -> AppResult<()> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE storage_providers SET is_default=false WHERE deleted_at IS NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE storage_providers SET is_default=true WHERE id=$1")
        .bind(replacement)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE quota_rules SET default_storage_provider_id=$1, updated_at=now() WHERE default_storage_provider_id=$2")
        .bind(replacement)
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn test_connection(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let row = storage_registry::active_provider_by_id(&state, provider_id).await?;
    let provider = match storage_registry::build_provider(&state, &row).await {
        Ok(provider) => provider,
        Err(err) => {
            return Ok(Json(tide_shared::ok(storage_test_failure(
                "test-connection",
                err,
            ))));
        }
    };
    match provider.health_check().await {
        Ok(()) => Ok(Json(tide_shared::ok(json!({
            "ok": true,
            "healthy": true,
            "operation": "test-connection"
        })))),
        Err(err) => Ok(Json(tide_shared::ok(storage_test_failure_with_stage(
            "test-connection",
            "connection",
            err,
        )))),
    }
}

async fn test_upload(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let row = storage_registry::active_provider_by_id(&state, provider_id).await?;
    let provider = match storage_registry::build_provider(&state, &row).await {
        Ok(provider) => provider,
        Err(err) => {
            return Ok(Json(tide_shared::ok(storage_test_failure(
                "test-upload",
                err,
            ))));
        }
    };
    let result: AppResult<serde_json::Value> = async {
        let key = format!("health/{}.txt", Uuid::new_v4());
        let stored = provider
            .put_object(&key, b"tide-storage-test", "text/plain")
            .await
            .map_err(|err| storage_test_stage_error("put", err))?;
        let read_back = provider
            .get_object(&stored.object_key)
            .await
            .map_err(|err| storage_test_stage_error("get", err))?;
        let read_back_ok = read_back == b"tide-storage-test";
        let _ = provider.delete_object(&stored.object_key).await;
        let url = stored
            .public_url
            .map(|url| images::normalize_public_url_for_base(&state.config.public_base_url, url));
        let public_url_accessible = row
            .config_json
            .get("public_domain")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        let access_status = match (url.as_deref(), public_url_accessible) {
            (Some(url), true) if url.starts_with("http://") || url.starts_with("https://") => state
                .http
                .get(url)
                .send()
                .await
                .map(|response| response.status().as_u16())
                .ok(),
            _ => None,
        };
        Ok(json!({
            "ok": read_back_ok,
            "healthy": read_back_ok,
            "operation": "test-upload",
            "object_key": stored.object_key,
            "size": stored.size,
            "url": url,
            "read_back_ok": read_back_ok,
            "access_status": access_status,
            "public_url_accessible": public_url_accessible
        }))
    }
    .await;
    Ok(Json(tide_shared::ok(match result {
        Ok(value) => value,
        Err(err) => storage_test_failure("test-upload", err),
    })))
}

async fn test_delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let row = storage_registry::active_provider_by_id(&state, provider_id).await?;
    let provider = match storage_registry::build_provider(&state, &row).await {
        Ok(provider) => provider,
        Err(err) => {
            return Ok(Json(tide_shared::ok(storage_test_failure(
                "test-delete",
                err,
            ))));
        }
    };
    let result: AppResult<serde_json::Value> = async {
        let key = format!("health/{}.txt", Uuid::new_v4());
        provider
            .put_object(&key, b"tide-storage-delete-test", "text/plain")
            .await
            .map_err(|err| storage_test_stage_error("put", err))?;
        provider
            .delete_object(&key)
            .await
            .map_err(|err| storage_test_stage_error("delete", err))?;
        Ok(json!({
            "ok": true,
            "healthy": true,
            "operation": "test-delete",
            "deleted": true
        }))
    }
    .await;
    Ok(Json(tide_shared::ok(match result {
        Ok(value) => value,
        Err(err) => storage_test_failure("test-delete", err),
    })))
}

fn storage_test_failure(operation: &str, err: impl std::fmt::Display) -> serde_json::Value {
    json!({
        "ok": false,
        "healthy": false,
        "operation": operation,
        "error": err.to_string()
    })
}

fn storage_test_failure_with_stage(
    operation: &str,
    stage: &str,
    err: impl std::fmt::Display,
) -> serde_json::Value {
    json!({
        "ok": false,
        "healthy": false,
        "operation": operation,
        "stage": stage,
        "error": storage_test_error_message(stage, err)
    })
}

fn storage_test_stage_error(stage: &'static str, err: AppError) -> AppError {
    AppError::External(storage_test_error_message(stage, err))
}

fn storage_test_error_message(stage: &str, err: impl std::fmt::Display) -> String {
    let label = match stage {
        "connection" => "连接检查",
        "put" => "写入对象",
        "get" => "读取对象",
        "delete" => "删除对象",
        _ => "存储测试",
    };
    format!("{label}失败：{err}")
}

async fn set_default_storage(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    storage_registry::active_provider_by_id(&state, provider_id).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE storage_providers SET is_default=false WHERE deleted_at IS NULL")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE storage_providers SET is_default=true, enabled=true WHERE id=$1 AND deleted_at IS NULL")
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE quota_rules SET default_storage_provider_id=$1, updated_at=now()")
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    log_admin_operation(
        &state,
        &user,
        "storage_provider.set_default",
        "storage_provider",
        Some(provider_id),
        json!({}),
    )
    .await?;
    Ok(Json(tide_shared::ok(
        storage_provider_json_by_id(&state, provider_id).await?,
    )))
}

async fn storage_routes(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let sql = storage_routes_select_sql(
        "ORDER BY sr.enabled DESC, sr.priority ASC, sr.created_at DESC LIMIT 500",
    );
    let rows = sqlx::query_as::<_, StorageRouteRow>(&sql)
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(tide_shared::ok(
        rows.into_iter().map(storage_route_json).collect(),
    )))
}

async fn storage_routes_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(20);
    let q = query.search_pattern();
    let scope = query.role_value();
    let status = query.status_value();
    let rows = sqlx::query_as::<_, StorageRouteRow>(
        "SELECT sr.id,sr.name,sr.scope_type,sr.scope_value,sr.storage_provider_id,
                sp.name AS storage_provider_name,sp.provider_type AS storage_provider_type,
                sr.enabled,sr.priority,sr.note
         FROM storage_routes sr
         JOIN storage_providers sp ON sp.id=sr.storage_provider_id
         WHERE ($1::text IS NULL OR lower(sr.name) LIKE $1 OR lower(sr.scope_value) LIKE $1 OR lower(sr.id::text) LIKE $1 OR lower(sp.name) LIKE $1)
           AND ($2='' OR sr.scope_type=$2)
           AND ($3='' OR ($3='enabled' AND sr.enabled=true) OR ($3='disabled' AND sr.enabled=false))
         ORDER BY sr.enabled DESC, sr.priority ASC, sr.created_at DESC
         LIMIT $4 OFFSET $5",
    )
    .bind(&q)
    .bind(&scope)
    .bind(&status)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM storage_routes sr
         JOIN storage_providers sp ON sp.id=sr.storage_provider_id
         WHERE ($1::text IS NULL OR lower(sr.name) LIKE $1 OR lower(sr.scope_value) LIKE $1 OR lower(sr.id::text) LIKE $1 OR lower(sp.name) LIKE $1)
           AND ($2='' OR sr.scope_type=$2)
           AND ($3='' OR ($3='enabled' AND sr.enabled=true) OR ($3='disabled' AND sr.enabled=false))",
    )
    .bind(&q)
    .bind(&scope)
    .bind(&status)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows.into_iter().map(storage_route_json).collect(),
        page,
        page_size,
        total,
    })))
}

async fn storage_routes_summary(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let by_scope: Vec<serde_json::Value> = sqlx::query_as::<_, (String, i64)>(
        "SELECT scope_type, COUNT(*) FROM storage_routes GROUP BY scope_type ORDER BY scope_type",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| json!({"scope_type": row.0, "count": row.1}))
    .collect();
    let active_routes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM storage_routes WHERE enabled=true")
            .fetch_one(&state.pool)
            .await?;
    let routed_providers: Vec<serde_json::Value> = sqlx::query_as::<_, (Uuid, String, i64)>(
        "SELECT sp.id,sp.name,COUNT(sr.id)
         FROM storage_providers sp
         LEFT JOIN storage_routes sr ON sr.storage_provider_id=sp.id AND sr.enabled=true
         WHERE sp.deleted_at IS NULL
         GROUP BY sp.id,sp.name
         ORDER BY COUNT(sr.id) DESC, sp.priority ASC",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| json!({"id": row.0, "name": row.1, "active_routes": row.2}))
    .collect();
    Ok(Json(tide_shared::ok(json!({
        "active_routes": active_routes,
        "by_scope": by_scope,
        "providers": routed_providers,
        "precedence": ["user", "group", "role", "global", "quota_rule", "default_provider"]
    }))))
}

async fn storage_route_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(route_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let row = load_storage_route(&state, route_id).await?;
    Ok(Json(tide_shared::ok(storage_route_json(row))))
}

async fn create_storage_route(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<StorageRouteRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let provider_id = req
        .storage_provider_id
        .ok_or_else(|| AppError::BadRequest("storage_provider_id is required".to_string()))?;
    storage_registry::active_enabled_provider_by_id(&state, provider_id).await?;
    let scope_type = req.scope_type.unwrap_or_else(|| "global".to_string());
    let scope_value = normalize_storage_route_scope(&scope_type, req.scope_value.as_deref())?;
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| storage_route_default_name(&scope_type, &scope_value))
        .to_string();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO storage_routes (name,scope_type,scope_value,storage_provider_id,enabled,priority,note)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING id",
    )
    .bind(name)
    .bind(&scope_type)
    .bind(&scope_value)
    .bind(provider_id)
    .bind(req.enabled.unwrap_or(true))
    .bind(req.priority.unwrap_or(100))
    .bind(req.note.unwrap_or_default())
    .fetch_one(&state.pool)
    .await?;
    log_admin_operation(
        &state,
        &user,
        "storage_route.create",
        "storage_route",
        Some(id),
        json!({"scope_type":scope_type,"scope_value":scope_value,"storage_provider_id":provider_id}),
    )
    .await?;
    Ok(Json(tide_shared::ok(json!({"id": id}))))
}

async fn update_storage_route(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(route_id): Path<Uuid>,
    Json(req): Json<StorageRouteRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let current = load_storage_route(&state, route_id).await?;
    let scope_type = req.scope_type.unwrap_or(current.scope_type);
    let scope_value = normalize_storage_route_scope(
        &scope_type,
        req.scope_value.as_deref().or(Some(&current.scope_value)),
    )?;
    if let Some(provider_id) = req.storage_provider_id {
        storage_registry::active_enabled_provider_by_id(&state, provider_id).await?;
    }
    let name = req.name.unwrap_or(current.name);
    let note = req.note.unwrap_or(current.note);
    sqlx::query(
        "UPDATE storage_routes
         SET name=$2,scope_type=$3,scope_value=$4,storage_provider_id=COALESCE($5,storage_provider_id),
             enabled=COALESCE($6,enabled),priority=COALESCE($7,priority),note=$8,updated_at=now()
         WHERE id=$1",
    )
    .bind(route_id)
    .bind(name.trim())
    .bind(&scope_type)
    .bind(&scope_value)
    .bind(req.storage_provider_id)
    .bind(req.enabled)
    .bind(req.priority)
    .bind(note)
    .execute(&state.pool)
    .await?;
    log_admin_operation(
        &state,
        &user,
        "storage_route.update",
        "storage_route",
        Some(route_id),
        json!({"scope_type":scope_type,"scope_value":scope_value}),
    )
    .await?;
    Ok(empty_ok())
}

async fn delete_storage_route(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(route_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("DELETE FROM storage_routes WHERE id=$1")
        .bind(route_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "storage_route.delete",
        "storage_route",
        Some(route_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}

async fn enable_storage_route(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(route_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    set_storage_route_enabled(&state, &user, route_id, true).await?;
    Ok(empty_ok())
}

async fn disable_storage_route(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(route_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    set_storage_route_enabled(&state, &user, route_id, false).await?;
    Ok(empty_ok())
}

async fn storage_health(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<_, crate::models::StorageProviderRow>("SELECT id,name,provider_type,config_json,is_default,enabled,priority FROM storage_providers WHERE deleted_at IS NULL ORDER BY priority ASC").fetch_all(&state.pool).await?;
    Ok(Json(tide_shared::ok(
        storage_health_for_rows(&state, rows).await,
    )))
}

#[derive(Debug, Deserialize)]
struct StorageHealthPageQuery {
    ids: Option<String>,
}

async fn storage_health_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<StorageHealthPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let ids = query
        .ids
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| AppError::BadRequest("ids must contain valid uuid values".to_string()))
        })
        .collect::<AppResult<Vec<_>>>()?;
    if ids.is_empty() {
        return Ok(Json(tide_shared::ok(Vec::new())));
    }
    let rows = sqlx::query_as::<_, crate::models::StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority
         FROM storage_providers
         WHERE deleted_at IS NULL AND id = ANY($1)
         ORDER BY priority ASC",
    )
    .bind(&ids)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(
        storage_health_for_rows(&state, rows).await,
    )))
}

async fn storage_health_for_rows(
    state: &AppState,
    rows: Vec<crate::models::StorageProviderRow>,
) -> Vec<serde_json::Value> {
    stream::iter(rows.into_iter().map(|row| {
        let state = state.clone();
        async move { storage_health_for_row(&state, row).await }
    }))
    .buffered(6)
    .collect()
    .await
}

async fn storage_health_for_row(
    state: &AppState,
    row: crate::models::StorageProviderRow,
) -> serde_json::Value {
    if !row.enabled {
        return json!({
            "id": row.id,
            "name": row.name,
            "provider_type": row.provider_type,
            "healthy": false,
            "enabled": row.enabled,
            "error": "storage provider is disabled"
        });
    }
    match storage_registry::build_provider(state, &row).await {
        Ok(provider) => match timeout(TokioDuration::from_secs(5), provider.health_check()).await {
            Err(_) => json!({
                "id": row.id,
                "name": row.name,
                "provider_type": row.provider_type,
                "healthy": false,
                "enabled": row.enabled,
                "error": "storage health check timed out"
            }),
            Ok(Ok(())) => json!({
                "id": row.id,
                "name": row.name,
                "provider_type": row.provider_type,
                "healthy": true,
                "enabled": row.enabled,
                "error": null
            }),
            Ok(Err(err)) => json!({
                "id": row.id,
                "name": row.name,
                "provider_type": row.provider_type,
                "healthy": false,
                "enabled": row.enabled,
                "error": err.to_string()
            }),
        },
        Err(err) => json!({
            "id": row.id,
            "name": row.name,
            "provider_type": row.provider_type,
            "healthy": false,
            "enabled": row.enabled,
            "error": err.to_string()
        }),
    }
}

async fn user_groups(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, bool)>(
        "SELECT id,name,code,description,is_default FROM user_groups ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(
        rows.into_iter()
            .map(|r| json!({"id":r.0,"name":r.1,"code":r.2,"description":r.3,"is_default":r.4}))
            .collect(),
    )))
}

async fn quota_rules(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            i32,
            i64,
            i64,
            i64,
            i32,
            i32,
            bool,
            bool,
            bool,
            bool,
            Option<Uuid>,
        ),
    >(
        "SELECT ug.id,ug.code,qr.daily_upload_count,qr.daily_upload_bytes,qr.max_file_size,qr.total_storage_bytes,qr.daily_api_calls,qr.daily_random_calls,qr.require_review,qr.require_captcha,qr.allow_batch_upload,qr.allow_tag_create,qr.default_storage_provider_id FROM user_groups ug JOIN quota_rules qr ON qr.group_id=ug.id ORDER BY ug.created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(
        rows.into_iter()
            .map(|r| {
                json!({
                    "group_id":r.0,
                    "code":r.1,
                    "daily_upload_count":r.2,
                    "daily_upload_bytes":r.3,
                    "max_file_size":r.4,
                    "total_storage_bytes":r.5,
                    "daily_api_calls":r.6,
                    "daily_random_calls":r.7,
                    "require_review":r.8,
                    "require_captcha":r.9,
                    "allow_batch_upload":r.10,
                    "allow_tag_create":r.11,
                    "default_storage_provider_id":r.12,
                })
            })
            .collect(),
    )))
}

async fn create_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<GroupRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO user_groups (name,code,description,is_default) VALUES ($1,$2,$3,$4) RETURNING id",
    )
    .bind(req.name)
    .bind(req.code)
    .bind(req.description.unwrap_or_default())
    .bind(req.is_default.unwrap_or(false))
    .fetch_one(&state.pool)
    .await?;
    sqlx::query("INSERT INTO quota_rules (group_id,daily_upload_count,daily_upload_bytes,max_file_size,total_storage_bytes,daily_api_calls,daily_random_calls,require_review,require_captcha,allow_batch_upload,allow_tag_create) VALUES ($1,100,1073741824,52428800,10737418240,5000,5000,true,false,true,true)")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(Json(tide_shared::ok(json!({"id":id}))))
}
async fn update_group_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(group_id): Path<Uuid>,
    Json(req): Json<GroupRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE user_groups SET name=$2, code=$3, description=$4, is_default=$5, updated_at=now() WHERE id=$1")
        .bind(group_id)
        .bind(req.name)
        .bind(req.code)
        .bind(req.description.unwrap_or_default())
        .bind(req.is_default.unwrap_or(false))
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}
async fn delete_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("DELETE FROM quota_rules WHERE group_id=$1")
        .bind(group_id)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM user_groups WHERE id=$1 AND is_default=false")
        .bind(group_id)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}
async fn update_quota_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(group_id): Path<Uuid>,
    Json(req): Json<QuotaRuleRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE quota_rules SET daily_upload_count=COALESCE($2,daily_upload_count), daily_upload_bytes=COALESCE($3,daily_upload_bytes), max_file_size=COALESCE($4,max_file_size), total_storage_bytes=COALESCE($5,total_storage_bytes), daily_api_calls=COALESCE($6,daily_api_calls), daily_random_calls=COALESCE($7,daily_random_calls), require_review=COALESCE($8,require_review), require_captcha=COALESCE($9,require_captcha), allow_batch_upload=COALESCE($10,allow_batch_upload), allow_tag_create=COALESCE($11,allow_tag_create), default_storage_provider_id=COALESCE($12,default_storage_provider_id), updated_at=now() WHERE group_id=$1")
        .bind(group_id)
        .bind(req.daily_upload_count)
        .bind(req.daily_upload_bytes)
        .bind(req.max_file_size)
        .bind(req.total_storage_bytes)
        .bind(req.daily_api_calls)
        .bind(req.daily_random_calls)
        .bind(req.require_review)
        .bind(req.require_captcha)
        .bind(req.allow_batch_upload)
        .bind(req.allow_tag_create)
        .bind(req.default_storage_provider_id)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}
async fn quota_usage(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<_, (Uuid, chrono::NaiveDate, i32, i64, i32, i32)>(
        "SELECT user_id,date,uploaded_count,uploaded_bytes,api_calls,random_calls FROM quota_usage ORDER BY date DESC LIMIT 500",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows.into_iter().map(|r| json!({"user_id":r.0,"date":r.1,"uploaded_count":r.2,"uploaded_bytes":r.3,"api_calls":r.4,"random_calls":r.5})).collect())))
}
async fn audit_tasks(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows = sqlx::query_as::<_, (Uuid, Uuid, String, String, String, i32, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id,image_id,audit_type,provider,status,retry_count,error_message,created_at FROM audit_tasks ORDER BY created_at DESC LIMIT 500",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows.into_iter().map(|r| json!({"id":r.0,"image_id":r.1,"audit_type":r.2,"provider":r.3,"status":r.4,"retry_count":r.5,"error_message":r.6,"created_at":r.7})).collect())))
}

async fn audit_tasks_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let q = query.search_pattern();
    let status = query.status_value();
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            String,
            String,
            i32,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id,image_id,audit_type,provider,status,retry_count,error_message,created_at
         FROM audit_tasks
         WHERE ($1='' OR status=$1)
           AND ($2::text IS NULL OR lower(image_id::text) LIKE $2 OR lower(id::text) LIKE $2 OR lower(provider) LIKE $2)
         ORDER BY created_at DESC
         LIMIT $3 OFFSET $4",
    )
    .bind(&status)
    .bind(&q)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM audit_tasks
         WHERE ($1='' OR status=$1)
           AND ($2::text IS NULL OR lower(image_id::text) LIKE $2 OR lower(id::text) LIKE $2 OR lower(provider) LIKE $2)",
    )
    .bind(&status)
    .bind(&q)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.0,
                    "image_id": r.1,
                    "audit_type": r.2,
                    "provider": r.3,
                    "status": r.4,
                    "retry_count": r.5,
                    "error_message": r.6,
                    "created_at": r.7
                })
            })
            .collect(),
        page,
        page_size,
        total,
    })))
}
async fn audit_task_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let task: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(audit_tasks) FROM audit_tasks WHERE id=$1")
            .bind(task_id)
            .fetch_one(&state.pool)
            .await?;
    let results: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(audit_results) FROM audit_results WHERE audit_task_id=$1 ORDER BY created_at DESC")
            .bind(task_id)
            .fetch_all(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(
        json!({"task":task,"results":results}),
    )))
}
async fn approve_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let image_id: Uuid = sqlx::query_scalar(
        "UPDATE audit_tasks SET status='passed', finished_at=now() WHERE id=$1 RETURNING image_id",
    )
    .bind(task_id)
    .fetch_one(&state.pool)
    .await?;
    sqlx::query("UPDATE images SET status='active', updated_at=now() WHERE id=$1")
        .bind(image_id)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "audit_task.approve",
        "audit_task",
        Some(task_id),
        json!({"image_id":image_id}),
    )
    .await?;
    Ok(empty_ok())
}
async fn reject_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let image_id: Uuid = sqlx::query_scalar("UPDATE audit_tasks SET status='rejected', finished_at=now() WHERE id=$1 RETURNING image_id")
        .bind(task_id)
        .fetch_one(&state.pool)
        .await?;
    images::permanent_delete_with_reason(&state, Some(&user), image_id, true, "管理员审核任务拒绝")
        .await?;
    log_admin_operation(
        &state,
        &user,
        "audit_task.reject",
        "audit_task",
        Some(task_id),
        json!({"image_id":image_id}),
    )
    .await?;
    Ok(empty_ok())
}
async fn retry_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    audit::retry_audit_task(&state, task_id).await?;
    log_admin_operation(
        &state,
        &user,
        "audit_task.retry",
        "audit_task",
        Some(task_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}
async fn audit_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let value = sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='audit'")
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or_else(|| json!({"ai_enabled":true,"failure_strategy":"manual_required","keyword_enabled":true}));
    Ok(Json(tide_shared::ok(security::redact_sensitive_json(
        value,
    ))))
}
async fn update_audit_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    upsert_secure_setting(&state, "site_settings", "audit", body).await?;
    log_admin_operation(
        &state,
        &user,
        "settings.audit.update",
        "settings",
        None,
        json!({"key":"audit"}),
    )
    .await?;
    Ok(empty_ok())
}
async fn audit_logs(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(audit_results) FROM audit_results ORDER BY created_at DESC LIMIT 500",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows)))
}

async fn audit_logs_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let q = query.search_pattern();
    let status = query.status_value();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(audit_results)
         FROM audit_results
         WHERE ($1='' OR result=$1 OR risk_level=$1)
           AND ($2::text IS NULL OR lower(image_id::text) LIKE $2 OR lower(provider) LIKE $2 OR lower(reason) LIKE $2)
         ORDER BY created_at DESC
         LIMIT $3 OFFSET $4",
    )
    .bind(&status)
    .bind(&q)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM audit_results
         WHERE ($1='' OR result=$1 OR risk_level=$1)
           AND ($2::text IS NULL OR lower(image_id::text) LIKE $2 OR lower(provider) LIKE $2 OR lower(reason) LIKE $2)",
    )
    .bind(&status)
    .bind(&q)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows,
        page,
        page_size,
        total,
    })))
}
async fn migrations(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(migration_tasks) FROM migration_tasks ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows)))
}

async fn migrations_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let status = query.status_value();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(migration_tasks)
         FROM migration_tasks
         WHERE ($1='' OR status=$1)
         ORDER BY created_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&status)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM migration_tasks WHERE ($1='' OR status=$1)")
            .bind(&status)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows,
        page,
        page_size,
        total,
    })))
}
async fn create_migration(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let source = parse_uuid(&body, "source_storage_provider_id")?;
    let target = parse_uuid(&body, "target_storage_provider_id")?;
    let mode = body
        .get("migration_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("copy");
    let filter_json = body
        .get("filter_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result =
        tasks::create_migration_task(&state, source, target, mode, filter_json, user.id).await?;
    tasks::spawn_migration(state.clone(), result.task_id);
    log_admin_operation(
        &state,
        &user,
        "migration.create",
        "migration_task",
        Some(result.task_id),
        json!({"source_storage_provider_id":source,"target_storage_provider_id":target,"migration_mode":mode,"total_count":result.total_count}),
    )
    .await?;
    Ok(Json(tide_shared::ok(
        json!({"id":result.task_id,"total_count":result.total_count}),
    )))
}
async fn migration_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let task: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(migration_tasks) FROM migration_tasks WHERE id=$1")
            .bind(task_id)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(task)))
}
async fn task_pause(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    update_task_status(&state, "migration_tasks", task_id, "paused").await?;
    log_admin_operation(
        &state,
        &user,
        "migration.pause",
        "migration_task",
        Some(task_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}
async fn task_resume(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    update_task_status(&state, "migration_tasks", task_id, "running").await?;
    tasks::spawn_migration(state.clone(), task_id);
    log_admin_operation(
        &state,
        &user,
        "migration.resume",
        "migration_task",
        Some(task_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}
async fn task_cancel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    update_task_status(&state, "migration_tasks", task_id, "cancelled").await?;
    log_admin_operation(
        &state,
        &user,
        "migration.cancel",
        "migration_task",
        Some(task_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}
async fn task_retry(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    sqlx::query("UPDATE migration_task_items SET status='pending', retry_count=retry_count+1, error_message=NULL WHERE migration_task_id=$1 AND status='failed'")
        .bind(task_id)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE migration_tasks SET status='pending' WHERE id=$1")
        .bind(task_id)
        .execute(&state.pool)
        .await?;
    tasks::spawn_migration(state.clone(), task_id);
    log_admin_operation(
        &state,
        &user,
        "migration.retry_failed",
        "migration_task",
        Some(task_id),
        json!({}),
    )
    .await?;
    Ok(empty_ok())
}
async fn migration_items(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(migration_task_items) FROM migration_task_items WHERE migration_task_id=$1 ORDER BY created_at LIMIT 1000")
            .bind(task_id)
            .fetch_all(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(rows)))
}

async fn migration_items_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(task_id): Path<Uuid>,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(80);
    let status = query.status_value();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(migration_task_items)
         FROM migration_task_items
         WHERE migration_task_id=$1 AND ($2='' OR status=$2)
         ORDER BY created_at
         LIMIT $3 OFFSET $4",
    )
    .bind(task_id)
    .bind(&status)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM migration_task_items WHERE migration_task_id=$1 AND ($2='' OR status=$2)",
    )
    .bind(task_id)
    .bind(&status)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows,
        page,
        page_size,
        total,
    })))
}
async fn backups(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(backup_tasks) FROM backup_tasks ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows)))
}

async fn backups_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let status = query.status_value();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(backup_tasks)
         FROM backup_tasks
         WHERE ($1='' OR status=$1)
         ORDER BY created_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&status)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM backup_tasks WHERE ($1='' OR status=$1)")
            .bind(&status)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows,
        page,
        page_size,
        total,
    })))
}
async fn create_backup(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let target = body
        .get("target_storage_provider_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    let backup_type = body
        .get("backup_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("manual");
    let id: Uuid = sqlx::query_scalar("INSERT INTO backup_tasks (backup_type,target_storage_provider_id,status,include_files,include_logs,created_by,started_at) VALUES ($1,$2,'running',$3,$4,$5,now()) RETURNING id")
        .bind(backup_type)
        .bind(target)
        .bind(body.get("include_files").and_then(serde_json::Value::as_bool).unwrap_or(false))
        .bind(body.get("include_logs").and_then(serde_json::Value::as_bool).unwrap_or(true))
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    tasks::spawn_backup(state.clone(), id, target);
    log_admin_operation(
        &state,
        &user,
        "backup.create",
        "backup_task",
        Some(id),
        json!({"backup_type":backup_type,"target_storage_provider_id":target}),
    )
    .await?;
    Ok(Json(tide_shared::ok(json!({"id":id}))))
}
async fn backup_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(backup_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let task: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(backup_tasks) FROM backup_tasks WHERE id=$1")
            .bind(backup_id)
            .fetch_one(&state.pool)
            .await?;
    let files: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(backup_files) FROM backup_files WHERE backup_task_id=$1",
    )
    .bind(backup_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(json!({"task":task,"files":files}))))
}
async fn backup_download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(backup_id): Path<Uuid>,
) -> AppResult<Response> {
    ensure_admin(&user)?;
    let file = tasks::load_backup_file(&state, backup_id).await?;
    let mut response = Response::new(Body::from(file.bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"{}\"",
            download_file_name(&file.file_name)
        ))
        .map_err(|err| AppError::BadRequest(err.to_string()))?,
    );
    Ok(response)
}
async fn create_restore(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let backup_id = body
        .get("backup_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| AppError::BadRequest("backup_id is required".to_string()))?;
    let id = tasks::restore_settings_from_backup(
        &state,
        backup_id,
        user.id,
        body.get("restore_options_json")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )
    .await?;
    log_admin_operation(
        &state,
        &user,
        "restore.create",
        "restore_task",
        Some(id),
        json!({"backup_id":backup_id}),
    )
    .await?;
    Ok(Json(tide_shared::ok(json!({"id":id}))))
}
async fn restore_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(restore_id): Path<Uuid>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let task: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(restore_tasks) FROM restore_tasks WHERE id=$1")
            .bind(restore_id)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(task)))
}
async fn backup_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let value = sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='backup'")
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or_else(|| json!({"scheduled":false,"include_files":false,"include_logs":true}));
    Ok(Json(tide_shared::ok(security::redact_sensitive_json(
        value,
    ))))
}
async fn update_backup_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    upsert_secure_setting(&state, "site_settings", "backup", body).await?;
    log_admin_operation(
        &state,
        &user,
        "settings.backup.update",
        "settings",
        None,
        json!({"key":"backup"}),
    )
    .await?;
    Ok(empty_ok())
}
async fn update_site_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    upsert_secure_setting(&state, "site_settings", "site", body).await?;
    log_admin_operation(
        &state,
        &user,
        "settings.site.update",
        "settings",
        None,
        json!({"key":"site"}),
    )
    .await?;
    Ok(empty_ok())
}
async fn update_theme_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    upsert_setting(&state, "theme_settings", "theme", body).await?;
    log_admin_operation(
        &state,
        &user,
        "settings.theme.update",
        "settings",
        None,
        json!({"key":"theme"}),
    )
    .await?;
    Ok(empty_ok())
}

async fn update_upload_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    upsert_secure_setting(&state, "site_settings", "upload", body).await?;
    log_admin_operation(
        &state,
        &user,
        "settings.upload.update",
        "settings",
        None,
        json!({"key":"upload"}),
    )
    .await?;
    Ok(empty_ok())
}

async fn update_random_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    upsert_secure_setting(&state, "site_settings", "random", body).await?;
    log_admin_operation(
        &state,
        &user,
        "settings.random.update",
        "settings",
        None,
        json!({"key":"random"}),
    )
    .await?;
    Ok(empty_ok())
}

async fn smtp_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let value = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT jsonb_build_object(
            'id', id,
            'name', name,
            'host', host,
            'port', port,
            'username', username,
            'password', password_encrypted,
            'password_encrypted', password_encrypted,
            'from_email', from_email,
            'from_name', from_name,
            'enabled', enabled,
            'created_at', created_at,
            'updated_at', updated_at
         ) FROM smtp_settings ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_else(|| {
        json!({
            "name": "SMTP",
            "host": "",
            "port": 587,
            "username": "",
            "password": "",
            "from_email": "",
            "from_name": "潮汐图床",
            "enabled": false
        })
    });
    Ok(Json(tide_shared::ok(security::redact_sensitive_json(
        value,
    ))))
}

async fn update_smtp_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    ensure_admin(&user)?;
    let encrypted_password = body
        .get("password")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| security::encrypt_value(&state.config, value))
        .transpose()?
        .unwrap_or_default();
    sqlx::query("INSERT INTO smtp_settings (name,host,port,username,password_encrypted,from_email,from_name,enabled) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(body.get("name").and_then(serde_json::Value::as_str).unwrap_or("SMTP"))
        .bind(body.get("host").and_then(serde_json::Value::as_str).unwrap_or(""))
        .bind(body.get("port").and_then(serde_json::Value::as_i64).unwrap_or(587) as i32)
        .bind(body.get("username").and_then(serde_json::Value::as_str).unwrap_or(""))
        .bind(encrypted_password)
        .bind(body.get("from_email").and_then(serde_json::Value::as_str).unwrap_or(""))
        .bind(body.get("from_name").and_then(serde_json::Value::as_str).unwrap_or(""))
        .bind(body.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(false))
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        &state,
        &user,
        "settings.smtp.update",
        "settings",
        None,
        json!({"host":body.get("host").and_then(serde_json::Value::as_str).unwrap_or("")}),
    )
    .await?;
    Ok(empty_ok())
}

async fn system_logs(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(system_logs) FROM system_logs ORDER BY created_at DESC LIMIT 500",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(rows)))
}

async fn system_logs_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let q = query.search_pattern();
    let level = query.level_value();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(system_logs)
         FROM system_logs
         WHERE ($1='' OR level=$1)
           AND ($2::text IS NULL OR lower(module) LIKE $2 OR lower(message) LIKE $2)
         ORDER BY created_at DESC
         LIMIT $3 OFFSET $4",
    )
    .bind(&level)
    .bind(&q)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM system_logs
         WHERE ($1='' OR level=$1)
           AND ($2::text IS NULL OR lower(module) LIKE $2 OR lower(message) LIKE $2)",
    )
    .bind(&level)
    .bind(&q)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows,
        page,
        page_size,
        total,
    })))
}
async fn operation_logs(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<tide_shared::ApiResponse<Vec<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let rows: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(admin_operation_logs) FROM admin_operation_logs ORDER BY created_at DESC LIMIT 500")
            .fetch_all(&state.pool)
            .await?;
    Ok(Json(tide_shared::ok(rows)))
}

async fn operation_logs_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AdminPageQuery>,
) -> AppResult<Json<tide_shared::ApiResponse<Page<serde_json::Value>>>> {
    ensure_admin(&user)?;
    let (page, page_size, offset) = query.page_values(40);
    let q = query.search_pattern();
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(admin_operation_logs)
         FROM admin_operation_logs
         WHERE $1::text IS NULL
            OR lower(action) LIKE $1
            OR lower(target_type) LIKE $1
            OR lower(target_id::text) LIKE $1
            OR lower(admin_user_id::text) LIKE $1
         ORDER BY created_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&q)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM admin_operation_logs
         WHERE $1::text IS NULL
            OR lower(action) LIKE $1
            OR lower(target_type) LIKE $1
            OR lower(target_id::text) LIKE $1
            OR lower(admin_user_id::text) LIKE $1",
    )
    .bind(&q)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(tide_shared::ok(Page {
        items: rows,
        page,
        page_size,
        total,
    })))
}

async fn upsert_setting(
    state: &AppState,
    table: &str,
    key: &str,
    value: serde_json::Value,
) -> AppResult<()> {
    let sql = match table {
        "site_settings" => {
            "INSERT INTO site_settings (key,value_json) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value_json=$2, updated_at=now()"
        }
        "theme_settings" => {
            "INSERT INTO theme_settings (key,value_json) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value_json=$2, updated_at=now()"
        }
        _ => return Err(AppError::BadRequest("invalid settings table".to_string())),
    };
    sqlx::query(sql)
        .bind(key)
        .bind(value)
        .execute(&state.pool)
        .await?;
    Ok(())
}

async fn upsert_secure_setting(
    state: &AppState,
    table: &str,
    key: &str,
    value: serde_json::Value,
) -> AppResult<()> {
    let existing = load_existing_setting(state, table, key).await?;
    let value = security::preserve_redacted_sensitive_json(value, existing);
    let value = security::encrypt_sensitive_json(&state.config, value)?;
    upsert_setting(state, table, key, value).await
}

async fn load_existing_setting(
    state: &AppState,
    table: &str,
    key: &str,
) -> AppResult<serde_json::Value> {
    let sql = match table {
        "site_settings" => "SELECT value_json FROM site_settings WHERE key=$1",
        "theme_settings" => "SELECT value_json FROM theme_settings WHERE key=$1",
        _ => return Err(AppError::BadRequest("invalid settings table".to_string())),
    };
    Ok(sqlx::query_scalar(sql)
        .bind(key)
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or_else(|| json!({})))
}

async fn log_admin_operation(
    state: &AppState,
    user: &CurrentUser,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
    detail_json: serde_json::Value,
) -> AppResult<()> {
    sqlx::query("INSERT INTO admin_operation_logs (admin_user_id,action,target_type,target_id,context_json,detail_json) VALUES ($1,$2,$3,$4,$5,$5)")
        .bind(user.id)
        .bind(action)
        .bind(target_type)
        .bind(target_id)
        .bind(security::redact_sensitive_json(detail_json))
        .execute(&state.pool)
        .await?;
    Ok(())
}

fn json_object_keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn normalize_storage_route_scope(scope_type: &str, scope_value: Option<&str>) -> AppResult<String> {
    let value = scope_value.unwrap_or_default().trim();
    match scope_type {
        "global" => Ok(String::new()),
        "role" | "group" => {
            if value.is_empty() {
                Err(AppError::BadRequest(format!(
                    "{scope_type} storage route requires scope_value"
                )))
            } else {
                Ok(value.to_string())
            }
        }
        "user" => {
            if value.is_empty() {
                return Err(AppError::BadRequest(
                    "user storage route requires scope_value".to_string(),
                ));
            }
            Uuid::parse_str(value)
                .map(|uuid| uuid.to_string())
                .map_err(|_| AppError::BadRequest("user scope_value must be a uuid".to_string()))
        }
        _ => Err(AppError::BadRequest(format!(
            "unsupported storage route scope_type {scope_type}"
        ))),
    }
}

fn storage_route_default_name<'a>(scope_type: &'a str, scope_value: &'a str) -> &'a str {
    match scope_type {
        "global" => "全局存储路由",
        "role" if scope_value == "user" => "普通用户存储路由",
        "role" if scope_value == "trusted" => "可信用户存储路由",
        "role" if scope_value == "supporter" => "支持者存储路由",
        "role" if scope_value == "admin" => "管理员存储路由",
        "group" => "用户组存储路由",
        "user" => "用户专属存储路由",
        _ => "存储路由",
    }
}

async fn set_storage_route_enabled(
    state: &AppState,
    user: &CurrentUser,
    route_id: Uuid,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query("UPDATE storage_routes SET enabled=$2, updated_at=now() WHERE id=$1")
        .bind(route_id)
        .bind(enabled)
        .execute(&state.pool)
        .await?;
    log_admin_operation(
        state,
        user,
        if enabled {
            "storage_route.enable"
        } else {
            "storage_route.disable"
        },
        "storage_route",
        Some(route_id),
        json!({"enabled": enabled}),
    )
    .await
}

fn parse_uuid(body: &serde_json::Value, key: &str) -> AppResult<Uuid> {
    body.get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| AppError::BadRequest(format!("{key} is required")))
}

fn normalize_storage_provider_type(provider_type: &str) -> String {
    let normalized = provider_type.trim().to_ascii_lowercase();
    let normalized = normalized
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    match normalized.as_str() {
        "" => "local".to_string(),
        "r2" | "cloudflare" | "cloudflare_r2" | "cloudflare_r2_s3" => "cloudflare_r2".to_string(),
        "oci_s3" | "oracle_s3" => "oracle_s3".to_string(),
        "s3compatible" | "s3_compatible" | "generic_s3" | "custom_s3" => {
            "s3_compatible".to_string()
        }
        _ => normalized,
    }
}

fn normalize_storage_config(
    provider_type: &str,
    mut config: serde_json::Value,
) -> serde_json::Value {
    if matches!(
        provider_type,
        "cloudflare_r2" | "oracle_s3" | "s3_compatible"
    ) {
        normalize_s3_config_aliases(&mut config);
        if provider_type == "cloudflare_r2" {
            normalize_r2_config(&mut config);
        }
        normalize_url_field(&mut config, "endpoint");
        normalize_url_field(&mut config, "public_domain");
        normalize_presigned_url_ttl(&mut config);
    }
    config
}

fn normalize_r2_config(config: &mut serde_json::Value) {
    normalize_string_aliases(
        config,
        "account_id",
        &[
            "accountId",
            "accountID",
            "account",
            "cloudflare_account_id",
            "cloudflareAccountId",
            "r2_account_id",
            "r2AccountId",
        ],
    );
    normalize_string_aliases(
        config,
        "jurisdiction",
        &["location", "jurisdiction_hint", "jurisdictionHint"],
    );

    let account_id = config
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let endpoint = config
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    if !account_id.is_empty() && endpoint.is_empty() {
        if let Some((account_id, jurisdiction)) = r2_endpoint_parts(&account_id) {
            config["account_id"] = json!(account_id);
            if !jurisdiction.is_empty() {
                config["jurisdiction"] = json!(jurisdiction);
            }
            config["endpoint"] = serde_json::Value::String(String::new());
        }
    } else if account_id.is_empty() && !endpoint.is_empty() {
        if let Some((account_id, jurisdiction)) = r2_endpoint_parts(&endpoint) {
            config["account_id"] = json!(account_id);
            if !jurisdiction.is_empty() {
                config["jurisdiction"] = json!(jurisdiction);
            }
        } else if !endpoint.contains("://") && !endpoint.contains('.') && !endpoint.contains('/') {
            config["account_id"] = json!(endpoint.clone());
            config["endpoint"] = serde_json::Value::String(String::new());
        }
    }

    if config
        .get("region")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        config["region"] = json!("auto");
    }
}

fn normalize_s3_config_aliases(config: &mut serde_json::Value) {
    normalize_string_aliases(
        config,
        "endpoint",
        &[
            "endpoint_url",
            "endpointUrl",
            "service_url",
            "serviceUrl",
            "server_url",
            "serverUrl",
            "url",
        ],
    );
    normalize_string_aliases(
        config,
        "region",
        &["region_name", "regionName", "awsRegion"],
    );
    normalize_string_aliases(config, "bucket", &["bucket_name", "bucketName"]);
    normalize_string_aliases(
        config,
        "access_key_id",
        &[
            "accessKeyId",
            "access_key",
            "accessKey",
            "key_id",
            "keyId",
            "aws_access_key_id",
            "awsAccessKeyId",
            "r2_access_key_id",
            "r2AccessKeyId",
        ],
    );
    normalize_string_aliases(
        config,
        "secret_access_key",
        &[
            "secretAccessKey",
            "secret_key",
            "secretKey",
            "access_key_secret",
            "accessKeySecret",
            "aws_secret_access_key",
            "awsSecretAccessKey",
            "r2_secret_access_key",
            "r2SecretAccessKey",
        ],
    );
    normalize_string_aliases(
        config,
        "session_token",
        &[
            "sessionToken",
            "aws_session_token",
            "awsSessionToken",
            "r2_session_token",
            "r2SessionToken",
        ],
    );
    normalize_string_aliases(
        config,
        "public_domain",
        &[
            "publicDomain",
            "custom_domain",
            "customDomain",
            "domain",
            "public_url",
            "publicUrl",
        ],
    );
    normalize_string_aliases(
        config,
        "path_prefix",
        &["pathPrefix", "object_prefix", "objectPrefix", "prefix"],
    );
    normalize_value_aliases(
        config,
        "presigned_url_ttl_seconds",
        &[
            "presignedUrlTtlSeconds",
            "presigned_url_ttl",
            "presignedUrlTtl",
            "signed_url_ttl_seconds",
            "signedUrlTtlSeconds",
            "ttl_seconds",
            "ttlSeconds",
            "ttl",
        ],
    );
}

fn normalize_string_aliases(config: &mut serde_json::Value, canonical: &str, aliases: &[&str]) {
    let Some(map) = config.as_object_mut() else {
        return;
    };
    let canonical_present = map
        .get(canonical)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let mut replacement = None;
    for alias in aliases {
        if let Some(value) = map.remove(*alias)
            && replacement.is_none()
            && let Some(value) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        {
            replacement = Some(value.to_string());
        }
    }
    if !canonical_present && let Some(value) = replacement {
        map.insert(canonical.to_string(), json!(value));
    }
}

fn normalize_value_aliases(config: &mut serde_json::Value, canonical: &str, aliases: &[&str]) {
    let Some(map) = config.as_object_mut() else {
        return;
    };
    let canonical_present = map.get(canonical).is_some_and(json_value_present);
    let mut replacement = None;
    for alias in aliases {
        if let Some(value) = map.remove(*alias)
            && replacement.is_none()
            && json_value_present(&value)
        {
            replacement = Some(value);
        }
    }
    if !canonical_present && let Some(value) = replacement {
        map.insert(canonical.to_string(), value);
    }
}

fn json_value_present(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(values) => !values.is_empty(),
        serde_json::Value::Object(values) => !values.is_empty(),
        _ => true,
    }
}

fn normalize_presigned_url_ttl(config: &mut serde_json::Value) {
    let value = config
        .get("presigned_url_ttl_seconds")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
        .unwrap_or(3600)
        .clamp(60, 604800);
    config["presigned_url_ttl_seconds"] = json!(value);
}

fn normalize_url_field(config: &mut serde_json::Value, field: &str) {
    let Some(value) = config
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    config[field] = json!(normalize_url(value));
}

fn normalize_url(value: &str) -> String {
    let value = value.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{}", value.trim_start_matches('/'))
    }
}

fn r2_endpoint_parts(value: &str) -> Option<(String, String)> {
    let value = value.trim();
    let host = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value)
        .split('/')
        .next()
        .unwrap_or_default();
    let suffixes = [
        (".fedramp.r2.cloudflarestorage.com", "fedramp"),
        (".eu.r2.cloudflarestorage.com", "eu"),
        (".r2.cloudflarestorage.com", "default"),
    ];
    suffixes
        .iter()
        .find_map(|(suffix, jurisdiction)| {
            host.strip_suffix(suffix)
                .map(|account_id| (account_id, *jurisdiction))
        })
        .map(|(account_id, jurisdiction)| (account_id.trim(), jurisdiction))
        .filter(|(account_id, _)| !account_id.is_empty())
        .map(|(account_id, jurisdiction)| (account_id.to_string(), jurisdiction.to_string()))
}

fn validate_storage_config(provider_type: &str, config: &serde_json::Value) -> AppResult<()> {
    match provider_type {
        "local" => {
            validate_optional_path(config, "root", true)?;
            validate_public_prefix(config)?;
            validate_optional_path(config, "path_prefix", false)?;
        }
        "cloudflare_r2" => {
            require_config_fields(config, &["bucket", "access_key_id", "secret_access_key"])?;
            if config
                .get("account_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
                && config
                    .get("endpoint")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
            {
                return Err(AppError::BadRequest(
                    "cloudflare_r2 account_id or endpoint is required".to_string(),
                ));
            }
            validate_r2_account_id(config)?;
            validate_r2_jurisdiction(config)?;
            validate_optional_url(config, "endpoint")?;
            validate_optional_path(config, "path_prefix", false)?;
            validate_optional_url(config, "public_domain")?;
        }
        "oracle_s3" | "s3_compatible" => {
            require_config_fields(
                config,
                &[
                    "endpoint",
                    "region",
                    "bucket",
                    "access_key_id",
                    "secret_access_key",
                ],
            )?;
            validate_optional_path(config, "path_prefix", false)?;
            validate_optional_url(config, "public_domain")?;
        }
        "onedrive" => {
            require_config_fields(
                config,
                &["client_id", "tenant_id", "client_secret", "root_dir"],
            )?;
            if config
                .get("email")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
                && config
                    .get("refresh_token")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
            {
                return Err(AppError::BadRequest(
                    "onedrive email or refresh_token is required".to_string(),
                ));
            }
            validate_optional_path(config, "root_dir", true)?;
            validate_optional_path(config, "path_prefix", false)?;
        }
        "oracle_oci_native" => {
            require_config_fields(
                config,
                &[
                    "region",
                    "namespace",
                    "bucket",
                    "tenancy_ocid",
                    "user_ocid",
                    "fingerprint",
                    "private_key",
                ],
            )?;
            validate_optional_path(config, "path_prefix", false)?;
            validate_optional_url(config, "public_domain")?;
            validate_oci_native_config(config)?;
        }
        _ => {
            return Err(AppError::BadRequest(format!(
                "unsupported storage provider type {provider_type}"
            )));
        }
    }
    Ok(())
}

fn require_config_fields(config: &serde_json::Value, fields: &[&str]) -> AppResult<()> {
    for field in fields {
        let Some(value) = config.get(field).and_then(serde_json::Value::as_str) else {
            return Err(AppError::BadRequest(format!(
                "missing storage config field {field}"
            )));
        };
        let value = value.trim();
        if value.is_empty() || value == security::REDACTED_VALUE {
            return Err(AppError::BadRequest(format!(
                "missing storage config field {field}"
            )));
        }
    }
    Ok(())
}

fn validate_public_prefix(config: &serde_json::Value) -> AppResult<()> {
    let value = config
        .get("public_prefix")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("/files")
        .trim();
    if value.is_empty() || !value.starts_with('/') || value.contains("://") || value.contains("..")
    {
        return Err(AppError::BadRequest(
            "public_prefix must be an absolute path like /files".to_string(),
        ));
    }
    Ok(())
}

fn validate_optional_path(
    config: &serde_json::Value,
    field: &str,
    allow_absolute: bool,
) -> AppResult<()> {
    let Some(value) = config.get(field).and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.contains("://") || value.split('/').any(|part| part == "..") {
        return Err(AppError::BadRequest(format!(
            "{field} must not contain URL syntax or parent directory segments"
        )));
    }
    if !allow_absolute && value.starts_with('/') {
        return Err(AppError::BadRequest(format!(
            "{field} must be a relative storage path"
        )));
    }
    Ok(())
}

fn validate_optional_url(config: &serde_json::Value, field: &str) -> AppResult<()> {
    let Some(value) = config.get(field).and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() || value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "{field} must start with http:// or https://"
        )))
    }
}

fn validate_oci_native_config(config: &serde_json::Value) -> AppResult<()> {
    let tenancy_ocid = config
        .get("tenancy_ocid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    if !tenancy_ocid.starts_with("ocid1.tenancy.") {
        return Err(AppError::BadRequest(
            "OCI Tenancy OCID 必须以 ocid1.tenancy. 开头，不要填写 Bucket OCID".to_string(),
        ));
    }
    let user_ocid = config
        .get("user_ocid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    if !user_ocid.starts_with("ocid1.user.") {
        return Err(AppError::BadRequest(
            "OCI User OCID 必须以 ocid1.user. 开头".to_string(),
        ));
    }
    let private_key = config
        .get("private_key")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .replace("\\n", "\n")
        .replace('\0', "");
    if !private_key.contains("-----BEGIN ") || !private_key.contains("-----END ") {
        return Err(AppError::BadRequest(
            "OCI 私钥必须填写 PEM 正文，包含 -----BEGIN ... PRIVATE KEY----- 和 -----END ... PRIVATE KEY-----".to_string(),
        ));
    }
    Ok(())
}

fn validate_r2_account_id(config: &serde_json::Value) -> AppResult<()> {
    let Some(value) = config
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if value.contains("://") || value.contains('/') || value.chars().any(char::is_whitespace) {
        return Err(AppError::BadRequest(
            "cloudflare_r2 account_id is invalid".to_string(),
        ));
    }
    Ok(())
}

fn validate_r2_jurisdiction(config: &serde_json::Value) -> AppResult<()> {
    let value = config
        .get("jurisdiction")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_ascii_lowercase();
    if matches!(
        value.as_str(),
        "default" | "global" | "auto" | "eu" | "fedramp"
    ) {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "unsupported cloudflare_r2 jurisdiction {value}"
        )))
    }
}

fn download_file_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>()
        .trim_matches('.')
        .to_string()
}

async fn update_task_status(
    state: &AppState,
    table: &str,
    task_id: Uuid,
    status: &str,
) -> AppResult<()> {
    let sql = match table {
        "migration_tasks" => "UPDATE migration_tasks SET status=$2 WHERE id=$1",
        _ => return Err(AppError::BadRequest("invalid task table".to_string())),
    };
    sqlx::query(sql)
        .bind(task_id)
        .bind(status)
        .execute(&state.pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_validation_accepts_local_defaults() {
        assert!(
            validate_storage_config(
                "local",
                &json!({"root":"/data/storage","public_prefix":"/files","path_prefix":""})
            )
            .is_ok()
        );
    }

    #[test]
    fn storage_config_validation_rejects_bad_local_public_prefix() {
        assert!(matches!(
            validate_storage_config("local", &json!({"public_prefix":"files"})),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            validate_storage_config(
                "local",
                &json!({"public_prefix":"https://example.com/files"})
            ),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn storage_config_validation_accepts_onedrive_absolute_root_dir() {
        assert!(
            validate_storage_config(
                "onedrive",
                &json!({
                    "client_id": "client",
                    "tenant_id": "tenant",
                    "client_secret": "secret",
                    "email": "images@example.com",
                    "root_dir": "/TideImages",
                    "path_prefix": "site-a"
                })
            )
            .is_ok()
        );
    }

    #[test]
    fn storage_config_validation_accepts_r2_account_id_or_legacy_endpoint() {
        assert!(matches!(
            validate_storage_config("cloudflare_r2", &json!({"bucket":"images"})),
            Err(AppError::BadRequest(_))
        ));
        assert!(
            validate_storage_config(
                "cloudflare_r2",
                &json!({
                    "endpoint":"https://example.r2.cloudflarestorage.com",
                    "region":"auto",
                    "bucket":"images",
                    "access_key_id":"id",
                    "secret_access_key":"secret",
                    "path_prefix":"site-a",
                    "public_domain":"https://img.example.com"
                })
            )
            .is_ok()
        );
        assert!(
            validate_storage_config(
                "cloudflare_r2",
                &json!({
                    "account_id":"abc123",
                    "jurisdiction":"eu",
                    "bucket":"images",
                    "access_key_id":"id",
                    "secret_access_key":"secret",
                    "path_prefix":"site-a",
                    "public_domain":"https://img.example.com",
                    "presigned_url_ttl_seconds":3600
                })
            )
            .is_ok()
        );
    }

    #[test]
    fn storage_config_normalizes_common_r2_endpoint_inputs() {
        let config = normalize_storage_config(
            "cloudflare_r2",
            json!({
                "account_id":"abc123.r2.cloudflarestorage.com",
                "bucket":"images",
                "access_key_id":"id",
                "secret_access_key":"secret",
                "public_domain":"img.example.com"
            }),
        );
        assert_eq!(
            config.get("account_id").and_then(serde_json::Value::as_str),
            Some("abc123")
        );
        assert_eq!(
            config.get("endpoint").and_then(serde_json::Value::as_str),
            Some("")
        );
        assert_eq!(
            config
                .get("public_domain")
                .and_then(serde_json::Value::as_str),
            Some("https://img.example.com")
        );
        assert_eq!(
            config.get("region").and_then(serde_json::Value::as_str),
            Some("auto")
        );
        assert!(validate_storage_config("cloudflare_r2", &config).is_ok());

        let config = normalize_storage_config(
            "cloudflare_r2",
            json!({
                "endpoint":"abc123",
                "bucket":"images",
                "access_key_id":"id",
                "secret_access_key":"secret"
            }),
        );
        assert_eq!(
            config.get("account_id").and_then(serde_json::Value::as_str),
            Some("abc123")
        );
        assert_eq!(
            config.get("endpoint").and_then(serde_json::Value::as_str),
            Some("")
        );
        assert!(validate_storage_config("cloudflare_r2", &config).is_ok());

        let config = normalize_storage_config(
            "cloudflare_r2",
            json!({
                "endpoint":"https://abc123.eu.r2.cloudflarestorage.com",
                "bucket":"images",
                "access_key_id":"id",
                "secret_access_key":"secret"
            }),
        );
        assert_eq!(
            config.get("account_id").and_then(serde_json::Value::as_str),
            Some("abc123")
        );
        assert_eq!(
            config.get("endpoint").and_then(serde_json::Value::as_str),
            Some("https://abc123.eu.r2.cloudflarestorage.com")
        );
        assert!(validate_storage_config("cloudflare_r2", &config).is_ok());
    }

    #[test]
    fn storage_config_normalizes_r2_alias_fields() {
        assert_eq!(normalize_storage_provider_type("R2"), "cloudflare_r2");
        assert_eq!(
            normalize_storage_provider_type("s3-compatible"),
            "s3_compatible"
        );

        let config = normalize_storage_config(
            "cloudflare_r2",
            json!({
                "accountId":"abc123",
                "bucketName":"images",
                "accessKeyId":"id",
                "secretAccessKey":"secret",
                "publicDomain":"img.example.com",
                "presignedUrlTtlSeconds":"999999"
            }),
        );
        assert_eq!(
            config.get("account_id").and_then(serde_json::Value::as_str),
            Some("abc123")
        );
        assert_eq!(
            config
                .get("access_key_id")
                .and_then(serde_json::Value::as_str),
            Some("id")
        );
        assert_eq!(
            config
                .get("secret_access_key")
                .and_then(serde_json::Value::as_str),
            Some("secret")
        );
        assert!(config.get("accessKeyId").is_none());
        assert!(config.get("secretAccessKey").is_none());
        assert_eq!(
            config
                .get("public_domain")
                .and_then(serde_json::Value::as_str),
            Some("https://img.example.com")
        );
        assert_eq!(
            config
                .get("presigned_url_ttl_seconds")
                .and_then(serde_json::Value::as_i64),
            Some(604800)
        );
        assert!(validate_storage_config("cloudflare_r2", &config).is_ok());
    }

    #[test]
    fn storage_config_validation_keeps_oracle_and_generic_s3_strict() {
        assert!(matches!(
            validate_storage_config(
                "s3_compatible",
                &json!({
                    "bucket":"images",
                    "access_key_id":"id",
                    "secret_access_key":"secret"
                })
            ),
            Err(AppError::BadRequest(_))
        ));
        assert!(
            validate_storage_config(
                "oracle_s3",
                &json!({
                    "endpoint":"https://namespace.compat.objectstorage.ap-singapore-1.oraclecloud.com",
                    "region":"ap-singapore-1",
                    "bucket":"images",
                    "access_key_id":"id",
                    "secret_access_key":"secret"
                })
            )
            .is_ok()
        );
    }

    #[test]
    fn storage_config_validation_rejects_redacted_s3_secret_as_missing() {
        assert!(matches!(
            validate_storage_config(
                "oracle_s3",
                &json!({
                    "endpoint":"https://namespace.compat.objectstorage.ap-singapore-1.oraclecloud.com",
                    "region":"ap-singapore-1",
                    "bucket":"images",
                    "access_key_id":"id",
                    "secret_access_key": security::REDACTED_VALUE
                })
            ),
            Err(AppError::BadRequest(message)) if message.contains("secret_access_key")
        ));
    }
}
