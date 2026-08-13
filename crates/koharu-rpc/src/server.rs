//! Server bootstrap - configures the runtime store, initializes ML, and
//! attaches the router to a TCP listener.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::api;
use crate::state::App;

/// Run the headless HTTP server until the process is stopped.
///
/// host/port are the bind address; data is the persistent root (projects +
/// model packages), defaulting to the platform data dir + "Koharu". cpu
/// forces CPU inference.
pub async fn serve(host: &str, port: u16, data: Option<PathBuf>, cpu: bool) -> Result<()> {
    let data_dir = data.unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Koharu")
    });
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create {}", data_dir.display()))?;

    // Runtime package store (torch/llama/diffusion artifacts) lives under the
    // data directory so it can be persisted as a volume.
    koharu_runtime::Store::configure(data_dir.join("packages"))?;

    koharu_ml::init()
        .await
        .context("failed to initialize the ML runtime")?;
    let device = koharu_ml::device(cpu);

    tracing::info!(
        data_dir = %data_dir.display(),
        device = ?device.backend,
        "starting Koharu headless server"
    );

    let app = App::new(device, data_dir)?;
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("failed to bind {host}:{port}"))?;
    axum::serve(listener, api::router(app))
        .await
        .context("server error")
}
