use crate::app::AppState;
use crate::error::{AppError, AppResult};
use crate::services::{security, storage_registry};
use crate::storage::backup_key;
use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Postgres;
use sqlx::QueryBuilder;
use uuid::Uuid;

pub fn spawn_migration(state: AppState, task_id: Uuid) {
    tokio::spawn(async move {
        if let Err(err) = run_migration(&state, task_id).await {
            let _ = sqlx::query(
                "UPDATE migration_tasks SET status='failed', failed_count=GREATEST(failed_count,1), completed_at=now() WHERE id=$1",
            )
            .bind(task_id)
            .execute(&state.pool)
            .await;
            let _ = sqlx::query(
                "INSERT INTO system_logs (level,module,message,context_json) VALUES ('error','migration',$1,$2)",
            )
            .bind(err.to_string())
            .bind(json!({"task_id":task_id}))
            .execute(&state.pool)
            .await;
        }
    });
}

pub fn spawn_backup(state: AppState, backup_id: Uuid, target_provider_id: Option<Uuid>) {
    tokio::spawn(async move {
        if let Err(err) = create_backup_snapshot(&state, backup_id, target_provider_id).await {
            let _ = sqlx::query(
                "INSERT INTO system_logs (level,module,message,context_json) VALUES ('error','backup',$1,$2)",
            )
            .bind(err.to_string())
            .bind(json!({"backup_id":backup_id}))
            .execute(&state.pool)
            .await;
        }
    });
}

pub fn spawn_backup_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(err) = maybe_create_scheduled_backup(&state).await {
                let _ = sqlx::query(
                    "INSERT INTO system_logs (level,module,message,context_json) VALUES ('error','backup_scheduler',$1,'{}')",
                )
                .bind(err.to_string())
                .execute(&state.pool)
                .await;
            }
        }
    });
}

pub struct MigrationCreateResult {
    pub task_id: Uuid,
    pub total_count: i32,
}

#[derive(Default)]
struct MigrationFilters {
    user_id: Option<Uuid>,
    user_group_code: Option<String>,
    tag: Option<String>,
    status: Option<String>,
    created_after: Option<DateTime<Utc>>,
    created_before: Option<DateTime<Utc>>,
    object_types: Vec<String>,
}

pub async fn create_migration_task(
    state: &AppState,
    source_storage_provider_id: Uuid,
    target_storage_provider_id: Uuid,
    migration_mode: &str,
    filter_json: serde_json::Value,
    created_by: Uuid,
) -> AppResult<MigrationCreateResult> {
    if source_storage_provider_id == target_storage_provider_id {
        return Err(AppError::BadRequest(
            "source and target storage providers must differ".to_string(),
        ));
    }
    if !matches!(migration_mode, "copy" | "move" | "backup") {
        return Err(AppError::BadRequest("invalid migration_mode".to_string()));
    }
    storage_registry::provider_by_id(state, source_storage_provider_id).await?;
    storage_registry::provider_by_id(state, target_storage_provider_id).await?;
    let filters = parse_migration_filters(&filter_json)?;
    let source_objects =
        select_migration_objects(state, source_storage_provider_id, &filters).await?;
    let total_count = i32::try_from(source_objects.len())
        .map_err(|_| AppError::BadRequest("too many migration objects".to_string()))?;
    let mut tx = state.pool.begin().await?;
    let task_id: Uuid = sqlx::query_scalar("INSERT INTO migration_tasks (source_storage_provider_id,target_storage_provider_id,migration_mode,filter_json,total_count,status,created_by) VALUES ($1,$2,$3,$4,$5,'pending',$6) RETURNING id")
        .bind(source_storage_provider_id)
        .bind(target_storage_provider_id)
        .bind(migration_mode)
        .bind(filter_json)
        .bind(total_count)
        .bind(created_by)
        .fetch_one(&mut *tx)
        .await?;
    for object in source_objects {
        sqlx::query("INSERT INTO migration_task_items (migration_task_id,storage_object_id,source_object_key,target_object_key,status) VALUES ($1,$2,$3,$4,'pending')")
            .bind(task_id)
            .bind(object.id)
            .bind(&object.object_key)
            .bind(target_object_key(migration_mode, &object.object_key))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(MigrationCreateResult {
        task_id,
        total_count,
    })
}

async fn maybe_create_scheduled_backup(state: &AppState) -> AppResult<()> {
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='backup'")
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| json!({}));
    if !settings
        .get("scheduled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    if scheduled_backup_running(state).await? {
        return Ok(());
    }
    let interval_hours = scheduled_backup_interval_hours(&settings);
    let recent: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM backup_tasks WHERE backup_type IN ('scheduled','incremental') AND created_at >= now() - ($1::text || ' hours')::interval ORDER BY created_at DESC LIMIT 1",
    )
    .bind(interval_hours.to_string())
    .fetch_optional(&state.pool)
    .await?;
    if recent.is_some() {
        return Ok(());
    }
    let target = settings
        .get("target_storage_provider_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    let backup_type = scheduled_backup_type(&settings);
    let include_files = settings
        .get("include_files")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let include_logs = settings
        .get("include_logs")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let id: Uuid = sqlx::query_scalar("INSERT INTO backup_tasks (backup_type,target_storage_provider_id,status,include_files,include_logs,started_at) VALUES ($1,$2,'running',$3,$4,now()) RETURNING id")
        .bind(backup_type)
        .bind(target)
        .bind(include_files)
        .bind(include_logs)
        .fetch_one(&state.pool)
        .await?;
    spawn_backup(state.clone(), id, target);
    Ok(())
}

async fn scheduled_backup_running(state: &AppState) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM backup_tasks WHERE backup_type IN ('scheduled','incremental') AND status IN ('pending','running'))",
    )
    .fetch_one(&state.pool)
    .await?)
}

fn scheduled_backup_interval_hours(settings: &serde_json::Value) -> i64 {
    settings
        .get("interval_hours")
        .or_else(|| settings.get("schedule_hours"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(24)
        .clamp(1, 24 * 30)
}

fn scheduled_backup_type(settings: &serde_json::Value) -> &'static str {
    if settings
        .get("incremental")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "incremental"
    } else {
        "scheduled"
    }
}

