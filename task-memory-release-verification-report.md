# Meitner 子代理的任务内存释放验证报告

本次交付的是 `/home/qinzhu/koharu/koharu-docker` 的只读验证方案，用于验证一次任务结束后清空三个模型持有位置和任务图像缓存。使用者在仓库根执行下述单次 `cargo test` 命令，并在目标进程运行期间执行内存审计命令。本轮未生成代码，因此生成代码的运行损耗不变；审计读取 `/proc` 并写出报告文件；本轮未执行编译，编译期损耗不变。

## 报告词汇定义

| 对话用名 | 代码名 | 出处 | 定案情况 |
|---|---|---|---|
| 任务 | 一次 `Pipeline::execute` 调用 | `task-memory-release-plan.md:82` | 已定案 |
| 模型持有位置 | `ModelCell<M>` 中的 `Option<M>` | `crates/koharu-pipeline/src/model_cell.rs:5` | 已定案 |
| 任务图像缓存 | `Execution.images` 与 `ImageCache.decoded` | `crates/koharu-pipeline/src/execution.rs:32`、`crates/koharu-pipeline/src/images.rs:10` | 已定案 |
| `SwapPss` | 按使用者分摊后属于进程的交换空间 | `MEMORY_USAGE_ANALYSIS.md:19` | 已定案 |
| 测试夹具 | 测试代码建立的内存场景、图像和模型测试值 | `crates/koharu-pipeline/src/tests.rs:153` | 已定案 |

## 仓库已有无需真实权重的构造入口

- 规格要求释放三个持有位置，见 `task-memory-release-plan.md:38`。实现中的三个持有位置分别是 `crates/koharu-pipeline/src/stages/detection.rs:61`、`crates/koharu-pipeline/src/stages/ocr.rs:19`、`crates/koharu-pipeline/src/stages/inpainting.rs:62`。
- `crates/koharu-pipeline/src/model_cell.rs:10` 的 `ModelCell::new` 与 `crates/koharu-pipeline/src/model_cell.rs:16` 的 `ensure` 可装入 `u32` 等测试值；`crates/koharu-pipeline/src/model_cell.rs:56` 和 `crates/koharu-pipeline/src/model_cell.rs:67` 已直接检查持有、释放、锁等待和重新装入。
- `crates/koharu-pipeline/src/stages/mod.rs:78` 的 `Stages::new` 可构造三个真实处理器且不调用 `load`，但处理器字段及三个 `ModelCell` 状态均无测试读取入口。
- `crates/koharu-pipeline/src/tests.rs:153` 的 `pipeline` 使用内存配置和 CPU；`crates/koharu-pipeline/src/tests.rs:22` 的 `Session::memory` 提供无文件场景。
- `crates/koharu-pipeline/src/stages/mod.rs:135` 的 `asset` 建立 1×1 图像，`crates/koharu-pipeline/src/stages/mod.rs:28` 的 `StageInput::new` 可注入 `ImageCache`；`crates/koharu-pipeline/src/images.rs:15` 可填充图像缓存，但当前没有项数读取方法。

## 四个场景的最小测试

下表中的 `task_memory_release_*` 名称是本轮计划新增名称；现有测试名称来自 `crates/koharu-pipeline/src/tests.rs:9` 和 `crates/koharu-pipeline/src/tests.rs:88`。

- 成功：在组合测试的 `Ok(())` 分支装入三个测试模型值和一个图像项，调用统一释放入口，检查三个值为空且图像项数等于 0。
- 失败：扩展 `detection_batch_failure_releases_before_inpainting_loads`，在返回 `PipelineError` 前检查三个释放调用顺序和图像项数等于 0。
- 取消：扩展 `stop_is_a_successful_partial_result`，在 `RunStatus::Stopped` 路径检查相同状态。
- 连续两个任务：在同一组合测试中执行两次准备、释放、重新装入；第二次要求三个测试模型值重新出现，并再次清空图像项数。

## Debug 级聚焦测试命令

```bash
cargo test -p koharu-pipeline --lib tests::task_memory_release_lifecycle -- --exact --nocapture
```

该命令使用 Cargo 默认 debug 配置，只运行一个组合测试一次。

## 实现完成后的内存审计

```bash
pid="$(pgrep -n -x koharu)" && \
sudo python3 scripts/audit_koharu_memory.py \
  --pid "$pid" \
  --output "/home/qinzhu/koharu-memory-audit-after-release-$(date +%Y%m%d-%H%M%S)"
```

- 输出文件是 `report.json`、`status.txt`、`smaps_rollup.txt`、`maps.txt`、`summary.txt`，写入位置由 `--output` 指定，见 `scripts/audit_koharu_memory.py:235`。
- `report.json` 包含 `status_kib`、`rollup_kib`、`map_groups_bytes`、`cgroup_memory`、`swap_by_mapping`、`open_model_files` 等字段，见 `scripts/audit_koharu_memory.py:220`。
- 主比较指标是 `summary.txt` 的 `SwapPss` 或 `report.json.rollup_kib.SwapPss`，要求低于 `5,196.9 MiB`；基线来自 `task-memory-release-plan.md:14`。
- 同时记录 `map_groups_bytes.anonymous`、`[heap]`、`memfd:*` 和普通文件映射。当前 `category` 将 `[heap]` 合入 `anonymous`，因此这些值需从 `maps.txt` 或 `swap_by_mapping` 分组计算，见 `scripts/audit_koharu_memory.py:124`。
- 执行前必须存在可读取的目标 `koharu` 进程和运行中的 `koharu-headless` 容器；进程不存在时脚本直接退出，见 `scripts/audit_koharu_memory.py:43`。`smaps`、文件描述符和 cgroup 信息必须可读，非 root 会提示信息不完整，见 `scripts/audit_koharu_memory.py:201`。

## 尚无仓库测试覆盖的计划项目

- `task-memory-release-plan.md:69` 的完整成功任务无法在不加载权重的情况下完成真实检测、Baberu OCR 和 LaMa 推理；最小替代是组合测试验证释放状态，并在目标运行时保存一份结构化输出作前后比较。
- `task-memory-release-plan.md:70` 的失败与取消已有结果测试，但没有检查三个持有位置和图像项数；最小替代是扩展现有两个测试并检查统一释放记录。
- `task-memory-release-plan.md:71` 的连续两个任务没有现有测试；最小替代是同一组合测试执行两次测试值装入与释放，真实第二次推理需在目标运行时验证。
- `task-memory-release-plan.md:72` 的结果一致性缺少权重和基准输出；最小替代是固定一份输入，在改动前后分别运行并比较检测结果、Baberu 文本和 LaMa 图像输出。
- `task-memory-release-plan.md:76` 的交换空间下降只能通过运行中的目标进程审计验证，Cargo 测试不能覆盖主机交换占用。

本报告仅告知验证方案和覆盖缺口，不需要用户决定。
