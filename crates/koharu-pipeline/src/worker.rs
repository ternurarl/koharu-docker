//! The task subprocess boundary.
//!
//! One pipeline task loads its models in a child of the current executable.
//! The parent exports its scene into a scratch project, the child opens that
//! scratch project and runs the pipeline there, and it sends each committed
//! stage patch back so the parent can commit it to the session it already
//! owns. The child exits after `TaskExecution::release_resources`. Exiting is
//! what returns the anonymous mappings, heap, memfd allocations, and swap
//! pages the model runtimes keep after their Rust owners are dropped.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use koharu_scene::{Patch, PatchTransfer, Revision, Snapshot};
use revision::{DeserializeRevisioned, SerializeRevisioned};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    process::Command,
};

use crate::{
    Committer, ErrorKind, Pipeline, PipelineError, Report, Request, RunStatus, Stage, StageOutput,
    StopToken,
    pipeline::TaskRelease,
    progress::{self, Progress},
};

const WORKER_BINARY: &str = "pipeline-task";
const FRAME_LIMIT: usize = 512 * 1024 * 1024;

/// Runs `request` against `snapshot` inside a task subprocess.
///
/// The child works on a scratch copy of `snapshot`, so it never takes the
/// project lock the parent already holds. Every patch the child commits is
/// committed again by `committer` on the parent's session, in the same order.
pub(crate) async fn execute(
    snapshot: Snapshot,
    request: Request,
    committer: &mut dyn Committer,
) -> Result<TaskRelease> {
    let scratch = tempfile::tempdir().context("failed to create the task scratch project")?;
    export_scene(&snapshot, scratch.path()).await?;

    let executable = worker_executable()?;
    let mut child = Command::new(executable)
        .arg(scratch.path())
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("failed to start the pipeline task subprocess")?;
    let mut stdin = child
        .stdin
        .take()
        .context("pipeline task subprocess has no standard input")?;
    let mut stdout = child
        .stdout
        .take()
        .context("pipeline task subprocess has no standard output")?;

    let stop = request.stop.clone();
    let progress = request.progress.clone();
    // The scratch copy has its own document and its own revision sequence, so
    // the child cannot read either off the patches it builds. The parent sends
    // its own, and the child stamps every transfer with them.
    let project = snapshot.project_id().document();
    let base = snapshot.revision();
    let assignment = WorkerAssignment::from_request(request, project, base)?;
    let mut input = encode(&assignment)?;
    stdin
        .write_all(&input)
        .await
        .context("failed to send the task assignment")?;
    input.clear();
    // The cancel byte rides the same stdin as the assignment. The child reads
    // the assignment by its length prefix first, so the byte never lands
    // inside a frame.
    let cancel = async {
        stop.cancelled().await;
        let _ = stdin.write_all(&[1]).await;
    };
    let read = async {
        let mut scene = snapshot;
        let mut outcome = None;
        loop {
            let Some(frame) = read_frame(&mut stdout).await? else {
                break;
            };
            match decode::<WorkerEvent>(&frame)? {
                WorkerEvent::Progress(event) => {
                    progress::emit(progress.as_ref(), event.try_into()?);
                }
                WorkerEvent::Patch(transfer) => {
                    let stage = parse_stage(&transfer.stage)?;
                    let page = transfer.page;
                    let patch = Patch::from_transfer(&scene, transfer)
                        .context("task subprocess returned a patch for a different scene")?;
                    scene = committer
                        .commit(StageOutput { page, stage, patch })
                        .await
                        .with_context(|| format!("failed to commit {stage} output for page {page}"))?;
                }
                WorkerEvent::Finished(finished) => {
                    outcome = Some(finished);
                    break;
                }
            }
        }
        outcome.context("task subprocess closed its output before reporting an outcome")
    };

    // The outcome always comes from `read`. Cancelling only has to deliver the
    // stop byte, after which the child finishes the run and reports the same
    // outcome, so the two branches never compete over what the caller sees.
    tokio::pin!(read);
    let finished = tokio::select! {
        outcome = &mut read => outcome?,
        () = cancel => read.await?,
    };
    // The child's cancel reader is a blocking read on this pipe, and its
    // runtime shutdown waits for that read to finish before the process can
    // exit. Closing stdin is what releases it, so dropping the handle here is
    // what lets `wait` return instead of deadlocking against the child.
    drop(stdin);
    let status = child
        .wait()
        .await
        .context("failed to wait for the pipeline task subprocess")?;
    if !status.success() {
        bail!("pipeline task subprocess exited with {status}");
    }
    // The child has its own stop flag. The parent's token stays as the caller
    // left it, so a caller can reuse it for the next task.
    let _scratch = scratch;
    finished.into_release()
}

