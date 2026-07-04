use crate::app::AppState;
use crate::error::{AppError, AppResult};
use crate::services::{images, quota, security};
use chrono::{Duration, Utc};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Instant;
use uuid::Uuid;

pub async fn create_upload_audit_task(
    state: &AppState,
    image_id: Uuid,
    require_review: bool,
) -> AppResult<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO audit_tasks (image_id,audit_type,provider,status,require_review) VALUES ($1,'ai','local','pending',$2) RETURNING id",
    )
    .bind(image_id)
    .bind(require_review)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::from)
}

pub fn spawn_upload_audit(state: AppState, task_id: Uuid) {
    tokio::spawn(async move {
        if let Err(err) = run_audit_task(&state, task_id, None).await {
            let _ = sqlx::query("UPDATE audit_tasks SET status='failed', error_message=$2, finished_at=now() WHERE id=$1")
                .bind(task_id)
                .bind(err.to_string())
                .execute(&state.pool)
                .await;
            let _ = sqlx::query(
                "INSERT INTO system_logs (level,module,message,context_json) VALUES ('error','audit',$1,$2)",
            )
            .bind(err.to_string())
            .bind(json!({"task_id":task_id}))
            .execute(&state.pool)
            .await;
        }
    });
}

pub async fn run_audit_task(
    state: &AppState,
    task_id: Uuid,
    original_name: Option<&str>,
) -> AppResult<()> {
    let (image_id, stored_original_name, require_review): (Uuid, String, bool) = sqlx::query_as(
        "SELECT i.id,i.original_name,at.require_review FROM audit_tasks at JOIN images i ON i.id=at.image_id WHERE at.id=$1",
    )
    .bind(task_id)
    .fetch_one(&state.pool)
    .await?;
    let original_name = original_name.unwrap_or(&stored_original_name);
    let settings = audit_settings(state).await?;
    sqlx::query(
        "UPDATE audit_tasks SET status='running', error_message=NULL, started_at=now(), finished_at=NULL WHERE id=$1",
    )
    .bind(task_id)
    .execute(&state.pool)
    .await?;
    if keyword_rejected(Some(original_name), &settings) {
        write_result(
            state,
            task_id,
            image_id,
            AuditDecision {
                task_status: "rejected",
                image_status: "rejected",
                result: "rejected",
                risk_level: "high",
                reason: "文件名命中关键词审核".to_string(),
                provider: "keyword",
                request_payload: json!({"keyword_enabled": true}),
                response_payload: json!({"keyword_enabled": true}),
                duration_ms: 0,
            },
            &settings,
        )
        .await?;
        return Ok(());
    }
    let ai_enabled = settings
        .get("ai_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let decision = if ai_enabled {
        match call_ai(state, image_id, &settings).await {
            Ok(outcome) => decision_from_ai(require_review, &outcome),
            Err(err) => failure_decision(require_review, &settings, err.to_string()),
        }
    } else {
        manual_or_pass_decision(require_review)
    };
    write_result(state, task_id, image_id, decision, &settings).await?;
    Ok(())
}

pub async fn retry_audit_task(state: &AppState, task_id: Uuid) -> AppResult<()> {
    let (image_id, original_name, require_review): (Uuid, String, bool) = sqlx::query_as(
        "SELECT i.id,i.original_name,at.require_review FROM audit_tasks at JOIN images i ON i.id=at.image_id WHERE at.id=$1",
    )
    .bind(task_id)
    .fetch_one(&state.pool)
    .await?;
    let settings = audit_settings(state).await?;
    sqlx::query("UPDATE audit_tasks SET status='running', retry_count=retry_count+1, error_message=NULL, started_at=now(), finished_at=NULL WHERE id=$1")
        .bind(task_id)
        .execute(&state.pool)
        .await?;
    let decision = if keyword_rejected(Some(&original_name), &settings) {
        AuditDecision {
            task_status: "rejected",
            image_status: "rejected",
            result: "rejected",
            risk_level: "high",
            reason: "文件名命中关键词审核".to_string(),
            provider: "keyword",
            request_payload: json!({"keyword_enabled": true}),
            response_payload: json!({"keyword_enabled": true}),
            duration_ms: 0,
        }
    } else if settings
        .get("ai_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        match call_ai(state, image_id, &settings).await {
            Ok(outcome) => decision_from_ai(require_review, &outcome),
            Err(err) => failure_decision(require_review, &settings, err.to_string()),
        }
    } else {
        manual_or_pass_decision(require_review)
    };
    write_result(state, task_id, image_id, decision, &settings).await
}

