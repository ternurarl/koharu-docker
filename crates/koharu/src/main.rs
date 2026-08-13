#![cfg_attr(
    all(not(debug_assertions), feature = "gui"),
    windows_subsystem = "windows"
)]

use clap::Parser as _;
use koharu::panic;
use koharu::sentry;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[derive(clap::Parser)]
#[command(version, about)]
struct Cli {
    /// Run the headless HTTP server instead of the desktop GUI.
    #[arg(long)]
    headless: bool,

    /// Bind address for the headless server.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// Bind port for the headless server.
    #[arg(long, default_value_t = 4000)]
    port: u16,

    /// Persistent data directory (projects + model packages).
    #[arg(long)]
    data: Option<std::path::PathBuf>,

    /// Force CPU inference, ignoring any discovered accelerator.
    #[arg(long)]
    cpu: bool,
}

#[tokio::main]
#[cfg_attr(feature = "gui", tauri::cef_entry_point)]
async fn main() {
    let cli = Cli::parse();
    let _guard = sentry::initialize();
    panic::install();
    let filter = tracing_subscriber::filter::EnvFilter::builder()
        .with_default_directive(tracing::Level::INFO.into())
        .from_env_lossy();
    tracing_subscriber::registry()
        .with(filter)
        .with(sentry::tracing_layer())
        .with(koharu::tracing::TimingLayer::new())
        .init();

    if cli.headless {
        run_headless(cli).await;
        return;
    }
    run_gui().await;
}

#[cfg(feature = "headless")]
async fn run_headless(cli: Cli) {
    if let Err(error) = koharu_rpc::serve(&cli.host, cli.port, cli.data, cli.cpu).await {
        tracing::error!(%error, "headless server failed");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "headless"))]
async fn run_headless(_cli: Cli) {
    eprintln!(
        "error: --headless requested but this binary was not built with the headless feature"
    );
    std::process::exit(2);
}

#[cfg(feature = "gui")]
async fn run_gui() {
    tokio::task::block_in_place(|| koharu_app::run(tauri::generate_context!()))
        .expect("failed to run the desktop application");
}

#[cfg(not(feature = "gui"))]
async fn run_gui() {
    eprintln!("error: this binary was built headless-only; the desktop GUI is unavailable");
    std::process::exit(2);
}