fn parse_migration_filters(filter_json: &serde_json::Value) -> AppResult<MigrationFilters> {
    let mut filters = MigrationFilters {
        user_id: parse_optional_uuid(filter_json, "user_id")?,
        user_group_code: optional_string(filter_json, "user_group_code")
            .or_else(|| optional_string(filter_json, "group_code")),
        tag: optional_string(filter_json, "tag")
            .or_else(|| optional_string(filter_json, "tag_slug")),
        status: optional_string(filter_json, "status"),
        created_after: parse_optional_datetime(filter_json, "created_after")?
            .or(parse_optional_datetime(filter_json, "uploaded_after")?),
        created_before: parse_optional_datetime(filter_json, "created_before")?
            .or(parse_optional_datetime(filter_json, "uploaded_before")?),
        object_types: parse_object_types(filter_json)?,
    };
    if filters.object_types.is_empty() {
        filters.object_types = vec!["original".to_string(), "preview".to_string()];
    }
    Ok(filters)
}

async fn select_migration_objects(
    state: &AppState,
    source_storage_provider_id: Uuid,
    filters: &MigrationFilters,
) -> AppResult<Vec<MigrationObjectRow>> {
    let mut query = QueryBuilder::<Postgres>::new(
        "SELECT DISTINCT so.id, so.object_key FROM storage_objects so \
         JOIN file_objects fo ON fo.id=so.file_object_id \
         JOIN images i ON i.file_object_id=fo.id \
         JOIN users u ON u.id=i.user_id",
    );
    if filters.tag.is_some() {
        query.push(" JOIN image_tags it ON it.image_id=i.id JOIN tags tg ON tg.id=it.tag_id");
    }
    query.push(" WHERE so.storage_provider_id=");
    query.push_bind(source_storage_provider_id);
    query.push(" AND so.status='active'");
    if !filters.object_types.is_empty() {
        query.push(" AND so.object_type = ANY(");
        query.push_bind(filters.object_types.clone());
        query.push(")");
    }
    if let Some(user_id) = filters.user_id {
        query.push(" AND i.user_id=");
        query.push_bind(user_id);
    }
    if let Some(group_code) = &filters.user_group_code {
        query.push(" AND CASE u.role WHEN 'guest_account' THEN 'guest' WHEN 'user' THEN 'normal' WHEN 'super_admin' THEN 'admin' ELSE u.role END = ");
        query.push_bind(group_code);
    }
    if let Some(tag) = &filters.tag {
        query.push(" AND (tg.name=");
        query.push_bind(tag);
        query.push(" OR tg.slug=");
        query.push_bind(tag);
        query.push(")");
    }
    if let Some(status) = &filters.status {
        query.push(" AND i.status=");
        query.push_bind(status);
    }
    if let Some(created_after) = filters.created_after {
        query.push(" AND i.created_at >= ");
        query.push_bind(created_after);
    }
    if let Some(created_before) = filters.created_before {
        query.push(" AND i.created_at <= ");
        query.push_bind(created_before);
    }
    query.push(" ORDER BY so.object_key");
    Ok(query
        .build_query_as::<MigrationObjectRow>()
        .fetch_all(&state.pool)
        .await?)
}

fn target_object_key(migration_mode: &str, object_key: &str) -> String {
    if migration_mode == "backup" {
        format!("migration-backups/{}", object_key.trim_start_matches('/'))
    } else {
        object_key.to_string()
    }
}

fn optional_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn parse_optional_uuid(value: &serde_json::Value, key: &str) -> AppResult<Option<Uuid>> {
    optional_string(value, key)
        .map(|raw| {
            Uuid::parse_str(&raw)
                .map_err(|err| AppError::BadRequest(format!("invalid {key}: {err}")))
        })
        .transpose()
}

fn parse_optional_datetime(
    value: &serde_json::Value,
    key: &str,
) -> AppResult<Option<DateTime<Utc>>> {
    optional_string(value, key)
        .map(|raw| {
            DateTime::parse_from_rfc3339(&raw)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|err| AppError::BadRequest(format!("invalid {key}: {err}")))
        })
        .transpose()
}

fn parse_object_types(value: &serde_json::Value) -> AppResult<Vec<String>> {
    if value
        .get("only_original")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(vec!["original".to_string()]);
    }
    if value
        .get("only_preview")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(vec!["preview".to_string()]);
    }
    let mut values = Vec::new();
    if let Some(object_type) = optional_string(value, "object_type") {
        values.push(object_type);
    }
    if let Some(items) = value
        .get("object_types")
        .and_then(serde_json::Value::as_array)
    {
        values.extend(
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
        );
    }
    if values.is_empty() {
        let include_original = value
            .get("include_original")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let include_preview = value
            .get("include_preview")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if include_original {
            values.push("original".to_string());
        }
        if include_preview {
            values.push("preview".to_string());
        }
    }
    values.sort();
    values.dedup();
    for object_type in &values {
        if !matches!(object_type.as_str(), "original" | "preview" | "avatar") {
            return Err(AppError::BadRequest(format!(
                "invalid migration object_type {object_type}"
            )));
        }
    }
    Ok(values)
}

pub async fn run_migration(state: &AppState, task_id: Uuid) -> AppResult<()> {
    let task: MigrationTaskRow = sqlx::query_as("SELECT id,source_storage_provider_id,target_storage_provider_id,migration_mode,status FROM migration_tasks WHERE id=$1")
        .bind(task_id)
        .fetch_one(&state.pool)
        .await?;
    if task.status == "cancelled" {
        return Ok(());
    }
    sqlx::query("UPDATE migration_tasks SET status='running', started_at=COALESCE(started_at,now()) WHERE id=$1")
        .bind(task.id)
        .execute(&state.pool)
        .await?;
    let source_row =
        storage_registry::provider_by_id(state, task.source_storage_provider_id).await?;
    let target_row =
        storage_registry::provider_by_id(state, task.target_storage_provider_id).await?;
    let source = storage_registry::build_provider(state, &source_row).await?;
    let target = storage_registry::build_provider(state, &target_row).await?;
    let items = sqlx::query_as::<_, MigrationItemRow>(
        "SELECT id,storage_object_id,source_object_key,target_object_key FROM migration_task_items WHERE migration_task_id=$1 AND status IN ('pending','failed') ORDER BY created_at",
    )
    .bind(task.id)
    .fetch_all(&state.pool)
    .await?;
    for item in items {
        let paused_or_cancelled: Option<String> =
            sqlx::query_scalar("SELECT status FROM migration_tasks WHERE id=$1")
                .bind(task.id)
                .fetch_optional(&state.pool)
                .await?;
        if matches!(paused_or_cancelled.as_deref(), Some("paused" | "cancelled")) {
            return Ok(());
        }
        match migrate_item(state, &task, &item, source.as_ref(), target.as_ref()).await {
            Ok(()) => {}
            Err(err) => {
                sqlx::query("UPDATE migration_task_items SET status='failed', retry_count=retry_count+1, error_message=$2, updated_at=now() WHERE id=$1")
                    .bind(item.id)
                    .bind(err.to_string())
                    .execute(&state.pool)
                    .await?;
            }
        }
    }
    refresh_migration_progress(state, task.id).await?;
    Ok(())
}

