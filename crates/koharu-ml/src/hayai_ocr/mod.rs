//! Hayai OCR v2.7 Supernova: crop-level OCR for Japanese, Chinese, Korean,
//! and English text. A SigLIP2 NaFlex encoder's 2x2 patch blocks are merged
//! into one token each by a projector that also reads them in both writing
//! directions, then a small GQA transformer decoder reads them over a shared
//! visual and textual prefix, with every encoder depth fused into its visual
//! tokens, block-causal attention, and 2D multimodal RoPE.
//!
//! Upstream:
//! https://huggingface.co/JustANormalTinkerer/hayai-ocr-v2.7-supernova/tree/b00317ed69fccee5f787187d913fcf8a96f37f65

mod config;
mod model;
mod processor;

use anyhow::{Context, Result, ensure};
use image::DynamicImage;
use koharu_torch::Device;

use crate::backend::TryIntoDevice;

use self::{
    config::HayaiConfig,
    model::Model,
    processor::{ImageProcessor, Tokenizer, normalize_text},
};

crate::model_repository!("JustANormalTinkerer/hayai-ocr-v2.7-supernova" @ "b00317ed69fccee5f787187d913fcf8a96f37f65" {
    CONFIG = "config.json",
    WEIGHTS = "model.safetensors",
    TOKENIZER = "tokenizer.json",
});

#[derive(Debug)]
pub struct HayaiOcr {
    device: Device,
    model: Model,
    processor: ImageProcessor,
    tokenizer: Tokenizer,
}

impl HayaiOcr {
    pub async fn load(device: crate::Device) -> Result<Self> {
        let device: Device = device.try_into_device()?;
        let (config_path, weights_path, tokenizer_path) = tokio::try_join!(
            async {
                CONFIG
                    .resolve()
                    .await
                    .context("failed to resolve Hayai OCR config")
            },
            async {
                WEIGHTS
                    .resolve()
                    .await
                    .context("failed to resolve Hayai OCR weights")
            },
            async {
                TOKENIZER
                    .resolve()
                    .await
                    .context("failed to resolve Hayai OCR tokenizer")
            },
        )?;
        let config = HayaiConfig::from_file(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)?;
        ensure!(
            tokenizer.len() == config.vocab_size as usize,
            "Hayai OCR vocabulary has {} entries but the decoder has {} outputs",
            tokenizer.len(),
            config.vocab_size
        );
        let processor = ImageProcessor::new(device);

        let mut model = Model::new(&config, &tokenizer, device)?;
        model
            .load(&weights_path)
            .with_context(|| format!("failed to load {}", weights_path.display()))?;
        Ok(Self {
            device,
            model,
            processor,
            tokenizer,
        })
    }

    /// The text of each crop, the crops decoded together, normalized as the
    /// upstream release prescribes.
    pub fn inference(&self, images: &[DynamicImage]) -> Result<Vec<String>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        koharu_torch::no_grad(|| {
            let inputs = images
                .iter()
                .map(|image| self.processor.preprocess(image))
                .collect::<Result<Vec<_>>>()?;
            let token_ids = self.model.generate(&inputs, &self.tokenizer, self.device)?;
            token_ids
                .iter()
                .map(|token_ids| {
                    self.tokenizer
                        .decode(token_ids)
                        .map(|text| normalize_text(&text))
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::HayaiOcr;

    #[tokio::test]
    #[ignore = "downloads the checkpoint and requires the LibTorch runtime"]
    async fn checkpoint_matches_upstream_character_sequence() -> anyhow::Result<()> {
        crate::init().await?;
        let model = HayaiOcr::load(crate::Device::cpu()).await?;
        let input =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches/fixtures/ocr/title.png");
        let text = model.inference(&[image::open(input)?])?;
        // Verified against the pinned upstream `HayaiModel.generate` in greedy
        // mode on the identical fixture crop, then normalized.
        assert_eq!(text, ["対策委員会です!"]);
        Ok(())
    }
}