#[derive(Serialize)]
struct AiPayload<'a> {
    image_url: String,
    options: &'a Value,
}

#[derive(Deserialize, Serialize)]
struct AiModerationResult {
    passed: bool,
    risk_level: Option<String>,
    categories: Option<Value>,
    labels: Option<Vec<String>>,
    suggested_tags: Option<Vec<String>>,
    tags: Option<Vec<String>>,
    description: Option<String>,
    ocr_text: Option<String>,
    reason: Option<String>,
    confidence: Option<f64>,
    model: Option<String>,
}

struct AiCallOutcome {
    result: AiModerationResult,
    request_payload: Value,
    response_payload: Value,
    duration_ms: i32,
}

struct AuditDecision {
    task_status: &'static str,
    image_status: &'static str,
    result: &'static str,
    risk_level: &'static str,
    reason: String,
    provider: &'static str,
    request_payload: Value,
    response_payload: Value,
    duration_ms: i32,
}

async fn audit_settings(state: &AppState) -> AppResult<Value> {
    let value = sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='audit'")
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or_else(|| {
            json!({
                "ai_enabled": true,
                "service_type": "fastapi",
                "failure_strategy": "manual_required",
                "keyword_enabled": true,
                "filename_keyword_enabled": true,
                "ocr_enabled": true,
                "description_enabled": true,
                "tag_suggestions_enabled": true,
                "keywords": []
            })
        });
    crate::services::security::decrypt_sensitive_json(&state.config, value)
}

fn keyword_rejected(original_name: Option<&str>, settings: &Value) -> bool {
    if !settings
        .get("keyword_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return false;
    }
    let Some(name) = original_name else {
        return false;
    };
    let name = name.to_lowercase();
    settings
        .get("keywords")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_lowercase)
        .any(|keyword| !keyword.is_empty() && name.contains(&keyword))
}

async fn call_ai(state: &AppState, image_id: Uuid, settings: &Value) -> AppResult<AiCallOutcome> {
    let image_url = public_image_url(state, image_id).await?;
    let service_url = settings
        .get("service_url")
        .and_then(Value::as_str)
        .unwrap_or(&state.config.ai_service_url);
    let payload = AiPayload {
        image_url,
        options: settings,
    };
    let started = Instant::now();
    let moderation_raw =
        post_ai_json(state, service_url, "moderate-image", settings, &payload).await?;
    let mut result: AiModerationResult = serde_json::from_value(moderation_raw.clone())
        .map_err(|err| AppError::External(format!("invalid ai moderation response: {err}")))?;
    let mut raw = json!({"moderation": moderation_raw});
    let mut enrichment_errors = Vec::new();

    if setting_enabled(
        settings,
        &[
            "image_analysis_enabled",
            "analysis_enabled",
            "tag_suggestions_enabled",
            "description_enabled",
            "ocr_enabled",
        ],
        true,
    ) {
        match post_ai_json(state, service_url, "analyze-image", settings, &payload).await {
            Ok(value) => {
                merge_ai_result(&mut result, &value);
                raw["analysis"] = value;
            }
            Err(err) => enrichment_errors.push(format!("analyze-image: {err}")),
        }
    }
    if setting_enabled(
        settings,
        &[
            "description_enabled",
            "alt_text_enabled",
            "enable_description",
        ],
        true,
    ) && result
        .description
        .as_ref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        match post_ai_json(state, service_url, "generate-alt-text", settings, &payload).await {
            Ok(value) => {
                result.description = string_field(&value, &["alt_text", "description"]);
                raw["alt_text"] = value;
            }
            Err(err) => enrichment_errors.push(format!("generate-alt-text: {err}")),
        }
    }
    if setting_enabled(settings, &["ocr_enabled", "enable_ocr"], true)
        && result
            .ocr_text
            .as_ref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
    {
        match post_ai_json(state, service_url, "ocr", settings, &payload).await {
            Ok(value) => {
                result.ocr_text = string_field(&value, &["ocr_text"]);
                raw["ocr"] = value;
            }
            Err(err) => enrichment_errors.push(format!("ocr: {err}")),
        }
    }

    let mut response_payload = serde_json::to_value(&result)
        .map_err(|err| AppError::External(format!("invalid ai response payload: {err}")))?;
    response_payload["_raw"] = raw;
    if !enrichment_errors.is_empty() {
        response_payload["enrichment_errors"] = json!(enrichment_errors);
    }
    Ok(AiCallOutcome {
        result,
        request_payload: security::redact_sensitive_json(json!({
            "image_url": payload.image_url,
            "options": settings
        })),
        response_payload,
        duration_ms: started.elapsed().as_millis().min(i32::MAX as u128) as i32,
    })
}

