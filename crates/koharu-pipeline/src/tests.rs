use super::*;

use std::{collections::BTreeMap, sync::Arc};

use image::{ExtendedColorType, ImageEncoder as _};
use koharu_scene::AssetRole;

#[test]
fn configuration_ignores_unknown_fields() {
    let config = toml::from_str::<PipelineConfig>("legacy_limit = 1").unwrap();
    assert_eq!(config, PipelineConfig::default());
}

#[tokio::test]
async fn task_memory_release_lifecycle() {
    let pipeline = pipeline(Default::default());
    let empty_snapshot = koharu_scene::Session::memory().await.unwrap().snapshot();

    let success = execute_task(&pipeline, empty_snapshot.clone(), Request::default()).await;
    assert_release_order(&success);
    assert_eq!(success.result.unwrap().status, RunStatus::Completed);

    let (image_snapshot, image_page, image_cache) = image_fixture().await;
    image_cache
        .get(&image_snapshot, image_page, "source")
        .await
        .unwrap();
    assert_eq!(image_cache.len(), 1);
    image_cache.clear();
    assert_eq!(image_cache.len(), 0);

    let failure = execute_task(
        &pipeline,
        image_snapshot.clone(),
        Request {
            operation: Operation::Only {
                stage: Stage::Translation,
            },
            inpainting_mask: Some(InpaintingMask {
                page: image_page,
                png: Arc::from([0_u8]),
            }),
            ..Request::default()
        },
    )
    .await;
    assert_release_order(&failure);
    assert_eq!(failure.result.unwrap_err().kind, ErrorKind::InvalidInput);

    let stopped = StopToken::default();
    stopped.stop();
    let cancellation = execute_task(
        &pipeline,
        empty_snapshot.clone(),
        Request {
            stop: stopped,
            ..Request::default()
        },
    )
    .await;
    assert_release_order(&cancellation);
    assert_eq!(cancellation.result.unwrap().status, RunStatus::Stopped);

    for _ in 0..2 {
        let models = TestModelCells::default();
        models.load().await;
        assert_eq!(
            models.release_task_models().await,
            vec![
                (Stage::Detection, true),
                (Stage::Ocr, true),
                (Stage::Inpainting, true),
            ]
        );
        models.assert_empty().await;

        let release = execute_task(&pipeline, empty_snapshot.clone(), Request::default()).await;
        assert_release_order(&release);
        assert_eq!(release.result.unwrap().status, RunStatus::Completed);
    }
}

#[test]
fn operations_expand_to_the_supported_workflows() {
    assert_eq!(
        Operation::Through {
            stage: Stage::Translation,
        }
        .stages()
        .unwrap(),
        vec![Stage::Detection, Stage::Ocr, Stage::Translation],
    );
    assert_eq!(
        Operation::Through {
            stage: Stage::Inpainting,
        }
        .stages()
        .unwrap(),
        vec![Stage::Detection, Stage::Inpainting],
    );
    assert_eq!(
        Operation::Only {
            stage: Stage::Translation,
        }
        .stages()
        .unwrap(),
        vec![Stage::Translation],
    );
    assert_eq!(
        Operation::Stages {
            stages: vec![Stage::Translation, Stage::Detection, Stage::Translation],
        }
        .stages()
        .unwrap(),
        vec![Stage::Detection, Stage::Translation],
    );
}

async fn execute_task(
    pipeline: &Pipeline,
    snapshot: koharu_scene::Snapshot,
    request: Request,
) -> TaskRelease {
    pipeline
        .execute(snapshot, request, &mut RejectCommitter)
        .await
        .release_resources()
        .await
}

fn assert_release_order(release: &TaskRelease) {
    assert_eq!(
        release
            .released_models
            .iter()
            .map(|(stage, _)| *stage)
            .collect::<Vec<_>>(),
        vec![Stage::Detection, Stage::Ocr, Stage::Inpainting]
    );
    assert_eq!(release.image_cache_items, 0);
}

#[derive(Default)]
struct TestModelCells {
    detection: ModelCell<u32>,
    ocr: ModelCell<u32>,
    inpainting: ModelCell<u32>,
}

impl TestModelCells {
    async fn load(&self) {
        self.detection.ensure(|| async { Ok(1) }).await.unwrap();
        self.ocr.ensure(|| async { Ok(2) }).await.unwrap();
        self.inpainting.ensure(|| async { Ok(3) }).await.unwrap();
    }

    async fn release_task_models(&self) -> Vec<(Stage, bool)> {
        vec![
            (Stage::Detection, self.detection.release().await),
            (Stage::Ocr, self.ocr.release().await),
            (Stage::Inpainting, self.inpainting.release().await),
        ]
    }

    async fn assert_empty(&self) {
        assert!(self.detection.lock().await.is_none());
        assert!(self.ocr.lock().await.is_none());
        assert!(self.inpainting.lock().await.is_none());
    }
}

async fn image_fixture() -> (koharu_scene::Snapshot, koharu_scene::EntityId, ImageCache) {
    let mut session = koharu_scene::Session::memory().await.unwrap();
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            page = Some(edit.add_page(
                koharu_scene::PageDraft::new("page", 1.0, 1.0),
                koharu_scene::At::End,
            )?);
            let mut encoded = Vec::new();
            image::codecs::png::PngEncoder::new(&mut encoded).write_image(
                &[0, 0, 0, 255],
                1,
                1,
                ExtendedColorType::Rgba8,
            )
            .map_err(|error| koharu_scene::Error::Invalid(error.to_string()))?;
            edit.set_asset(
                page.expect("page ID is assigned by the edit"),
                &AssetRole::new("source")?,
                koharu_scene::AssetInput::new(
                    Arc::<[u8]>::from(encoded),
                    "image/png",
                    koharu_scene::AssetMetadata {
                        width: Some(1),
                        height: Some(1),
                        attributes: BTreeMap::new(),
                    },
                ),
            )
        })
        .unwrap();
    session.commit(patch).await.unwrap();
    let snapshot = session.snapshot();
    (snapshot, page.unwrap(), ImageCache::default())
}

struct RejectCommitter;

fn pipeline(translation: TranslationConfig) -> Pipeline {
    let config = PipelineConfig {
        translation,
        ..PipelineConfig::default()
    };
    Pipeline::from_config(
        koharu_config::Config::memory(config),
        koharu_config::Config::memory(koharu_translator::ProvidersConfig::default()),
        koharu_ml::Device::cpu(),
    )
    .unwrap()
}

#[async_trait::async_trait]
impl Committer for RejectCommitter {
    async fn commit(&mut self, _output: StageOutput) -> anyhow::Result<koharu_scene::Snapshot> {
        anyhow::bail!("task memory lifecycle must not commit")
    }
}
