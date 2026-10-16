# Koharu 升级 Hayai OCR v2.5 Nova 技术迁移指南与 Agent 上下文包

本文档为本地编码 Agent 提供在 [koharu-rs/koharu](https%3A%2F%2Fgithub.com%2Fkoharu-rs%2Fkoharu) 中将 OCR 模块从 [JustANormalTinkerer/hayai-ocr-v2](https%3A%2F%2Fhuggingface.co%2FJustANormalTinkerer%2Fhayai-ocr-v2) 升级至 [JustANormalTinkerer/hayai-ocr-v2.5-nova](https%3A%2F%2Fhuggingface.co%2FJustANormalTinkerer%2Fhayai-ocr-v2.5-nova) 的完整上下文、架构契约对比、逐步 Rust 代码实现以及验证流程。

## 1. 给本地 Agent 的任务指令（可直接复制作为 Prompt）

> **任务目标**：将仓库 `crates/koharu-ml/src/hayai_ocr/` 中的 Hayai OCR 模型从 `JustANormalTinkerer/hayai-ocr-v2` 升级为 `JustANormalTinkerer/hayai-ocr-v2.5-nova`（Commit: `2803d0dc005fa4396ab1e7c5a472d2c450a247f7`），针对 **纯 CPU（FP32）+ 8GB 内存** 环境完成适配并通过编译与推理验证。
>
> **执行约束**：
> 1. 先阅读 `crates/koharu-ml/src/hayai_ocr/` 下所有源码文件（`mod.rs`、`config.rs`、`model.rs`、`processor.rs` 及可能存在的子模块如 `decoder.rs`）以及 `crates/koharu-torch/` 中导出的 `Tensor` 与 `nn` 接口。
> 2. 对照本指南第 2、3 节中的 Tensor 契约与权重路径，先用脚本或测试验证 `hayai-ocr-v2.5-nova` 的 `model.safetensors` 精确键名（Key Names）与偏置项（Bias），再修改 Rust 代码。
> 3. 完成修改后，运行 `cargo check -p koharu-ml` 与 `cargo run -p koharu-ml --bin hayai_ocr` 进行端到端 CPU 推理验证。

## 2. 核心上下文与模型架构差异对比

### 2.1 模型仓库与版本基线

| 维度 | 当前实现 (Hayai OCR v2 / v2.1) | 目标实现 (Hayai OCR v2.5 Nova) |
| :--- | :--- | :--- |
| **Hugging Face Repo ID** | [JustANormalTinkerer/hayai-ocr-v2](https%3A%2F%2Fhuggingface.co%2FJustANormalTinkerer%2Fhayai-ocr-v2) | [JustANormalTinkerer/hayai-ocr-v2.5-nova](https%3A%2F%2Fhuggingface.co%2FJustANormalTinkerer%2Fhayai-ocr-v2.5-nova) |
| **Commit Revision SHA** | `4a4ce477c9a8841f208b94e1d9ed5c0938965e05` | `2803d0dc005fa4396ab1e7c5a472d2c450a247f7` (或 ONNX 对齐版 `e34d7755ed11e626c5ba39544af5d66f20ee57cc`) |
| **下载文件清单** | `config.json`, `model.safetensors` (623 MB), `tokenizer.json` | `config.json`, `model.safetensors` (628 MB), `tokenizer.json` |
| **CPU 推理精度** | `koharu_torch::Kind::Float` (FP32) | `koharu_torch::Kind::Float` (FP32) |
| **视觉编码器 (Vision Tower)** | `google/siglip2-base-patch16-naflex` (`d_vision = 768`, 12 层, Patch = 16) | 完全一致 (`d_vision = 768`, 12 层, Patch = 16) |
| **视觉投影器 (Projector)** | 2层 MLP (`768 -> 512 -> 512`)，1 Patch $\rightarrow$ 1 Token | **`DSCProjector`**：$2\times 2$ Pixel Unshuffle (`4 patches -> 1 token`) + `LayerNorm(3072)` + 2层 MLP (`3072 -> 512 -> 512`) + `RMSNorm(512)` |
| **解码器视觉序列长度** | $N = H \times W$（最大 `MAX_NUM_PATCHES = 256`） | $M = H_{\text{out}} \times W_{\text{out}} = \lceil H/2 \rceil \times \lceil W/2 \rceil$（最大 `MAX_NUM_PATCHES / 4`） |
| **2D mRoPE 位置编码网格** | 原始 Patch 网格 `(H, W)` | 下采样后的压缩网格 **`(H_out, W_out)`** |
| **解码器残差连接** | `x + attn_res_scale * attn(...)` / `x + ffn_res_scale * ffn(...)` | 一致（含每通道可学习缩放参数 `attn_res_scale` 与 `ffn_res_scale`，维度 `[512]`） |
| **词表与特殊 Token** | `vocab_size = 16004`, `BOS = 16001`, `EOS = 16002` | 一致 (`vocab_size = 16004`, `BOS = 16001`, `EOS = 16002`) |

