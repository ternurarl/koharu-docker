//! GET /meta - server metadata.

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaInfo {
    pub version: String,
    pub ml_device: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/meta", get(get_meta))
}

async fn get_meta(State(app): State<AppState>) -> ApiResult<Json<MetaInfo>> {
    Ok(Json(MetaInfo {
        version: app.version.clone(),
        ml_device: app.ml_device_label(),
    }))
}
