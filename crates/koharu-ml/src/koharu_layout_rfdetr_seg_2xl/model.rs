//! Inference-only RF-DETR Seg 2XL port for the KoharuLayout checkpoint.
//!
//! The module tree and forward order follow RF-DETR upstream:
//! https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/lwdetr.py
//! https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/transformer.py

use std::path::Path;

use anyhow::Result;
use koharu_torch::{
    Device, IndexOp, Kind, Tensor,
    nn::{self, Module},
};

/// The square input side the checkpoint was trained and exported at. The
/// positional embedding table is registered on this grid, and `mod.rs` checks
/// the checkpoint config against it.
pub(super) const CHECKPOINT_RESOLUTION: i64 = 1920;

/// Both network sides must be a multiple of this: the backbone splits the
/// patch grid into `NUM_WINDOWS²` windows and the segmentation head projects at
/// a quarter of the input, so the patch size times the window count bounds
/// every later division.
pub(super) const RESOLUTION_MULTIPLE: i64 = PATCH_SIZE * NUM_WINDOWS;

const PATCH_SIZE: i64 = 12;
const NUM_WINDOWS: i64 = 2;
/// Interpolated positional embeddings kept per input size. Pages of different
/// aspect ratios resolve to different sizes, so only repeated pages and
/// same-size batches hit the cache; once it holds this many sizes it is dropped
/// whole rather than evicted one by one.
const POS_EMBED_CACHE_ENTRIES: usize = 8;
/// Encoder proposal grids kept per `Transformer`, dropped whole when full, like
/// the positional embedding cache: a page of a different size resolves to a
/// different grid.
const PROPOSALS_CACHE_ENTRIES: usize = 8;
const DINO_DIM: i64 = 384;
const DINO_HEADS: i64 = 6;
const HIDDEN_DIM: i64 = 256;
const NUM_QUERIES: i64 = 300;
const NUM_CLASSES_WITH_BACKGROUND: i64 = 5;
const GROUP_DETR: i64 = 13;
const DECODER_LAYERS: usize = 6;

#[derive(Debug)]
pub(super) struct Output {
    pub pred_logits: Tensor,
    pub pred_boxes: Tensor,
    /// The mask projection inputs; the product itself is taken per kept query
    /// in the processor, see [`MaskFeatures::select`].
    pub masks: MaskFeatures,
}

/// What the segmentation head has to keep for the mask product.
///
/// Upstream multiplies every query against the spatial features and then drops
/// the rows its postprocessing rejects, which at 1920 writes a 300×480×480
/// tensor (276 MB) to keep at most a few dozen rows. Only the processor knows
/// which queries survive thresholding, so the head stops one step earlier and
/// hands these three tensors over.
#[derive(Debug)]
pub(super) struct MaskFeatures {
    /// `[1, HIDDEN_DIM, mask_h, mask_w]`, the projected spatial features.
    spatial: Tensor,
    /// `[1, NUM_QUERIES, HIDDEN_DIM]`, the projected decoder queries.
    query: Tensor,
    /// `[1]`, the head's learned mask bias.
    bias: Tensor,
}

impl MaskFeatures {
    /// Projects the masks of `queries` only, returning `[K, mask_h, mask_w]`.
    ///
    /// Each row of the mask product is its own sum over `HIDDEN_DIM`, so
    /// projecting the kept queries gives exactly the rows a full 300-query
    /// product would give for those queries: this is a zero-loss change, not an
    /// approximation.
    pub(super) fn select(&self, queries: &Tensor) -> Tensor {
        let mask_h = self.spatial.size()[2];
        let mask_w = self.spatial.size()[3];
        let kept = queries.size()[0];
        self.query
            .i(0)
            .index_select(0, queries)
            .matmul(&self.spatial.i(0).flatten(1, -1))
            .view([kept, mask_h, mask_w])
            + &self.bias
    }
}

#[derive(Debug)]
pub(super) struct Model {
    var_store: nn::VarStore,
    backbone: Backbone,
    transformer: Transformer,
    segmentation_head: SegmentationHead,
    class_embed: nn::Linear,
    bbox_embed: Mlp,
    refpoint_embed: nn::Embedding,
    query_feat: nn::Embedding,
}

impl Model {
    pub fn new(device: Device) -> Self {
        let mut var_store = nn::VarStore::new(device);
        crate::backend::set_precision(&mut var_store);
        let root = var_store.root();
        // Joiner is nn.Sequential(backbone, position_embedding), hence the `0`.
        let backbone = Backbone::new(&(&root / "backbone" / 0));
        let transformer = Transformer::new(&(&root / "transformer"));
        let segmentation_head = SegmentationHead::new(&(&root / "segmentation_head"));
        let class_embed = nn::linear(
            &root / "class_embed",
            HIDDEN_DIM,
            NUM_CLASSES_WITH_BACKGROUND,
            Default::default(),
        );
        let bbox_embed = Mlp::new(&(&root / "bbox_embed"), HIDDEN_DIM, HIDDEN_DIM, 4, 3);
        let refpoint_embed = nn::embedding(
            &root / "refpoint_embed",
            NUM_QUERIES * GROUP_DETR,
            4,
            Default::default(),
        );
        let query_feat = nn::embedding(
            &root / "query_feat",
            NUM_QUERIES * GROUP_DETR,
            HIDDEN_DIM,
            Default::default(),
        );
        Self {
            var_store,
            backbone,
            transformer,
            segmentation_head,
            class_embed,
            bbox_embed,
            refpoint_embed,
            query_feat,
        }
    }

    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
        // The complete RF-DETR tree is registered before this strict VarStore load.
        self.var_store.load(path)?;
        crate::backend::set_precision(&mut self.var_store);
        Ok(())
    }

    pub fn forward(&self, pixel_values: &Tensor) -> Output {
        let input_size = pixel_values.size();
        let pixel_values = pixel_values.to_kind(self.var_store.kind());
        let features = self.backbone.forward(&pixel_values);
        let transformer_output = self.transformer.forward(
            &features,
            &self.refpoint_embed.ws.i(0..NUM_QUERIES),
            &self.query_feat.ws.i(0..NUM_QUERIES),
        );

        let hs = transformer_output.hs;
        let reference = transformer_output.references;
        let delta = self.bbox_embed.forward(&hs);
        let pred_boxes = Tensor::cat(
            &[
                delta.i((.., .., 0..2)) * reference.i((.., .., 2..4)) + reference.i((.., .., 0..2)),
                delta.i((.., .., 2..4)).exp() * reference.i((.., .., 2..4)),
            ],
            -1,
        );
        let pred_logits = self.class_embed.forward(&hs);
        // The segmentation head projects to a quarter of whatever the
        // backbone consumed, so it takes the actual input size rather than the
        // checkpoint's.
        let masks =
            self.segmentation_head
                .features(&features[0], &hs, (input_size[2], input_size[3]));
        Output {
            pred_logits,
            pred_boxes,
            masks,
        }
    }
}