/// Locates the task subprocess binary next to the running executable.
///
/// Tests run inside the package's test harness, which lives beside the
/// compiled binaries, so the same lookup works for the application and the
/// lifecycle test.
fn worker_executable() -> Result<PathBuf> {
    let current = std::env::current_exe().context("failed to locate the current executable")?;
    let directory = current
        .parent()
        .context("current executable has no directory")?;
    let name = format!("{WORKER_BINARY}{}", std::env::consts::EXE_SUFFIX);
    let candidate = directory.join(&name);
    if candidate.is_file() {
        return Ok(candidate);
    }
    // `cargo test` places the harness in `deps` and the binaries one level up.
    let sibling = directory.join("..").join(name);
    if sibling.is_file() {
        return Ok(sibling);
    }
    bail!("pipeline task subprocess binary {WORKER_BINARY} was not found next to the application")
}

/// Copies `snapshot` into a project directory a subprocess can open.
///
/// The copy gets a fresh document identity, so a patch built against it cannot
/// be applied to the parent's project by accident. The caller sends the
/// transfer form of each patch instead, which the parent rebuilds against its
/// own snapshot.
async fn export_scene(snapshot: &Snapshot, directory: &Path) -> Result<()> {
    let checkpoint = snapshot.checkpoint()?;
    let mut blobs = Vec::new();
    for id in snapshot.blob_ids() {
        blobs.push((id, snapshot.read_blob(id).await?));
    }
    koharu_scene::Session::from_checkpoint(directory, &checkpoint, blobs)
        .await
        .context("failed to export the scene for the task subprocess")?;
    Ok(())
}

/// Entry point of the task subprocess.
///
/// The child reads one assignment, runs the pipeline against the scratch
/// project the parent exported, and writes one frame per progress event and
/// committed patch.
pub async fn run(project: PathBuf) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let assignment = decode::<WorkerAssignment>(&read_assignment(&mut stdin).await?)
        .context("pipeline task assignment is not readable")?;
    drop(stdin);
    let mut stdout = tokio::io::stdout();
    let stop = StopToken::default();
    let cancelling = stop.clone();
    // This read stays on a plain thread on purpose. A Tokio blocking task
    // would hold up `BlockingPool::shutdown`, and this read only finishes when
    // the parent sends the stop byte or closes the pipe, so the process could
    // never exit. A detached thread is abandoned when `main` returns instead.
    let _cancel = std::thread::spawn(move || {
        let mut byte = [0_u8; 1];
        if std::io::Read::read_exact(&mut std::io::stdin(), &mut byte).is_ok() {
            cancelling.stop();
        }
    });
    let outcome = match run_assignment(&project, assignment, &mut stdout, stop).await {
        Ok(outcome) => outcome,
        Err(error) => WorkerOutcome::Failed(WorkerFailure::from_error(&error)),
    };
    write_frame(&mut stdout, &WorkerEvent::Finished(outcome)).await?;
    stdout.shutdown().await?;
    Ok(())
}

