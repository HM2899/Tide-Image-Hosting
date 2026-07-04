use crate::app::AppState;
use crate::error::{AppError, AppResult};
use crate::models::StorageProviderRow;
use crate::services::security;
use crate::storage::{
    LocalStorageProvider, OneDriveProvider, OracleOciNativeProvider, S3CompatibleProvider,
    StorageProvider,
};
use std::sync::Arc;
use uuid::Uuid;

pub async fn default_provider(state: &AppState) -> AppResult<StorageProviderRow> {
    sqlx::query_as::<_, StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority FROM storage_providers WHERE enabled=true AND deleted_at IS NULL ORDER BY is_default DESC, priority ASC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::BadRequest("no enabled storage provider".to_string()))
}

pub async fn provider_by_id(state: &AppState, id: Uuid) -> AppResult<StorageProviderRow> {
    sqlx::query_as::<_, StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority FROM storage_providers WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("storage provider not found".to_string()))
}

pub async fn active_provider_by_id(state: &AppState, id: Uuid) -> AppResult<StorageProviderRow> {
    sqlx::query_as::<_, StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority FROM storage_providers WHERE id=$1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("storage provider not found".to_string()))
}

pub async fn active_enabled_provider_by_id(
    state: &AppState,
    id: Uuid,
) -> AppResult<StorageProviderRow> {
    sqlx::query_as::<_, StorageProviderRow>(
        "SELECT id,name,provider_type,config_json,is_default,enabled,priority
         FROM storage_providers
         WHERE id=$1 AND enabled=true AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("enabled storage provider not found".to_string()))
}

pub async fn provider_for_upload(
    state: &AppState,
    user_id: Uuid,
    role: &str,
    group_code: &str,
    group_provider_id: Option<Uuid>,
) -> AppResult<StorageProviderRow> {
    let user_scope = user_id.to_string();
    let route_provider = sqlx::query_as::<_, StorageProviderRow>(
        "SELECT sp.id,sp.name,sp.provider_type,sp.config_json,sp.is_default,sp.enabled,sp.priority
         FROM storage_routes sr
         JOIN storage_providers sp ON sp.id=sr.storage_provider_id
         WHERE sr.enabled=true
           AND sp.enabled=true
           AND sp.deleted_at IS NULL
           AND (
             (sr.scope_type='user' AND sr.scope_value=$1)
             OR (sr.scope_type='group' AND sr.scope_value=$2)
             OR (sr.scope_type='role' AND sr.scope_value=$3)
             OR sr.scope_type='global'
           )
         ORDER BY
           CASE sr.scope_type
             WHEN 'user' THEN 0
             WHEN 'group' THEN 1
             WHEN 'role' THEN 2
             ELSE 3
           END,
           sr.priority ASC,
           sr.created_at DESC
         LIMIT 1",
    )
    .bind(&user_scope)
    .bind(group_code)
    .bind(role)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(provider) = route_provider {
        return Ok(provider);
    }

    if let Some(provider_id) = group_provider_id {
        match active_enabled_provider_by_id(state, provider_id).await {
            Ok(provider) => return Ok(provider),
            Err(AppError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }

    let provider = default_provider(state).await?;
    if group_provider_id.is_some() && group_code != "admin" {
        sqlx::query("INSERT INTO system_logs (level,module,message,context_json) VALUES ('warn','storage','group storage provider unavailable, fallback to default',$1)")
            .bind(serde_json::json!({"group_code":group_code,"provider_id":group_provider_id,"fallback_provider_id":provider.id}))
            .execute(&state.pool)
            .await?;
    }
    Ok(provider)
}

#[cfg(test)]
fn storage_route_scope_rank(scope_type: &str) -> i32 {
    match scope_type {
        "user" => 0,
        "group" => 1,
        "role" => 2,
        "global" => 3,
        _ => 9,
    }
}

pub async fn build_provider(
    state: &AppState,
    row: &StorageProviderRow,
) -> AppResult<Arc<dyn StorageProvider>> {
    match row.provider_type.as_str() {
        "local" => Ok(Arc::new(LocalStorageProvider::from_config(
            &state.config,
            &row.config_json,
        ))),
        "cloudflare_r2" | "oracle_s3" | "s3_compatible" => Ok(Arc::new(
            S3CompatibleProvider::from_config(
                row.provider_type.clone(),
                security::decrypt_sensitive_json(&state.config, row.config_json.clone())?,
                row.id,
            )
            .await?,
        )),
        "onedrive" => Ok(Arc::new(OneDriveProvider::new(
            security::decrypt_sensitive_json(&state.config, row.config_json.clone())?,
            row.id,
        ))),
        "oracle_oci_native" => Ok(Arc::new(OracleOciNativeProvider::from_config(
            security::decrypt_sensitive_json(&state.config, row.config_json.clone())?,
            row.id,
        )?)),
        _ => Err(AppError::BadRequest(format!(
            "unsupported storage provider type {}",
            row.provider_type
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_route_scopes_are_ranked_by_specificity() {
        assert!(storage_route_scope_rank("user") < storage_route_scope_rank("group"));
        assert!(storage_route_scope_rank("group") < storage_route_scope_rank("role"));
        assert!(storage_route_scope_rank("role") < storage_route_scope_rank("global"));
        assert!(storage_route_scope_rank("unknown") > storage_route_scope_rank("global"));
    }
}