// https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/backbone/backbone.py
#[derive(Debug)]
struct Backbone {
    encoder: DinoBackbone,
    projector: MultiScaleProjector,
}

impl Backbone {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            encoder: DinoBackbone::new(&(path / "encoder" / "encoder")),
            projector: MultiScaleProjector::new(&(path / "projector")),
        }
    }

    fn forward(&self, pixel_values: &Tensor) -> Vec<Tensor> {
        self.projector.forward(&self.encoder.forward(pixel_values))
    }
}

// https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/backbone/dinov2_with_windowed_attn.py
#[derive(Debug)]
struct DinoBackbone {
    embeddings: DinoEmbeddings,
    layers: Vec<DinoLayer>,
    layernorm: nn::LayerNorm,
}

impl DinoBackbone {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            embeddings: DinoEmbeddings::new(&(path / "embeddings")),
            layers: (0..12)
                .map(|index| DinoLayer::new(&(path / "encoder" / "layer" / index)))
                .collect(),
            layernorm: layer_norm(&(path / "layernorm"), DINO_DIM, 1e-6),
        }
    }

    fn forward(&self, pixel_values: &Tensor) -> Vec<Tensor> {
        let mut hidden_states = self.embeddings.forward(pixel_values);
        let mut outputs = Vec::with_capacity(4);
        for (index, layer) in self.layers.iter().enumerate() {
            let run_full_attention = matches!(index, 3 | 6 | 9 | 12);
            hidden_states = layer.forward(&hidden_states, run_full_attention);
            if matches!(index + 1, 3 | 6 | 9 | 12) {
                outputs.push(self.feature_map(pixel_values, &hidden_states));
            }
        }
        outputs
    }

    fn feature_map(&self, pixel_values: &Tensor, hidden_states: &Tensor) -> Tensor {
        let hidden_states = self.layernorm.forward(hidden_states).i((.., 1..));
        let input_size = pixel_values.size();
        let batch_size = input_size[0];
        let height = input_size[2] / PATCH_SIZE;
        let width = input_size[3] / PATCH_SIZE;
        let height_per_window = height / NUM_WINDOWS;
        let width_per_window = width / NUM_WINDOWS;
        let size = hidden_states.size();
        let batch_windows = size[0];
        let tokens_per_window = size[1];
        hidden_states
            .reshape([
                batch_windows / (NUM_WINDOWS * NUM_WINDOWS),
                NUM_WINDOWS * NUM_WINDOWS * tokens_per_window,
                DINO_DIM,
            ])
            // Preserve the upstream window reconstruction, including its
            // height/width ordering quirk.
            .reshape([
                batch_size * NUM_WINDOWS,
                NUM_WINDOWS,
                height_per_window,
                width_per_window,
                DINO_DIM,
            ])
            .permute([0, 2, 1, 3, 4])
            .reshape([batch_size, height, width, DINO_DIM])
            .permute([0, 3, 1, 2])
            .contiguous()
    }
}

#[derive(Debug)]
struct DinoEmbeddings {
    cls_token: Tensor,
    _mask_token: Tensor,
    position_embeddings: Tensor,
    projection: nn::Conv2D,
    /// `forward` takes `&self`, and the interpolated table depends only on the
    /// input size, so the cache is the only interior mutability needed here.
    pos_embed_cache: std::sync::Mutex<std::collections::HashMap<(i64, i64), Tensor>>,
}

