//! GET /operations, DELETE /operations/{id} - job registry + cancellation.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::{AppState, JobSummary};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListOperationsResponse {
    pub operations: Vec<JobSummary>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/operations", get(list_operations))
        .route("/operations/{id}", delete(cancel_operation))
}

async fn list_operations(State(app): State<AppState>) -> ApiResult<Json<ListOperationsResponse>> {
    Ok(Json(ListOperationsResponse {
        operations: app.jobs_snapshot(),
    }))
}

async fn cancel_operation(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if let Some(stop) = app.cancels.lock().get(&id) {
        stop.stop();
    }
    app.jobs.lock().remove(&id);
    Ok(StatusCode::NO_CONTENT)
}