async fn run_assignment(
    project: &Path,
    assignment: WorkerAssignment,
    output: &mut tokio::io::Stdout,
    stop: StopToken,
) -> Result<WorkerOutcome> {
    // The child resolves its runtimes from the same package store as the
    // parent. Left unset it would install its own copy of every package into
    // the default cache directory before the first task can start.
    koharu_runtime::Store::configure(assignment.store.as_str())
        .context("failed to adopt the parent's runtime package store")?;
    // The child initializes the same runtimes the parent did. A CPU-only parent
    // has no llama.cpp or stable-diffusion.cpp, and retrying their init here
    // would fail the task on a machine that has no GPU.
    match assignment.runtime {
        WorkerRuntime::Cpu => koharu_ml::init_cpu().await?,
        WorkerRuntime::Gpu => koharu_ml::init_gpu().await?,
        WorkerRuntime::Full => koharu_ml::init().await?,
    }
    let device = koharu_ml::device(matches!(assignment.runtime, WorkerRuntime::Cpu));
    let pipeline = Pipeline::from_config(
        crate::PipelineConfig::load()?,
        koharu_translator::ProvidersConfig::load()?,
        device,
    )?;
    let mut session = koharu_scene::Session::open(project).await?;
    let snapshot = session.snapshot();
    let (progress_sender, mut progress_receiver) =
        tokio::sync::mpsc::unbounded_channel::<WorkerProgress>();
    let project = assignment.project;
    let mut base = assignment.base;
    let mut request = assignment.into_request()?;
    request.stop = stop;
    request.progress = Some(Arc::new(move |event| {
        let _ = progress_sender.send(WorkerProgress::from(event));
    }));
    let mut committer = WorkerCommitter {
        output,
        session: &mut session,
        progress: &mut progress_receiver,
        project,
        base: &mut base,
    };
    let task = pipeline.execute(snapshot, request, &mut committer).await;
    committer.drain_progress().await?;
    let release = task.release_resources().await;
    Ok(WorkerOutcome::from_release(release))
}

struct WorkerCommitter<'a> {
    output: &'a mut tokio::io::Stdout,
    session: &'a mut koharu_scene::Session,
    progress: &'a mut tokio::sync::mpsc::UnboundedReceiver<WorkerProgress>,
    /// The parent's document. The scratch copy has a different one.
    project: koharu_scene::DocumentId,
    /// The revision the parent's next commit expects, advanced after each one.
    base: &'a mut Revision,
}

#[async_trait::async_trait]
impl Committer for WorkerCommitter<'_> {
    async fn commit(&mut self, output: StageOutput) -> Result<Snapshot> {
        self.drain_progress().await?;
        let stage = output.stage.to_string();
        let page = output.page;
        // The patch was built against the scratch copy, whose document and
        // revision the parent would reject. The parent commits these in order
        // and nothing else writes to its session during the task, so its
        // revision advances by one per commit.
        let mut transfer = output.patch.to_transfer(stage, page);
        transfer.project = self.project;
        transfer.base = *self.base;
        let snapshot = self.session.commit(output.patch).await?.snapshot;
        write_frame(self.output, &WorkerEvent::Patch(transfer)).await?;
        *self.base = self
            .base
            .next()
            .context("parent revision overflow while forwarding a committed stage")?;
        Ok(snapshot)
    }
}

impl WorkerCommitter<'_> {
    async fn drain_progress(&mut self) -> Result<()> {
        while let Ok(progress) = self.progress.try_recv() {
            write_frame(self.output, &WorkerEvent::Progress(progress)).await?;
        }
        Ok(())
    }
}

#[revision::revisioned(revision = 1)]
#[derive(Clone)]
struct WorkerAssignment {
    operation: String,
    scope: String,
    inpainting_mask: Option<WorkerMask>,
    /// The parent's document, written into every patch the child sends back.
    project: koharu_scene::DocumentId,
    /// The parent's revision when the task started.
    base: Revision,
    runtime: WorkerRuntime,
    /// The package store the parent resolved its runtimes from. Without it the
    /// child falls back to the default cache directory and installs its own
    /// copy of Torch, stable-diffusion.cpp, and the model packages.
    store: String,
}

