use crate::error::{AppError, AppResult};
use crate::models::{QuotaRow, today};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use tide_shared::QuotaView;
use uuid::Uuid;

pub async fn load_quota(pool: &PgPool, user_id: Uuid, role: &str) -> AppResult<QuotaRow> {
    let group_code = match role {
        "guest_account" => "guest",
        "trusted" => "trusted",
        "supporter" => "supporter",
        "admin" | "super_admin" => "admin",
        _ => "normal",
    };
    let date = today();
    sqlx::query(
        "INSERT INTO quota_usage (user_id,date) VALUES ($1,$2) ON CONFLICT (user_id,date) DO NOTHING",
    )
    .bind(user_id)
    .bind(date)
    .execute(pool)
    .await?;
    let mut row = sqlx::query_as::<_, QuotaRow>(
        "SELECT ug.code AS group_code, qr.daily_upload_count, qr.daily_upload_bytes, qr.max_file_size, qr.total_storage_bytes, qr.daily_api_calls, qr.daily_random_calls, qr.require_review, qr.require_captcha, qr.allow_batch_upload, qr.allow_tag_create, qr.default_storage_provider_id, qu.uploaded_count AS used_count_today, qu.uploaded_bytes AS used_bytes_today FROM user_groups ug JOIN quota_rules qr ON qr.group_id=ug.id JOIN quota_usage qu ON qu.user_id=$1 AND qu.date=$2 WHERE ug.code=$3",
    )
    .bind(user_id)
    .bind(date)
    .bind(group_code)
    .fetch_one(pool)
    .await?;
    if let Some(value) = sqlx::query_scalar::<_, Value>(
        "SELECT quota_json FROM user_quota_overrides WHERE user_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    {
        apply_override(&mut row, &value);
    }
    Ok(row)
}

pub async fn view(pool: &PgPool, user_id: Uuid, row: &QuotaRow) -> AppResult<QuotaView> {
    let used_storage_bytes = used_storage_bytes(pool, user_id).await?;
    Ok(QuotaView {
        group_code: row.group_code.clone(),
        daily_upload_count: row.daily_upload_count,
        daily_upload_bytes: row.daily_upload_bytes,
        max_file_size: row.max_file_size,
        total_storage_bytes: row.total_storage_bytes,
        used_storage_bytes,
        remaining_storage_bytes: (row.total_storage_bytes - used_storage_bytes).max(0),
        daily_api_calls: row.daily_api_calls,
        daily_random_calls: row.daily_random_calls,
        used_count_today: row.used_count_today,
        used_bytes_today: row.used_bytes_today,
        remaining_count_today: (row.daily_upload_count - row.used_count_today).max(0),
        remaining_bytes_today: (row.daily_upload_bytes - row.used_bytes_today).max(0),
        require_review: row.require_review,
        require_captcha: row.require_captcha,
        allow_batch_upload: row.allow_batch_upload,
        allow_tag_create: row.allow_tag_create,
        default_storage_provider_id: row.default_storage_provider_id,
    })
}

pub async fn ensure_upload_allowed(
    pool: &PgPool,
    user_id: Uuid,
    row: &QuotaRow,
    bytes: i64,
) -> AppResult<()> {
    if let Some(error) = upload_size_policy_error(row, bytes) {
        return Err(error);
    }
    if row.group_code == "admin" {
        return Ok(());
    }
    let used_storage_bytes = used_storage_bytes(pool, user_id).await?;
    if let Some(error) = storage_quota_exceeded(row, used_storage_bytes, bytes) {
        return Err(error);
    }
    Ok(())
}

fn upload_size_policy_error(row: &QuotaRow, bytes: i64) -> Option<AppError> {
    if row.group_code == "admin" {
        return None;
    }
    if bytes > row.max_file_size {
        return Some(AppError::FileTooLarge(
            "file exceeds max file size".to_string(),
        ));
    }
    if row.used_count_today + 1 > row.daily_upload_count {
        return Some(AppError::Quota("daily upload count exceeded".to_string()));
    }
    if row.used_bytes_today + bytes > row.daily_upload_bytes {
        return Some(AppError::Quota("daily upload bytes exceeded".to_string()));
    }
    None
}

fn storage_quota_exceeded(row: &QuotaRow, used_storage_bytes: i64, bytes: i64) -> Option<AppError> {
    if row.group_code == "admin" {
        return None;
    }
    if used_storage_bytes.saturating_add(bytes) > row.total_storage_bytes {
        Some(AppError::Quota("total storage quota exceeded".to_string()))
    } else {
        None
    }
}

pub async fn used_storage_bytes(pool: &PgPool, user_id: Uuid) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(SUM(fo.size),0)::BIGINT FROM file_objects fo WHERE fo.id IN (SELECT DISTINCT file_object_id FROM images WHERE user_id=$1 AND status <> 'deleted')",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?)
}

pub async fn ensure_api_allowed(pool: &PgPool, user_id: Uuid, role: &str) -> AppResult<()> {
    let row = load_quota(pool, user_id, role).await?;
    if row.group_code == "admin" {
        return Ok(());
    }
    let api_calls: i32 =
        sqlx::query_scalar("SELECT api_calls FROM quota_usage WHERE user_id=$1 AND date=$2")
            .bind(user_id)
            .bind(today())
            .fetch_one(pool)
            .await?;
    if api_calls + 1 > row.daily_api_calls {
        Err(AppError::Quota("daily api call limit exceeded".to_string()))
    } else {
        Ok(())
    }
}