async fn post_ai_json(
    state: &AppState,
    service_url: &str,
    path: &str,
    settings: &Value,
    payload: &AiPayload<'_>,
) -> AppResult<Value> {
    let mut request = state
        .http
        .post(format!(
            "{}/ai/{}",
            service_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        ))
        .json(payload);
    if let Some(token) = settings
        .get("api_token")
        .or_else(|| settings.get("fastapi_token"))
        .and_then(Value::as_str)
        && !token.is_empty()
    {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(AppError::External(format!(
            "ai {path} failed with {}",
            response.status()
        )));
    }
    Ok(response.json().await?)
}

async fn public_image_url(state: &AppState, image_id: Uuid) -> AppResult<String> {
    let row: (Uuid, String, Option<String>) = sqlx::query_as(
        "SELECT so.storage_provider_id,so.object_key,so.public_url FROM images i JOIN storage_objects so ON so.file_object_id=i.file_object_id WHERE i.id=$1 AND so.object_type='original' AND so.status='active' LIMIT 1",
    )
    .bind(image_id)
    .fetch_one(&state.pool)
    .await?;
    let url = crate::services::images::storage_url(state, row.0, &row.1, row.2);
    append_internal_file_token(state, image_id, &url)
}

fn append_internal_file_token(state: &AppState, image_id: Uuid, url: &str) -> AppResult<String> {
    let exp = Utc::now()
        .checked_add_signed(Duration::minutes(15))
        .ok_or_else(|| AppError::BadRequest("invalid expiration".to_string()))?
        .timestamp() as usize;
    let token = encode(
        &Header::default(),
        &crate::models::TokenClaims {
            sub: image_id.to_string(),
            role: "file_read".to_string(),
            exp,
        },
        &EncodingKey::from_secret(state.config.session_secret.as_bytes()),
    )
    .map_err(|err| AppError::Unauthorized(err.to_string()))?;
    let separator = if url.contains('?') { '&' } else { '?' };
    Ok(format!(
        "{}{}token={}",
        url,
        separator,
        urlencoding::encode(&token)
    ))
}

fn decision_from_ai(require_review: bool, outcome: &AiCallOutcome) -> AuditDecision {
    if !outcome.result.passed {
        AuditDecision {
            task_status: "rejected",
            image_status: "rejected",
            result: "rejected",
            risk_level: "high",
            reason: outcome
                .result
                .reason
                .clone()
                .unwrap_or_else(|| "AI 审核拒绝".to_string()),
            provider: "fastapi",
            request_payload: outcome.request_payload.clone(),
            response_payload: outcome.response_payload.clone(),
            duration_ms: outcome.duration_ms,
        }
    } else if require_review {
        AuditDecision {
            task_status: "manual_required",
            image_status: "active",
            result: "manual_required",
            risk_level: "low",
            reason: "AI 审核通过，等待人工审核".to_string(),
            provider: "fastapi",
            request_payload: outcome.request_payload.clone(),
            response_payload: outcome.response_payload.clone(),
            duration_ms: outcome.duration_ms,
        }
    } else {
        AuditDecision {
            task_status: "passed",
            image_status: "active",
            result: "passed",
            risk_level: "low",
            reason: outcome
                .result
                .reason
                .clone()
                .unwrap_or_else(|| "AI 审核通过".to_string()),
            provider: "fastapi",
            request_payload: outcome.request_payload.clone(),
            response_payload: outcome.response_payload.clone(),
            duration_ms: outcome.duration_ms,
        }
    }
}