/// Which ML runtimes the parent initialized, so the child initializes the same
/// ones instead of discovering a GPU the parent deliberately skipped.
#[revision::revisioned(revision = 1)]
#[derive(Clone, Copy)]
enum WorkerRuntime {
    /// Torch only.
    Cpu,
    /// Torch and stable-diffusion.cpp.
    Gpu,
    /// Torch, llama.cpp, and stable-diffusion.cpp.
    Full,
}

impl WorkerRuntime {
    /// Reads the runtimes the parent initialized. A parent that never
    /// initialized any, such as the in-process test pipeline, has only Torch
    /// available, so the child does the same.
    fn current() -> Self {
        match koharu_ml::runtime_init() {
            Some(koharu_ml::RuntimeInit::Gpu) => Self::Gpu,
            Some(koharu_ml::RuntimeInit::Full) => Self::Full,
            Some(koharu_ml::RuntimeInit::Cpu) | None => Self::Cpu,
        }
    }
}

#[revision::revisioned(revision = 1)]
#[derive(Clone)]
struct WorkerMask {
    page: koharu_scene::EntityId,
    png: Vec<u8>,
}

impl WorkerAssignment {
    fn from_request(
        request: Request,
        project: koharu_scene::DocumentId,
        base: Revision,
    ) -> Result<Self> {
        Ok(Self {
            operation: serde_json::to_string(&request.operation)?,
            scope: serde_json::to_string(&request.scope)?,
            inpainting_mask: request.inpainting_mask.map(|mask| WorkerMask {
                page: mask.page,
                png: mask.png.to_vec(),
            }),
            project,
            base,
            runtime: WorkerRuntime::current(),
            store: koharu_runtime::Store::root().display().to_string(),
        })
    }

    fn into_request(self) -> Result<Request> {
        Ok(Request {
            operation: serde_json::from_str(&self.operation)?,
            scope: serde_json::from_str(&self.scope)?,
            stop: StopToken::default(),
            progress: None,
            inpainting_mask: self.inpainting_mask.map(|mask| crate::InpaintingMask {
                page: mask.page,
                png: Arc::from(mask.png),
            }),
        })
    }
}

#[revision::revisioned(revision = 1)]
enum WorkerEvent {
    Progress(WorkerProgress),
    Patch(PatchTransfer),
    Finished(WorkerOutcome),
}

#[revision::revisioned(revision = 1)]
enum WorkerProgress {
    Started {
        pages: Vec<koharu_scene::EntityId>,
        stages: Vec<String>,
    },
    Loading {
        page: koharu_scene::EntityId,
        stage: String,
        model: String,
    },
    Running {
        page: koharu_scene::EntityId,
        stage: String,
        model: String,
    },
    Finished {
        page: koharu_scene::EntityId,
        stage: String,
        model: String,
        elapsed_ms: u64,
    },
    Skipped {
        page: koharu_scene::EntityId,
        stage: String,
    },
}

impl From<Progress> for WorkerProgress {
    fn from(progress: Progress) -> Self {
        match progress {
            Progress::Started { pages, stages } => Self::Started {
                pages,
                stages: stages.iter().map(ToString::to_string).collect(),
            },
            Progress::Loading { page, stage, model } => Self::Loading {
                page,
                stage: stage.to_string(),
                model,
            },
            Progress::Running { page, stage, model } => Self::Running {
                page,
                stage: stage.to_string(),
                model,
            },
            Progress::Finished {
                page,
                stage,
                model,
                elapsed,
            } => Self::Finished {
                page,
                stage: stage.to_string(),
                model,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            },
            Progress::Skipped { page, stage } => Self::Skipped {
                page,
                stage: stage.to_string(),
            },
        }
    }
}

impl TryFrom<WorkerProgress> for Progress {
    type Error = anyhow::Error;