impl DinoEmbeddings {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            cls_token: path.var("cls_token", &[1, 1, DINO_DIM], nn::Init::Const(0.0)),
            _mask_token: path.var("mask_token", &[1, DINO_DIM], nn::Init::Const(0.0)),
            position_embeddings: path.var(
                "position_embeddings",
                &[1, (CHECKPOINT_RESOLUTION / PATCH_SIZE).pow(2) + 1, DINO_DIM],
                nn::Init::Const(0.0),
            ),
            projection: nn::conv2d(
                path / "patch_embeddings" / "projection",
                3,
                DINO_DIM,
                PATCH_SIZE,
                nn::ConvConfig {
                    stride: PATCH_SIZE,
                    ..Default::default()
                },
            ),
            pos_embed_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The positional embedding table resized to a `target_h`×`target_w` input.
    ///
    /// Follows upstream `make_new_interpolated_pos_encoding`
    /// (roboflow/rf-detr@4ab7c18 src/rfdetr/models/backbone/dinov2.py:215-244):
    /// the patch part is viewed on its native square grid, bicubically resized
    /// to the target token grid, and concatenated behind the class-token entry.
    ///
    /// Divergence: upstream resizes with `antialias=True`, but the wrapper this
    /// crate builds on only exports `upsample_bicubic2d`
    /// (crates/koharu-torch/src/wrappers/tensor_generated.rs:19549), so this
    /// resize is not antialiased. That can change numbers only when a target
    /// side carries fewer tokens than the checkpoint's 160, which the
    /// `balanced` and `fast` budgets do, and only for this smooth table.
    fn interpolate_pos_embed(&self, target_h: i64, target_w: i64) -> Tensor {
        if (target_h, target_w) == (CHECKPOINT_RESOLUTION, CHECKPOINT_RESOLUTION) {
            return self.position_embeddings.shallow_clone();
        }
        let key = (target_h, target_w);
        if let Ok(cache) = self.pos_embed_cache.lock()
            && let Some(cached) = cache.get(&key)
        {
            return cached.shallow_clone();
        }
        let grid = CHECKPOINT_RESOLUTION / PATCH_SIZE;
        let class_pos_embed = self.position_embeddings.i((.., ..1));
        let patch_pos_embed = self
            .position_embeddings
            .i((.., 1..))
            .reshape([1, grid, grid, DINO_DIM])
            .permute([0, 3, 1, 2])
            .to_kind(Kind::Float)
            .upsample_bicubic2d(
                [target_h / PATCH_SIZE, target_w / PATCH_SIZE],
                false,
                None,
                None,
            )
            .permute([0, 2, 3, 1])
            .reshape([1, -1, DINO_DIM])
            .to_kind(self.position_embeddings.kind());
        let pos_embed = Tensor::cat(&[class_pos_embed, patch_pos_embed], 1).contiguous();
        if let Ok(mut cache) = self.pos_embed_cache.lock() {
            if cache.len() >= POS_EMBED_CACHE_ENTRIES {
                cache.clear();
            }
            cache.insert(key, pos_embed.shallow_clone());
        }
        pos_embed
    }

    fn forward(&self, pixel_values: &Tensor) -> Tensor {
        let size = pixel_values.size();
        // Upstream accepts rectangular inputs whose sides both divide by the
        // patch grid of one window (dinov2.py:271-277).
        assert!(
            size[2] % RESOLUTION_MULTIPLE == 0 && size[3] % RESOLUTION_MULTIPLE == 0,
            "RF-DETR input sides must divide by {RESOLUTION_MULTIPLE}, got {}x{}",
            size[2],
            size[3]
        );
        let batch_size = size[0];
        let embeddings = self
            .projection
            .forward(pixel_values)
            .flatten(2, -1)
            .transpose(1, 2);
        let embeddings = Tensor::cat(
            &[
                self.cls_token.expand([batch_size, -1, -1], false),
                embeddings,
            ],
            1,
        ) + self.interpolate_pos_embed(size[2], size[3]);

        let height = size[2] / PATCH_SIZE;
        let width = size[3] / PATCH_SIZE;
        let height_per_window = height / NUM_WINDOWS;
        let width_per_window = width / NUM_WINDOWS;
        let cls = embeddings.i((.., ..1)).repeat([NUM_WINDOWS.pow(2), 1, 1]);
        let windows = embeddings
            .i((.., 1..))
            .view([batch_size, height, width, DINO_DIM])
            .reshape([
                batch_size * NUM_WINDOWS,
                height_per_window,
                NUM_WINDOWS,
                width_per_window,
                DINO_DIM,
            ])
            .permute([0, 2, 1, 3, 4])
            .reshape([
                batch_size * NUM_WINDOWS.pow(2),
                height_per_window * width_per_window,
                DINO_DIM,
            ]);
        Tensor::cat(&[cls, windows], 1)
    }
}

#[derive(Debug)]
struct DinoLayer {
    norm1: nn::LayerNorm,
    attention: DinoAttention,
    layer_scale1: Tensor,
    norm2: nn::LayerNorm,
    mlp: DinoMlp,
    layer_scale2: Tensor,
}

impl DinoLayer {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            norm1: layer_norm(&(path / "norm1"), DINO_DIM, 1e-6),
            attention: DinoAttention::new(&(path / "attention")),
            layer_scale1: (path / "layer_scale1").var("lambda1", &[DINO_DIM], nn::Init::Const(1.0)),
            norm2: layer_norm(&(path / "norm2"), DINO_DIM, 1e-6),
            mlp: DinoMlp::new(&(path / "mlp")),
            layer_scale2: (path / "layer_scale2").var("lambda1", &[DINO_DIM], nn::Init::Const(1.0)),
        }
    }

    fn forward(&self, hidden_states: &Tensor, run_full_attention: bool) -> Tensor {
        let shortcut = hidden_states;
        let attention_input = if run_full_attention {
            let size = hidden_states.size();
            hidden_states.view([
                size[0] / NUM_WINDOWS.pow(2),
                NUM_WINDOWS.pow(2) * size[1],
                size[2],
            ])
        } else {
            hidden_states.shallow_clone()
        };
        let mut attention = self
            .attention
            .forward(&self.norm1.forward(&attention_input));
        if run_full_attention {
            let size = attention.size();
            attention = attention.view([
                size[0] * NUM_WINDOWS.pow(2),
                size[1] / NUM_WINDOWS.pow(2),
                size[2],
            ]);
        }
        let hidden_states = shortcut + attention * &self.layer_scale1;
        let layer_output = self.mlp.forward(&self.norm2.forward(&hidden_states));
        hidden_states + layer_output * &self.layer_scale2
    }
}

#[derive(Debug)]
struct DinoAttention {
    query: nn::Linear,
    key: nn::Linear,
    value: nn::Linear,
    dense: nn::Linear,
}

