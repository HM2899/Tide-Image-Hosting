use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub status: String,
    pub login_failed_count: i32,
    pub locked_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QuotaRow {
    pub group_code: String,
    pub daily_upload_count: i32,
    pub daily_upload_bytes: i64,
    pub max_file_size: i64,
    pub total_storage_bytes: i64,
    pub daily_api_calls: i32,
    pub daily_random_calls: i32,
    pub require_review: bool,
    pub require_captcha: bool,
    pub allow_batch_upload: bool,
    pub allow_tag_create: bool,
    pub default_storage_provider_id: Option<Uuid>,
    pub used_count_today: i32,
    pub used_bytes_today: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StorageProviderRow {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,
    pub config_json: Value,
    pub is_default: bool,
    pub enabled: bool,
    pub priority: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StorageRouteRow {
    pub id: Uuid,
    pub name: String,
    pub scope_type: String,
    pub scope_value: String,
    pub storage_provider_id: Uuid,
    pub storage_provider_name: String,
    pub storage_provider_type: String,
    pub enabled: bool,
    pub priority: i32,
    pub note: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ImageRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub file_object_id: Uuid,
    pub original_name: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub sha256: String,
    pub size: i64,
    pub _mime_type: String,
    pub width: i32,
    pub height: i32,
    pub orientation: String,
    pub aspect_ratio: String,
    pub ref_count: i32,
}

#[derive(Debug, Deserialize)]
pub struct ImageQuery {
    pub tag: Option<String>,
    pub status: Option<String>,
    pub orientation: Option<String>,
    pub min_width: Option<i32>,
    pub min_height: Option<i32>,
    pub storage_provider_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub is_guest_upload: Option<bool>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct RandomQuery {
    pub tag: Option<String>,
    pub tags: Option<String>,
    pub mode: Option<String>,
    pub orientation: Option<String>,
    pub min_width: Option<i32>,
    pub min_height: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub ratio: Option<String>,
    pub r#match: Option<String>,
    pub tolerance: Option<f32>,
    pub r#type: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenClaims {
    pub sub: String,
    pub role: String,
    pub exp: usize,
}

pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}