### 2.2 `hayai-ocr-v2.5-nova` 的 `config.json` 原文

```json
{
  "architectures": [
    "HayaiModel"
  ],
  "auto_map": {
    "AutoConfig": "configuration_hayai.HayaiConfig",
    "AutoModel": "modeling_hayai.HayaiModel"
  },
  "d_ffn": 2048,
  "d_model": 512,
  "d_vision": 768,
  "dtype": "float32",
  "model_type": "hayai",
  "n_layers": 12,
  "num_ids_classes": 226,
  "transformers_version": "5.0.0",
  "vocab_size": 16004
}
```

> **注意**：`num_ids_classes: 226` 是训练期用于汉字部件（IDS）辅助分类头的超参数，推理阶段完全不使用，无任何计算开销。

### 2.3 参考 Python 实现与 Tensor 契约（源自 `modeling_hayai.py` 与 [bixii/hayai-ocr-v2.5-nova-onnx](https%3A%2F%2Fhuggingface.co%2Fbixii%2Fhayai-ocr-v2.5-nova-onnx)）

在 [JustANormalTinkerer/hayai-ocr-v2.5-nova](https%3A%2F%2Fhuggingface.co%2FJustANormalTinkerer%2Fhayai-ocr-v2.5-nova) 的 `modeling_hayai.py` 中，`DSCProjector` 的数学定义与计算顺序如下：

1. **输入特征与网格**：
   * 视觉编码器输出：`visual_features`，形状为 `[1, N_max, 768]`，其中前 `H * W` 个位置是有效 Patch（`H = spatial_shape[0]`, `W = spatial_shape[1]`）。
2. **奇偶边界复制填充（Replicate Padding）与 $2\times 2$ Pixel Unshuffle**：
   * 目标压缩网格尺寸：
     $$H_{\text{out}} = \frac{H + 1}{2}, \quad W_{\text{out}} = \frac{W + 1}{2}$$
   * 若 $H$ 为奇数（`H % 2 == 1`），复制最后一行（行索引 `H - 1`）追加到底部，使高度变为 $2 H_{\text{out}}$；若 $W$ 为奇数（`W % 2 == 1`），复制最后一列（列索引 `W - 1`）追加到右侧，使宽度变为 $2 W_{\text{out}}$。
   * 对每个下采样网格位置 $(y, x) \in [0, H_{\text{out}}) \times [0, W_{\text{out}})$，收集其对应的 $2\times 2$ Patch 块 $(2y + dy, 2x + dx)$（其中 $dy \in \{0, 1\}, dx \in \{0, 1\}$）。
   * **通道折叠顺序（极为关键）**：
     在 `modeling_hayai.py` 中，收集到的 $2\times 2$ 块形状为 `[B, L, 4, 768]`（其中 `4` 对应 `dy * 2 + dx`），随后执行 `g.transpose(2, 3).reshape(B, L, 3072)`（等价于对 `[1, 768, 2*H_out, 2*W_out]` 执行标准 `F.pixel_unshuffle(downscale_factor=2)` 后再将空间维展平到序列维）。
     因此，拼接后的 `3072` 维向量中，原始通道 `c`（`0..768`）与子块偏移 `(dy, dx)` 的精确扁平索引公式为：
     $$\text{unshuffled\_channel\_idx} = c \times 4 + dy \times 2 + dx$$
3. **投影器线性与归一化层**：
   * `self.norm`: `nn.LayerNorm(3072, eps=1e-5)`（含 `weight` 和 `bias`）
   * `self.mlp`:
     * `mlp.0`: `nn.Linear(3072, 512)`
     * 激活函数：`GELU`
     * `mlp.2`: `nn.Linear(512, 512)`
   * `self.out_norm`: `RMSNorm(512, eps=1e-6)`（仅含 `weight`）
