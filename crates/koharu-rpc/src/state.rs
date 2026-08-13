//! Shared application state threaded through every State<AppState> handler.
//!
//! Owns the process-wide pipeline, renderer, event bus, job/cancel registries,
//! and the currently open project. Everything here is Tauri-free; the desktop
//! app manages its own equivalent state through Tauri's managed state.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use koharu_pipeline::StopToken;
use koharu_scene::EntityId;
use parking_lot::Mutex;
use serde::Serialize;

use crate::project::{Project, ProjectLibrary};

pub type AppState = Arc<App>;

#[derive(Clone, Serialize)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: AppEvent,
}

/// Broadcast bus with a monotonic sequence number for SSE "id:" fields.
///
/// Subscribers observe the live tail; a fresh connection is seeded with a
/// snapshot by the /events handler before switching to live delivery.
#[derive(Clone)]
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<SequencedEvent>,
    seq: Arc<AtomicU64>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            tx,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: AppEvent) {
        let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.tx.send(SequencedEvent { seq, event });
    }

    pub fn latest_seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

/// Server-wide state shared behind an Arc.
pub struct App {
    pub version: String,
    pub device: koharu_ml::Device,
    pub pipeline: koharu_pipeline::Pipeline,
    pub renderer: koharu_renderer::Renderer,
    pub project: tokio::sync::Mutex<Option<Project>>,
    pub library: ProjectLibrary,
    pub jobs: Mutex<HashMap<String, JobSummary>>,
    pub cancels: Mutex<HashMap<String, StopToken>>,
    pub bus: EventBus,
    pub data_dir: PathBuf,
}

impl App {
    /// Assemble the live state. device is the runtime-selected accelerator;
    /// data_dir is the server's persistent root (projects + model packages).
    pub fn new(device: koharu_ml::Device, data_dir: PathBuf) -> anyhow::Result<AppState> {
        let pipeline = koharu_pipeline::Pipeline::load(device.clone())?;
        let renderer = koharu_renderer::Renderer::new()?;
        let library = ProjectLibrary::new(data_dir.join("projects"))?;
        Ok(Arc::new(Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            device,
            pipeline,
            renderer,
            project: tokio::sync::Mutex::new(None),
            library,
            jobs: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            bus: EventBus::new(),
            data_dir,
        }))
    }

    /// Human-readable accelerator label for /meta.
    pub fn ml_device_label(&self) -> String {
        match &self.device.backend {
            koharu_runtime::Backend::Cpu => "cpu".to_owned(),
            koharu_runtime::Backend::Cuda => "cuda".to_owned(),
            koharu_runtime::Backend::Rocm => "rocm".to_owned(),
            koharu_runtime::Backend::Vulkan => "vulkan".to_owned(),
            koharu_runtime::Backend::Metal => "metal".to_owned(),
            koharu_runtime::Backend::Other(name) => name.clone(),
        }
    }

    pub fn jobs_snapshot(&self) -> Vec<JobSummary> {
        self.jobs.lock().values().cloned().collect()
    }
}

/// One pipeline job, surfaced through /operations and /events.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub id: String,
    pub kind: String,
    pub status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// Events streamed over /events. The "event" field is the discriminator;
/// frames carry no SSE event: name, matching the legacy contract.
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum AppEvent {
    Snapshot { jobs: Vec<JobSummary> },
    JobStarted { id: String, kind: String },
    JobProgress(JobProgress),
    JobFinished(JobFinished),
    JobWarning(JobWarning),
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobProgress {
    pub id: String,
    pub status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_page: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overall_percent: Option<f32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobFinished {
    pub id: String,
    pub status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobWarning {
    pub id: String,
    pub message: String,
}
