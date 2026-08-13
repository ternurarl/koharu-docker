//! Axum router assembly. Every domain route lives under /api/v1.

use axum::{Router, extract::DefaultBodyLimit};
use tower_http::cors::CorsLayer;

use crate::routes;
use crate::state::AppState;

const MAX_BODY_SIZE: usize = 1024 * 1024 * 1024;

/// Ready-to-serve router with CORS and a 1 GiB body limit.
pub fn router(app: AppState) -> Router {
    let api = routes::meta::router()
        .merge(routes::operations::router())
        .merge(routes::llm::router())
        .merge(routes::projects::router())
        .merge(routes::config::router())
        .merge(routes::pipelines::router())
        .merge(crate::events::router())
        .with_state(app);

    Router::new()
        .nest("/api/v1", api)
        .layer(CorsLayer::very_permissive())
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
}