fn manual_or_pass_decision(require_review: bool) -> AuditDecision {
    if require_review {
        AuditDecision {
            task_status: "manual_required",
            image_status: "active",
            result: "manual_required",
            risk_level: "low",
            reason: "按用户组策略等待人工审核".to_string(),
            provider: "local",
            request_payload: json!({"require_review": true}),
            response_payload: json!({"passed": false}),
            duration_ms: 0,
        }
    } else {
        AuditDecision {
            task_status: "passed",
            image_status: "active",
            result: "passed",
            risk_level: "low",
            reason: "按用户组策略自动通过".to_string(),
            provider: "local",
            request_payload: json!({"require_review": false}),
            response_payload: json!({"passed": true}),
            duration_ms: 0,
        }
    }
}

fn failure_decision(require_review: bool, settings: &Value, reason: String) -> AuditDecision {
    match settings
        .get("failure_strategy")
        .and_then(Value::as_str)
        .unwrap_or("manual_required")
    {
        "reject" => AuditDecision {
            task_status: "failed",
            image_status: "rejected",
            result: "failed",
            risk_level: "unknown",
            reason,
            provider: "fastapi",
            request_payload: json!({}),
            response_payload: json!({"passed": false}),
            duration_ms: 0,
        },
        "pass" if !require_review => AuditDecision {
            task_status: "passed",
            image_status: "active",
            result: "passed",
            risk_level: "unknown",
            reason,
            provider: "fastapi",
            request_payload: json!({}),
            response_payload: json!({"passed": true}),
            duration_ms: 0,
        },
        _ => AuditDecision {
            task_status: "manual_required",
            image_status: "active",
            result: "manual_required",
            risk_level: "unknown",
            reason,
            provider: "fastapi",
            request_payload: json!({}),
            response_payload: json!({"passed": false}),
            duration_ms: 0,
        },
    }
}

async fn write_result(
    state: &AppState,
    task_id: Uuid,
    image_id: Uuid,
    decision: AuditDecision,
    settings: &Value,
) -> AppResult<()> {
    sqlx::query("UPDATE audit_tasks SET status=$2,provider=$3,finished_at=now(),error_message=CASE WHEN $2='failed' THEN $4 ELSE NULL END WHERE id=$1")
        .bind(task_id)
        .bind(decision.task_status)
        .bind(decision.provider)
        .bind(&decision.reason)
        .execute(&state.pool)
        .await?;
    sqlx::query(
        "INSERT INTO audit_results (audit_task_id,image_id,result,risk_level,reason,labels_json,categories_json,ocr_text,provider,model,request_payload,response_payload,duration_ms) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(task_id)
    .bind(image_id)
    .bind(decision.result)
    .bind(decision.risk_level)
    .bind(&decision.reason)
    .bind(labels_json(&decision.response_payload))
    .bind(decision.response_payload.get("categories").cloned().unwrap_or_else(|| json!({})))
    .bind(audit_ocr_text(&decision.response_payload, settings))
    .bind(decision.provider)
    .bind(model_name(&decision.response_payload))
    .bind(decision.request_payload.clone())
    .bind(decision.response_payload.clone())
    .bind(decision.duration_ms)
    .execute(&state.pool)
    .await?;
    if decision.image_status == "rejected" {
        images::permanent_delete_by_system(state, image_id, &decision.reason).await?;
    } else {
        sqlx::query("UPDATE images SET status=$2, updated_at=now() WHERE id=$1")
            .bind(image_id)
            .bind(decision.image_status)
            .execute(&state.pool)
            .await?;
    }
    apply_ai_metadata(state, image_id, &decision, settings).await?;
    Ok(())
}

async fn apply_ai_metadata(
    state: &AppState,
    image_id: Uuid,
    decision: &AuditDecision,
    settings: &Value,
) -> AppResult<()> {
    if decision.provider != "fastapi" || !matches!(decision.result, "passed" | "manual_required") {
        return Ok(());
    }
    if setting_enabled(
        settings,
        &[
            "description_enabled",
            "alt_text_enabled",
            "enable_description",
        ],
        true,
    ) && let Some(description) = metadata_description(&decision.response_payload)
    {
        sqlx::query(
            "UPDATE images SET description=$2, updated_at=now() WHERE id=$1 AND description=''",
        )
        .bind(image_id)
        .bind(description)
        .execute(&state.pool)
        .await?;
    }
    if setting_enabled(
        settings,
        &[
            "tag_suggestions_enabled",
            "enable_tag_suggestions",
            "apply_ai_tags",
        ],
        true,
    ) {
        let tags = suggested_tags(&decision.response_payload);
        if !tags.is_empty() {
            attach_ai_tags(state, image_id, &tags).await?;
        }
    }
    Ok(())
}

async fn attach_ai_tags(state: &AppState, image_id: Uuid, tags: &[String]) -> AppResult<()> {
    let Some((user_id, role)) = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT u.id,u.role FROM images i JOIN users u ON u.id=i.user_id WHERE i.id=$1",
    )
    .bind(image_id)
    .fetch_optional(&state.pool)
    .await?
    else {
        return Ok(());
    };
    let quota_row = quota::load_quota(&state.pool, user_id, &role).await?;
    let policy = images::load_tag_policy(state).await?;
    let current_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM image_tags WHERE image_id=$1")
            .bind(image_id)
            .fetch_one(&state.pool)
            .await?;
    let remaining = policy
        .remaining_slots(current_count.max(0) as usize)
        .min(tags.len());
    if remaining == 0 {
        return Ok(());
    }
    let tags = tags.iter().take(remaining).cloned().collect::<Vec<_>>();
    match images::attach_tags(
        state,
        image_id,
        user_id,
        &role,
        &tags,
        quota_row.allow_tag_create,
        &policy,
    )
    .await
    {
        Ok(()) | Err(AppError::BadRequest(_)) | Err(AppError::Forbidden(_)) => Ok(()),
        Err(err) => Err(err),
    }
}