    fn try_from(progress: WorkerProgress) -> Result<Self> {
        Ok(match progress {
            WorkerProgress::Started { pages, stages } => Self::Started {
                pages,
                stages: stages.iter().map(|name| parse_stage(name)).collect::<Result<_>>()?,
            },
            WorkerProgress::Loading { page, stage, model } => Self::Loading {
                page,
                stage: parse_stage(&stage)?,
                model,
            },
            WorkerProgress::Running { page, stage, model } => Self::Running {
                page,
                stage: parse_stage(&stage)?,
                model,
            },
            WorkerProgress::Finished {
                page,
                stage,
                model,
                elapsed_ms,
            } => Self::Finished {
                page,
                stage: parse_stage(&stage)?,
                model,
                elapsed: Duration::from_millis(elapsed_ms),
            },
            WorkerProgress::Skipped { page, stage } => Self::Skipped {
                page,
                stage: parse_stage(&stage)?,
            },
        })
    }
}

#[revision::revisioned(revision = 1)]
enum WorkerOutcome {
    Completed(WorkerReport),
    Failed(WorkerFailure),
}

#[revision::revisioned(revision = 1)]
struct WorkerReport {
    status: WorkerStatus,
    base: koharu_scene::Revision,
    final_revision: koharu_scene::Revision,
    completed: u64,
    total: u64,
    elapsed_ms: u64,
    released_models: Vec<WorkerModelRelease>,
    image_cache_items: u64,
}

#[revision::revisioned(revision = 1)]
enum WorkerStatus {
    Completed,
    Stopped,
}

#[revision::revisioned(revision = 1)]
struct WorkerFailure {
    stage: Option<String>,
    kind: WorkerErrorKind,
    message: String,
    released_models: Vec<WorkerModelRelease>,
    image_cache_items: u64,
}

#[revision::revisioned(revision = 1)]
enum WorkerErrorKind {
    InvalidInput,
    ModelLoad,
    Processing,
    Commit,
    InvalidOutput,
}

#[revision::revisioned(revision = 1)]
struct WorkerModelRelease {
    stage: String,
    released: bool,
}

impl WorkerOutcome {
    fn from_release(release: TaskRelease) -> Self {
        let released_models = worker_releases(&release.released_models);
        let image_cache_items = u64::try_from(release.image_cache_items).unwrap_or(u64::MAX);
        match release.result {
            Ok(report) => Self::Completed(WorkerReport {
                status: match report.status {
                    RunStatus::Completed => WorkerStatus::Completed,
                    RunStatus::Stopped => WorkerStatus::Stopped,
                },
                base: report.base,
                final_revision: report.final_revision,
                completed: u64::try_from(report.completed).unwrap_or(u64::MAX),
                total: u64::try_from(report.total).unwrap_or(u64::MAX),
                elapsed_ms: u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
                released_models,
                image_cache_items,
            }),
            Err(error) => Self::Failed(WorkerFailure {
                stage: error.stage.map(|stage| stage.to_string()),
                kind: WorkerErrorKind::from(error.kind),
                message: format!("{error:#}"),
                released_models,
                image_cache_items,
            }),
        }
    }

    fn into_release(self) -> Result<TaskRelease> {
        match self {
            Self::Completed(report) => Ok(TaskRelease {
                result: Ok(Report {
                    status: match report.status {
                        WorkerStatus::Completed => RunStatus::Completed,
                        WorkerStatus::Stopped => RunStatus::Stopped,
                    },
                    base: report.base,
                    final_revision: report.final_revision,
                    completed: usize::try_from(report.completed).unwrap_or(usize::MAX),
                    total: usize::try_from(report.total).unwrap_or(usize::MAX),
                    elapsed: Duration::from_millis(report.elapsed_ms),
                    released_models: model_releases(&report.released_models)?,
                }),
                released_models: model_releases(&report.released_models)?,
                image_cache_items: usize::try_from(report.image_cache_items).unwrap_or(usize::MAX),
            }),
            Self::Failed(failure) => Ok(TaskRelease {
                result: Err(PipelineError::new(
                    failure.kind.into(),
                    failure.stage.as_deref().map(parse_stage).transpose()?,
                    anyhow!(failure.message),
                )),
                released_models: model_releases(&failure.released_models)?,
                image_cache_items: usize::try_from(failure.image_cache_items).unwrap_or(usize::MAX),
            }),
        }
    }
}

