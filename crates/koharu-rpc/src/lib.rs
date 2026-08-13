//! Headless HTTP transport (REST + SSE) over Koharu's Tauri-free engine.
//!
//! Re-establishes the legacy /api/v1 REST surface on the rebuilt (0.66.x)
//! pipeline/scene/config/translator crates, with no Tauri or desktop coupling.

mod api;
mod error;
mod events;
mod project;
mod routes;
mod server;
mod state;

pub use error::{ApiError, ApiResult};
pub use server::serve;
pub use state::{App, AppEvent, AppState, EventBus, JobStatus, JobSummary};