fn setting_enabled(settings: &Value, keys: &[&str], default: bool) -> bool {
    keys.iter()
        .find_map(|key| settings.get(*key).and_then(Value::as_bool))
        .unwrap_or(default)
}

fn merge_ai_result(target: &mut AiModerationResult, value: &Value) {
    let Ok(update) = serde_json::from_value::<AiModerationResult>(value.clone()) else {
        return;
    };
    if target.risk_level.is_none() {
        target.risk_level = update.risk_level;
    }
    if target.categories.is_none() {
        target.categories = update.categories;
    }
    target.labels = merge_string_vecs(target.labels.take(), update.labels);
    target.suggested_tags = merge_string_vecs(target.suggested_tags.take(), update.suggested_tags);
    target.tags = merge_string_vecs(target.tags.take(), update.tags);
    if target
        .description
        .as_ref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        target.description = update.description;
    }
    if target
        .ocr_text
        .as_ref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        target.ocr_text = update.ocr_text;
    }
    if target.reason.is_none() {
        target.reason = update.reason;
    }
    if target.confidence.is_none() {
        target.confidence = update.confidence;
    }
    if target.model.is_none() {
        target.model = update.model;
    }
}

fn merge_string_vecs(left: Option<Vec<String>>, right: Option<Vec<String>>) -> Option<Vec<String>> {
    let mut values = BTreeSet::new();
    for value in left
        .into_iter()
        .flatten()
        .chain(right.into_iter().flatten())
    {
        let value = value.trim();
        if !value.is_empty() {
            values.insert(value.to_string());
        }
    }
    if values.is_empty() {
        None
    } else {
        Some(values.into_iter().collect())
    }
}

fn labels_json(payload: &Value) -> Value {
    payload.get("labels").cloned().unwrap_or_else(|| json!([]))
}