impl DinoAttention {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            query: nn::linear(
                path / "attention" / "query",
                DINO_DIM,
                DINO_DIM,
                Default::default(),
            ),
            key: nn::linear(
                path / "attention" / "key",
                DINO_DIM,
                DINO_DIM,
                Default::default(),
            ),
            value: nn::linear(
                path / "attention" / "value",
                DINO_DIM,
                DINO_DIM,
                Default::default(),
            ),
            dense: nn::linear(
                path / "output" / "dense",
                DINO_DIM,
                DINO_DIM,
                Default::default(),
            ),
        }
    }

    fn forward(&self, hidden_states: &Tensor) -> Tensor {
        let size = hidden_states.size();
        let batch_size = size[0];
        let sequence_length = size[1];
        let head_dim = DINO_DIM / DINO_HEADS;
        let project = |linear: &nn::Linear| {
            linear
                .forward(hidden_states)
                .view([batch_size, sequence_length, DINO_HEADS, head_dim])
                .permute([0, 2, 1, 3])
        };
        let query = project(&self.query);
        let key = project(&self.key);
        let value = project(&self.value);
        let context = attention(&query, &key, &value)
            .permute([0, 2, 1, 3])
            .contiguous()
            .view([batch_size, sequence_length, DINO_DIM]);
        self.dense.forward(&context)
    }
}

/// Upstream's `scaled_dot_product_attention`, a chunk of query rows at a time
/// where libtorch has no fused kernel, as on ROCm for RDNA GPUs
/// (koharu-rs/koharu#1408). Its math fallback holds the whole score matrix:
/// for the full-attention layers' 25,604 tokens, 6 x 25,604^2 scores, 7.9 GB
/// in BF16 and again for their softmax, past a 16 GB GPU. Each query row's
/// softmax is its own, so the chunks give the single call's result. A fused
/// kernel takes the whole call, as upstream's does: it holds no score matrix,
/// and it rounds differently by chunk, enough to move this model's scores.
fn attention(query: &Tensor, key: &Tensor, value: &Tensor) -> Tensor {
    // PyTorch's `SDPBackend::math`.
    const MATH: i64 = 0;
    // Query rows a chunk takes on the math fallback.
    const ROWS: i64 = 1024;
    let attend = |query: &Tensor| {
        Tensor::scaled_dot_product_attention::<&Tensor>(
            query, key, value, None, 0.0, false, None, false,
        )
    };
    let rows = query.size()[2];
    // libtorch registers its kernel choice for CUDA devices, which HIP ones
    // are to it, and not for MPS.
    let math = matches!(query.device(), Device::Cuda(_))
        && Tensor::internal_fused_sdp_choice::<&Tensor>(
            query, key, value, None, 0.0, false, None, false,
        ) == MATH;
    if !math || rows <= ROWS {
        return attend(query);
    }
    let chunks = (0..rows)
        .step_by(ROWS as usize)
        .map(|start| attend(&query.narrow(2, start, ROWS.min(rows - start))))
        .collect::<Vec<_>>();
    Tensor::cat(&chunks, 2)
}

#[derive(Debug)]
struct DinoMlp {
    fc1: nn::Linear,
    fc2: nn::Linear,
}

impl DinoMlp {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            fc1: nn::linear(path / "fc1", DINO_DIM, DINO_DIM * 4, Default::default()),
            fc2: nn::linear(path / "fc2", DINO_DIM * 4, DINO_DIM, Default::default()),
        }
    }

    fn forward(&self, hidden_states: &Tensor) -> Tensor {
        self.fc2
            .forward(&self.fc1.forward(hidden_states).gelu("none"))
    }
}

// https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/backbone/projector.py
#[derive(Debug)]
struct MultiScaleProjector {
    stage: C2f,
    norm: ChannelLayerNorm,
}

impl MultiScaleProjector {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            stage: C2f::new(&(path / "stages" / 0 / 0), DINO_DIM * 4, HIDDEN_DIM),
            norm: ChannelLayerNorm::new(&(path / "stages" / 0 / 1), HIDDEN_DIM, 1e-6),
        }
    }

    fn forward(&self, features: &[Tensor]) -> Vec<Tensor> {
        vec![
            self.norm
                .forward(&self.stage.forward(&Tensor::cat(features, 1))),
        ]
    }
}

#[derive(Debug)]
struct C2f {
    hidden_channels: i64,
    cv1: ConvX,
    cv2: ConvX,
    bottlenecks: Vec<Bottleneck>,
}

impl C2f {
    fn new(path: &nn::Path<'_>, input_channels: i64, output_channels: i64) -> Self {
        let hidden_channels = output_channels / 2;
        Self {
            hidden_channels,
            cv1: ConvX::new(&(path / "cv1"), input_channels, hidden_channels * 2, 1),
            cv2: ConvX::new(&(path / "cv2"), hidden_channels * 5, output_channels, 1),
            bottlenecks: (0..3)
                .map(|index| Bottleneck::new(&(path / "m" / index), hidden_channels))
                .collect(),
        }
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        let split = self
            .cv1
            .forward(xs)
            .split_with_sizes([self.hidden_channels, self.hidden_channels], 1);
        let mut values = vec![split[0].shallow_clone(), split[1].shallow_clone()];
        for bottleneck in &self.bottlenecks {
            values.push(bottleneck.forward(values.last().unwrap()));
        }
        self.cv2.forward(&Tensor::cat(&values, 1))
    }
}

#[derive(Debug)]
struct Bottleneck {
    cv1: ConvX,
    cv2: ConvX,
}

