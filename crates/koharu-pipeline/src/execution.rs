use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context as _, Result, ensure};
use futures::{StreamExt as _, stream::FuturesUnordered};
use koharu_scene::{EntityId, Snapshot};

use crate::{
    Committer, ErrorKind, PipelineError, Progress, ProgressSink, Report, Request, RunStatus, Stage,
    StageOutput, StopToken,
    images::ImageCache,
    progress,
    resources::ResourceMonitor,
    scheduler::Scheduler,
    scope::NormalizedScope,
    stage_runner::{StageCompletion, StageJob, StageOutcome, StageRunner},
    stages::StageInput,
};

pub(crate) struct Execution<'a> {
    runner: Arc<StageRunner>,
    resources: Arc<ResourceMonitor>,
    committer: &'a mut dyn Committer,
    stop: StopToken,
    progress: Option<ProgressSink>,
    scope: NormalizedScope,
    scheduler: Scheduler,
    scene: Snapshot,
    /// Decoded source images, one cache per live page.
    ///
    /// A page's cache is dropped once that page has finished every stage, and
    /// every cache is cleared when the run ends. Stage batches no longer clear
    /// them: under concurrency another stage may still be decoding from a cache
    /// this one has finished with, so a page's decoded source image stays alive
    /// from Detection through Inpainting. The stage-overlap plan's memory
    /// estimate does not include those images; it is a trade-off to measure.
    images: BTreeMap<EntityId, Arc<ImageCache>>,
    /// Jobs dispatched per stage and not yet completed, including jobs that
    /// ended in failure or with `Stopped`.
    running: BTreeMap<Stage, usize>,
    /// Stages whose models are loaded and not released yet.
    resident: BTreeSet<Stage>,
    completed: usize,
    failure: Option<PipelineError>,
    base: koharu_scene::Revision,
    started: Instant,
    inpainting_mask: Option<crate::InpaintingMask>,
    released: Vec<(Stage, bool)>,
}

pub(crate) struct RunOutcome {
    pub(crate) result: std::result::Result<Report, PipelineError>,
    pub(crate) images: Vec<Arc<ImageCache>>,
}

/// The resident stage models that may be released right now, in release order.
///
/// Translation is released first and on its own: it is not part of the
/// canonical `Detection` -> `Ocr` -> `Inpainting` order and `release_task_models`
/// never covers it, so a resident translator is freed only here. The canonical
/// stages are released in that order and the walk stops at the first resident
/// stage whose work has not drained, which is what keeps `released_models`
/// ordered `Detection` -> `Ocr` -> `Inpainting`.
pub(crate) fn release_plan(
    resident: &BTreeSet<Stage>,
    running: &BTreeMap<Stage, usize>,
    stage_finished: impl Fn(Stage) -> bool,
) -> Vec<Stage> {
    let drained =
        |stage: Stage| running.get(&stage).copied().unwrap_or(0) == 0 && stage_finished(stage);
    let mut plan = Vec::with_capacity(3);
    if resident.contains(&Stage::Translation) && drained(Stage::Translation) {
        plan.push(Stage::Translation);
    }
    for stage in [Stage::Detection, Stage::Ocr, Stage::Inpainting] {
        if !resident.contains(&stage) {
            continue;
        }
        if !drained(stage) {
            break;
        }
        plan.push(stage);
    }
    plan
}

impl<'a> Execution<'a> {
    pub(crate) fn new(
        runner: Arc<StageRunner>,
        resources: Arc<ResourceMonitor>,
        snapshot: Snapshot,
        request: Request,
        committer: &'a mut dyn Committer,
    ) -> std::result::Result<Self, PipelineError> {
        let started = Instant::now();
        let base = snapshot.revision();
        let stages = request
            .operation
            .stages()
            .map_err(|error| PipelineError::new(ErrorKind::InvalidInput, None, error))?;
        let scope = NormalizedScope::new(&snapshot, &request.scope, &stages)
            .map_err(|error| PipelineError::new(ErrorKind::InvalidInput, None, error))?;
        let pages = scope.pages().to_vec();
        if let Some(mask) = request.inpainting_mask.as_ref()
            && (!pages.contains(&mask.page) || !stages.contains(&Stage::Inpainting))
        {
            return Err(PipelineError::new(
                ErrorKind::InvalidInput,
                Some(Stage::Inpainting),
                anyhow::anyhow!("the inpainting mask page is outside the inpainting scope"),
            ));
        }
        progress::emit(
            request.progress.as_ref(),
            Progress::Started {
                pages: pages.clone(),
                stages: stages.clone(),
            },
        );

        Ok(Self {
            runner,
            resources,
            committer,
            stop: request.stop,
            progress: request.progress,
            scope,
            scheduler: Scheduler::new(&pages, &stages),
            scene: snapshot,
            images: BTreeMap::new(),
            running: BTreeMap::new(),
            resident: BTreeSet::new(),
            completed: 0,
            failure: None,
            base,
            started,
            inpainting_mask: request.inpainting_mask,
            released: Vec::new(),
        })
    }

