//! POST /pipelines - start a pipeline run as a long-running operation.
//!
//! Returns an operationId. Progress, warnings, and completion flow through
//! the /events SSE stream. Cancellation goes to DELETE /operations/{id}.

use std::sync::Arc;

use anyhow::Context as _;
use axum::{Json, Router, extract::State, routing::post};
use koharu_pipeline::{
    Committer, Operation, Progress, Request, RunStatus, Scope, StageOutput, StopToken,
};
use koharu_scene::Snapshot;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppEvent, AppState, JobProgress, JobStatus, JobSummary};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPipelineRequest {
    #[serde(default)]
    pub operation: Operation,
    #[serde(default)]
    pub scope: Scope,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPipelineResponse {
    pub operation_id: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/pipelines", post(start_pipeline))
}

async fn start_pipeline(
    State(app): State<AppState>,
    Json(req): Json<StartPipelineRequest>,
) -> ApiResult<Json<StartPipelineResponse>> {
    let snapshot = {
        let guard = app.project.lock().await;
        let project = guard
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("no project open"))?;
        project.snapshot()
    };

    let operation_id = Uuid::new_v4().to_string();
    let stop = StopToken::default();
    app.cancels.lock().insert(operation_id.clone(), stop.clone());
    app.jobs.lock().insert(
        operation_id.clone(),
        JobSummary {
            id: operation_id.clone(),
            kind: "pipeline".to_owned(),
            status: JobStatus::Running,
            error: None,
            stage: None,
            page: None,
            model: None,
            completed: None,
            total: None,
        },
    );
    app.bus.publish(AppEvent::JobStarted {
        id: operation_id.clone(),
        kind: "pipeline".to_owned(),
    });

    let app_c = app.clone();
    let op_id_c = operation_id.clone();
    tokio::spawn(async move {
        let progress = Arc::new(Mutex::new((0_usize, 0_usize)));
        let progress_app = app_c.clone();
        let progress_id = op_id_c.clone();
        let request = Request {
            operation: req.operation,
            scope: req.scope,
            stop: stop.clone(),
            progress: Some(Arc::new(move |event: Progress| {
                let update = match event {
                    Progress::Started { pages, stages } => {
                        let mut progress = progress.lock();
                        *progress = (0, pages.len().saturating_mul(stages.len()));
                        Some((progress.0, progress.1, None, None, None))
                    }
                    Progress::Loading { page, stage, model } => {
                        let progress = progress.lock();
                        Some((progress.0, progress.1, Some(page), Some(stage), Some(model)))
                    }
                    Progress::Finished { page, stage, model, .. } => {
                        let mut progress = progress.lock();
                        progress.0 = progress.0.saturating_add(1).min(progress.1);
                        Some((progress.0, progress.1, Some(page), Some(stage), Some(model)))
                    }
                    Progress::Skipped { page, stage } => {
                        let mut progress = progress.lock();
                        progress.0 = progress.0.saturating_add(1).min(progress.1);
                        Some((progress.0, progress.1, Some(page), Some(stage), None))
                    }
                    Progress::Running { .. } => None,
                };
                let Some((completed, total, page, stage, model)) = update else {
                    return;
                };
                let stage_name = stage.map(|stage| stage.to_string());
                let overall_percent = (total > 0).then(|| (completed as f32 / total as f32) * 100.0);
                {
                    let mut jobs = progress_app.jobs.lock();
                    if let Some(job) = jobs.get_mut(&progress_id) {
                        job.completed = Some(completed);
                        job.total = Some(total);
                        job.page = page;
                        job.stage = stage_name.clone();
                        job.model = model;
                    }
                }
                progress_app.bus.publish(AppEvent::JobProgress(JobProgress {
                    id: progress_id.clone(),
                    status: JobStatus::Running,
                    stage: stage_name,
                    current_page: page,
                    completed: Some(completed),
                    total: Some(total),
                    overall_percent,
                }));
            })),
            inpainting_mask: None,
        };

        struct SessionCommitter {
            app: AppState,
        }

        #[async_trait::async_trait]
        impl Committer for SessionCommitter {
            async fn commit(&mut self, output: StageOutput) -> anyhow::Result<Snapshot> {
                let mut guard = self.app.project.lock().await;
                let project = guard.as_mut().context("no project is open")?;
                let commit = project.session.commit(output.patch).await?;
                Ok(commit.snapshot)
            }
        }

        let mut committer = SessionCommitter { app: app_c.clone() };
        let result = app_c.pipeline.execute(snapshot, request, &mut committer).await;
        let (status, error) = match result {
            Ok(report) if report.status == RunStatus::Stopped => (JobStatus::Cancelled, None),
            Ok(_) => (JobStatus::Completed, None),
            Err(error) => {
                tracing::error!(operation_id = %op_id_c, "pipeline run failed: {error:#}");
                (JobStatus::Failed, Some(format!("{error:#}")))
            }
        };

        app_c.cancels.lock().remove(&op_id_c);
        if let Some(job) = app_c.jobs.lock().get_mut(&op_id_c) {
            job.status = status;
            job.error = error.clone();
        }
        app_c.bus.publish(AppEvent::JobFinished(crate::state::JobFinished {
            id: op_id_c.clone(),
            status,
            error,
        }));
    });

    Ok(Json(StartPipelineResponse { operation_id }))
}
