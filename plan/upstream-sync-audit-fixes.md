# 上游同步逐项审计：P1/P2 修复与 PDF 导入移植设计

日期：2026-10-15。本文是对 `plan/UPSTREAM_SYNC_PLAN.md`（实现在 `plan/UPSTREAM_SYNC_PLAN.md`，本轮审计对象）中「P1 — rasterizer limits 修复移植」「P1 — ML/流水线修复」「P1 — PDF/归档导入」「P2 — 导出渲染质量」「保留 — CPU 导出 workaround」五节的逐提交核对结果，只做审计与移植设计，不实施移植。

基线事实（本轮重新核实）：fork 仓库 `D:\Github\koharu-docker` 的 `main` 在审计开始时为 `a23301f5`，审计进行期间工作树上的未提交改动被提交为 `5358464e`、`4ccad2d3`、`91ed2f2e`，当前 `main` 为 `91ed2f2e`。remote-tracking ref `upstream/main` 为 `52340cbf`，merge-base 为 `c79882d9`。本文所有 fork 文件行号以 `91ed2f2e` 工作树为准。

已移植判定方法：fork 提交的 commit message body 中写有 `Upstream: <上游SHA>` 的记为已移植。fork 全部此类提交为 `e592ffb9`（对应上游 `f8a25e2a`）、`68650b09`（对应上游 `dceb8485 + b87b93b2` 的 pipeline/ml 子集）、`03106e91`（对应上游 `6959411c`）。其中 `68650b09` 与本审计范围相交，下文第 1 节逐项核对其残留差异。

状态五档定义：已移植（注明 fork 提交号）／可 cherry-pick（上游补丁上下文与 fork 目标文件一致，可直接应用）／手工移植（需按本文件给出的改动点手工落补丁）／跳过-GUI（该提交只触及排除路径，不移植）／暂缓（注明原因）。

---

## 1. 逐提交状态表

