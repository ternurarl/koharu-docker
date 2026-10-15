# 上游同步计划（headless）

日期：2026-10-15
基线：fork `main` @ `5358464e`（`a23301f5` 之后的流水线页级并发提交；上游同步必须落在这份并发代码之上，不落在 `a23301f5`），上游 `koharu-rs/koharu:main` @ `52340cbf`（2026-10-15 重新解析），领先 25、落后 384。
文中 SHA 均锚定该快照；上游持续移动，执行时以实际 `upstream/main` 重新解析为准。

## 范围与不变量

同步目标只有 headless/core：翻译、OCR/ML、流水线、推理运行时、导出渲染。

- fork 独有提交必须全部保留：headless REST（`koharu-rpc`）、CPU-only / iGPU 模式、任务内存子进程（`pipeline-task`）、CPU 导出 workaround（`de9aae2d`）。同步在临时集成分支上做，只把审过的补丁落回 `main`。
- 不引入 `koharu-app` / `koharu-desktop` / `koharu-canvas` / `packages/*` 的改动；不引入 `koharu-metrics`（Google Analytics 遥测）；CI/发布链不需要。
- 只取核心提交，不整体合并上游 `main`。移植遵守 AGENTS.md 的 Upstream Alignment：结构可追溯到钉死的上游提交，有意分歧在受影响代码旁注明。
- 冲突热点：`koharu-pipeline` 与 `koharu-renderer` 同时被 fork 私有改动（内存子进程、CPU 导出）和上游重构（`6ebec470` WebGPU 画布、`b87b93b2` 应用重构）触及，逐提交审。

## P0 — 翻译层（`koharu-translator`）

fork 以远程 provider 翻译为主，收益最直接。范围：该 crate 的 26 个上游提交，重点：

- 新 provider 与模型目录：Grok、MiniMax（`800e32bb`）；目录自动发现（`412822d2`）与刷新（`08601a8f`）
- 新模型：Qwen 3.8 27B（`c17d7a07`）、uncensored Qwen 3.8（`e4c3d069`）
- 本地模型 reasoning（`87104fcf`）、采样调优（`9962e14c`）、prompt 强化（`efb28e30`）
- 性能：结构化推理优化（`e47620c0`）
- 健壮性：JSON 修复层容错（`e6d330c8`）、模型能力协商（`f842172e`、`5b6672cc`）、Gemini thinking 映射（`f53631b4`、`3da33914`）、LM Studio vision（`8ab7cde1`）、模型列表客户端超时缓存（`bc1a51b6`）

已知约束（2026-08-20 审计结论，仍适用）：

- `efb28e30` 的 Rust prompt 部分可直接取；其 manifest/lockfile 与 fork 漂移，单独对待。
- `8ab7cde1` 假设 `capabilities.vision` 必返，上游后续已改为可选；vision 判定按可选解析、缺省 false。
- `c17d7a07` 的生成参数以 `9962e14c` 的修正为准，不取过期值。
- `08601a8f` 之后的 `412822d2` 将多个目录改为动态发现，两者按最终形态取。

## P0 — 推理运行时（`koharu-runtime` 及 torch/llama/diffusion 绑定）

Docker GPU 镜像的命脉。范围：`koharu-runtime` / `koharu-torch*` / `koharu-llama*` / `koharu-diffusion*` 的 50+ 提交，重点：

- 平台支持：CUDA 13.3 + Torch 2.13、ROCm 10 全目标、Linux ARM64 CUDA、Linux ROCm Torch
- 可复现构建：bundled native releases、HuggingFace 修订钉死
- 冷启动：multipart 下载加速与断点重试
- 加速器发现简化、按设备能力选精度（`ce2e6fbe`）

已知约束：

