//! Typed API errors. Every handler returns `ApiResult<T>`; failures land as
//! JSON `{ "status": u16, "message": String }` matching the legacy contract.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status: status.as_u16(),
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// A failure whose details belong in the server log, not in the response.
    ///
    /// The anyhow chain can carry a request URL or an upstream body, so the
    /// client gets the outermost message and the rest is logged.
    pub fn service_unavailable(error: anyhow::Error) -> Self {
        tracing::error!("request could not be served: {error:#}");
        Self::new(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
    }

    pub fn internal(error: anyhow::Error) -> Self {
        tracing::error!("request failed: {error:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::internal(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self)).into_response()
    }
}