fn ocr_text(payload: &Value) -> String {
    payload
        .get("ocr_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn audit_ocr_text(payload: &Value, settings: &Value) -> String {
    if setting_enabled(settings, &["ocr_enabled", "enable_ocr"], true) {
        ocr_text(payload)
    } else {
        String::new()
    }
}

fn model_name(payload: &Value) -> String {
    payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn metadata_description(payload: &Value) -> Option<String> {
    string_field(payload, &["description", "alt_text"]).filter(|value| !value.trim().is_empty())
}

fn suggested_tags(payload: &Value) -> Vec<String> {
    let mut values = BTreeSet::new();
    for key in ["suggested_tags", "tags", "labels"] {
        if let Some(items) = payload.get(key).and_then(Value::as_array) {
            for item in items.iter().filter_map(Value::as_str) {
                if let Some(label) = visible_tag_label(item) {
                    values.insert(label);
                }
            }
        }
    }
    values.into_iter().collect()
}

fn visible_tag_label(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mapped = match value.to_ascii_lowercase().as_str() {
        "safe" => "安全",
        "adult" => "成人内容",
        "violence" => "暴力",
        "political" => "政治",
        "illegal" => "违法",
        "hate" => "仇恨",
        "privacy" => "隐私",
        _ => value,
    };
    if mapped.is_ascii() {
        None
    } else {
        Some(mapped.to_string())
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation, decode};

    #[test]
    fn setting_enabled_accepts_docs_aliases() {
        let settings = json!({"enable_ocr": false, "description_enabled": true});
        assert!(!setting_enabled(
            &settings,
            &["ocr_enabled", "enable_ocr"],
            true
        ));
        assert!(setting_enabled(
            &settings,
            &["description_enabled", "alt_text_enabled"],
            false
        ));
        assert!(setting_enabled(&json!({}), &["ocr_enabled"], true));
    }

    #[test]
    fn ai_metadata_helpers_keep_visible_chinese_tags_only() {
        let payload = json!({
            "description": "海边日落",
            "ocr_text": "欢迎",
            "labels": ["safe", "wallpaper", "风景"],
            "suggested_tags": ["天空", "adult", ""],
            "tags": ["城市"]
        });
        assert_eq!(metadata_description(&payload).as_deref(), Some("海边日落"));
        assert_eq!(ocr_text(&payload), "欢迎");
        assert_eq!(audit_ocr_text(&payload, &json!({"ocr_enabled": false})), "");
        assert_eq!(
            suggested_tags(&payload),
            vec!["城市", "天空", "安全", "成人内容", "风景"]
        );
    }

    #[test]
    fn merge_ai_result_preserves_primary_decision_and_adds_enrichment() {
        let mut result = AiModerationResult {
            passed: true,
            risk_level: Some("low".to_string()),
            categories: None,
            labels: Some(vec!["安全".to_string()]),
            suggested_tags: None,
            tags: None,
            description: None,
            ocr_text: Some(String::new()),
            reason: Some("通过".to_string()),
            confidence: Some(0.9),
            model: None,
        };
        merge_ai_result(
            &mut result,
            &json!({
                "passed": false,
                "labels": ["风景"],
                "description": "山谷",
                "ocr_text": "文字",
                "model": "local"
            }),
        );
        assert!(result.passed);
        assert_eq!(result.description.as_deref(), Some("山谷"));
        assert_eq!(result.ocr_text.as_deref(), Some("文字"));
        assert_eq!(
            result.labels,
            Some(vec!["安全".to_string(), "风景".to_string()])
        );
        assert_eq!(result.model.as_deref(), Some("local"));
    }

    #[test]
    fn post_audit_manual_required_keeps_image_active() {
        let decision = manual_or_pass_decision(true);
        assert_eq!(decision.task_status, "manual_required");
        assert_eq!(decision.image_status, "active");
        assert_eq!(decision.result, "manual_required");
    }

    #[test]
    fn audit_failure_manual_strategy_keeps_image_active() {
        let decision = failure_decision(
            true,
            &json!({"failure_strategy":"manual_required"}),
            "down".to_string(),
        );
        assert_eq!(decision.task_status, "manual_required");
        assert_eq!(decision.image_status, "active");
    }

    #[tokio::test]
    async fn internal_file_token_is_appended_and_decodable() {
        let config = crate::app::AppConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "https://img.example.com".to_string(),
            session_secret: "test-secret".to_string(),
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
        let image_id = Uuid::from_u128(42);

        let url = append_internal_file_token(&state, image_id, "/files/a.jpg?download=1")
            .expect("append token");
        let token = url
            .split("token=")
            .nth(1)
            .and_then(|value| urlencoding::decode(value).ok())
            .expect("token");
        let claims = decode::<crate::models::TokenClaims>(
            &token,
            &DecodingKey::from_secret(state.config.session_secret.as_bytes()),
            &Validation::default(),
        )
        .expect("decode token")
        .claims;

        assert!(url.contains("&token="));
        assert_eq!(claims.sub, image_id.to_string());
        assert_eq!(claims.role, "file_read");
    }
}
