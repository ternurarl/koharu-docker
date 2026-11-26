//! High-resolution manga layout instance segmentation with RF-DETR Seg 2XL.
//!
//! Koharu Layout HighRes checkpoint, run at its 1920-pixel resolution:
//! https://huggingface.co/mayocream/koharu-layout-highres/tree/68077eb09401a46e548190ff2786bd405113ea8b
//! RF-DETR upstream implementation:
//! https://github.com/roboflow/rf-detr/tree/4ab7c18729de9d02ffd0495795d0831b5630f01b
//!
//! Upstream koharu-rs/koharu names this module `koharu_layout_highres` since
//! 15914d76; the rename here comes with the pipeline port that commit also
//! carries, so the path stays put until then.

mod config;
mod measure;
mod model;
mod processor;
mod resolution;
mod suppress;

use anyhow::{Context, Result};
use image::DynamicImage;
use koharu_torch::Device;

use crate::backend::TryIntoDevice;

pub use self::{
    config::{KoharuLayoutRFDetrSeg2XLConfig, KoharuLayoutThresholds},
    measure::glyph_mask,
    processor::{
        KoharuLayoutDetection, KoharuLayoutDetections, KoharuLayoutMask,
        KoharuLayoutRFDetrImageProcessor,
    },
};

use self::model::Model;

crate::model_repository!("mayocream/koharu-layout-highres" @ "68077eb09401a46e548190ff2786bd405113ea8b" {
    CONFIG = "config.json",
    WEIGHTS = "model.safetensors",
});

/// The lowest confidence an onomatopoeia detection keeps by default. The
/// checkpoint recommends 0.3, but on 212 pages of Blue Archive's comics about
/// two in five onomatopoeia scoring under 0.4 were drawn marks, such as anger
/// veins, sweat drops, and motion lines, or lettering a text detection
/// already covers. Upstream 4eefd2f0 measured this and overrides the
/// checkpoint's value in `koharu_layout_highres/mod.rs`.
pub const ONOMATOPOEIA_THRESHOLD: f32 = 0.4;

/// The lowest confidence a bubble detection keeps by default. The checkpoint
/// recommends 0.5, but on the same pages most bubbles scoring between 0.4 and
/// 0.5 were plain speech bubbles, and inpainting fills only a detected
/// bubble's text with its paper. Upstream e31c3e2d measured this and
/// overrides the checkpoint's value in `koharu_layout_highres/mod.rs`.
pub const BUBBLE_THRESHOLD: f32 = 0.4;

#[derive(Debug)]
pub struct KoharuLayoutRFDetrSeg2XL {
    device: Device,
    model: Model,
    processor: KoharuLayoutRFDetrImageProcessor,
    thresholds: KoharuLayoutThresholds,
}

impl KoharuLayoutRFDetrSeg2XL {
    pub async fn load(device: crate::Device) -> Result<Self> {
        let device: Device = device.try_into_device()?;
        let config_path = CONFIG
            .resolve()
            .await
            .context("failed to resolve KoharuLayout RF-DETR inference config")?;
        let weights_path = WEIGHTS
            .resolve()
            .await
            .context("failed to resolve KoharuLayout RF-DETR weights")?;
        let config = KoharuLayoutRFDetrSeg2XLConfig::from_file(&config_path)?;
        // The processor checks the checkpoint's resolution against this port's;
        // the context keeps the failure readable from this call site.
        let processor = KoharuLayoutRFDetrImageProcessor::new(&config)
            .context("the checkpoint config does not match this port")?;
        let thresholds = KoharuLayoutThresholds {
            onomatopoeia: ONOMATOPOEIA_THRESHOLD,
            bubble: BUBBLE_THRESHOLD,
            ..config.recommended_thresholds
        };
        let mut model = Model::new(device);
        model
            .load(&weights_path)
            .with_context(|| format!("failed to load {}", weights_path.display()))?;
        Ok(Self {
            device,
            model,
            processor,
            thresholds,
        })
    }

    pub fn inference(&self, image: &DynamicImage) -> Result<KoharuLayoutDetections> {
        self.inference_with_thresholds(image, self.thresholds)
    }

    /// The network's detections with the duplicates separate queries predict
    /// removed, so each object is one detection.
    pub fn inference_with_thresholds(
        &self,
        image: &DynamicImage,
        thresholds: KoharuLayoutThresholds,
    ) -> Result<KoharuLayoutDetections> {
        let mut detections = koharu_torch::no_grad(|| {
            let pixel_values = self.processor.preprocess(image, self.device)?;
            let output = self.model.forward(&pixel_values);
            self.processor
                .postprocess(&output, image.width(), image.height(), thresholds)
        })?;
        suppress::suppress(&mut detections.detections);
        Ok(detections)
    }

    /// The default class thresholds: the checkpoint's recommended ones,
    /// except for onomatopoeia and bubbles.
    pub fn recommended_thresholds(&self) -> KoharuLayoutThresholds {
        self.thresholds
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::Result;

    use super::KoharuLayoutRFDetrSeg2XL;

    #[tokio::test]
    #[ignore = "downloads the checkpoint and requires CUDA"]
    async fn checkpoint_matches_rfdetr_upstream_structured_output() -> Result<()> {
        crate::init().await?;
        let image = image::open(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("benches/fixtures/object_detection/1.jpg"),
        )?;
        let model = KoharuLayoutRFDetrSeg2XL::load(crate::Device::cuda(0)).await?;
        let result = model.inference(&image)?;

        // Koharu Layout HighRes, rfdetr 1.7.0 in F32 on CUDA, the page
        // letterboxed as in training; the values are upstream 9eac4b06's for
        // this checkpoint. CUDA kernels vary across LibTorch releases, so
        // compare structured geometry in addition to bounded confidence
        // differences. Mask areas are left out: the local processor reports
        // raw mask pixels, where upstream measures the glyph-fitted mask the
        // pipeline port brings.
        let best = &result.detections[0];
        assert_eq!(best.label, "bubble");
        assert!((best.score - 0.977_458_1).abs() < 0.01);
        for (actual, expected) in best
            .bbox
            .into_iter()
            .zip([567.304_3, 547.317_1, 691.978, 720.860_9])
        {
            assert!((actual - expected).abs() < 6.0);
        }

        let lower_panel = result
            .detections
            .iter()
            .find(|detection| detection.label == "panel" && detection.bbox[1] > 700.0)
            .expect("lower page panel");
        let middle_panel = result
            .detections
            .iter()
            .find(|detection| {
                detection.label == "panel" && detection.bbox[1] > 500.0 && detection.bbox[1] < 700.0
            })
            .expect("middle page panel");
        for (actual, expected) in
            lower_panel
                .bbox
                .into_iter()
                .zip([69.583_76, 798.296, 684.963_1, 1_009.382_4])
        {
            assert!((actual - expected).abs() < 20.0);
        }
        for (actual, expected) in middle_panel
            .bbox
            .into_iter()
            .zip([68.926, 552.693_5, 699.970_5, 788.836_5])
        {
            assert!((actual - expected).abs() < 5.0);
        }
        Ok(())
    }
}
