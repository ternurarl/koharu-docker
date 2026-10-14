//! Task subprocess for one pipeline run.
//!
//! The parent starts this binary with the scratch project it exported and
//! writes one assignment to standard input. This process loads the models,
//! runs the pipeline, writes the committed patches back, releases its models
//! and image cache, and exits. The exit is the resource boundary: the
//! operating system reclaims the mappings the model runtimes retain.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let project = arguments
        .next()
        .map(PathBuf::from)
        .context("pipeline task subprocess requires the scratch project path")?;
    koharu_pipeline::run_task_subprocess(project).await
}