- 下载链是演进序列（`b608a9f7` → `22dab373` → `1143fe77`），按最终形态取，不取中间态。
- ROCm 链（`7462036c`、`cf914e8b`、`01bc9705`、`8199a8b2`、`6c21c9ad`、`c6dab0c0`、`b830b495`、`1dda0844` 等）是线性依赖链，整链取，不拆开。
- `6959411c`（diffusion/ggml FFI）是高风险组：C ABI、Rust 包装层、原生包版本绑定在一起，需在目标设备上对着真实原生库构建验证，不能只过 Rust 检查。
- fork 的 `torch.rs` 与上游有 ROCm/布局漂移，相关改动按手移植对待。
- `4d9f3aca`（ROCm gfx1032 Winograd）仅在 gfx1032 是部署目标时才取。

## P1 — rasterizer limits 修复移植（`bcac8a89`）

结论（置信度高，详见评估）：该修复与背景丢失 bug 无关，**不能**替代 CPU 导出 workaround；但它修的 #1130 是 fork 同样存在的独立缺陷——>8192px 的 surface 被 wgpu 默认 limits 硬拒（长条扫描页、双页跨页、supersampled 导出会踩中）。

范围：上游 `request_device()`（向 adapter 申请其上限 limits）落到 fork 的 `koharu-renderer/src/raster.rs`（`RenderContext`/`device_id` 相关约 8 处），保留 fork 现有 `check_device_limit`，带 4×9000 surface 回归测试。评估已确认上游实现与 vello 0.9 的 `RenderContext` 初始化逐字段一致，可直接落进 `try_new`。

## P1 — ML/流水线修复

cherry-pick 级别的少量提交：

- `dceb8485` 检测阈值越界恢复（只取 `koharu-pipeline` 的 `detection.rs` / `stages/mod.rs`；`Processor::new` 变 infallible 与其调用点是同一单元）
- `acb7d99c` 检测/inpainting 重跑优化
- `e09feb87` RF-DETR 推理对齐上游
- `156c5fcf` Hayai OCR 模型
- `653eb14b` PaddleOCR-VL 采样：只取 PaddleOCR 自有的 512 token / 1.2 repetition penalty；其对全局 `GenerationOptions::default()` 的改动不属 OCR 所有，不取。

## P1 — PDF/归档导入（移植，非 cherry-pick）

用户决定保留为目标。上游实现（`79cc4d6c`）全在 GUI 命令层（`koharu-app/src/commands/import/{pdf,rar,zip}.rs`），不经过公共 crate，对 headless 无现成收益。需要自行移植到 `koharu-rpc` 路由层（对齐现有 projects 路由形态），上游实现仅作参考实现。

## P2 — 导出渲染质量（文字排版）

- `d4c075fe` 语言感知换行、`098d697f` 文本方向默认值、`b5f3c41c` 拼接气泡文字位置
- 仅取排版行为修复，避开 `6ebec470` / `b87b93b2` 的架构性改动。

## 保留 — CPU 导出 workaround（`de9aae2d`，不可删）

这是目前唯一挡住 vello atlas 背景丢失 bug 的东西：

- 根因在 vello_encoding 的 `MAX_ATLAS_SIZE=8192`² 装箱失败后静默清零尺寸；vello 0.11 仍未修（已核对 v0.11.0 源码）。
- 上游不是用 `bcac8a89` 修掉的，而是 `6ebec470` 用 GpuCompositor 架构性绕开（图像层不再走 `draw_image`）。
- 对 Docker 无头环境，CPU 合成反而更稳：图像层不依赖 GPU，软件 Vulkan / iGPU / 真 GPU 行为一致（atlas bug 是纯 CPU 侧逻辑，与驱动无关）。

残留缺口（已知，暂不处理）：旋转/错切图像层仍回退 vello，仍暴露于 atlas。若未来要彻底对齐上游，目标是移植 GpuCompositor 整套（图像层独立纹理 + 分块，golden 基线需重做），不是删 workaround。

## 顺序

translator → runtime → limits 修复 → ml/pipeline → PDF 移植 → renderer 排版。

translator 先行（最干净）；runtime 体量大，单独一轮；limits 修复与 workaround 结论绑定，不要在移植时顺手删 CPU 路径；pipeline/renderer 与 fork 私有改动同文件，逐提交审。