4. **2D mRoPE（二维旋转位置编码）坐标**：
   * 频率表基底：$\theta_k = 1 / 10000^{2k / 32}$，$k \in [0, 16)$（因为 `head_dim = 64`，拆分为前 32 维管 $y$、后 32 维管 $x$，各 16 对旋转频率）。
   * 视觉 Token 在压缩网格 $(y, x) \in [0, H_{\text{out}}) \times [0, W_{\text{out}})$ 上的频率角为 $[y \cdot \theta_k, x \cdot \theta_k]$。
   * 文本 Token 在位置 $t$（`[BOS]` 对应 $t = 0$，后续生成 Token 依次为 $t = 1, 2, \dots$）上的频率角为 $[t \cdot \theta_k, t \cdot \theta_k]$。

## 3. Koharu 源码逐步修改指南

### 步骤 1：修改 `crates/koharu-ml/src/hayai_ocr/mod.rs`

将模型仓库宏指向 `JustANormalTinkerer/hayai-ocr-v2.5-nova`：

```rust
crate::model_repository!("JustANormalTinkerer/hayai-ocr-v2.5-nova" @ "2803d0dc005fa4396ab1e7c5a472d2c450a247f7" {
    CONFIG = "config.json",
    WEIGHTS = "model.safetensors",
    TOKENIZER = "tokenizer.json",
});
```

### 步骤 2：修改 `crates/koharu-ml/src/hayai_ocr/config.rs`

1. **检查 `HayaiConfig` 结构体**：
   确保 `#[derive(Deserialize)]` **没有** `#[serde(deny_unknown_fields)]`，或者显式加上可选字段以兼容 Nova 的 `config.json`：
   ```rust
   #[derive(Debug, Clone, serde::Deserialize)]
   pub struct HayaiConfig {
       pub vocab_size: i64,
       pub d_model: i64,
       pub d_vision: i64,
       pub d_ffn: i64,
       pub n_layers: usize,
       #[serde(default)]
       pub num_ids_classes: Option<i64>,
   }
   ```
2. **调整 Patch 预算与解码器视觉 Token 常量**：
   在 `config.rs` 的 `siglip2` / `decoder` 常量模块中：
   * 纯 CPU 8GB 内存场景下，`MAX_NUM_PATCHES` 可设为 `256`（极速模式，解码器最多仅 **64** 个视觉 Token）或 `384`（Nova 官方默认均衡模式，解码器最多 **96** 个视觉 Token）。
   * 新增下采样因子与压缩后最大视觉 Token 数常量：
   ```rust
   pub const MAX_NUM_PATCHES: usize = 384; // 若追求极致 CPU 速度可设为 256
   pub const SPATIAL_DOWNSCALE: i64 = 2;
   pub const MAX_DECODER_VISION_TOKENS: usize = MAX_NUM_PATCHES / 4; // 384 -> 96, 256 -> 64
   ```

### 步骤 3：修改 `crates/koharu-ml/src/hayai_ocr/model.rs`（及子模块）中的 `Projector`

找到定义视觉投影器（`Projector` 结构，路径为 `&decoder_path / "projector"`）的位置，将其从 2 层 MLP 替换为 `DSCProjector`。

> **Agent 动手前必查项**：
> 在加载 `model.safetensors` 时，先检查 `decoder.projector.mlp.0` 和 `decoder.projector.mlp.2` 是否包含 `.bias` 张量（在部分版本中 `mlp` 的 `Linear` 设置了 `bias=False`，而另一部分导出版本含 `bias=True`；同时检查 `out_norm` 是 `RMSNorm` 还是 `LayerNorm`）。可用一行 Python 或 `safetensors` 头部检查确认：
> ```bash
> python3 -c 'import json, struct; f=open("model.safetensors","rb"); n=struct.unpack("<Q",f.read(8))[0]; h=json.loads(f.read(n)); print([k for k in h.keys() if "projector" in k or "ids" in k])'
> ```

#### `DSCProjector` 的 Rust (`koharu-torch`) 结构与实现模板