impl Bottleneck {
    fn new(path: &nn::Path<'_>, channels: i64) -> Self {
        Self {
            cv1: ConvX::new(&(path / "cv1"), channels, channels, 3),
            cv2: ConvX::new(&(path / "cv2"), channels, channels, 3),
        }
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        self.cv2.forward(&self.cv1.forward(xs))
    }
}

#[derive(Debug)]
struct ConvX {
    conv: nn::Conv2D,
    bn: ChannelLayerNorm,
}

impl ConvX {
    fn new(path: &nn::Path<'_>, input_channels: i64, output_channels: i64, kernel: i64) -> Self {
        Self {
            conv: nn::conv2d(
                path / "conv",
                input_channels,
                output_channels,
                kernel,
                nn::ConvConfig {
                    padding: kernel / 2,
                    bias: false,
                    ..Default::default()
                },
            ),
            bn: ChannelLayerNorm::new(&(path / "bn"), output_channels, 1e-6),
        }
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        self.bn.forward(&self.conv.forward(&xs.contiguous())).silu()
    }
}

#[derive(Debug)]
struct ChannelLayerNorm(nn::LayerNorm);

impl ChannelLayerNorm {
    fn new(path: &nn::Path<'_>, channels: i64, eps: f64) -> Self {
        Self(layer_norm(path, channels, eps))
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        self.0
            .forward(&xs.permute([0, 2, 3, 1]))
            .permute([0, 3, 1, 2])
    }
}

// https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/transformer.py
#[derive(Debug)]
struct Transformer {
    decoder: TransformerDecoder,
    encoder_groups: Vec<EncoderGroup>,
    /// The encoder's proposals depend only on the memory grid and on its kind
    /// and device, so a page whose grid was already solved reuses them. The key
    /// carries all four: a tensor built for one kind or device cannot be used
    /// by another.
    proposals_cache: std::sync::Mutex<
        std::collections::HashMap<(i64, i64, i64, Kind, Device), (Tensor, Tensor)>,
    >,
}

#[derive(Debug)]
struct TransformerOutput {
    /// `[1, NUM_QUERIES, HIDDEN_DIM]`, the last decoder layer's output.
    hs: Tensor,
    /// `[1, NUM_QUERIES, 4]`, normalized center/size reference boxes.
    references: Tensor,
}

impl Transformer {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            decoder: TransformerDecoder::new(&(path / "decoder")),
            encoder_groups: (0..GROUP_DETR)
                .map(|index| EncoderGroup::new(path, index))
                .collect(),
            proposals_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The encoder proposals and their validity mask for one memory grid.
    ///
    /// Upstream rebuilds the `meshgrid` and the `[0.01, 0.99]` mask on every
    /// call, and both depend only on the grid shape, kind and device. The two
    /// tensors come back by `shallow_clone`, so callers must not write into
    /// them: `forward` only reads them and writes the masked copies it needs.
    fn encoder_proposals(
        &self,
        batch: i64,
        height: i64,
        width: i64,
        kind: Kind,
        device: Device,
    ) -> (Tensor, Tensor) {
        let key = (batch, height, width, kind, device);
        if let Ok(cache) = self.proposals_cache.lock()
            && let Some((proposals, valid)) = cache.get(&key)
        {
            return (proposals.shallow_clone(), valid.shallow_clone());
        }
        let (proposals, valid) = build_encoder_proposals(batch, height, width, kind, device);
        if let Ok(mut cache) = self.proposals_cache.lock() {
            if cache.len() >= PROPOSALS_CACHE_ENTRIES {
                cache.clear();
            }
            cache.insert(key, (proposals.shallow_clone(), valid.shallow_clone()));
        }
        (proposals, valid)
    }

    fn forward(
        &self,
        features: &[Tensor],
        refpoint_embed: &Tensor,
        query_feat: &Tensor,
    ) -> TransformerOutput {
        assert_eq!(features.len(), 1);
        let feature_size = features[0].size();
        let height = feature_size[2];
        let width = feature_size[3];
        let memory = features[0].flatten(2, -1).transpose(1, 2);
        let (proposals, valid) = self.encoder_proposals(
            memory.size()[0],
            height,
            width,
            memory.kind(),
            memory.device(),
        );
        let output_memory = memory.masked_fill(&valid.logical_not(), 0.0);
        let output_proposals = proposals.masked_fill(&valid.logical_not(), 0.0);
        let encoder = &self.encoder_groups[0];
        let encoded_memory = encoder
            .output_norm
            .forward(&encoder.output.forward(&output_memory));
        let encoder_class = encoder.class_embed.forward(&encoded_memory);
        let topk = encoder_class
            .max_dim(-1, false)
            .0
            .topk(NUM_QUERIES, 1, true, true)
            .1
            .i(0);
        let selected_memory = encoded_memory.i(0).index_select(0, &topk).unsqueeze(0);
        let selected_proposals = output_proposals.i(0).index_select(0, &topk).unsqueeze(0);
        // The encoder box MLP is token-pointwise. Latest upstream gathers ranked
        // tokens first instead of evaluating and discarding every unselected box.
        let delta = encoder.bbox_embed.forward(&selected_memory);
        let topk_boxes = Tensor::cat(
            &[
                delta.i((.., .., 0..2)) * selected_proposals.i((.., .., 2..4))
                    + selected_proposals.i((.., .., 0..2)),
                delta.i((.., .., 2..4)).exp() * selected_proposals.i((.., .., 2..4)),
            ],
            -1,
        );

        let learned_refpoints = refpoint_embed.unsqueeze(0);
        let references = Tensor::cat(
            &[
                learned_refpoints.i((.., .., 0..2)) * topk_boxes.i((.., .., 2..4))
                    + topk_boxes.i((.., .., 0..2)),
                learned_refpoints.i((.., .., 2..4)).exp() * topk_boxes.i((.., .., 2..4)),
            ],
            -1,
        );
        let target = query_feat.unsqueeze(0);
        let hs = self
            .decoder
            .forward(&target, &memory, &references, (height, width));
        TransformerOutput { hs, references }
    }
}