impl WorkerFailure {
    fn from_error(error: &anyhow::Error) -> Self {
        Self {
            stage: None,
            kind: WorkerErrorKind::Processing,
            message: format!("{error:#}"),
            released_models: Vec::new(),
            image_cache_items: 0,
        }
    }
}

impl From<ErrorKind> for WorkerErrorKind {
    fn from(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::InvalidInput => Self::InvalidInput,
            ErrorKind::ModelLoad => Self::ModelLoad,
            ErrorKind::Processing => Self::Processing,
            ErrorKind::Commit => Self::Commit,
            ErrorKind::InvalidOutput => Self::InvalidOutput,
        }
    }
}

impl From<WorkerErrorKind> for ErrorKind {
    fn from(kind: WorkerErrorKind) -> Self {
        match kind {
            WorkerErrorKind::InvalidInput => Self::InvalidInput,
            WorkerErrorKind::ModelLoad => Self::ModelLoad,
            WorkerErrorKind::Processing => Self::Processing,
            WorkerErrorKind::Commit => Self::Commit,
            WorkerErrorKind::InvalidOutput => Self::InvalidOutput,
        }
    }
}

fn parse_stage(name: &str) -> Result<Stage> {
    name.parse()
        .with_context(|| format!("task subprocess returned an unknown stage {name}"))
}

fn worker_releases(releases: &[(Stage, bool)]) -> Vec<WorkerModelRelease> {
    releases
        .iter()
        .map(|(stage, released)| WorkerModelRelease {
            stage: stage.to_string(),
            released: *released,
        })
        .collect()
}

fn model_releases(releases: &[WorkerModelRelease]) -> Result<Vec<(Stage, bool)>> {
    releases
        .iter()
        .map(|release| Ok((parse_stage(&release.stage)?, release.released)))
        .collect()
}

fn encode<T: SerializeRevisioned>(value: &T) -> Result<Vec<u8>> {
    let body = revision::to_vec(value)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    let length = u32::try_from(body.len()).context("frame exceeds 4 GiB")?;
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

fn decode<T: DeserializeRevisioned>(frame: &[u8]) -> Result<T> {
    revision::from_slice(frame).context("failed to decode a task subprocess frame")
}

async fn write_frame(output: &mut tokio::io::Stdout, event: &WorkerEvent) -> Result<()> {
    let frame = encode(event)?;
    output.write_all(&frame).await?;
    output.flush().await?;
    Ok(())
}

async fn read_frame(input: &mut tokio::process::ChildStdout) -> Result<Option<Vec<u8>>> {
    let mut length = [0_u8; 4];
    match input.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error).context("failed to read a task subprocess frame"),
    }
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if length > FRAME_LIMIT {
        bail!("task subprocess frame exceeds {FRAME_LIMIT} bytes");
    }
    let mut frame = vec![0_u8; length];
    input
        .read_exact(&mut frame)
        .await
        .context("task subprocess frame ended early")?;
    Ok(Some(frame))
}

async fn read_assignment(input: &mut tokio::io::Stdin) -> Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    input
        .read_exact(&mut length)
        .await
        .context("pipeline task assignment is empty")?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if length > FRAME_LIMIT {
        bail!("pipeline task assignment exceeds {FRAME_LIMIT} bytes");
    }
    let mut encoded = vec![0_u8; length];
    input
        .read_exact(&mut encoded)
        .await
        .context("pipeline task assignment ended early")?;
    Ok(encoded)
}