    pub(crate) async fn run(mut self) -> RunOutcome {
        if self.stopped() {
            self.release_everything().await;
            self.clear_images();
            let result = Ok(self.report(RunStatus::Stopped));
            return self.finish(result);
        }

        self.resources.start();
        self.resources.wait_for_sample().await;

        let runner = self.runner.clone();
        let mut running = FuturesUnordered::new();
        loop {
            while let Some(job) = self.take_ready_job().await {
                running.push(runner.run(job));
            }

            let Some(completion) = running.next().await else {
                break;
            };
            let completed_stage = completion.stage;
            self.job_finished(completed_stage);
            // A stopped or failed run keeps its remaining jobs running to the
            // end but discards their output, so nothing is committed past that
            // point. The drain still runs: a job that did finish may have been
            // the last one holding a model.
            if !self.stopped() && self.failure.is_none()
                && let Err(error) = self.apply_completion(completion).await
            {
                self.failure = Some(error);
            }
            self.release_drained().await;
        }

        self.release_everything().await;
        self.clear_images();
        let result = self.finalize();
        self.finish(result)
    }

    fn finish(self, result: std::result::Result<Report, PipelineError>) -> RunOutcome {
        RunOutcome {
            result,
            images: self.images.into_values().collect(),
        }
    }

    async fn take_ready_job(&mut self) -> Option<StageJob> {
        if self.stopped() || self.failure.is_some() {
            return None;
        }
        let (page, stage) = self.scheduler.start_next(&self.running)?;
        self.job_started(stage);
        let images = self
            .images
            .entry(page)
            .or_insert_with(|| Arc::new(ImageCache::default()))
            .clone();
        Some(StageJob::new(
            stage,
            StageInput::new(
                self.scene.clone(),
                page,
                self.scope.entities(),
                self.scope.region(page),
                images,
                self.inpainting_mask
                    .as_ref()
                    .filter(|mask| stage == Stage::Inpainting && mask.page == page)
                    .cloned(),
            ),
            self.stop.clone(),
            self.progress.clone(),
        ))
    }

    fn job_started(&mut self, stage: Stage) {
        *self.running.entry(stage).or_default() += 1;
        self.resident.insert(stage);
    }

    fn job_finished(&mut self, stage: Stage) {
        if let Some(count) = self.running.get_mut(&stage) {
            *count = count.saturating_sub(1);
        }
    }

    async fn apply_completion(
        &mut self,
        completion: StageCompletion,
    ) -> std::result::Result<(), PipelineError> {
        let StageCompletion {
            page,
            stage,
            model,
            elapsed,
            outcome,
        } = completion;
        match outcome? {
            StageOutcome::Stopped => {}
            StageOutcome::Skipped => {
                self.mark_page_stage(page, stage);
                progress::emit(self.progress.as_ref(), Progress::Skipped { page, stage });
            }
            StageOutcome::Patch(patch) => {
                // Commit before marking the stage complete. `mark_page_stage`
                // is what satisfies downstream prerequisites, so a stage that
                // is marked first lets the next one build its patch against a
                // scene that does not contain this output yet. Nothing in the
                // patch layer catches that: an out-of-date translation patch
                // commits cleanly and leaves a stale translation behind.
                if !self.commit_patch(page, stage, patch).await? {
                    return Ok(());
                }
                self.mark_page_stage(page, stage);
                progress::emit(
                    self.progress.as_ref(),
                    Progress::Finished {
                        page,
                        stage,
                        model,
                        elapsed,
                    },
                );
            }
        }
        Ok(())
    }