#[derive(Debug)]
struct EncoderGroup {
    output: nn::Linear,
    output_norm: nn::LayerNorm,
    bbox_embed: Mlp,
    class_embed: nn::Linear,
}

impl EncoderGroup {
    fn new(path: &nn::Path<'_>, index: i64) -> Self {
        Self {
            output: nn::linear(
                path / "enc_output" / index,
                HIDDEN_DIM,
                HIDDEN_DIM,
                Default::default(),
            ),
            output_norm: layer_norm(&(path / "enc_output_norm" / index), HIDDEN_DIM, 1e-5),
            bbox_embed: Mlp::new(
                &(path / "enc_out_bbox_embed" / index),
                HIDDEN_DIM,
                HIDDEN_DIM,
                4,
                3,
            ),
            class_embed: nn::linear(
                path / "enc_out_class_embed" / index,
                HIDDEN_DIM,
                NUM_CLASSES_WITH_BACKGROUND,
                Default::default(),
            ),
        }
    }
}

#[derive(Debug)]
struct TransformerDecoder {
    layers: Vec<TransformerDecoderLayer>,
    norm: nn::LayerNorm,
    ref_point_head: Mlp,
}

impl TransformerDecoder {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            layers: (0..DECODER_LAYERS)
                .map(|index| TransformerDecoderLayer::new(&(path / "layers" / index)))
                .collect(),
            norm: layer_norm(&(path / "norm"), HIDDEN_DIM, 1e-5),
            ref_point_head: Mlp::new(
                &(path / "ref_point_head"),
                HIDDEN_DIM * 2,
                HIDDEN_DIM,
                HIDDEN_DIM,
                2,
            ),
        }
    }

    fn forward(
        &self,
        target: &Tensor,
        memory: &Tensor,
        references: &Tensor,
        spatial_shape: (i64, i64),
    ) -> Tensor {
        let sine = gen_sineembed_for_position(references);
        let query_position = self.ref_point_head.forward(&sine);
        let reference_points = references.unsqueeze(2);
        let mut output = target.shallow_clone();
        for layer in &self.layers {
            output = layer.forward(
                &output,
                memory,
                &query_position,
                &reference_points,
                spatial_shape,
            );
        }
        // Upstream's inference path (lwdetr.py) reads only the last of the six
        // decoder layers' logits, boxes and masks; the earlier layers exist for
        // the training-time auxiliary losses. Returning the last layer alone is
        // a structural match to that path, not a numerical approximation.
        self.norm.forward(&output)
    }
}

#[derive(Debug)]
struct TransformerDecoderLayer {
    self_attn: MultiheadAttention,
    cross_attn: MultiscaleDeformableAttention,
    linear1: nn::Linear,
    linear2: nn::Linear,
    norm1: nn::LayerNorm,
    norm2: nn::LayerNorm,
    norm3: nn::LayerNorm,
}

impl TransformerDecoderLayer {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            self_attn: MultiheadAttention::new(&(path / "self_attn"), HIDDEN_DIM, 8),
            cross_attn: MultiscaleDeformableAttention::new(&(path / "cross_attn")),
            linear1: nn::linear(path / "linear1", HIDDEN_DIM, 2048, Default::default()),
            linear2: nn::linear(path / "linear2", 2048, HIDDEN_DIM, Default::default()),
            norm1: layer_norm(&(path / "norm1"), HIDDEN_DIM, 1e-5),
            norm2: layer_norm(&(path / "norm2"), HIDDEN_DIM, 1e-5),
            norm3: layer_norm(&(path / "norm3"), HIDDEN_DIM, 1e-5),
        }
    }

    fn forward(
        &self,
        target: &Tensor,
        memory: &Tensor,
        query_position: &Tensor,
        reference_points: &Tensor,
        spatial_shape: (i64, i64),
    ) -> Tensor {
        let query_key = target + query_position;
        let target2 = self.self_attn.forward(&query_key, target);
        let target = self.norm1.forward(&(target + target2));
        let target2 = self.cross_attn.forward(
            &(&target + query_position),
            reference_points,
            memory,
            spatial_shape,
        );
        let target = self.norm2.forward(&(target + target2));
        let target2 = self.linear2.forward(&self.linear1.forward(&target).relu());
        self.norm3.forward(&(target + target2))
    }
}

#[derive(Debug)]
struct MultiheadAttention {
    in_proj_weight: Tensor,
    in_proj_bias: Tensor,
    out_proj: nn::Linear,
    num_heads: i64,
}

impl MultiheadAttention {
    fn new(path: &nn::Path<'_>, embed_dim: i64, num_heads: i64) -> Self {
        Self {
            in_proj_weight: path.var(
                "in_proj_weight",
                &[embed_dim * 3, embed_dim],
                nn::Init::Const(0.0),
            ),
            in_proj_bias: path.var("in_proj_bias", &[embed_dim * 3], nn::Init::Const(0.0)),
            out_proj: nn::linear(path / "out_proj", embed_dim, embed_dim, Default::default()),
            num_heads,
        }
    }

