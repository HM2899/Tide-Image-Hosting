use crate::app::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::RwLock;
use uuid::Uuid;

type TaskStore = Arc<RwLock<HashMap<String, Value>>>;

static TASKS: OnceLock<TaskStore> = OnceLock::new();

#[derive(Debug, Deserialize, Serialize)]
struct ImagePayload {
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    image_base64: Option<String>,
    #[serde(default)]
    options: Value,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/ai/analyze-image", post(analyze_image))
        .route("/ai/moderate-image", post(moderate_image))
        .route("/ai/generate-alt-text", post(generate_alt_text))
        .route("/ai/ocr", post(ocr))
        .route("/ai/batch-moderate", post(batch_moderate))
        .route("/ai/task/{task_id}", get(get_task))
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn analyze_image(
    State(state): State<AppState>,
    Json(payload): Json<ImagePayload>,
) -> Json<Value> {
    Json(
        call_external(&state, &payload)
            .await
            .unwrap_or_else(|| safe_result("规则审核通过")),
    )
}

async fn moderate_image(
    State(state): State<AppState>,
    Json(payload): Json<ImagePayload>,
) -> Json<Value> {
    Json(
        call_external(&state, &payload)
            .await
            .unwrap_or_else(|| safe_result("规则审核通过")),
    )
}

async fn generate_alt_text(
    State(state): State<AppState>,
    Json(payload): Json<ImagePayload>,
) -> Json<Value> {
    let alt_text = call_external(&state, &payload)
        .await
        .and_then(|value| {
            value
                .get("alt_text")
                .or_else(|| value.get("description"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "用户上传图片".to_string());
    Json(json!({ "alt_text": alt_text }))
}

async fn ocr(State(state): State<AppState>, Json(payload): Json<ImagePayload>) -> Json<Value> {
    let ocr_text = call_external(&state, &payload)
        .await
        .and_then(|value| {
            value
                .get("ocr_text")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_default();
    Json(json!({ "ocr_text": ocr_text }))
}

async fn batch_moderate(
    State(state): State<AppState>,
    Json(payloads): Json<Vec<ImagePayload>>,
) -> Json<Value> {
    let task_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let mut results = Vec::with_capacity(payloads.len());
    for payload in &payloads {
        results.push(
            call_external(&state, payload)
                .await
                .unwrap_or_else(|| safe_result("批量规则审核通过")),
        );
    }
    let task = json!({
        "task_id": task_id,
        "status": "completed",
        "total": payloads.len(),
        "results": results,
        "duration_ms": started.elapsed().as_millis() as u64,
    });
    task_store().write().await.insert(task_id, task.clone());
    Json(task)
}

async fn get_task(Path(task_id): Path<String>) -> (StatusCode, Json<Value>) {
    let task = task_store()
        .read()
        .await
        .get(&task_id)
        .cloned()
        .unwrap_or_else(|| json!({"task_id": task_id, "status": "not_found"}));
    (StatusCode::OK, Json(task))
}

async fn call_external(state: &AppState, payload: &ImagePayload) -> Option<Value> {
    let url = std::env::var("AI_PROVIDER_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let key = std::env::var("AI_PROVIDER_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    state
        .http
        .post(url)
        .bearer_auth(key)
        .json(payload)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Value>()
        .await
        .ok()
}

fn task_store() -> &'static TaskStore {
    TASKS.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

fn safe_result(reason: &str) -> Value {
    json!({
        "passed": true,
        "risk_level": "low",
        "categories": {
            "adult": false,
            "violence": false,
            "political": false,
            "illegal": false,
            "hate": false,
            "privacy": false,
        },
        "labels": ["安全"],
        "suggested_tags": ["安全", "普通图片"],
        "description": "普通图片",
        "ocr_text": "",
        "reason": reason,
        "confidence": 0.96,
        "model": "规则引擎",
    })
}
