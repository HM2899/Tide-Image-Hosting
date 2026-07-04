use crate::app::AppState;
use axum::Router;

mod admin;
mod ai;
mod auth;
mod images;
mod settings;
mod user;

pub fn api_router(state: AppState) -> Router<AppState> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/user", user::router())
        .nest("/images", images::router())
        .nest("/guest/images", images::guest_router())
        .nest("/public", images::public_image_router())
        .nest("/tags", images::tag_router())
        .nest("/storage", images::storage_router())
        .nest("/admin", admin::router())
        .nest("/settings", settings::public_router())
        .with_state(state)
}

pub fn public_router(state: AppState) -> Router<AppState> {
    Router::new()
        .merge(ai::router())
        .route("/random", axum::routing::get(images::random))
        .route(
            "/files/{*object_key}",
            axum::routing::get(images::local_file),
        )
        .with_state(state)
}