    fn forward(&self, query_key: &Tensor, value: &Tensor) -> Tensor {
        let size = query_key.size();
        let batch_size = size[0];
        let sequence_length = size[1];
        let head_dim = HIDDEN_DIM / self.num_heads;
        // `_in_projection_packed` takes its three-linear branch because value
        // is distinct from the aliased query/key input.
        let weights = self.in_proj_weight.chunk(3, 0);
        let biases = self.in_proj_bias.chunk(3, 0);
        let query = query_key.linear(&weights[0], Some(&biases[0]));
        let key = query_key.linear(&weights[1], Some(&biases[1]));
        let value = value.linear(&weights[2], Some(&biases[2]));
        let reshape = |tensor: &Tensor| {
            tensor
                .view([batch_size, sequence_length, self.num_heads, head_dim])
                .transpose(1, 2)
        };
        let query = reshape(&query);
        let key = reshape(&key);
        let value = reshape(&value);
        let output = Tensor::scaled_dot_product_attention::<&Tensor>(
            &query, &key, &value, None, 0.0, false, None, false,
        )
        .transpose(1, 2)
        .contiguous()
        .view([batch_size, sequence_length, HIDDEN_DIM]);
        self.out_proj.forward(&output)
    }
}

// https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/ops/modules/ms_deform_attn.py
#[derive(Debug)]
struct MultiscaleDeformableAttention {
    sampling_offsets: nn::Linear,
    attention_weights: nn::Linear,
    value_proj: nn::Linear,
    output_proj: nn::Linear,
}

impl MultiscaleDeformableAttention {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            sampling_offsets: nn::linear(
                path / "sampling_offsets",
                HIDDEN_DIM,
                16 * 2 * 2,
                Default::default(),
            ),
            attention_weights: nn::linear(
                path / "attention_weights",
                HIDDEN_DIM,
                16 * 2,
                Default::default(),
            ),
            value_proj: nn::linear(
                path / "value_proj",
                HIDDEN_DIM,
                HIDDEN_DIM,
                Default::default(),
            ),
            output_proj: nn::linear(
                path / "output_proj",
                HIDDEN_DIM,
                HIDDEN_DIM,
                Default::default(),
            ),
        }
    }

    fn forward(
        &self,
        query: &Tensor,
        reference_points: &Tensor,
        memory: &Tensor,
        spatial_shape: (i64, i64),
    ) -> Tensor {
        let batch_size = query.size()[0];
        let num_queries = query.size()[1];
        let num_heads = 16;
        let num_points = 2;
        let head_dim = HIDDEN_DIM / num_heads;
        let value = self.value_proj.forward(memory);
        let offsets = self.sampling_offsets.forward(query).view([
            batch_size,
            num_queries,
            num_heads,
            1,
            num_points,
            2,
        ]);
        let weights = self
            .attention_weights
            .forward(query)
            .view([batch_size, num_queries, num_heads, num_points])
            .softmax(-1, None::<Kind>)
            .unsqueeze(3);
        let locations = reference_points
            .i((.., .., .., 0..2))
            .unsqueeze(2)
            .unsqueeze(4)
            + offsets / num_points as f64
                * reference_points
                    .i((.., .., .., 2..4))
                    .unsqueeze(2)
                    .unsqueeze(4)
                * 0.5;

        let value = value
            .transpose(1, 2)
            .contiguous()
            .view([batch_size, num_heads, head_dim, -1]);
        let value = value.view([
            batch_size * num_heads,
            head_dim,
            spatial_shape.0,
            spatial_shape.1,
        ]);
        let grid = (locations * 2.0 - 1.0)
            .i((.., .., .., 0))
            .transpose(1, 2)
            .flatten(0, 1);
        let sampled = value.grid_sampler_2d(&grid, 0, 0, false);
        let weights =
            weights
                .transpose(1, 2)
                .reshape([batch_size * num_heads, 1, num_queries, num_points]);
        let output = (sampled * weights)
            .sum_dim_intlist(&[-1i64][..], false, None::<Kind>)
            .view([batch_size, HIDDEN_DIM, num_queries])
            .transpose(1, 2)
            .contiguous();
        self.output_proj.forward(&output)
    }
}

/// The encoder's grid proposals and their validity mask, `[batch, height *
/// width, 4]` and `[batch, height * width, 1]`.
///
/// Upstream builds these from the memory grid alone
/// (`transformer.py:generate_encoder_output_proposals`), never from the memory
/// values, which is what lets `Transformer::encoder_proposals` cache them.
fn build_encoder_proposals(
    batch: i64,
    height: i64,
    width: i64,
    kind: Kind,
    device: Device,
) -> (Tensor, Tensor) {
    let options = (kind, device);
    let grid_y = Tensor::linspace(0.0, (height - 1) as f64, height, options);
    let grid_x = Tensor::linspace(0.0, (width - 1) as f64, width, options);
    let mesh = Tensor::meshgrid_indexing(&[grid_y, grid_x], "ij");
    let grid = Tensor::cat(&[mesh[1].unsqueeze(-1), mesh[0].unsqueeze(-1)], -1);
    let scale = Tensor::from_slice(&[width as f32, height as f32])
        .to_device(device)
        .to_kind(kind)
        .view([1, 1, 2]);
    let grid = (grid.unsqueeze(0) + 0.5) / scale;
    let wh = Tensor::ones_like(&grid) * 0.05;
    let proposals = Tensor::cat(&[grid, wh], -1).reshape([batch, height * width, 4]);
    let valid = proposals
        .gt(0.01)
        .logical_and(&proposals.lt(0.99))
        .all_dim(-1, true);
    (proposals, valid)
}

fn gen_sineembed_for_position(position: &Tensor) -> Tensor {
    let dim = HIDDEN_DIM / 2;
    let dim_t = Tensor::arange(dim, (position.kind(), position.device()));
    let exponent = dim_t.floor_divide_scalar(2) * 2.0 / dim as f64;
    let dim_t = (exponent * 10000f64.ln()).exp();
    let encode = |index: i64| {
        let embedded =
            position.i((.., .., index)).unsqueeze(-1) * (2.0 * std::f64::consts::PI) / &dim_t;
        Tensor::stack(
            &[
                embedded.slice(-1, 0, dim, 2).sin(),
                embedded.slice(-1, 1, dim, 2).cos(),
            ],
            3,
        )
        .flatten(2, 3)
    };
    Tensor::cat(&[encode(1), encode(0), encode(2), encode(3)], 2)
}