async fn refresh_migration_progress(state: &AppState, task_id: Uuid) -> AppResult<()> {
    let (success, failed, pending): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE status='completed'), COUNT(*) FILTER (WHERE status='failed'), COUNT(*) FILTER (WHERE status IN ('pending','running')) FROM migration_task_items WHERE migration_task_id=$1",
    )
    .bind(task_id)
    .fetch_one(&state.pool)
    .await?;
    let status = if failed > 0 { "failed" } else { "completed" };
    let status = if pending > 0 { "running" } else { status };
    sqlx::query("UPDATE migration_tasks SET success_count=$2, failed_count=$3, status=$4, completed_at=CASE WHEN $4='completed' THEN now() ELSE completed_at END WHERE id=$1")
        .bind(task_id)
        .bind(success as i32)
        .bind(failed as i32)
        .bind(status)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn create_backup_snapshot(
    state: &AppState,
    backup_id: Uuid,
    target_provider_id: Option<Uuid>,
) -> AppResult<()> {
    match create_backup_snapshot_inner(state, backup_id, target_provider_id).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = sqlx::query(
                "UPDATE backup_tasks SET status='failed', error_message=$2, completed_at=now() WHERE id=$1",
            )
            .bind(backup_id)
            .bind(err.to_string())
            .execute(&state.pool)
            .await;
            Err(err)
        }
    }
}

async fn create_backup_snapshot_inner(
    state: &AppState,
    backup_id: Uuid,
    target_provider_id: Option<Uuid>,
) -> AppResult<()> {
    let (include_files, include_logs): (bool, bool) =
        sqlx::query_as("SELECT include_files,include_logs FROM backup_tasks WHERE id=$1")
            .bind(backup_id)
            .fetch_one(&state.pool)
            .await?;
    let provider_row = if let Some(id) = target_provider_id {
        storage_registry::provider_by_id(state, id).await?
    } else {
        storage_registry::default_provider(state).await?
    };
    let provider = storage_registry::build_provider(state, &provider_row).await?;
    let snapshot = json!({
        "site_settings": json_rows(state, "site_settings").await?,
        "theme_settings": json_rows(state, "theme_settings").await?,
        "smtp_settings": json_rows(state, "smtp_settings").await?,
        "users": json_rows(state, "users").await?,
        "user_profiles": json_rows(state, "user_profiles").await?,
        "user_groups": json_rows(state, "user_groups").await?,
        "quota_rules": json_rows(state, "quota_rules").await?,
        "quota_usage": json_rows(state, "quota_usage").await?,
        "user_quota_overrides": json_rows(state, "user_quota_overrides").await?,
        "api_tokens": json_rows(state, "api_tokens").await?,
        "file_objects": json_rows(state, "file_objects").await?,
        "images": json_rows(state, "images").await?,
        "storage_providers": json_rows(state, "storage_providers").await?,
        "storage_routes": json_rows(state, "storage_routes").await?,
        "storage_objects": json_rows(state, "storage_objects").await?,
        "tags": json_rows(state, "tags").await?,
        "image_tags": json_rows(state, "image_tags").await?,
        "audit_tasks": json_rows(state, "audit_tasks").await?,
        "audit_results": json_rows(state, "audit_results").await?,
        "system_logs": if include_logs { json_rows(state, "system_logs").await? } else { Vec::new() },
        "admin_operation_logs": if include_logs { json_rows(state, "admin_operation_logs").await? } else { Vec::new() },
    });
    let plain = serde_json::to_vec(&snapshot).map_err(|err| AppError::External(err.to_string()))?;
    let encrypted = security::encrypt_value(
        &state.config,
        std::str::from_utf8(&plain).map_err(|err| AppError::External(err.to_string()))?,
    )?;
    let bytes = encrypted.into_bytes();
    let object_key = backup_key(&backup_id.to_string()).replace(".tar.zst", ".json.enc");
    let stored = provider
        .put_object(&object_key, &bytes, "application/octet-stream")
        .await?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    sqlx::query("INSERT INTO backup_files (backup_task_id,storage_provider_id,object_key,file_name,size,sha256,encrypted) VALUES ($1,$2,$3,$4,$5,$6,true)")
        .bind(backup_id)
        .bind(provider_row.id)
        .bind(stored.object_key)
        .bind(format!("{backup_id}.json.enc"))
        .bind(stored.size)
        .bind(sha256)
        .execute(&state.pool)
        .await?;
    let file_backup_size = if include_files {
        backup_image_files(state, backup_id, &provider_row, provider.as_ref()).await?
    } else {
        0
    };
    sqlx::query("UPDATE backup_tasks SET status='completed', backup_size=$2, completed_at=now() WHERE id=$1")
        .bind(backup_id)
        .bind(bytes.len() as i64 + file_backup_size)
        .execute(&state.pool)
        .await?;
    Ok(())
}

