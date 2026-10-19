//! GET/PATCH /config and provider secret routes.
//!
//! Configuration is section-based (config.toml): "pipeline", "providers",
//! "typesetting". Credentials never enter that file: they arrive with their own
//! request.
//!
//! Where a credential is held depends on the provider. `deepseek` and
//! `openai-compatible` take the key from the request, keep it in memory for the
//! one task that uses it, and drop it when that task ends, so a shared host
//! never writes it anywhere. The other providers keep reading the OS credential
//! service, mirroring the desktop preferences layer.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
};
use koharu_pipeline::PipelineConfig;
use koharu_renderer::TypesettingConfig;
use koharu_translator::{Provider, ProvidersConfig};
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
    State(app): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ProviderSecretRequest>,
) -> ApiResult<StatusCode> {
    match id.parse::<Provider>() {
        Ok(provider) if provider.takes_task_secret() => app.secrets.lock().insert(provider, req.secret),
        // Anything else, including an id this build does not know, keeps the
        // previous behaviour and lands in the OS credential service under `id`.
        _ => {
            let secret = koharu_secrets::SecretString::from(req.secret);
            koharu_secrets::set(&id, &secret).map_err(ApiError::internal)?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_provider_secret(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    match id.parse::<Provider>() {
        Ok(provider) if provider.takes_task_secret() => app.secrets.lock().remove(provider),
        _ => koharu_secrets::delete(&id).map_err(ApiError::internal)?,
    }
    Ok(StatusCode::NO_CONTENT)
}
