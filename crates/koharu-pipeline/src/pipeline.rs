use std::sync::Arc;

use anyhow::{Context as _, Result};
use arc_swap::ArcSwap;
use koharu_config::Config;
use koharu_scene::Snapshot;

use crate::{
    Committer, PipelineConfig, PipelineError, Report, Request, ResourceSnapshot, Stage,
    execution::{Execution, RunOutcome},
    resources::ResourceMonitor,
    stage_runner::StageRunner,
};

#[derive(Clone)]
pub struct Pipeline {
    current: Arc<ArcSwap<StageRunner>>,
    resources: Arc<ResourceMonitor>,
    execution: Arc<tokio::sync::Mutex<()>>,
}

impl Pipeline {
    pub fn load(device: koharu_ml::Device) -> Result<Self> {
        Self::from_config(
            PipelineConfig::load()?,
            koharu_translator::ProvidersConfig::load()?,
            device,
        )
    }

    #[tracing::instrument(skip_all)]
    pub fn from_config(
        config: Config<PipelineConfig>,
        providers: Config<koharu_translator::ProvidersConfig>,
        device: koharu_ml::Device,
    ) -> Result<Self> {
        let translator = koharu_translator::Translator::from_config(device.clone(), providers)?;
        let resources = ResourceMonitor::new(&device);
        let runner = {
            let value = config.read()?;
            StageRunner::new(&value, translator.clone(), &device, resources.clone())?
        };
        let current = Arc::new(ArcSwap::from_pointee(runner));
        let watched = current.clone();
        let watched_resources = resources.clone();
        let _watcher = tokio::runtime::Handle::try_current()
            .context("pipeline requires a Tokio runtime")?
            .spawn(async move {
                let mut changes = config.subscribe();
                while changes.changed().await.is_ok() {
                    let runner = config.read().and_then(|value| {
                        StageRunner::new(
                            &value,
                            translator.clone(),
                            &device,
                            watched_resources.clone(),
                        )
                    });
                    match runner {
                        Ok(runner) => watched.store(Arc::new(runner)),
                        Err(error) => tracing::error!(%error, "failed to reload pipeline"),
                    }
                }
            });
        Ok(Self {
            current,
            resources,
            execution: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn subscribe_resources(&self) -> tokio::sync::watch::Receiver<ResourceSnapshot> {
        self.resources.start();
        self.resources.subscribe()
    }

    #[tracing::instrument(skip_all)]
    pub async fn execute(
        &self,
        snapshot: Snapshot,
        request: Request,
        committer: &mut dyn Committer,
    ) -> TaskExecution {
        let _execution = self.execution.lock().await;
        let runner = self.current.load_full();
        let RunOutcome { result, images } = match Execution::new(
            runner.clone(),
            self.resources.clone(),
            snapshot,
            request,
            committer,
        ) {
            Ok(execution) => execution.run().await,
            Err(error) => RunOutcome {
                result: Err(error),
                images: Vec::new(),
            },
        };
        TaskExecution {
            result,
            runner,
            images,
        }
    }
}

pub struct TaskExecution {
    result: std::result::Result<Report, PipelineError>,
    runner: Arc<StageRunner>,
    images: Vec<Arc<crate::ImageCache>>,
}

impl TaskExecution {
    pub async fn release_resources(self) -> TaskRelease {
        let released_models = self.runner.release_task_models().await;
        for images in &self.images {
            images.clear();
        }
        let image_cache_items = self.images.iter().map(|images| images.len()).sum();
        TaskRelease {
            result: self.result,
            released_models,
            image_cache_items,
        }
    }
}

pub struct TaskRelease {
    pub result: std::result::Result<Report, PipelineError>,
    pub released_models: Vec<(Stage, bool)>,
    pub image_cache_items: usize,
}
