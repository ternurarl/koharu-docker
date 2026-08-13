//! GET/PATCH /config and provider secret routes.
//!
//! Configuration is section-based (config.toml): "pipeline", "providers",
//! "typesetting". Provider API keys are stored in the OS keyring under the
//! provider id, mirroring the desktop preferences layer.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
};
use koharu_pipeline::PipelineConfig;
use koharu_renderer::TypesettingConfig;
use koharu_translator::ProvidersConfig;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub pipeline: PipelineConfig,
    pub providers: ProvidersConfig,
    pub typesetting: TypesettingConfig,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfigPatch {
    pub pipeline: Option<PipelineConfig>,
    pub providers: Option<ProvidersConfig>,
    pub typesetting: Option<TypesettingConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSecretRequest {
    pub secret: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/config", get(get_config).patch(patch_config))
        .route(
            "/config/providers/{id}/secret",
            put(set_provider_secret).delete(clear_provider_secret),
        )
}

fn read_config() -> ApiResult<AppConfig> {
    let pipeline = (*PipelineConfig::load()?.read()?).clone();
    let providers = (*ProvidersConfig::load()?.read()?).clone();
    let typesetting = (*TypesettingConfig::load()?.read()?).clone();
    Ok(AppConfig {
        pipeline,
        providers,
        typesetting,
    })
}

async fn get_config(State(_app): State<AppState>) -> ApiResult<Json<AppConfig>> {
    Ok(Json(read_config()?))
}

async fn patch_config(
    State(_app): State<AppState>,
    Json(patch): Json<AppConfigPatch>,
) -> ApiResult<Json<AppConfig>> {
    if let Some(pipeline) = patch.pipeline {
        let config = PipelineConfig::load()?;
        let mut value = config.write()?;
        *value = pipeline;
        value.save()?;
    }
    if let Some(providers) = patch.providers {
        let config = ProvidersConfig::load()?;
        let mut value = config.write()?;
        *value = providers;
        value.save()?;
    }
    if let Some(typesetting) = patch.typesetting {
        let config = TypesettingConfig::load()?;
        let mut value = config.write()?;
        *value = typesetting;
        value.save()?;
    }
    Ok(Json(read_config()?))
}

async fn set_provider_secret(
    State(_app): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ProviderSecretRequest>,
) -> ApiResult<StatusCode> {
    let secret = koharu_secrets::SecretString::from(req.secret);
    koharu_secrets::set(&id, &secret).map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_provider_secret(
    State(_app): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    koharu_secrets::delete(&id).map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}