```rust
#[derive(Debug)]
pub(super) struct DscProjector {
    norm: nn::LayerNorm,
    fc1: nn::Linear,
    fc2: nn::Linear,
    out_norm: RmsNorm, // 复用文件中已有的 RmsNorm 结构体
    in_dim: i64,
    out_dim: i64,
}

impl DscProjector {
    pub fn new(vs: &nn::Path, in_dim: i64, out_dim: i64, mlp_bias: bool) -> Result<Self> {
        let unshuffled_dim = in_dim * 4; // 768 * 4 = 3072
        let norm_cfg = nn::LayerNormConfig {
            eps: 1e-5,
            ..Default::default()
        };
        let norm = nn::layer_norm(vs / "norm", vec![unshuffled_dim], norm_cfg)?;

        let linear_cfg = nn::LinearConfig {
            bs: mlp_bias, // 根据 safetensors 实际是否含 decoder.projector.mlp.0.bias 设为 true 或 false
            ..Default::default()
        };
        let mlp_path = vs / "mlp";
        let fc1 = nn::linear(&mlp_path / "0", unshuffled_dim, out_dim, linear_cfg)?;
        let fc2 = nn::linear(&mlp_path / "2", out_dim, out_dim, linear_cfg)?;
        let out_norm = RmsNorm::new(&(vs / "out_norm"), out_dim, 1e-6)?;

        Ok(Self {
            norm,
            fc1,
            fc2,
            out_norm,
            in_dim,
            out_dim,
        })
    }

    /// 输入:
    /// - `visual_features`: [1, N_max, 768] (来自 SigLIP2 vision_encoder)
    /// - `patch_height`: H (有效 patch 行数)
    /// - `patch_width`: W (有效 patch 列数)
    /// 输出:
    /// - `(vision_embeddings, h_out, w_out)`，其中 `vision_embeddings` 形状为 `[1, h_out * w_out, 512]`
    pub fn forward(
        &self,
        visual_features: &Tensor,
        patch_height: i64,
        patch_width: i64,
    ) -> Result<(Tensor, i64, i64)> {
        let valid_patches = patch_height * patch_width;
        let c = self.in_dim; // 768

        // 1. 截取前 H * W 个有效 patch 并重塑为 [1, H, W, C] -> 转置为 [1, C, H, W]
        let feat_valid = visual_features.narrow(1, 0, valid_patches)?;
        let feat_2d = feat_valid
            .reshape([1, patch_height, patch_width, c])?
            .permute([0, 3, 1, 2])?
            .contiguous()?;

        // 2. 对奇数 H 或 W 进行右侧/底部 Replicate Padding，使其变为偶数
        let pad_h = patch_height % 2;
        let pad_w = patch_width % 2;
        let feat_padded = if pad_h > 0 || pad_w > 0 {
            let mut t = feat_2d;
            if pad_h > 0 {
                let last_row = t.narrow(2, patch_height - 1, 1)?;
                t = Tensor::cat(&[&t, &last_row], 2)?;
            }
            if pad_w > 0 {
                let cur_w = patch_width;
                let last_col = t.narrow(3, cur_w - 1, 1)?;
                t = Tensor::cat(&[&t, &last_col], 3)?;
            }
            t
        } else {
            feat_2d
        };

        let h_out = (patch_height + 1) / 2;
        let w_out = (patch_width + 1) / 2;

        // 3. 2x2 Pixel Unshuffle:
        // [1, C, h_out * 2, w_out * 2]
        // -> reshape [1, C, h_out, 2, w_out, 2]
        // -> permute [0, 2, 4, 1, 3, 5] (即 [1, h_out, w_out, C, 2, 2]，保证通道顺序为 c * 4 + dy * 2 + dx)
        // -> reshape [1, h_out * w_out, C * 4]
        let unshuffled = feat_padded
            .reshape([1, c, h_out, 2, w_out, 2])?
            .permute([0, 2, 4, 1, 3, 5])?
            .contiguous()?
            .reshape([1, h_out * w_out, c * 4])?;

        // 4. LayerNorm(3072) -> Linear(3072, 512) -> GELU -> Linear(512, 512) -> RMSNorm(512)
        let normed = self.norm.forward(&unshuffled)?;
        let hidden = self.fc1.forward(&normed)?.gelu("none")?;
        let proj = self.fc2.forward(&hidden)?;
        let out = self.out_norm.forward(&proj)?;

        Ok((out, h_out, w_out))
    }
}
```

