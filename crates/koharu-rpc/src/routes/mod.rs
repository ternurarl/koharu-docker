//! Per-domain route modules. Each exposes a plain axum Router<AppState>
//! merged into the top-level router in api.rs.

pub mod config;
pub mod llm;
pub mod meta;
pub mod operations;
pub mod pipelines;
pub mod projects;