// https://github.com/roboflow/rf-detr/blob/4ab7c18729de9d02ffd0495795d0831b5630f01b/src/rfdetr/models/heads/segmentation.py
#[derive(Debug)]
struct SegmentationHead {
    blocks: Vec<DepthwiseConvBlock>,
    spatial_features_proj: nn::Conv2D,
    query_features_block: QueryMlpBlock,
    query_features_proj: nn::Linear,
    bias: Tensor,
}

impl SegmentationHead {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            blocks: (0..DECODER_LAYERS)
                .map(|index| DepthwiseConvBlock::new(&(path / "blocks" / index)))
                .collect(),
            spatial_features_proj: nn::conv2d(
                path / "spatial_features_proj",
                HIDDEN_DIM,
                HIDDEN_DIM,
                1,
                Default::default(),
            ),
            query_features_block: QueryMlpBlock::new(&(path / "query_features_block")),
            query_features_proj: nn::linear(
                path / "query_features_proj",
                HIDDEN_DIM,
                HIDDEN_DIM,
                Default::default(),
            ),
            bias: path.var("bias", &[1], nn::Init::Const(0.0)),
        }
    }

    /// The mask-projection inputs for this page.
    ///
    /// Only the final mask tensor is caller-visible. Running the six spatial
    /// blocks first and projecting the final query is algebraically identical
    /// to materializing all six intermediate mask tensors upstream.
    fn features(
        &self,
        spatial_features: &Tensor,
        query_features: &Tensor,
        image_size: (i64, i64),
    ) -> MaskFeatures {
        let mut spatial_features = spatial_features.upsample_bilinear2d(
            [image_size.0 / 4, image_size.1 / 4],
            false,
            None,
            None,
        );
        for block in &self.blocks {
            spatial_features = block.forward(&spatial_features);
        }
        MaskFeatures {
            spatial: self.spatial_features_proj.forward(&spatial_features),
            query: self
                .query_features_proj
                .forward(&self.query_features_block.forward(query_features)),
            bias: self.bias.shallow_clone(),
        }
    }
}

#[derive(Debug)]
struct DepthwiseConvBlock {
    dwconv: nn::Conv2D,
    norm: nn::LayerNorm,
    pwconv1: nn::Linear,
}

impl DepthwiseConvBlock {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            dwconv: nn::conv2d(
                path / "dwconv",
                HIDDEN_DIM,
                HIDDEN_DIM,
                3,
                nn::ConvConfig {
                    padding: 1,
                    groups: HIDDEN_DIM,
                    ..Default::default()
                },
            ),
            norm: layer_norm(&(path / "norm"), HIDDEN_DIM, 1e-6),
            pwconv1: nn::linear(path / "pwconv1", HIDDEN_DIM, HIDDEN_DIM, Default::default()),
        }
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        let residual = xs;
        let xs = self
            .norm
            .forward(&self.dwconv.forward(xs).permute([0, 2, 3, 1]));
        residual + self.pwconv1.forward(&xs).gelu("none").permute([0, 3, 1, 2])
    }
}

#[derive(Debug)]
struct QueryMlpBlock {
    norm_in: nn::LayerNorm,
    first: nn::Linear,
    second: nn::Linear,
}

impl QueryMlpBlock {
    fn new(path: &nn::Path<'_>) -> Self {
        Self {
            norm_in: layer_norm(&(path / "norm_in"), HIDDEN_DIM, 1e-5),
            first: nn::linear(
                path / "layers" / 0,
                HIDDEN_DIM,
                HIDDEN_DIM * 4,
                Default::default(),
            ),
            second: nn::linear(
                path / "layers" / 2,
                HIDDEN_DIM * 4,
                HIDDEN_DIM,
                Default::default(),
            ),
        }
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        xs + self
            .second
            .forward(&self.first.forward(&self.norm_in.forward(xs)).gelu("none"))
    }
}

#[derive(Debug)]
struct Mlp {
    layers: Vec<nn::Linear>,
}

impl Mlp {
    fn new(
        path: &nn::Path<'_>,
        input_dim: i64,
        hidden_dim: i64,
        output_dim: i64,
        num_layers: usize,
    ) -> Self {
        let mut layers = Vec::with_capacity(num_layers);
        for index in 0..num_layers {
            let in_dim = if index == 0 { input_dim } else { hidden_dim };
            let out_dim = if index + 1 == num_layers {
                output_dim
            } else {
                hidden_dim
            };
            layers.push(nn::linear(
                path / "layers" / index,
                in_dim,
                out_dim,
                Default::default(),
            ));
        }
        Self { layers }
    }

    fn forward(&self, xs: &Tensor) -> Tensor {
        let mut xs = xs.shallow_clone();
        for (index, layer) in self.layers.iter().enumerate() {
            xs = layer.forward(&xs);
            if index + 1 != self.layers.len() {
                xs = xs.relu();
            }
        }
        xs
    }
}

fn layer_norm(path: &nn::Path<'_>, hidden_dim: i64, eps: f64) -> nn::LayerNorm {
    nn::layer_norm(
        path,
        vec![hidden_dim],
        nn::LayerNormConfig {
            eps,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires the dynamically loaded LibTorch runtime"]
    async fn complete_checkpoint_tree_has_600_tensors() {
        crate::init().await.unwrap();
        let model = Model::new(Device::Cpu);
        assert_eq!(model.var_store.variables().len(), 600);
    }
}