### 步骤 4：修改 `prefill` 与 2D mRoPE 逻辑（`crates/koharu-ml/src/hayai_ocr/model.rs`）

在 `model.rs` 的 `prefill` 函数中，原本的逻辑是将 `[1, MAX_NUM_PATCHES, 768]` 直接映射为 `[1, MAX_NUM_PATCHES, 512]`，并用原始 `(patch_height, patch_width)` 计算 `visual_rope(patch_height, patch_width)`。升级到 Nova 后需做如下同步修改：

1. **调用 `DSCProjector` 获取下采样后的特征与尺寸 `(h_out, w_out)`**：
   ```rust
   let (vision_valid, h_out, w_out) = self
       .decoder
       .projector
       .forward(&visual_features, patch_height, patch_width)?;
   let covered_tokens = h_out * w_out;
   ```
2. **如果 `Model::prefill` 使用固定长度的视觉前缀缓冲区（Static Padded Buffer `m_vision`）**：
   * 将 `m_vision` 的固定长度从 `MAX_NUM_PATCHES`（256）改为 `MAX_DECODER_VISION_TOKENS`（`MAX_NUM_PATCHES / 4`，即 64 或 96）。
   * 若 `covered_tokens < m_vision`，在 `vision_valid` 后补零（Padding）至 `[1, m_vision, d_model]`，并在构建 Block-Causal Attention Mask 时，将 `[0..covered_tokens]` 标记为有效视觉位置，将 `[covered_tokens..m_vision]` 屏蔽（设为 `-inf` / 最小浮点数）。
   * 如果 `prefill` 支持动态视觉长度，则直接令 `m_vision = covered_tokens`，此时连 Padding Mask 都无需额外处理，全部 `covered_tokens` 均为有效视觉 Token，CPU 计算最快！
3. **使用 `(h_out, w_out)` 计算 2D mRoPE**：
   ```rust
   // 注意：这里必须传入下采样后的 (h_out, w_out)，而不是原始的 (patch_height, patch_width)！
   let (visual_cos, visual_sin) = self.visual_rope(h_out, w_out)?;
   ```
   * 随后的 Padding 补齐（若使用固定长度 `m_vision`）同样从 `covered_tokens = h_out * w_out` 补齐到 `m_vision`（填充 `cos = 1.0, sin = 0.0`），接着在位置 `m_vision` 处拼接 `[BOS]` Token 对应的文本 RoPE（位置索引 `t = 0`，即 `cos = 1.0, sin = 0.0`）。

### 步骤 5：处理可能存在的 `ids_head` 多余权重（VarStore 严格性检查）

在 `JustANormalTinkerer/hayai-ocr-v2.5-nova` 的训练过程中包含一个辅助分类头 `ids_head`。
* 检查 `crates/koharu-ml/src/hayai_ocr/model.rs` 中 `VarStore::load(&weights_path)` 的行为：
  * 如果 `koharu_torch::nn::VarStore::load` 仅按已注册的模块路径读取张量（忽略文件中未注册的键），则无需任何额外改动。
  * 如果 `VarStore::load` 会因为文件中存在未注册的键（如 `decoder.ids_head.weight`）而报错，则在 `Decoder` 中注册一个占位的线性层或使用 `load_partial` 忽略该键。

## 4. 本地 Agent 验证与测试流程

修改完成后，请 Agent 按顺序执行以下验证步骤：

1. **编译检查**：
   ```bash
   cargo check -p koharu-ml
   ```
2. **单元测试与基准测试检查**：
   ```bash
   cargo test -p koharu-ml -- hayai
   ```
3. **CLI 独立推理测试（验证模型自动下载、权重加载与单图 OCR 输出）**：
   `koharu-ml` 自带了一个独立的二进制入口 `crates/koharu-ml/src/bin/hayai_ocr.rs`，可直接用于 CPU 端到端测试：
   ```bash
   RUST_LOG=debug cargo run --release -p koharu-ml --bin hayai_ocr -- path/to/test_crop.png
   ```
4. **验收标准**：
   * `model.safetensors` 加载无维度不匹配或缺失键报错。
   * 对测试日文/中文漫画气泡裁切图输出的文本准确、无乱码或无限重复字符（确认贪婪解码在遇到 `EOS = 16002` 时正常终止）。
   * 完整编译桌面端流水线：`cargo check -p koharu-pipeline`。
