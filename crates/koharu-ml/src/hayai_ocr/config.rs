//! Hayai OCR configuration.
//!
//! Upstream configuration:
//! https://huggingface.co/JustANormalTinkerer/hayai-ocr-v2.5-nova/blob/2803d0dc005fa4396ab1e7c5a472d2c450a247f7/configuration_hayai.py
//! The vision encoder is the `Siglip2VisionModel` instantiated by
//! `modeling_hayai.HayaiModel` from `google/siglip2-base-patch16-naflex`, whose
//! released config only overrides the model type, so every value below is the
//! transformers default:
//! https://github.com/huggingface/transformers/blob/main/src/transformers/models/siglip2/configuration_siglip2.py

use std::{fs, path::Path};

use anyhow::{Result, ensure};
use serde::Deserialize;

/// Vision-encoder constants of `google/siglip2-base-patch16-naflex`.
pub(super) mod siglip2 {
    pub const HIDDEN_SIZE: i64 = 768;
    pub const INTERMEDIATE_SIZE: i64 = 3072;
    pub const NUM_HIDDEN_LAYERS: usize = 12;
    pub const NUM_ATTENTION_HEADS: i64 = 12;
    pub const HEAD_DIM: i64 = HIDDEN_SIZE / NUM_ATTENTION_HEADS;
    pub const NUM_CHANNELS: i64 = 3;
    pub const PATCH_SIZE: i64 = 16;
    /// NaFlex keeps a square base grid of position embeddings that is
    /// interpolated per image instead of forcing a fixed input resolution.
    pub const POSITION_EMBEDDING_SIZE: i64 = 16;
    pub const LAYER_NORM_EPS: f64 = 1e-6;
    /// Maximum number of 16x16 patches emitted by the SigLIP2 image processor.
    ///
    /// This is the spatial budget of the whole network, so it is the main
    /// quality/latency knob of the Nova release (upstream `max_num_patches`):
    ///
    /// | `MAX_NUM_PATCHES` | decoder visual tokens | CER | relative latency |
    /// | ---: | ---: | ---: | ---: |
    /// | 256 (throughput) | `<= 64` | 4.95% | 1.00x |
    /// | **384** (current; Nova's balanced default) | `<= 96` | 3.65% | ~1.19x |
    /// | 512 (upstream's quality pick) | `<= 128` | 3.10% | ~1.34x |
    ///
    /// 384 is the release default because 256 regresses against the v2.1
    /// checkpoint this replaces (CER 4.95% vs 3.23%). 512 is the only tier that
    /// beats v2.1 outright and is worth the ~34% latency cost on dense or
    /// vertically-set crops. The decoder token counts above assume a balanced
    /// patch grid; see [`decoder_vision_grid`] for the exact mapping.
    pub const MAX_NUM_PATCHES: usize = 384;
}

/// Decoder constants of `VisualCausalOCRDecoder`; upstream hardcodes them in
/// its layer construction rather than exposing them through the config file.
pub(super) mod decoder {
    pub const QUERY_HEADS: i64 = 8;
    pub const KEY_VALUE_HEADS: i64 = 2;
    pub const HEAD_DIM: i64 = 64;
    pub const ROPE_THETA: f64 = 10_000.0;
}

/// Spatial downsampling of the Nova `DSCProjector`: a `2x2` pixel unshuffle
/// merges every `SPATIAL_DOWNSCALE^2` patches into one decoder visual token.
pub(super) const SPATIAL_DOWNSCALE: i64 = 2;

/// Decoder visual grid of a `patch_height x patch_width` patch grid after the
/// `DSCProjector` unshuffle: `ceil(H / 2) x ceil(W / 2)`, i.e. `H_out * W_out`
/// visual tokens.
///
/// Upstream keeps a batch-wide static buffer of `MAX_NUM_PATCHES / 4` slots and
/// masks the tail; Koharu runs one crop at a time and sizes the sequence to the
/// exact count instead, so there is nothing to pad or mask. For a balanced grid
/// that count is `MAX_NUM_PATCHES / 4` (64 at 256 patches), but an extreme
/// aspect ratio raises it: a `1 x 256` strip compresses to `1 x 128`.
pub(super) fn decoder_vision_grid(patch_height: i64, patch_width: i64) -> (i64, i64) {
    (
        (patch_height + SPATIAL_DOWNSCALE - 1) / SPATIAL_DOWNSCALE,
        (patch_width + SPATIAL_DOWNSCALE - 1) / SPATIAL_DOWNSCALE,
    )
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HayaiConfig {
    pub(crate) vocab_size: i64,
    pub(crate) d_model: i64,
    pub(crate) d_vision: i64,
    #[allow(dead_code)]
    pub(crate) d_ffn: i64,
    pub(crate) n_layers: usize,
}

impl HayaiConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.vocab_size > 0,
            "Hayai OCR vocabulary size must be positive"
        );
        ensure!(
            self.d_vision == siglip2::HIDDEN_SIZE,
            "Hayai OCR expects the SigLIP2 NaFlex {}-dimensional vision encoder",
            siglip2::HIDDEN_SIZE
        );
        ensure!(
            self.d_model == decoder::QUERY_HEADS * decoder::HEAD_DIM,
            "decoder model size does not match the hardcoded query head layout"
        );
        ensure!(
            decoder::QUERY_HEADS % decoder::KEY_VALUE_HEADS == 0,
            "decoder query heads must be divisible by key/value heads"
        );
        ensure!(self.n_layers > 0, "decoder must have at least one layer");
        Ok(())
    }
}