    async fn commit_patch(
        &mut self,
        page: EntityId,
        stage: Stage,
        patch: koharu_scene::Patch,
    ) -> std::result::Result<bool, PipelineError> {
        // Jobs now finish out of order, so a patch is usually rebased onto a
        // scene that has moved since it was built. `rebase_on` plus
        // `validate_on` reject a patch whose observations no longer hold, and
        // `validate_commit` rejects a committer that did not advance the
        // revision: both stay as the safety net for out-of-order commits.
        let patch = patch
            .rebase_on(&self.scene)
            .and_then(|patch| {
                patch.validate_on(&self.scene)?;
                Ok(patch.with_label(format!("Pipeline {stage} for page {page}")))
            })
            .context("failed to rebase stage output onto the latest scene")
            .map_err(|error| PipelineError::new(ErrorKind::InvalidOutput, Some(stage), error))?;
        if self.stopped() {
            return Ok(false);
        }

        let next = self
            .committer
            .commit(StageOutput { page, stage, patch })
            .await
            .with_context(|| format!("failed to commit {stage} output for page {page}"))
            .map_err(|error| PipelineError::new(ErrorKind::Commit, Some(stage), error))?;
        validate_commit(&self.scene, &next)
            .map_err(|error| PipelineError::new(ErrorKind::Commit, Some(stage), error))?;
        self.scene = next;
        Ok(true)
    }

    /// Releases the resident models whose work has drained, in release order.
    async fn release_drained(&mut self) {
        let plan = release_plan(&self.resident, &self.running, |stage| {
            self.scheduler.stage_finished(stage)
        });
        for stage in plan {
            debug_assert_eq!(
                self.running.get(&stage).copied().unwrap_or(0),
                0,
                "a stage model cannot be released while one of its jobs is running"
            );
            self.resident.remove(&stage);
            let released = self.runner.release_stage(stage).await;
            self.released.push((stage, released));
        }
    }

    /// Closes whatever work is still open and releases every resident model.
    ///
    /// Nothing runs past this point, so a stage whose work never finished — a
    /// stopped or failed run leaves its open work items `Running` — is closed
    /// first, which is what lets the drain release its model as well.
    async fn release_everything(&mut self) {
        for stage in Stage::ALL {
            self.scheduler.finish_stage(stage);
        }
        self.release_drained().await;
    }

    fn mark_page_stage(&mut self, page: EntityId, stage: Stage) {
        if self.scheduler.complete_stage(page, stage) {
            // The page is done: no stage will read its source image again.
            self.images.remove(&page);
        }
        self.completed += 1;
    }

    fn clear_images(&mut self) {
        for images in self.images.values() {
            images.clear();
        }
        self.images.clear();
    }

    fn stopped(&self) -> bool {
        self.stop.stopped()
    }

    fn finalize(&mut self) -> std::result::Result<Report, PipelineError> {
        if let Some(mut error) = self.failure.take() {
            error.released_models.extend(self.released.iter().copied());
            return Err(error);
        }
        if !self.stopped() && self.completed != self.scheduler.total() {
            return Err(PipelineError::new(
                ErrorKind::InvalidOutput,
                None,
                anyhow::anyhow!(
                    "pipeline scheduler stopped after {} of {} work items",
                    self.completed,
                    self.scheduler.total()
                ),
            ));
        }
        let status = if self.stopped() {
            RunStatus::Stopped
        } else {
            RunStatus::Completed
        };
        Ok(self.report(status))
    }

    fn report(&self, status: RunStatus) -> Report {
        Report {
            status,
            base: self.base,
            final_revision: self.scene.revision(),
            completed: self.completed,
            total: self.scheduler.total(),
            elapsed: self.started.elapsed(),
            released_models: self.released.clone(),
        }
    }
}

fn validate_commit(previous: &Snapshot, next: &Snapshot) -> Result<()> {
    ensure!(
        previous.project_id() == next.project_id(),
        "committer returned a snapshot from another project"
    );
    ensure!(
        next.revision() > previous.revision(),
        "committer did not advance the scene revision"
    );
    Ok(())
}
