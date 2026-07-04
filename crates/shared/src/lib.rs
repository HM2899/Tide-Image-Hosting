use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    GuestAccount,
    User,
    Trusted,
    Supporter,
    Admin,
    SuperAdmin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageStatus {
    PendingReview,
    Active,
    Rejected,
    Trashed,
    Deleted,
    Blocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageProviderType {
    Local,
    CloudflareR2,
    Onedrive,
    OracleS3,
    OracleOciNative,
    S3Compatible,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub success: bool,
    pub error: ApiErrorBody,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page: i64,
    pub page_size: i64,
    pub total: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub username: String,
    pub password: String,
    pub code: Option<String>,
    pub captcha_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginRequest {
    #[serde(default, alias = "email")]
    pub identifier: String,
    pub password: String,
    pub captcha_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
    pub status: String,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: AuthUser,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadResponse {
    pub id: Uuid,
    pub url: String,
    pub preview_url: String,
    pub markdown: String,
    pub html: String,
    pub preview_markdown: String,
    pub preview_html: String,
    pub status: String,
    pub deduplicated: bool,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadBatchResponse {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub items: Vec<UploadBatchItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadBatchItem {
    pub file_name: String,
    pub success: bool,
    pub response: Option<UploadResponse>,
    pub error: Option<ApiErrorBody>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UploadResult {
    Single(UploadResponse),
    Batch(UploadBatchResponse),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageSummary {
    pub id: Uuid,
    pub original_name: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub visibility: String,
    pub url: String,
    pub preview_url: String,
    pub width: i32,
    pub height: i32,
    pub size: i64,
    pub orientation: String,
    pub sha256: String,
    pub ref_count: i32,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageLinks {
    pub url: String,
    pub preview_url: String,
    pub markdown: String,
    pub html: String,
    pub preview_markdown: String,
    pub preview_html: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaView {
    pub group_code: String,
    pub daily_upload_count: i32,
    pub daily_upload_bytes: i64,
    pub max_file_size: i64,
    pub total_storage_bytes: i64,
    pub used_storage_bytes: i64,
    pub remaining_storage_bytes: i64,
    pub daily_api_calls: i32,
    pub daily_random_calls: i32,
    pub used_count_today: i32,
    pub used_bytes_today: i64,
    pub remaining_count_today: i32,
    pub remaining_bytes_today: i64,
    pub require_review: bool,
    pub require_captcha: bool,
    pub allow_batch_upload: bool,
    pub allow_tag_create: bool,
    pub default_storage_provider_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TagView {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub usage_count: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageProviderView {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,
    pub config_json: Value,
    pub is_default: bool,
    pub enabled: bool,
    pub priority: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RandomImageResponse {
    pub id: Uuid,
    pub url: String,
    pub preview_url: String,
    pub width: i32,
    pub height: i32,
    pub orientation: String,
    pub ratio: String,
    pub tags: Vec<String>,
    pub markdown: String,
    pub html: String,
    pub preview_markdown: String,
    pub preview_html: String,
}

pub fn ok<T>(data: T) -> ApiResponse<T> {
    ApiResponse {
        success: true,
        data: Some(data),
        message: String::new(),
    }
}