| 上游 SHA | 标题 | 状态 | 涉及文件 | 备注 |
| --- | --- | --- | --- | --- |
| `bcac8a89` | fix(rasterizer): request the adapter's texture and buffer limits (#1168) | 手工移植 | 上游 `crates/koharu-rasterizer/src/native.rs`、`crates/koharu-rasterizer/tests/compositor.rs`、`crates/koharu-renderer/benches/rendering.rs`；落到 fork `crates/koharu-renderer/src/raster.rs` | 上游把光栅化器拆成了 `koharu-rasterizer` crate，fork 没有该 crate，等价代码在 `crates/koharu-renderer/src/raster.rs`。逐处改动点见第 2 节。不修背景丢失 bug，不替代 CPU 导出 workaround `de9aae2d`。上游的 bench 改动（`crates/koharu-renderer/benches/rendering.rs`）在 fork 侧可选，不列入必做项 |
| `dceb8485` | fix: recover from an out-of-range detection threshold (#912) | 已移植（fork `68650b09`） | 上游 `crates/koharu-pipeline/src/stages/detection.rs`、`crates/koharu-pipeline/src/stages/mod.rs` | 残留差异：fork `crates/koharu-pipeline/src/stages/detection.rs` 相对上游 `b87b93b2` 的同名文件只多 4 行 `async fn release`（fork 私有的模型内存释放方法，见 `f16c64e6` 及后续提交），阈值回退逻辑与测试都在位。`crates/koharu-pipeline/src/stages/mod.rs` 中 `detection::Processor::new(...)` 无 `?`，与上游 `dceb8485` 一致。无需再动 |
| `acb7d99c` | fix(pipeline): refine detection and inpainting reruns | 手工移植 | `crates/koharu-pipeline/src/stage_runner.rs`、`crates/koharu-pipeline/src/stages/detection.rs`、`crates/koharu-pipeline/src/stages/inpainting.rs`、`crates/koharu-pipeline/src/stages/mod.rs` | 语义：重跑时整页跳过已有文字的检测页；自动 inpaint 跳过已有 cleanup 结果的页；删除旧的 `previous_texts` / `remove_unmatched_texts` / `take_previous_text` / `geometry_bounds` / `overlap_over_smaller` 逐条匹配复用路径。fork 四个文件都在同一 `impl` 块或同一函数体带私有改动（`release` 方法、`5358464e` 的加速器门控），插入点必然冲突，详见第 6 节。上游对 `detection.rs` 的大块删除 hunk 上下文与 fork 一致，可整块搬 |
| `e09feb87` | fix(ml): align RF-DETR inference with upstream | 可 cherry-pick | `crates/koharu-ml/src/koharu_layout_rfdetr_seg_2xl/{mod,model,processor}.rs` | `model.rs`、`processor.rs` 与上游 `e09feb87~1` 逐字节一致（blob 一致）；`mod.rs` fork 只有第 25 行 `crate::model_repository!` 与上游 `model_repository!` 的 1 行差异，不在本提交任何 hunk 的上下文内。行为变化两处：`model.rs` 的 encoder 框解码改为先按分数取 topk 再算框（不再对全部 query 算框后丢弃）；`processor.rs` 的缩放从 `internal_upsample_bilinear2d_aa` 改为 `upsample_bilinear2d`（关掉抗锯齿，对齐训练期 Albumentations 缩放） |
| `156c5fcf` | feat: add Hayai OCR model (#988) | 手工移植 | 取：`crates/koharu-ml/src/hayai_ocr/{config,mod,model,processor}.rs`（新增 4 文件）、`crates/koharu-ml/src/bin/hayai_ocr.rs`、`crates/koharu-ml/benches/hayai_ocr.rs`、`crates/koharu-ml/Cargo.toml`、`crates/koharu-ml/src/lib.rs`、`crates/koharu-pipeline/src/{config.rs,bin/run.rs,stages/ocr.rs}`。不取：`README.md`、`bun.lock`、`docs/models/vision-and-inpainting.md`、`packages/bridge/src/protocol.ts`、`packages/koharu/components/preferences/PipelinePreferences.tsx`、`packages/koharu/components/preferences/models.ts` | 新模型文件可整取（`model.rs` 812 行、`processor.rs` 267 行、`mod.rs` 112 行、`config.rs` 78 行）。`crates/koharu-pipeline/src/config.rs` 与上游 `156c5fcf~1` 一致，hunk 干净。`crates/koharu-pipeline/src/stages/ocr.rs`、`crates/koharu-ml/src/lib.rs`、`crates/koharu-pipeline/src/bin/run.rs` 有 fork 私有漂移（`release` 方法、CPU/GPU 分离初始化 `init_cpu`/`init_gpu`、`RuntimeInit`），hunk 需手工贴入 |
| `653eb14b` | feat(ocr): tighten PaddleOCR-VL sampling | 手工移植 | 取：`crates/koharu-ml/src/paddle_ocr_vl/mod.rs`、`crates/koharu-ml/src/paddle_ocr_vl/model.rs`、`crates/koharu-ml/src/paddle_ocr_vl_quantized.rs`。不取：`crates/koharu-ml/src/llm/mod.rs` | 三个目标文件与上游 `653eb14b~1` 逐字节一致，取舍清单见第 3 节。不取的 `llm/mod.rs` hunk 把全局 `GenerationOptions::default()` 的 `repeat_penalty` 从 1.1 改成 1.2，会影响所有本地 LLM 翻译，不属于 PaddleOCR 采样参数 |
| `79cc4d6c` | feat: support archive and PDF page imports | 手工移植（本轮只做设计，不实现） | 上游 `crates/koharu-app/src/commands/import/{mod,pdf,rar,zip}.rs`、`crates/koharu-app/src/commands/lifecycle.rs`；目标为 fork `crates/koharu-rpc` | 上游实现全部在 Tauri GUI 命令层，按 cherry-pick 无法落到 headless。移植设计见第 4 节：在 `crates/koharu-rpc` 新建 import 模块并对齐 `POST /projects/current/pages` |
| `d4c075fe` | fix(renderer): apply language-aware line breaking | 可 cherry-pick | `crates/koharu-renderer/src/layout.rs`、`crates/koharu-renderer/src/segment.rs` | 两个文件与上游 `d4c075fe~1` 逐字节一致（blob 一致）。行为变化：`segment.rs` 增加韩文 `LineBreakWordOption::KeepAll` 分段器、中文保护区间改为 `contains_chinese` 判定并重命名 `apply_protected_ranges`；`layout.rs` 删掉 `with_chinese_word_segmentation()` 调用，中文分词改为默认开启 |
| `098d697f` | fix(renderer): respect text direction defaults | 手工移植 | `crates/koharu-renderer/src/renderer.rs` | fork 的 `resolve_writing_mode`（`crates/koharu-renderer/src/renderer.rs:1426-1447`）停留在文件创建时（fork `69d64490`）的形态：先判中日文再看 `typography.writing_mode`。上游 `098d697f` 的最终形态是：用户手改的 `writing_mode` 优先于语言默认，检测生成的 `writing_mode` 只对中日文生效。不能 cherry-pick（函数体上下文不一致），按最终形态整函数替换 |
| `b5f3c41c` | fix(renderer): preserve joined balloon text placement | 手工移植 | `crates/koharu-renderer/src/{bubble,layout,renderer,text_renderer}.rs` | 行为目标：拼接气泡（同一气泡内多个 `FlowsIn` 文字层）的文字位置在变换后保持排版形状。实现方式是把 `ResolvedPlacement::flow_contour`（`crates/koharu-renderer/src/renderer.rs:966`）整条链路删除，改为每个 flow 直接持久化自己的 `Geometry` 多边形；`layout.rs` 同步删掉 `with_comic_balloon_constraints` 多轮廓约束。前置依赖：上游 `6d533281` 把 `geometry_frame` 签名改成 `geometry_frame(geometry, angle_degrees)`（fork `crates/koharu-renderer/src/bubble.rs:632` 仍是单参数），`b5f3c41c` 的 `renderer.rs` hunk 调用了双参数形态。依赖取舍需要用户裁决，见第 8 节 |
| `6d533281` | fix(canvas): preserve authored text rotation | 手工移植（仅 headless 子集），是否纳入本轮由用户裁决 | 取：`crates/koharu-renderer/src/bubble.rs`、`crates/koharu-renderer/src/renderer.rs`、`crates/koharu-scene/src/components/layers.rs`。不取：`crates/koharu-app/src/commands/project.rs`、`crates/koharu-desktop/src/lib.rs`、`packages/*` | `b5f3c41c` 的前置。行为目标：旋转过的文字区域按作者坐标系测量包围盒，而不是按世界坐标轴测。`crates/koharu-scene` 不在排除清单内，可取；`koharu-app`、`koharu-desktop`、`packages/*` 按范围排除 |

范围排除核对：以上提交中触及排除路径的有 `dceb8485`（`packages/koharu/components/preferences/PreferenceFields.tsx`、`packages/koharu/tests/components/editor-components.test.tsx`，已随 `68650b09` 只取 pipeline 子集）、`156c5fcf`（4 个 packages/docs/README/bun.lock 文件）、`79cc4d6c`（`koharu-app` 全部、`packages/*`、`docs/*`）、`6d533281`（`koharu-app`、`koharu-desktop`、`packages/*`）。`acb7d99c`、`e09feb87`、`653eb14b`、`d4c075fe`、`098d697f`、`b5f3c41c`、`bcac8a89` 不触及排除路径（`bcac8a89` 的 `crates/koharu-renderer/benches/rendering.rs` 属 koharu-renderer，范围内，可选取舍）。

五档数量：已移植 1 项（`dceb8485`）；可 cherry-pick 2 项（`e09feb87`、`d4c075fe`）；手工移植 7 项（`bcac8a89`、`acb7d99c`、`156c5fcf`、`653eb14b`、`79cc4d6c`、`098d697f`、`b5f3c41c`，其中 `6d533281` 作为 `b5f3c41c` 前置另计）；跳过-GUI 0 项（GUI 排除发生在 hunk 级，见各行备注）；暂缓 0 项（`b5f3c41c` 的前置裁决点见第 8 节，未裁决前该提交本身不开工）。

---

## 2. `bcac8a89` 逐处改动点与回归测试方案

### 2.1 与 vello 0.9 的逐字段一致性核对结论

计划中的说法「上游实现与 vello 0.9 的 `RenderContext` 初始化逐字段一致、可直接落进 `try_new`」经核对成立，证据如下。vello 0.9.0 源码位置：`D:\RUST\.cargo\registry\src\rsproxy.cn-e3de039b2554c837\vello-0.9.0\src\util.rs`。

上游 `bcac8a89` 的 `request_device()`（`crates/koharu-rasterizer/src/native.rs`，该文件在 fork 不存在）与 vello 0.9.0 `RenderContext::new`（`util.rs:32-49`）构造 `wgpu::InstanceDescriptor` 的字段逐项对照：

| 字段 | vello 0.9.0 `util.rs` | 上游 `bcac8a89` `request_device()` | 是否一致 |
| --- | --- | --- | --- |
| `display` | `None`（`util.rs:39`） | `None` | 一致 |
| `backends` | `wgpu::Backends::from_env().unwrap_or_default()`（`util.rs:33`） | 相同表达式 | 一致 |
| `flags` | `wgpu::InstanceFlags::from_build_config().with_env()`（`util.rs:34`） | 相同表达式 | 一致 |
| `memory_budget_thresholds` | `wgpu::MemoryBudgetThresholds::default()`（`util.rs:35`） | 相同表达式 | 一致 |
| `backend_options` | `wgpu::BackendOptions::from_env_or_default()`（`util.rs:36`） | 相同表达式 | 一致 |

adapter 选取一致：vello 0.9.0 `new_device`（`util.rs:161-165`）调用 `wgpu::util::initialize_adapter_from_env_or_default(&self.instance, compatible_surface)`，fork 的 `device(None)` 路径传入 `compatible_surface = None`，与上游 `request_device()` 的 `initialize_adapter_from_env_or_default(&instance, None)` 相同。

设备特性一致：vello 0.9.0 `util.rs:166-168` 取 `adapter.features() & (CLEAR_TEXTURE | PIPELINE_CACHE)`；上游取值表达式相同。fork 的工作区 `Cargo.toml:129` 为 `vello = "0.9"`，未启用 `wgpu-profiler` 特性，因此 vello 0.9.0 `util.rs:169-170` 的条件编译分支不参与，无额外 timer 特性差异。

两处非实质差异：vello 0.9.0 `util.rs:174` 的 `DeviceDescriptor.label` 是 `None`，上游写 `Some("koharu rasterizer device")`（只影响调试标签）；vello 0.9.0 `util.rs:167` 的 `required_limits` 是 `Limits::default()`，上游替换为取 adapter 上限的 `max_texture_dimension_2d` 与 `max_buffer_size`，其余字段仍用 `Limits::default()`——这正是本修复的全部语义。

fork 依赖 wgpu 29.0.4（`Cargo.lock`），与 vello 0.9.0 同源编译，上述字段名在该 wgpu 版本存在，`request_device()` 代码可直接编译。

### 2.2 fork `crates/koharu-renderer/src/raster.rs` 的八处改动点

1. `crates/koharu-renderer/src/raster.rs:15`：从 `vello` 的 use 列表中删除 `util::RenderContext`（第 13-16 行的 use 块其余项保留）。
2. `crates/koharu-renderer/src/raster.rs:89-94`（`struct GpuState`）：删除第 90 行 `context: RenderContext` 与第 91 行 `device_id: usize`，插入 `device: wgpu::Device`、`queue: wgpu::Queue` 两个字段。`renderer`、`targets` 字段不动。fork 的 `GpuState` 没有上游的 `compositor: GpuCompositor` 字段（那是上游 `b87b93b2` 的 `koharu-renderer/src/compositor.rs`，fork 用 `crates/koharu-renderer/src/compose.rs` 的 CPU 合成替代），改动比上游 `bcac8a89` 少一处。
3. `crates/koharu-renderer/src/raster.rs:120-122`（`try_new`）：删除 `let mut context = RenderContext::new();` 与 `let device_id = pollster::block_on(context.device(None)).context("no WGPU adapter supports Vello's required features")?;`，替换为 `let (device, queue) = pollster::block_on(request_device())?;`。
4. `crates/koharu-renderer/src/raster.rs:124`：`vello::Renderer::new` 的第一个参数从 `&context.devices[device_id].device` 改为 `&device`。第 125-129 行的 `RendererOptions { antialiasing_support: AaSupport::area_only(), .. }` 不动。
5. `crates/koharu-renderer/src/raster.rs:132-139`：`Ok(Self { gpu: Mutex::new(GpuState { context, device_id, renderer, targets: Vec::new() }) })` 改为构造 `device, queue, renderer, targets`。
6. `crates/koharu-renderer/src/raster.rs:399-408`（`readback` 的锁内解构）：`let GpuState { context, device_id, renderer, targets } = &mut *gpu;` 改为 `let GpuState { device, queue, renderer, targets } = &mut *gpu;`，随后 `let device = context.devices[*device_id].device.clone(); let queue = &context.devices[*device_id].queue;` 改为 `let device = device.clone();`（`queue` 保持引用用法，`render_to_texture` 与 `queue.submit` 两处调用点在 421-451 行，签名不变）。
7. `crates/koharu-renderer/src/raster.rs` 第 488 行（`impl Rasterizer` 结束）之后、第 490 行 `impl RenderTarget` 之前：插入 `async fn request_device() -> Result<(wgpu::Device, wgpu::Queue)>`，函数体取上游 `bcac8a89` 的 `request_device()`，字段值按 2.1 节表格写入，错误消息保留 `"no WGPU adapter supports Vello's required features"`。fork 的 `raster.rs` 用 `anyhow::Result`，返回类型写 `Result<(wgpu::Device, wgpu::Queue)>`。
8. `crates/koharu-renderer/src/raster.rs` 文件末尾（第 533 行 `rgba` 函数之后）：新增 `#[cfg(test)] mod tests`，内容见 2.3。

### 2.3 保留项与回归测试方案

保留项：`crates/koharu-renderer/src/raster.rs:409-415` 的内联检查必须原样保留。该检查读取 `device.limits()`，当 `width` 或 `height` 超过 `limits.max_texture_dimension_2d` 时返回错误，是 fork 侧的设备上限守卫（上游 `bcac8a89` 对应函数名 `check_device_limit`，fork 没有同名函数，等价逻辑就是这 7 行）。本修复提高的是设备申请到的上限，检查逻辑本身不需要改。同样保留 `crates/koharu-renderer/src/compose.rs`（`de9aae2d` 的 CPU 图像合成）、`crates/koharu-renderer/src/frame.rs`、`crates/koharu-renderer/src/renderer.rs` 中 CPU 导出路径的全部代码：`bcac8a89` 只改 wgpu 设备申请方式，与 vello 图像图集装箱失败导致的背景丢失 bug 无关（该 bug 的根因在 vello_encoding 的 `MAX_ATLAS_SIZE` 装箱失败后静默清零尺寸，见 `plan/RENDER_BACKGROUND_LOSS_REPORT.md`），不能删 CPU 路径。

回归测试方案（对应上游 `crates/koharu-rasterizer/tests/compositor.rs` 的 `rasterizes_surfaces_beyond_the_default_wgpu_texture_limit`）：fork 的 `Rasterizer` 是 `pub(crate)`，但 `crates/koharu-renderer/src/raster.rs:330` 已有 `pub(crate) fn rasterize_scene(&self, scene, width, height, background, options)`，签名与上游测试调用的 `rasterize_scene` 一致，测试放在同文件 `#[cfg(test)] mod tests` 内即可直接调用：

```rust
#[test]
fn rasterizes_surfaces_beyond_the_default_wgpu_texture_limit() {
    let image = Rasterizer::new()
        .unwrap()
        .rasterize_scene(
            &Scene::new(),
            4,
            9000,
            [10, 20, 30, 255],
            RasterOptions::default(),
        )
        .unwrap();
    assert_eq!(image.dimensions(), (4, 9000));
    assert_eq!(image.get_pixel(3, 8999).0, [10, 20, 30, 255]);
}
```

环境前提：该测试要求 wgpu adapter 的 `max_texture_dimension_2d` 至少 9000。Docker 无头环境若用软件 Vulkan 实现且该实现自报 8192，测试无法通过，此时应把测试标 `#[ignore]` 并在注释里写明前提，而不是改断言。在有独显或上限更高的软件实现上必须跑通。

可选补充（不阻塞）：把上游 `crates/koharu-renderer/benches/rendering.rs` 的 4096×6144、8192×8192 基准补进 fork 同名文件，用于观察修复前后的大 surface 耗时。

---

## 3. `653eb14b` 取舍清单

取的行（fork 当前文件与上游 `653eb14b~1` 逐字节一致，行号可直接对照上游 `653eb14b` 的 diff）：

1. `crates/koharu-ml/src/paddle_ocr_vl/mod.rs`：在 `use self::{model::Model, processor::Processor};` 之后插入两行常量 `pub(super) const MAX_NEW_TOKENS: usize = 512;` 与 `pub(super) const REPETITION_PENALTY: f32 = 1.2;`；把推理调用里的字面量 `512`（fork 当前第 86 行）换成 `MAX_NEW_TOKENS`。
2. `crates/koharu-ml/src/paddle_ocr_vl/model.rs`：
   - `use std::path::Path;` 改为 `use std::{collections::HashSet, path::Path};`；
   - 引入 `REPETITION_PENALTY`；
   - 两处生成循环（增量解码与全量重算解码）把「`argmax` 取下一 token」改为「先算 `logits`，再由 `greedy_token(&logits, seen_ids.as_ref())` 取下一 token」，并维护 `unique_tokens: HashSet<i64>` 与 `seen_ids: Option<Tensor>`；
   - 新增函数 `fn greedy_token(logits: &Tensor, seen_ids: Option<&Tensor>) -> i64`：对已出现 token 的 logit 施加重复惩罚（正 logit 乘 `REPETITION_PENALTY`，负 logit 除 `REPETITION_PENALTY`）后取 `argmax`。
3. `crates/koharu-ml/src/paddle_ocr_vl_quantized.rs`（fork 是单文件，不是上游改名后的 `paddle_ocr_vl_quantized/mod.rs` 目录）：`GenerationOptions` 字面量中增加 `max_tokens: MAX_NEW_TOKENS`，`repeat_penalty` 从 `1.0` 改为 `REPETITION_PENALTY`，增加 `repeat_last_n: MAX_NEW_TOKENS as i32`。常量从 `paddle_ocr_vl` 模块以 `pub(super)` 可见性引入，或改为 `pub(crate)` 后跨模块引用。

不取的行：

1. `crates/koharu-ml/src/llm/mod.rs` 的 `GenerationOptions::default()` 中 `repeat_penalty: 1.1` 改 `1.2`（上游 diff 的 `llm/mod.rs` 那 1 行）。该默认值作用于全部本地 LLM 调用（翻译链路的 `GenerationOptions` 也从这里取默认），不属于 PaddleOCR 自有采样参数。fork 当前 `crates/koharu-ml/src/llm/mod.rs:276` 是 `repeat_penalty: 1.1`，保持不动。
2. 文件重命名 `paddle_ocr_vl_quantized.rs` → `paddle_ocr_vl_quantized/mod.rs`。这是纯目录结构调整，与采样语义无关；fork 保持单文件即可，避免无收益的路径变动。若后续要与上游目录对齐，单独做一次纯移动提交。

---

## 4. PDF 与归档导入的移植设计（任务书第 3 项，本轮不实现）

### 5.1 上游实现的构成

上游 `79cc4d6c` 在 `crates/koharu-app/src/commands/import/` 下新增四个文件，全部是 GUI 命令层私有（`pub(super)`），不经过任何公共 crate：

- `mod.rs`（148 行）：`Format` 枚举（Raster / Zip / Rar / Pdf，用 `strum` 从扩展名解析）、`EncodedPage`（名字 + 原始字节）、`Page`（名字 + 字节 + `image::ImageFormat` + 宽高）、`decode`（从字节猜格式并读尺寸）、入口 `import(Vec<PathBuf>)`（按文件名自然排序、`rayon` 并行、按扩展名分派到 zip/rar/pdf 或单图读取）。
- `pdf.rs`（133 行）：`render(&Path)` 用 `hayro` crate 把 PDF 逐页渲染成 300 DPI PNG（`PDF_SCALE = 300.0 / 72.0`，白底），页名 `{stem}-{页码:0宽度}.png`。
- `zip.rs`（140 行）：`extract(&Path)` 用 `zip::ZipArchive` 读 ZIP/CBZ，只留光栅图扩展名成员，跳过 `._*`、`.DS_Store`、`Thumbs.db`、`desktop.ini`、`__MACOSX`，按成员名自然排序。
- `rar.rs`（152 行）：`extract(&Path)` 用 `rars::ArchiveReader::read_path` + `extract_to` 回调，过滤规则与 zip.rs 相同，`PageWriter` 在 `Drop` 时把收集到的成员字节推入结果。

上游的 GUI 侧接线在 `crates/koharu-app/src/commands/lifecycle.rs` 的 `import_pages`：文件对话框过滤器改为 `import::Format` 的全部扩展名，解码逻辑从函数体内联代码改为调用 `import::import(files)`。

### 5.2 fork 侧落点与现有形态

fork 的 headless 命令面在 `crates/koharu-rpc`。路由组装在 `crates/koharu-rpc/src/api.rs:12-19`，其中 `routes::projects::router()` 定义在 `crates/koharu-rpc/src/routes/projects.rs:62-73`。现有页面导入端点是 `POST /projects/current/pages`（`routes/projects.rs:70`，处理函数 `import_pages` 在 `routes/projects.rs:129-173`），行为是：读 multipart 的每个字段，字段名作为页名，`image::guess_format` 猜格式，`ImageReader::into_dimensions` 读宽高，收集为 `Vec<(String, Vec<u8>, String, u32, u32)>`，交给 `crates/koharu-rpc/src/project.rs:177-206` 的 `import_images` 一次性 `patch` + `commit`，返回 `Json<ProjectInfo>`。`HEADLESS.md:93` 把该端点描述为「multipart image import (adds pages)」。`crates/koharu-rpc/src/project.rs:41-100` 的 `ProjectLibrary` 管理 `<data>/projects/<name>.khrproj`，`AppState` 在 `crates/koharu-rpc/src/state.rs`。

### 5.3 新建文件与职责

在 `crates/koharu-rpc/src/import/` 下新建四个文件，模块挂进 `crates/koharu-rpc/src/lib.rs`（现有 `mod routes;` 旁加 `mod import;`）：

| 新文件 | 职责 | 从上游搬什么 | 要重写什么 |
| --- | --- | --- | --- |
| `crates/koharu-rpc/src/import/mod.rs` | 格式识别、单图解码、入口编排 | `Format` 枚举、`EncodedPage`、`Page`、`decode` 全部照搬；`import` 的排序、并行、分派骨架照搬 | 入口签名从 `import(Vec<PathBuf>)` 重写为 `import_uploads(Vec<(String, Vec<u8>)>) -> anyhow::Result<Vec<Page>>`：输入是 multipart 上传的（文件名, 字节），不读文件系统；格式识别从「路径扩展名」改为「上传文件名扩展名 + `image::guess_format` 兜底」；`fs::read` 一段删除（字节已在手） |
| `crates/koharu-rpc/src/import/pdf.rs` | PDF 逐页渲染为 PNG | `PDF_SCALE` 常量、页命名规则、`hayro` 的 `InterpreterSettings`/`RenderSettings`/`RenderCache` 用法、单页测试夹具全部照搬 | `render(&Path)` 重写为 `render(name: &str, data: &[u8])`：去掉 `fs::read`，`Pdf::new` 直接吃字节；错误消息里的 `path.display()` 换成上传文件名 |
| `crates/koharu-rpc/src/import/zip.rs` | ZIP/CBZ 成员抽取 | 成员过滤规则（光栅扩展名、`._*`、系统垃圾文件、`__MACOSX`）、自然排序、空容器报错照搬 | `File::open(path)` + `ZipArchive::new(file)` 重写为 `ZipArchive::new(std::io::Cursor::new(data))`；测试夹具改为内存构造 |
| `crates/koharu-rpc/src/import/rar.rs` | RAR 成员抽取 | `PageWriter`（`Write` + `Drop` 收集）、过滤规则、自然排序照搬 | `ArchiveReader::read_path(path)` 需要重写为内存来源。`rars` 0.6 是否提供接受 `Read`/字节的构造入口，实现时查 `rars` 0.6.0 文档确认；若只有路径入口，把上传字节写进 `std::env::temp_dir()` 下的唯一临时文件再 `read_path`，用完删除 |

`crates/koharu-rpc/src/project.rs:177-206` 的 `import_images` 签名与实现不改：新模块的 `Page` 输出转换成它已有的五元组即可。

### 5.4 与 fork headless 命令面的对齐

`POST /projects/current/pages` 的路径、方法、响应类型（`Json<ProjectInfo>`）全部不变，变化只在 multipart 字段可接受的内容类型：从「PNG/JPEG/WebP 单图」扩为「单图 + zip/cbz + rar + pdf」。一个上传字段展开成多页时，页名沿用上游规则（压缩包成员名；PDF 为 `{stem}-{页码:0宽度}.png`）。`HEADLESS.md:93` 的一句描述同步改为「multipart page import (images, zip/cbz, rar, pdf)」，`HEADLESS.md:140` 的替换说明不需要动。不需要新路由、不需要查询参数：格式由上传文件名扩展名识别，与上游 `Format` 枚举的序列化值一致（`png`/`jpg`/`jpeg`/`webp`/`cbz`/`zip`/`rar`/`pdf`）。

限制说明：`crates/koharu-rpc/src/api.rs:22` 附近的 `DefaultBodyLimit` 已是 1 GiB，PDF 逐页渲染的峰值内存是一整页 RGBA（A4 300 DPI 约 33 MiB）加 PNG 编码缓冲，单请求处理在 `spawn_blocking` 里跑，不占 async worker。

### 5.5 依赖与预估改动行数

依赖：工作区 `Cargo.toml`（现有外部依赖段在第 44 行起）新增 `hayro = "0.7.1"` 与 `rars = "0.6.0"` 两行；`crates/koharu-rpc/Cargo.toml` 引入这两个依赖。`zip`（工作区 `Cargo.toml:80`）、`image`、`webp`、`rayon`、`alphanumeric-sort`、`strum` 工作区已有，不需要新增。上游 `79cc4d6c` 还动了 `Cargo.lock`（242 行）与 `crates/koharu-app/Cargo.toml`，后者按范围排除。

预估行数：`import/mod.rs` 约 170 行（上游 148 行 + 入口重写增量，含测试约 40 行）；`import/pdf.rs` 约 150 行（上游 133 行 + 字节入口改写，含测试约 65 行）；`import/zip.rs` 约 130 行（上游 140 行略减，含测试）；`import/rar.rs` 约 170 行（上游 152 行 + 临时文件回退分支，含测试）；`routes/projects.rs` 的 `import_pages` 函数（129-173 行）替换其中约 30 行解码代码为约 20 行调用；`lib.rs` 加 1 行；两个 `Cargo.toml` 各加 2 行。合计约 640 至 680 行新增或修改，其中约 360 行是从上游搬来或小幅改写的生产代码，约 180 行是搬来的测试。

---

## 5. 每项行为变更的最小验证与最小测试

统一验证命令（全部移植完成后跑一次）：

```
cargo check -p koharu --no-default-features --features headless
```

分项验证命令（每完成一项跑一次，比统一验证更快定位回归）：

```
cargo check -p koharu-renderer
cargo check -p koharu-pipeline
cargo check -p koharu-ml
cargo test -p koharu-renderer --lib
cargo test -p koharu-pipeline --lib
```

每项行为变更需要补的最小测试（上游已带的测试随补丁一起搬，此处只列必须存在的一条最小覆盖）：

| 提交 | 行为变更 | 最小测试 |
| --- | --- | --- |
| `bcac8a89` | 设备按 adapter 上限申请 `max_texture_dimension_2d` 与 `max_buffer_size` | `crates/koharu-renderer/src/raster.rs` 单元测试 `rasterizes_surfaces_beyond_the_default_wgpu_texture_limit`（4×9000 空场景，断言尺寸与角像素），见 2.3 |
| `acb7d99c` | 重跑时跳过已有文字的检测页；自动 inpaint 跳过已有 cleanup 结果的页 | 上游随补丁的两个测试照搬：`detection_skips_a_page_with_existing_text`（`detection.rs` 测试模块）与 `automatic_inpainting_skips_committed_cleanup`（`inpainting.rs` 测试模块）。手工移植时这两个测试必须一并落地，它们覆盖 `StageProcessor::skip` 的两个分支 |
| `e09feb87` | encoder 框解码改为 topk 后置；图像缩放关抗锯齿 | 上游只有 `#[ignore]` 的 CUDA checkpoint 一致性测试（`mod.rs`），CPU 侧无自动测试。最小覆盖改为：固定一张小图与固定随机权重（或录制的中间张量）跑一次 `KoharuLayoutRFDetrImageProcessor` 前处理，检查并要求等于缩放输出的尺寸与数值范围；`model.rs` 的 topk 后置属于等价变换，用现有检测回归页比对 bbox 变化量（容差沿用上游测试的 6.0 像素） |
| `156c5fcf` | 新增 Hayai OCR 模型选项 | 两层：`crates/koharu-pipeline/src/config.rs` 的 serde 往返测试（`"hayai-ocr"` 序列化再反序列化得到 `OcrModel::HayaiOcr`）；权重可下载环境下跑一次 `HayaiOcr::load` + `inference` 冒烟（上游 `crates/koharu-ml/src/bin/hayai_ocr.rs` 与 `benches/hayai_ocr.rs` 可作脚本） |
| `653eb14b` | PaddleOCR-VL 生成带 512 token 上限与 1.2 重复惩罚 | 新增 `greedy_token` 单元测试：给定两个相同 token 的 logit 向量与 `seen_ids`，检查并要求等于惩罚后 argmax 跳过已出现 token。量化路径的 `GenerationOptions` 字段用构造后字段读回检查 |
| `79cc4d6c` | 导入接受 zip/cbz、rar、pdf | 上游四个文件的测试照搬：自然排序、PDF 300 DPI 尺寸断言、zip 成员过滤、rar 抽取。REST 层补一条 `import_pages` 的 multipart 测试：上传一个两页 PDF 的字节，检查并要求返回的 `ProjectInfo.pages` 长度为 2 |
| `d4c075fe` | 韩文 KeepAll 断行；中文分词默认开启 | `segment.rs` 随补丁的测试照搬（含韩文断行与中文词保护用例）。手工移植时跑 `cargo test -p koharu-renderer --lib segment` |
| `098d697f` | 用户手改的书写方向优先于语言默认；检测生成的竖排只对中日文生效 | 上游随补丁的两个测试照搬并按 fork 的 `Generation`/`ProducerId` 构造改写：`user_writing_mode_overrides_language_default`、`generated_vertical_mode_is_limited_to_chinese_and_japanese` |
| `b5f3c41c` | 拼接气泡各 flow 持久化自己的排版多边形 | 上游随补丁的 `flow_cells_fallback_preserves_the_physical_contour`（`bubble.rs`）与 `joined_balloon_flows_preserve_layout_when_transformed`（`renderer.rs`）必须一并落地。后者按 fork 现有 `Renderer::default()` 与 `TextLayout` 构造微调夹具 |

---

## 6. 与 fork 改动的冲突面

任务书列出的未提交修改在审计开始时位于工作树，审计进行期间已被提交（本轮核实 `git log` 与 `git show --stat`）：

- `5358464e`（feat(pipeline): overlap OCR, translation, and inpainting across pages）：`crates/koharu-pipeline/src/{accelerator.rs,execution.rs,lib.rs,overlap_tests.rs,scheduler.rs,stage_runner.rs,tests.rs}` 与 `crates/koharu-pipeline/src/stages/mod.rs`，以及 `plan/` 下两个计划文件。
- `4ccad2d3`（fix(translator): send explicit thinking setting to compatible endpoints）：`crates/koharu-translator/src/remote/openai_compatible.rs`。
- `91ed2f2e`（chore(renderer): warn when missing source assets skip layers）：`crates/koharu-renderer/src/renderer.rs` 加 10 行 `tracing::warn!`（缺失 source 资产时跳过背景层与图像层各一处，位于第 583 行与第 695 行附近）。

移植清单与这些文件的相交结论：

| 移植项 | 相交文件 | 相交点 | 冲突程度 |
| --- | --- | --- | --- |
| `acb7d99c` | `crates/koharu-pipeline/src/stage_runner.rs` | 上游在 `run_with_recovery` 的 `job.stop.stopped()` 之后、`accelerator.acquire()` 之前插入 `self.stages.skip(...)` 调用；`5358464e` 把 `accelerator.acquire()` 一段改写成 `needs_accelerator` 门控（`stage_runner.rs:67-88` 当前形态），插入点的上下文已被替换 | 必然冲突，手工贴入：`skip` 调用放在 `let accelerated = self.stages.needs_accelerator(job.stage);` 之前，语义上「跳过」先于「占加速器名额」 |
| `acb7d99c` | `crates/koharu-pipeline/src/stages/mod.rs` | 上游给 `trait StageProcessor` 加 `fn skip`、给 `impl Stages` 加 `pub(crate) fn skip`；fork 在同一 trait 加了 `async fn release`（`f16c64e6` 链）、在同一 impl 加了 `pub(crate) async fn release` 与 `pub(crate) fn needs_accelerator`（`5358464e`） | 必然冲突（上下文行相邻），手工贴入，两个方法都保留 |
| `acb7d99c` | `crates/koharu-pipeline/src/stages/detection.rs` | 上游在 `impl StageProcessor for Processor` 的 `unload` 附近插 `fn skip`；fork 同一 impl 有 `async fn release`（文件相对 `b87b93b2` 的全部 4 行差异）。上游删除 `previous_texts` 等函数的大块 hunk 上下文与 fork 一致 | 小冲突（插方法处），大块删除可整块应用 |
| `acb7d99c` | `crates/koharu-pipeline/src/stages/inpainting.rs` | 同上：`fn skip` 与 fork 的 `async fn release` 同 impl 相邻 | 小冲突，手工贴入 |
| `156c5fcf` | `crates/koharu-pipeline/src/stages/ocr.rs` | 上游在 `unload`/`load` 附近加 `OcrModel::HayaiOcr` 分支；fork 同 impl 有 `async fn release`（`ocr.rs:46-52` 当前形态） | 上下文漂移，hunk 手工贴入，新增 match 臂本身无冲突 |
| `156c5fcf` | `crates/koharu-ml/src/lib.rs` | 上游加 `pub mod hayai_ocr;` 一行；fork 该文件相对上游 `156c5fcf~1` 有 `init_cpu`/`init_gpu`/`RuntimeInit` 等 85 行差异 | 单行插入，冲突面极小 |
| `098d697f`、`b5f3c41c` | `crates/koharu-renderer/src/renderer.rs` | `91ed2f2e` 的 10 行 `tracing::warn!` 在第 583、695 行附近；`098d697f` 改 `resolve_writing_mode`（第 1426-1447 行）与其测试（第 1683 行起）；`b5f3c41c` 改 `Traversal`（第 746 行起）、`resolve_balloon_flows`（第 981 行起）与其测试（第 1992 行起） | 区域不重叠，预期无冲突。`b5f3c41c` 的测试 hunk 若与 `5358464e` 新增的 `overlap_tests.rs` 无共享夹具则互不影响 |
| `bcac8a89` | 无 | 只改 `crates/koharu-renderer/src/raster.rs`，三个新提交未触及该文件 | 无冲突 |
| `d4c075fe`、`e09feb87`、`653eb14b`、`79cc4d6c` | 无 | 目标文件（`segment.rs`、`layout.rs`、`koharu_layout_rfdetr_seg_2xl/*`、`paddle_ocr_vl*`、`koharu-rpc`）未被三个新提交触及 | 无冲突 |

`5358464e` 的语义与 `acb7d99c` 的 `skip` 语义正交：前者决定翻译阶段是否占用加速器名额，后者决定整页是否跳过执行。移植 `acb7d99c` 时不要改写 `5358464e` 的 `needs_accelerator` 逻辑，两者在同一函数里按「先 skip 后 acquire」顺序共存。

---

## 7. 建议执行顺序

按依赖与风险从低到高，本清单内部顺序如下（整体计划的 translator → runtime 先行约束仍适用，本表只排 P1/P2 段）：

1. `d4c075fe`（可 cherry-pick，文件逐字节一致，风险最低，先落一个干净提交建立节奏）。
2. `e09feb87`（可 cherry-pick，模型对齐，独立于其他项）。
3. `bcac8a89`（手工移植 8 处 + 回归测试，独立于 pipeline，且必须与 `de9aae2d` 共存验证）。
4. `653eb14b`（手工移植 3 文件，取舍清单已定，不碰 `llm/mod.rs`）。
5. `156c5fcf`（新模型整取 + 调用点手工贴入，工作量最大但结构清晰）。
6. `acb7d99c`（手工移植 4 文件，与 `5358464e` 的冲突面已定位，放在 pipeline 稳定后做）。
7. `098d697f`（整函数替换 `resolve_writing_mode`，带两个测试）。
8. `79cc4d6c`（按第 4 节设计在 `koharu-rpc` 新建 import 模块，可与前面并行，无共享文件）。
9. `6d533281` headless 子集 + `b5f3c41c`（依赖裁决通过后连做，两者共享 `geometry_frame` 签名）。

每步完成后跑该步对应的 `cargo check`/`cargo test` 命令（第 5 节），全部完成后跑统一验证命令。

---

## 8. 需要用户裁决的点

1. `b5f3c41c` 依赖上游 `6d533281` 的 `geometry_frame` 签名变更（`crates/koharu-renderer/src/bubble.rs:632` 从单参数改双参数）。`6d533281` 同时改 `crates/koharu-app`、`crates/koharu-desktop`、`packages/*`（排除路径）与 `crates/koharu-scene/src/components/layers.rs`（未排除）。选项一：本轮取 `6d533281` 的 `bubble.rs` + `renderer.rs` + `koharu-scene` 子集，随后做 `b5f3c41c`；代价是 `koharu-scene` 组件字段变化需要在 fork 的 pipeline 检测输出侧确认无副作用。选项二：本轮跳过 `b5f3c41c`（连带 `6d533281`），拼接气泡文字位置保持现状；代价是已知的排版缺陷留在导出结果里。
2. `653eb14b` 的目录重命名是否跟随：保持 fork 的 `crates/koharu-ml/src/paddle_ocr_vl_quantized.rs` 单文件（本文件第 3 节的建议），还是随上游改成 `paddle_ocr_vl_quantized/mod.rs` 目录。前者少一次路径变动，后者与上游目录结构一致、后续 diff 更干净。
3. `79cc4d6c` 的 REST 形态：本设计选择扩展现有 `POST /projects/current/pages` 的 multipart 字段类型（第 4.4 节）。若希望导入归档与导入单图走不同端点（例如 `POST /projects/current/import`），路由设计需要重写，行数估计不变但 `HEADLESS.md` 的命令面描述要多一条。

以上三点选定后，第 7 节的执行顺序即可开工。