async fn backup_image_files(
    state: &AppState,
    backup_id: Uuid,
    target_provider_row: &crate::models::StorageProviderRow,
    target: &dyn crate::storage::StorageProvider,
) -> AppResult<i64> {
    let rows = sqlx::query_as::<_, BackupSourceObjectRow>(
        "SELECT so.file_object_id,so.storage_provider_id,so.object_key,so.object_type,fo.mime_type \
         FROM storage_objects so JOIN file_objects fo ON fo.id=so.file_object_id \
         WHERE so.status='active' AND so.object_type IN ('original','preview','avatar') \
         ORDER BY so.created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut total_size = 0_i64;
    for row in rows {
        let source_row = storage_registry::provider_by_id(state, row.storage_provider_id).await?;
        let source = storage_registry::build_provider(state, &source_row).await?;
        let bytes = source.get_object(&row.object_key).await?;
        let object_key = backup_file_key(&backup_id, &row.object_type, &row.object_key);
        let stored = target
            .put_object(&object_key, &bytes, &row.mime_type)
            .await?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        total_size += stored.size;
        sqlx::query("INSERT INTO backup_files (backup_task_id,storage_provider_id,object_key,file_name,size,sha256,encrypted) VALUES ($1,$2,$3,$4,$5,$6,false)")
            .bind(backup_id)
            .bind(target_provider_row.id)
            .bind(&stored.object_key)
            .bind(backup_file_name(&row.object_type, &row.object_key))
            .bind(stored.size)
            .bind(sha256)
            .execute(&state.pool)
            .await?;
        sqlx::query("INSERT INTO storage_objects (file_object_id,storage_provider_id,object_type,object_key,public_url,etag,size,status) VALUES ($1,$2,'backup',$3,$4,$5,$6,'active') ON CONFLICT (storage_provider_id,object_key) DO UPDATE SET public_url=EXCLUDED.public_url, etag=EXCLUDED.etag, size=EXCLUDED.size, status='active', updated_at=now()")
            .bind(row.file_object_id)
            .bind(target_provider_row.id)
            .bind(stored.object_key)
            .bind(stored.public_url)
            .bind(stored.etag)
            .bind(stored.size)
            .execute(&state.pool)
            .await?;
    }
    Ok(total_size)
}

fn backup_file_key(backup_id: &Uuid, object_type: &str, object_key: &str) -> String {
    format!(
        "backups/{backup_id}/files/{}/{}",
        object_type.trim_matches('/'),
        object_key.trim_start_matches('/')
    )
}

fn backup_file_name(object_type: &str, object_key: &str) -> String {
    format!(
        "{}-{}",
        object_type,
        object_key.trim_matches('/').replace(['/', '\\'], "_")
    )
}

pub async fn restore_settings_from_backup(
    state: &AppState,
    backup_id: Uuid,
    actor_id: Uuid,
    options: serde_json::Value,
) -> AppResult<Uuid> {
    let snapshot_id: Uuid = sqlx::query_scalar("INSERT INTO backup_tasks (backup_type,status,include_files,include_logs,created_by,started_at) VALUES ('manual','running',false,true,$1,now()) RETURNING id")
        .bind(actor_id)
        .fetch_one(&state.pool)
        .await?;
    create_backup_snapshot(state, snapshot_id, None).await?;
    let restore_id: Uuid = sqlx::query_scalar("INSERT INTO restore_tasks (backup_task_id,status,restore_options_json,created_by,started_at) VALUES ($1,'running',$2,$3,now()) RETURNING id")
        .bind(backup_id)
        .bind(&options)
        .bind(actor_id)
        .fetch_one(&state.pool)
        .await?;
    let snapshot = load_backup_snapshot(state, backup_id).await?;
    let restore_options = RestoreOptions::from_value(&options);
    if restore_options.site_settings {
        restore_key_value_table(state, "site_settings", &snapshot).await?;
    }
    if restore_options.theme_settings {
        restore_key_value_table(state, "theme_settings", &snapshot).await?;
    }
    if restore_options.smtp_settings {
        restore_smtp_settings(state, &snapshot).await?;
    }
    if restore_options.storage_providers {
        restore_storage_providers(state, &snapshot).await?;
    }
    if restore_options.metadata {
        restore_metadata_tables(state, &snapshot).await?;
    }
    if restore_options.audit {
        restore_audit_tables(state, &snapshot).await?;
    }
    if restore_options.logs {
        restore_log_tables(state, &snapshot).await?;
    }
    sqlx::query("UPDATE restore_tasks SET status='completed', completed_at=now() WHERE id=$1")
        .bind(restore_id)
        .execute(&state.pool)
        .await?;
    Ok(restore_id)
}

pub async fn load_backup_file(state: &AppState, backup_id: Uuid) -> AppResult<BackupFileContent> {
    let file: BackupFileRow = sqlx::query_as(
        "SELECT storage_provider_id,object_key,file_name,encrypted FROM backup_files WHERE backup_task_id=$1 AND encrypted=true ORDER BY created_at DESC LIMIT 1",
    )
    .bind(backup_id)
    .fetch_one(&state.pool)
    .await?;
    let storage_provider_id = file
        .storage_provider_id
        .ok_or_else(|| AppError::BadRequest("backup file storage provider missing".to_string()))?;
    let provider_row = storage_registry::provider_by_id(state, storage_provider_id).await?;
    let provider = storage_registry::build_provider(state, &provider_row).await?;
    let bytes = provider.get_object(&file.object_key).await?;
    Ok(BackupFileContent {
        file_name: file.file_name,
        encrypted: file.encrypted,
        bytes,
    })
}

async fn load_backup_snapshot(state: &AppState, backup_id: Uuid) -> AppResult<serde_json::Value> {
    let file = load_backup_file(state, backup_id).await?;
    let bytes = if file.encrypted {
        let encrypted =
            String::from_utf8(file.bytes).map_err(|err| AppError::External(err.to_string()))?;
        security::decrypt_value(&state.config, &encrypted)?.into_bytes()
    } else {
        file.bytes
    };
    serde_json::from_slice(&bytes).map_err(|err| AppError::External(err.to_string()))
}

struct RestoreOptions {
    site_settings: bool,
    theme_settings: bool,
    smtp_settings: bool,
    storage_providers: bool,
    metadata: bool,
    audit: bool,
    logs: bool,
}

impl RestoreOptions {
    fn from_value(value: &serde_json::Value) -> Self {
        let settings = value
            .get("settings")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let full_database = value
            .get("database")
            .or_else(|| value.get("full_database"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let metadata = full_database
            || value
                .get("metadata")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        let audit = full_database
            || value
                .get("audit")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        let logs = full_database
            || value
                .get("logs")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        Self {
            site_settings: settings && option_enabled(value, "site_settings", true),
            theme_settings: settings && option_enabled(value, "theme_settings", true),
            smtp_settings: full_database
                || (settings && option_enabled(value, "smtp_settings", false)),
            storage_providers: full_database
                || (settings && option_enabled(value, "storage_providers", false)),
            metadata,
            audit,
            logs,
        }
    }
}

fn option_enabled(value: &serde_json::Value, key: &str, default: bool) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
}

async fn restore_key_value_table(
    state: &AppState,
    table: &str,
    snapshot: &serde_json::Value,
) -> AppResult<()> {
    let rows = snapshot
        .get(table)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::BadRequest(format!("backup missing {table}")))?;
    for row in rows {
        let key = row
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AppError::BadRequest(format!("{table} row key missing")))?;
        let value = row
            .get("value_json")
            .cloned()
            .ok_or_else(|| AppError::BadRequest(format!("{table} value missing")))?;
        let sql = match table {
            "site_settings" => {
                "INSERT INTO site_settings (key,value_json) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value_json=$2, updated_at=now()"
            }
            "theme_settings" => {
                "INSERT INTO theme_settings (key,value_json) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value_json=$2, updated_at=now()"
            }
            _ => return Err(AppError::BadRequest("invalid restore table".to_string())),
        };
        sqlx::query(sql)
            .bind(key)
            .bind(value)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

async fn restore_smtp_settings(state: &AppState, snapshot: &serde_json::Value) -> AppResult<()> {
    let rows = snapshot_array(snapshot, "smtp_settings")?;
    for row in rows {
        let id = row_uuid(row, "id")?;
        let name = row_string(row, "name")?;
        let host = row_string(row, "host")?;
        let port = row_i32(row, "port")?;
        let username = row_string(row, "username")?;
        let password = row
            .get("password_encrypted")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let from_email = row_string(row, "from_email")?;
        let from_name = row_string(row, "from_name")?;
        let enabled = row_bool(row, "enabled")?;
        sqlx::query("INSERT INTO smtp_settings (id,name,host,port,username,password_encrypted,from_email,from_name,enabled) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (id) DO UPDATE SET name=$2, host=$3, port=$4, username=$5, password_encrypted=$6, from_email=$7, from_name=$8, enabled=$9, updated_at=now()")
            .bind(id)
            .bind(name)
            .bind(host)
            .bind(port)
            .bind(username)
            .bind(password)
            .bind(from_email)
            .bind(from_name)
            .bind(enabled)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

async fn restore_storage_providers(
    state: &AppState,
    snapshot: &serde_json::Value,
) -> AppResult<()> {
    let rows = snapshot_array(snapshot, "storage_providers")?;
    for row in rows {
        let id = row_uuid(row, "id")?;
        let name = row_string(row, "name")?;
        let provider_type = row_string(row, "provider_type")?;
        let config_json = row.get("config_json").cloned().unwrap_or_else(|| json!({}));
        let is_default = row_bool(row, "is_default")?;
        let enabled = row_bool(row, "enabled")?;
        let priority = row_i32(row, "priority")?;
        sqlx::query("INSERT INTO storage_providers (id,name,provider_type,config_json,is_default,enabled,priority) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO UPDATE SET name=$2, provider_type=$3, config_json=$4, is_default=$5, enabled=$6, priority=$7, updated_at=now()")
            .bind(id)
            .bind(name)
            .bind(provider_type)
            .bind(config_json)
            .bind(is_default)
            .bind(enabled)
            .bind(priority)
            .execute(&state.pool)
            .await?;
    }
    if let Some(routes) = snapshot
        .get("storage_routes")
        .and_then(serde_json::Value::as_array)
    {
        for row in routes {
            sqlx::query("INSERT INTO storage_routes SELECT * FROM jsonb_populate_record(NULL::storage_routes,$1) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, scope_type=EXCLUDED.scope_type, scope_value=EXCLUDED.scope_value, storage_provider_id=EXCLUDED.storage_provider_id, enabled=EXCLUDED.enabled, priority=EXCLUDED.priority, note=EXCLUDED.note, updated_at=now()")
                .bind(row.clone())
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(())
}

async fn restore_metadata_tables(state: &AppState, snapshot: &serde_json::Value) -> AppResult<()> {
    restore_json_record_table(state, "user_groups", snapshot).await?;
    restore_json_record_table(state, "users", snapshot).await?;
    restore_json_record_table(state, "user_profiles", snapshot).await?;
    restore_json_record_table(state, "quota_rules", snapshot).await?;
    restore_json_record_table(state, "quota_usage", snapshot).await?;
    restore_json_record_table(state, "user_quota_overrides", snapshot).await?;
    restore_json_record_table(state, "api_tokens", snapshot).await?;
    restore_json_record_table(state, "file_objects", snapshot).await?;
    restore_json_record_table(state, "images", snapshot).await?;
    restore_json_record_table(state, "storage_objects", snapshot).await?;
    restore_json_record_table(state, "tags", snapshot).await?;
    restore_image_tags(state, snapshot).await?;
    Ok(())
}

async fn restore_audit_tables(state: &AppState, snapshot: &serde_json::Value) -> AppResult<()> {
    restore_json_record_table(state, "audit_tasks", snapshot).await?;
    restore_json_record_table(state, "audit_results", snapshot).await?;
    Ok(())
}

async fn restore_log_tables(state: &AppState, snapshot: &serde_json::Value) -> AppResult<()> {
    restore_json_record_table(state, "system_logs", snapshot).await?;
    restore_json_record_table(state, "admin_operation_logs", snapshot).await?;
    Ok(())
}

async fn restore_json_record_table(
    state: &AppState,
    table: &str,
    snapshot: &serde_json::Value,
) -> AppResult<()> {
    let rows = snapshot_array(snapshot, table)?;
    let sql = match table {
        "user_groups" => {
            "INSERT INTO user_groups SELECT * FROM jsonb_populate_record(NULL::user_groups,$1) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, code=EXCLUDED.code, description=EXCLUDED.description, is_default=EXCLUDED.is_default, updated_at=now()"
        }
        "users" => {
            "INSERT INTO users SELECT * FROM jsonb_populate_record(NULL::users,$1) ON CONFLICT (id) DO UPDATE SET email=EXCLUDED.email, username=EXCLUDED.username, password_hash=EXCLUDED.password_hash, avatar_url=EXCLUDED.avatar_url, role=EXCLUDED.role, status=EXCLUDED.status, login_failed_count=EXCLUDED.login_failed_count, locked_until=EXCLUDED.locked_until, updated_at=now()"
        }
        "user_profiles" => {
            "INSERT INTO user_profiles SELECT * FROM jsonb_populate_record(NULL::user_profiles,$1) ON CONFLICT (user_id) DO UPDATE SET display_name=EXCLUDED.display_name, bio=EXCLUDED.bio, avatar_file_object_id=EXCLUDED.avatar_file_object_id, settings_json=EXCLUDED.settings_json, updated_at=now()"
        }
        "quota_rules" => {
            "INSERT INTO quota_rules SELECT * FROM jsonb_populate_record(NULL::quota_rules,$1) ON CONFLICT (group_id) DO UPDATE SET daily_upload_count=EXCLUDED.daily_upload_count, daily_upload_bytes=EXCLUDED.daily_upload_bytes, max_file_size=EXCLUDED.max_file_size, total_storage_bytes=EXCLUDED.total_storage_bytes, daily_api_calls=EXCLUDED.daily_api_calls, daily_random_calls=EXCLUDED.daily_random_calls, require_review=EXCLUDED.require_review, require_captcha=EXCLUDED.require_captcha, allow_batch_upload=EXCLUDED.allow_batch_upload, allow_tag_create=EXCLUDED.allow_tag_create, default_storage_provider_id=EXCLUDED.default_storage_provider_id, updated_at=now()"
        }
        "quota_usage" => {
            "INSERT INTO quota_usage SELECT * FROM jsonb_populate_record(NULL::quota_usage,$1) ON CONFLICT (user_id,date) DO UPDATE SET uploaded_count=EXCLUDED.uploaded_count, uploaded_bytes=EXCLUDED.uploaded_bytes, api_calls=EXCLUDED.api_calls, random_calls=EXCLUDED.random_calls, updated_at=now()"
        }
        "user_quota_overrides" => {
            "INSERT INTO user_quota_overrides SELECT * FROM jsonb_populate_record(NULL::user_quota_overrides,$1) ON CONFLICT (id) DO UPDATE SET user_id=EXCLUDED.user_id, quota_json=EXCLUDED.quota_json, reason=EXCLUDED.reason, created_by=EXCLUDED.created_by, updated_at=now()"
        }
        "api_tokens" => {
            "INSERT INTO api_tokens SELECT * FROM jsonb_populate_record(NULL::api_tokens,$1) ON CONFLICT (id) DO UPDATE SET user_id=EXCLUDED.user_id, name=EXCLUDED.name, token_hash=EXCLUDED.token_hash, scopes_json=EXCLUDED.scopes_json, expires_at=EXCLUDED.expires_at, last_used_at=EXCLUDED.last_used_at, revoked_at=EXCLUDED.revoked_at"
        }
        "file_objects" => {
            "INSERT INTO file_objects SELECT * FROM jsonb_populate_record(NULL::file_objects,$1) ON CONFLICT (id) DO UPDATE SET size=EXCLUDED.size, mime_type=EXCLUDED.mime_type, width=EXCLUDED.width, height=EXCLUDED.height, orientation=EXCLUDED.orientation, aspect_ratio=EXCLUDED.aspect_ratio, ref_count=EXCLUDED.ref_count, updated_at=now()"
        }
        "images" => {
            "INSERT INTO images SELECT * FROM jsonb_populate_record(NULL::images,$1) ON CONFLICT (id) DO UPDATE SET user_id=EXCLUDED.user_id, file_object_id=EXCLUDED.file_object_id, original_name=EXCLUDED.original_name, title=EXCLUDED.title, description=EXCLUDED.description, status=EXCLUDED.status, visibility=EXCLUDED.visibility, is_guest_upload=EXCLUDED.is_guest_upload, guest_ip=EXCLUDED.guest_ip, guest_user_agent=EXCLUDED.guest_user_agent, guest_fingerprint=EXCLUDED.guest_fingerprint, updated_at=now(), trashed_at=EXCLUDED.trashed_at, deleted_at=EXCLUDED.deleted_at, delete_reason=EXCLUDED.delete_reason, deleted_by=EXCLUDED.deleted_by, restore_until=EXCLUDED.restore_until"
        }
        "storage_objects" => {
            "INSERT INTO storage_objects SELECT * FROM jsonb_populate_record(NULL::storage_objects,$1) ON CONFLICT (storage_provider_id,object_key) DO UPDATE SET file_object_id=EXCLUDED.file_object_id, object_type=EXCLUDED.object_type, public_url=EXCLUDED.public_url, provider_file_id=EXCLUDED.provider_file_id, etag=EXCLUDED.etag, size=EXCLUDED.size, status=EXCLUDED.status, updated_at=now()"
        }
        "tags" => {
            "INSERT INTO tags SELECT * FROM jsonb_populate_record(NULL::tags,$1) ON CONFLICT (slug) DO UPDATE SET name=EXCLUDED.name, created_by=EXCLUDED.created_by, status=EXCLUDED.status, usage_count=EXCLUDED.usage_count, updated_at=now()"
        }
        "audit_tasks" => {
            "INSERT INTO audit_tasks SELECT * FROM jsonb_populate_record(NULL::audit_tasks,$1) ON CONFLICT (id) DO UPDATE SET image_id=EXCLUDED.image_id, audit_type=EXCLUDED.audit_type, provider=EXCLUDED.provider, status=EXCLUDED.status, require_review=EXCLUDED.require_review, retry_count=EXCLUDED.retry_count, error_message=EXCLUDED.error_message, started_at=EXCLUDED.started_at, finished_at=EXCLUDED.finished_at"
        }
        "audit_results" => {
            "INSERT INTO audit_results SELECT * FROM jsonb_populate_record(NULL::audit_results,$1) ON CONFLICT (id) DO UPDATE SET audit_task_id=EXCLUDED.audit_task_id, image_id=EXCLUDED.image_id, result=EXCLUDED.result, risk_level=EXCLUDED.risk_level, reason=EXCLUDED.reason, labels_json=EXCLUDED.labels_json, categories_json=EXCLUDED.categories_json, ocr_text=EXCLUDED.ocr_text, provider=EXCLUDED.provider, model=EXCLUDED.model, request_payload=EXCLUDED.request_payload, response_payload=EXCLUDED.response_payload, duration_ms=EXCLUDED.duration_ms"
        }
        "system_logs" => {
            "INSERT INTO system_logs SELECT * FROM jsonb_populate_record(NULL::system_logs,$1) ON CONFLICT (id) DO NOTHING"
        }
        "admin_operation_logs" => {
            "INSERT INTO admin_operation_logs SELECT * FROM jsonb_populate_record(NULL::admin_operation_logs,$1) ON CONFLICT (id) DO NOTHING"
        }
        _ => return Err(AppError::BadRequest("invalid restore table".to_string())),
    };
    for row in rows {
        sqlx::query(sql)
            .bind(row.clone())
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

async fn restore_image_tags(state: &AppState, snapshot: &serde_json::Value) -> AppResult<()> {
    let rows = snapshot_array(snapshot, "image_tags")?;
    for row in rows {
        let image_id = row_uuid(row, "image_id")?;
        let tag_id = row_uuid(row, "tag_id")?;
        let created_by = row
            .get("created_by")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok());
        sqlx::query("INSERT INTO image_tags (image_id,tag_id,created_by) VALUES ($1,$2,$3) ON CONFLICT (image_id,tag_id) DO UPDATE SET created_by=EXCLUDED.created_by")
            .bind(image_id)
            .bind(tag_id)
            .bind(created_by)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

fn snapshot_array<'a>(
    snapshot: &'a serde_json::Value,
    table: &str,
) -> AppResult<&'a Vec<serde_json::Value>> {
    snapshot
        .get(table)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::BadRequest(format!("backup missing {table}")))
}

fn row_uuid(row: &serde_json::Value, key: &str) -> AppResult<Uuid> {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AppError::BadRequest(format!("{key} missing")))?
        .parse()
        .map_err(|err| AppError::BadRequest(format!("invalid {key}: {err}")))
}

fn row_string(row: &serde_json::Value, key: &str) -> AppResult<String> {
    Ok(row
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string())
}

fn row_i32(row: &serde_json::Value, key: &str) -> AppResult<i32> {
    row.get(key)
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| AppError::BadRequest(format!("{key} missing or invalid")))
}

fn row_bool(row: &serde_json::Value, key: &str) -> AppResult<bool> {
    row.get(key)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| AppError::BadRequest(format!("{key} missing or invalid")))
}

async fn json_rows(state: &AppState, table: &str) -> AppResult<Vec<serde_json::Value>> {
    let sql = match table {
        "site_settings" => "SELECT to_jsonb(site_settings) FROM site_settings",
        "theme_settings" => "SELECT to_jsonb(theme_settings) FROM theme_settings",
        "smtp_settings" => "SELECT to_jsonb(smtp_settings) FROM smtp_settings",
        "users" => "SELECT to_jsonb(users) FROM users",
        "user_profiles" => "SELECT to_jsonb(user_profiles) FROM user_profiles",
        "user_groups" => "SELECT to_jsonb(user_groups) FROM user_groups",
        "quota_rules" => "SELECT to_jsonb(quota_rules) FROM quota_rules",
        "quota_usage" => "SELECT to_jsonb(quota_usage) FROM quota_usage",
        "user_quota_overrides" => "SELECT to_jsonb(user_quota_overrides) FROM user_quota_overrides",
        "api_tokens" => "SELECT to_jsonb(api_tokens) FROM api_tokens",
        "file_objects" => "SELECT to_jsonb(file_objects) FROM file_objects",
        "images" => "SELECT to_jsonb(images) FROM images",
        "storage_providers" => "SELECT to_jsonb(storage_providers) FROM storage_providers",
        "storage_routes" => "SELECT to_jsonb(storage_routes) FROM storage_routes",
        "storage_objects" => "SELECT to_jsonb(storage_objects) FROM storage_objects",
        "tags" => "SELECT to_jsonb(tags) FROM tags",
        "image_tags" => "SELECT to_jsonb(image_tags) FROM image_tags",
        "audit_tasks" => "SELECT to_jsonb(audit_tasks) FROM audit_tasks",
        "audit_results" => "SELECT to_jsonb(audit_results) FROM audit_results",
        "system_logs" => "SELECT to_jsonb(system_logs) FROM system_logs",
        "admin_operation_logs" => "SELECT to_jsonb(admin_operation_logs) FROM admin_operation_logs",
        _ => return Err(AppError::BadRequest("invalid backup table".to_string())),
    };
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(sql).fetch_all(&state.pool).await?;
    Ok(rows)
}

async fn migrate_item(
    state: &AppState,
    task: &MigrationTaskRow,
    item: &MigrationItemRow,
    source: &dyn crate::storage::StorageProvider,
    target: &dyn crate::storage::StorageProvider,
) -> AppResult<()> {
    let bytes = source.get_object(&item.source_object_key).await?;
    let (mime_type, object_type): (String, String) = sqlx::query_as(
        "SELECT fo.mime_type,so.object_type FROM storage_objects so JOIN file_objects fo ON fo.id=so.file_object_id WHERE so.id=$1",
    )
    .bind(item.storage_object_id)
    .fetch_one(&state.pool)
    .await?;
    let stored = target
        .put_object(&item.target_object_key, &bytes, &mime_type)
        .await?;
    let target_bytes = target.get_object(&stored.object_key).await?;
    verify_migration_target_hash(&bytes, &target_bytes)?;
    if object_type == "original" {
        let expected_hash: String =
            sqlx::query_scalar("SELECT sha256 FROM file_objects WHERE id=(SELECT file_object_id FROM storage_objects WHERE id=$1)")
                .bind(item.storage_object_id)
                .fetch_one(&state.pool)
                .await?;
        verify_original_hash(&bytes, &expected_hash)?;
        verify_original_hash(&target_bytes, &expected_hash)?;
    }
    if task.migration_mode == "move" {
        source.delete_object(&item.source_object_key).await?;
        sqlx::query("UPDATE storage_objects SET status='deleted', updated_at=now() WHERE id=$1")
            .bind(item.storage_object_id)
            .execute(&state.pool)
            .await?;
    }
    sqlx::query("INSERT INTO storage_objects (file_object_id,storage_provider_id,object_type,object_key,public_url,etag,size,status) SELECT file_object_id,$2,object_type,$3,$4,$5,$6,'active' FROM storage_objects WHERE id=$1 ON CONFLICT (storage_provider_id,object_key) DO UPDATE SET status='active', updated_at=now()")
        .bind(item.storage_object_id)
        .bind(task.target_storage_provider_id)
        .bind(stored.object_key)
        .bind(stored.public_url)
        .bind(stored.etag)
        .bind(stored.size)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE migration_task_items SET status='completed', error_message=NULL, updated_at=now() WHERE id=$1")
        .bind(item.id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

fn verify_migration_target_hash(source_bytes: &[u8], target_bytes: &[u8]) -> AppResult<()> {
    let source_hash = hex::encode(Sha256::digest(source_bytes));
    let target_hash = hex::encode(Sha256::digest(target_bytes));
    if source_hash != target_hash {
        return Err(AppError::External(
            "migration target hash verification failed".to_string(),
        ));
    }
    Ok(())
}

fn verify_original_hash(bytes: &[u8], expected_sha256: &str) -> AppResult<()> {
    let actual_hash = hex::encode(Sha256::digest(bytes));
    if actual_hash != expected_sha256 {
        return Err(AppError::External(
            "migration original hash verification failed".to_string(),
        ));
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct MigrationTaskRow {
    id: Uuid,
    source_storage_provider_id: Uuid,
    target_storage_provider_id: Uuid,
    migration_mode: String,
    status: String,
}

#[derive(sqlx::FromRow)]
struct MigrationObjectRow {
    id: Uuid,
    object_key: String,
}

#[derive(sqlx::FromRow)]
struct MigrationItemRow {
    id: Uuid,
    storage_object_id: Uuid,
    source_object_key: String,
    target_object_key: String,
}

#[derive(sqlx::FromRow)]
pub struct BackupFileContent {
    pub file_name: String,
    pub encrypted: bool,
    pub bytes: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct BackupFileRow {
    storage_provider_id: Option<Uuid>,
    object_key: String,
    file_name: String,
    encrypted: bool,
}

#[derive(sqlx::FromRow)]
struct BackupSourceObjectRow {
    file_object_id: Uuid,
    storage_provider_id: Uuid,
    object_key: String,
    object_type: String,
    mime_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_filters_default_to_original_and_preview() {
        let filters = parse_migration_filters(&json!({})).unwrap();
        assert_eq!(
            filters.object_types,
            vec!["original".to_string(), "preview".to_string()]
        );
    }

    #[test]
    fn migration_filters_accept_docs_object_type_shapes() {
        let original = parse_migration_filters(&json!({"only_original": true})).unwrap();
        assert_eq!(original.object_types, vec!["original"]);

        let preview = parse_migration_filters(&json!({"only_preview": true})).unwrap();
        assert_eq!(preview.object_types, vec!["preview"]);

        let both = parse_migration_filters(&json!({
            "object_types": ["preview", "original", "preview"],
            "tag_slug": "wallpaper",
            "group_code": "trusted"
        }))
        .unwrap();
        assert_eq!(both.object_types, vec!["original", "preview"]);
        assert_eq!(both.tag.as_deref(), Some("wallpaper"));
        assert_eq!(both.user_group_code.as_deref(), Some("trusted"));
    }

    #[test]
    fn migration_filters_reject_unknown_object_type() {
        assert!(matches!(
            parse_migration_filters(&json!({"object_type": "thumbnail"})),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn migration_target_key_keeps_modes_distinct() {
        assert_eq!(target_object_key("copy", "/images/a.jpg"), "/images/a.jpg");
        assert_eq!(target_object_key("move", "images/a.jpg"), "images/a.jpg");
        assert_eq!(
            target_object_key("backup", "/images/a.jpg"),
            "migration-backups/images/a.jpg"
        );
    }

    #[test]
    fn restore_options_follow_documented_switches() {
        let defaults = RestoreOptions::from_value(&json!({}));
        assert!(defaults.site_settings);
        assert!(defaults.theme_settings);
        assert!(!defaults.smtp_settings);
        assert!(!defaults.storage_providers);
        assert!(!defaults.metadata);
        assert!(!defaults.audit);
        assert!(!defaults.logs);

        let full = RestoreOptions::from_value(&json!({
            "full_database": true,
            "smtp_settings": true,
            "storage_providers": true
        }));
        assert!(full.metadata);
        assert!(full.audit);
        assert!(full.logs);
        assert!(full.smtp_settings);
        assert!(full.storage_providers);

        let disabled = RestoreOptions::from_value(&json!({"settings": false, "metadata": true}));
        assert!(!disabled.site_settings);
        assert!(!disabled.theme_settings);
        assert!(disabled.metadata);
    }

    #[test]
    fn backup_file_paths_are_scoped_to_backup_and_type() {
        let backup_id = Uuid::from_u128(7);
        assert_eq!(
            backup_file_key(&backup_id, "original", "/images/2026/a.jpg"),
            "backups/00000000-0000-0000-0000-000000000007/files/original/images/2026/a.jpg"
        );
        assert_eq!(
            backup_file_name("preview", "previews/2026/a.webp"),
            "preview-previews_2026_a.webp"
        );
    }

    #[test]
    fn migration_target_hash_verification_compares_written_bytes() {
        assert!(verify_migration_target_hash(b"image-bytes", b"image-bytes").is_ok());
        assert!(matches!(
            verify_migration_target_hash(b"image-bytes", b"corrupted"),
            Err(AppError::External(_))
        ));
    }

    #[test]
    fn migration_original_hash_verification_uses_file_object_sha256() {
        let expected = hex::encode(Sha256::digest(b"original"));
        assert!(verify_original_hash(b"original", &expected).is_ok());
        assert!(matches!(
            verify_original_hash(b"changed", &expected),
            Err(AppError::External(_))
        ));
    }

    #[test]
    fn scheduled_backup_settings_are_bounded() {
        assert_eq!(scheduled_backup_type(&json!({})), "scheduled");
        assert_eq!(
            scheduled_backup_type(&json!({"incremental": true})),
            "incremental"
        );
        assert_eq!(scheduled_backup_interval_hours(&json!({})), 24);
        assert_eq!(
            scheduled_backup_interval_hours(&json!({"interval_hours": 0})),
            1
        );
        assert_eq!(
            scheduled_backup_interval_hours(&json!({"schedule_hours": 9999})),
            24 * 30
        );
    }
}