pub async fn ensure_random_allowed(pool: &PgPool, user_id: Uuid, role: &str) -> AppResult<()> {
    let row = load_quota(pool, user_id, role).await?;
    if row.group_code == "admin" {
        return Ok(());
    }
    let random_calls: i32 =
        sqlx::query_scalar("SELECT random_calls FROM quota_usage WHERE user_id=$1 AND date=$2")
            .bind(user_id)
            .bind(today())
            .fetch_one(pool)
            .await?;
    if random_calls + 1 > row.daily_random_calls {
        Err(AppError::Quota(
            "daily random call limit exceeded".to_string(),
        ))
    } else {
        Ok(())
    }
}

pub async fn increment_upload(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    bytes: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO quota_usage (user_id,date,uploaded_count,uploaded_bytes) VALUES ($1,$2,1,$3) ON CONFLICT (user_id,date) DO UPDATE SET uploaded_count=quota_usage.uploaded_count+1, uploaded_bytes=quota_usage.uploaded_bytes+$3, updated_at=now()",
    )
    .bind(user_id)
    .bind(today())
    .bind(bytes)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn increment_api(pool: &PgPool, user_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO quota_usage (user_id,date,api_calls) VALUES ($1,$2,1) ON CONFLICT (user_id,date) DO UPDATE SET api_calls=quota_usage.api_calls+1, updated_at=now()",
    )
    .bind(user_id)
    .bind(today())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn increment_random(pool: &PgPool, user_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO quota_usage (user_id,date,random_calls) VALUES ($1,$2,1) ON CONFLICT (user_id,date) DO UPDATE SET random_calls=quota_usage.random_calls+1, updated_at=now()",
    )
    .bind(user_id)
    .bind(today())
    .execute(pool)
    .await?;
    Ok(())
}

fn apply_override(row: &mut QuotaRow, value: &Value) {
    if let Some(number) = int_value(value, "daily_upload_count") {
        row.daily_upload_count = number;
    }
    if let Some(number) = i64_value(value, "daily_upload_bytes") {
        row.daily_upload_bytes = number;
    }
    if let Some(number) = i64_value(value, "max_file_size") {
        row.max_file_size = number;
    }
    if let Some(number) = i64_value(value, "total_storage_bytes") {
        row.total_storage_bytes = number;
    }
    if let Some(number) = int_value(value, "daily_api_calls") {
        row.daily_api_calls = number;
    }
    if let Some(number) = int_value(value, "daily_random_calls") {
        row.daily_random_calls = number;
    }
    if let Some(flag) = value.get("require_review").and_then(Value::as_bool) {
        row.require_review = flag;
    }
    if let Some(flag) = value.get("require_captcha").and_then(Value::as_bool) {
        row.require_captcha = flag;
    }
    if let Some(flag) = value.get("allow_batch_upload").and_then(Value::as_bool) {
        row.allow_batch_upload = flag;
    }
    if let Some(flag) = value.get("allow_tag_create").and_then(Value::as_bool) {
        row.allow_tag_create = flag;
    }
    if let Some(value) = value
        .get("default_storage_provider_id")
        .and_then(Value::as_str)
    {
        row.default_storage_provider_id = Uuid::parse_str(value).ok();
    }
}

fn int_value(value: &Value, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
}

fn i64_value(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota_row() -> QuotaRow {
        QuotaRow {
            group_code: "normal".to_string(),
            daily_upload_count: 10,
            daily_upload_bytes: 1000,
            max_file_size: 100,
            total_storage_bytes: 5000,
            daily_api_calls: 50,
            daily_random_calls: 60,
            require_review: true,
            require_captcha: false,
            allow_batch_upload: true,
            allow_tag_create: true,
            default_storage_provider_id: None,
            used_count_today: 0,
            used_bytes_today: 0,
        }
    }

    #[test]
    fn quota_override_updates_supported_fields() {
        let mut row = quota_row();

        apply_override(
            &mut row,
            &serde_json::json!({
                "daily_upload_count": 20,
                "daily_upload_bytes": 2000,
                "max_file_size": 300,
                "daily_api_calls": 70,
                "daily_random_calls": 80,
                "require_review": false,
                "require_captcha": true,
                "allow_batch_upload": false,
                "allow_tag_create": false,
                "default_storage_provider_id": "00000000-0000-0000-0000-000000000001"
            }),
        );

        assert_eq!(row.daily_upload_count, 20);
        assert_eq!(row.daily_upload_bytes, 2000);
        assert_eq!(row.max_file_size, 300);
        assert_eq!(row.daily_api_calls, 70);
        assert_eq!(row.daily_random_calls, 80);
        assert!(!row.require_review);
        assert!(row.require_captcha);
        assert!(!row.allow_batch_upload);
        assert!(!row.allow_tag_create);
        assert_eq!(row.default_storage_provider_id, Some(Uuid::from_u128(1)));
    }

    #[test]
    fn upload_quota_returns_specific_errors() {
        let mut row = quota_row();

        assert!(matches!(
            upload_size_policy_error(&row, 101),
            Some(AppError::FileTooLarge(_))
        ));
        row.used_count_today = 10;
        assert!(matches!(
            upload_size_policy_error(&row, 10),
            Some(AppError::Quota(_))
        ));
        row.used_count_today = 0;
        row.used_bytes_today = 995;
        assert!(matches!(
            upload_size_policy_error(&row, 10),
            Some(AppError::Quota(_))
        ));
    }

    #[test]
    fn upload_quota_blocks_total_storage_overflow() {
        let row = quota_row();

        assert!(storage_quota_exceeded(&row, 4_950, 60).is_some());
        assert!(storage_quota_exceeded(&row, 4_000, 60).is_none());
        let mut admin = row;
        admin.group_code = "admin".to_string();
        assert!(storage_quota_exceeded(&admin, i64::MAX - 1, 100).is_none());
    }
}
