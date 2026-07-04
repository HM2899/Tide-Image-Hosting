use crate::app::AppState;
use crate::error::AppResult;
use crate::services::defaults;
use axum::Json;
use axum::extract::State;
use axum::routing::get;

pub fn public_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/site", get(site))
        .route("/theme", get(theme))
        .route("/upload", get(upload))
        .route("/random", get(random))
}

async fn site(
    State(state): State<AppState>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let value =
        defaults::public_site_setting(&state.pool, "site", defaults::site_settings()).await?;
    Ok(Json(tide_shared::ok(value)))
}

async fn theme(
    State(state): State<AppState>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let value =
        defaults::public_theme_setting(&state.pool, "theme", defaults::theme_settings()).await?;
    Ok(Json(tide_shared::ok(value)))
}

async fn upload(
    State(state): State<AppState>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let value =
        defaults::public_site_setting(&state.pool, "upload", defaults::upload_settings()).await?;
    Ok(Json(tide_shared::ok(value)))
}

async fn random(
    State(state): State<AppState>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let value =
        defaults::public_site_setting(&state.pool, "random", defaults::random_settings()).await?;
    Ok(Json(tide_shared::ok(value)))
}
