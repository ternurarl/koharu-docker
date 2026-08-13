//! LLM/translation lifecycle routes. The loaded translation model is a
//! singleton pipeline configuration: GET describes it, PUT selects it,
//! DELETE resets to the default. Loading happens lazily on first translation.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::get,
};
use koharu_pipeline::PipelineConfig;
use koharu_translator::ModelSelection;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmState {
    pub model: ModelSelection,
    pub target_language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmLoadRequest {
    pub model: ModelSelection,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmCatalog {
    pub models: Vec<koharu_translator::Model>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/llm/current",
            get(get_current_llm).put(put_current_llm).delete(delete_current_llm),
        )
        .route("/llm/catalog", get(get_catalog))
}

async fn current_state() -> ApiResult<LlmState> {
    let config = PipelineConfig::load()?;
    let translation = config.read()?.translation.clone();
    Ok(LlmState {
        model: translation.model,
        target_language: translation.target_language.tag().to_owned(),
        instructions: translation.instructions,
    })
}

async fn get_current_llm(State(_app): State<AppState>) -> ApiResult<Json<LlmState>> {
    Ok(Json(current_state().await?))
}

async fn put_current_llm(
    State(_app): State<AppState>,
    Json(req): Json<LlmLoadRequest>,
) -> ApiResult<StatusCode> {
    let config = PipelineConfig::load()?;
    {
        let mut value = config.write()?;
        value.translation.model = req.model;
        value.translation.instructions = req.instructions;
        value.save()?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_current_llm(State(_app): State<AppState>) -> ApiResult<StatusCode> {
    let config = PipelineConfig::load()?;
    {
        let mut value = config.write()?;
        value.translation.model = ModelSelection::default();
        value.translation.instructions = None;
        value.save()?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_catalog(State(_app): State<AppState>) -> ApiResult<Json<LlmCatalog>> {
    let models = koharu_translator::Translator::models()
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(LlmCatalog { models }))
}
