mod app;
mod auth;
mod error;
mod models;
mod routes;
mod services;
mod storage;

use app::{AppConfig, AppState};
use axum::{
    Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if env::args().any(|arg| arg == "--healthcheck") {
        println!("ok");
        return Ok(());
    }

    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();

    let config = AppConfig::from_env()?;
    let pool = sqlx::PgPool::connect(&config.database_url).await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    services::bootstrap::ensure_initial_state(&pool, &config).await?;

    let state = AppState::new(pool, config.clone());
    services::tasks::spawn_backup_scheduler(state.clone());
    let frontend_dir = frontend_dir();
    let root_frontend_dir = frontend_dir.clone();
    let index_frontend_dir = frontend_dir.clone();
    tracing::info!(path = %frontend_dir.display(), "serving frontend");
    let app = Router::new()
        .route(
            "/",
            get(move || {
                let frontend_dir = root_frontend_dir.clone();
                async move { serve_index(frontend_dir).await }
            }),
        )
        .route(
            "/index.html",
            get(move || {
                let frontend_dir = index_frontend_dir.clone();
                async move { serve_index(frontend_dir).await }
            }),
        )
        .route("/favicon.ico", get(favicon))
        .nest("/api", routes::api_router(state.clone()))
        .merge(routes::public_router(state.clone()))
        .fallback_service(ServeDir::new(frontend_dir))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!(addr, "tide server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn frontend_dir() -> PathBuf {
    if let Ok(path) = env::var("FRONTEND_DIR") {
        let path = PathBuf::from(path);
        if path.exists() {
            return path;
        }
    }
    let cwd_frontend = PathBuf::from("frontend");
    if cwd_frontend.exists() {
        return cwd_frontend;
    }
    if let Ok(exe) = env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let beside_exe = parent.join("frontend");
        if beside_exe.exists() {
            return beside_exe;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("frontend")
}

async fn serve_index(frontend_dir: PathBuf) -> Response {
    let path = frontend_dir.join("index.html");
    match tokio::fs::read_to_string(path).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
                (header::PRAGMA, "no-cache"),
                (header::EXPIRES, "0"),
            ],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn favicon() -> StatusCode {
    StatusCode::NO_CONTENT
}
