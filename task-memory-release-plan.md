# 任务结束后释放内存计划

记录日期：2026-10-12
依据：[MEMORY_USAGE_ANALYSIS.md](</D:/Github/koharu-docker/plan/MEMORY_USAGE_ANALYSIS.md>) 与 `koharu-memory-audit-20261012-112946`。

## 目标

每个传入的流水线任务结束后，释放默认检测、Baberu OCR、LaMa 修补三个阶段持有的模型，并清空该任务的图像缓存。

不修改 Torch、分配器、模型结构、权重加载、预处理、推理或后处理。

## 当前证据

2026-10-12 11:29 的只读审计显示，PID `3785200` 的 `PSS` 为 `58.7 MiB`，`SwapPss` 为 `5,196.9 MiB`。容器 cgroup 的历史物理内存峰值是 `3.19 GiB`，当前交换占用是 `5.48 GiB`。

交换量最大的前 100 个映射中：

- 93 个匿名映射合计 `5,097.3 MiB`。
- 4 个已删除的 `memfd:allocation fd` 合计 `419.6 MiB`。
- `[heap]` 为 `71.4 MiB`。
- 普通文件映射为 `9.2 MiB`。
- 默认检测、Baberu、LaMa 的权重文件没有被进程打开或映射。

匿名映射中，`63–64 MiB` 档合计 `1,867.0 MiB`，`62–63 MiB` 档合计 `811.0 MiB`。

## 当前代码边界

一次传入任务是 RPC 路由中的一次 `pipeline.execute` 调用。调用位置见 [pipelines.rs](</D:/Github/koharu-docker/crates/koharu-rpc/src/routes/pipelines.rs:150>)。成功、失败和取消都在这次调用返回后进入任务收尾。

`Pipeline::execute` 在整次执行期间持有执行锁，见 [pipeline.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/pipeline.rs:76>)。阶段批次结束时，现有执行器会调用 `release_stage`，见 [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs:238>)。`ModelCell::release` 会等待模型锁并取出模型，见 [model_cell.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/model_cell.rs:39>)。

任务级释放是最终清空，不替换阶段批次结束时的现有释放。

## 执行要求

在 `pipeline.execute` 返回后、写入最终任务状态前执行释放。成功、失败和取消使用同一条路径。

释放顺序固定为：

1. 检测模型。
2. Baberu OCR 模型。
3. LaMa 修补模型。
4. 本次任务的流水线 `ImageCache`。

只使用 `StageRunner`、阶段处理器和 `ModelCell` 的现有释放入口。不增加新的模型容器，不改 `ModelCell` 的锁行为，不调用 Torch 缓存清理接口。

配置替换产生新的 `StageRunner` 时，本次任务仍释放它开始时取得的 runner。下一个任务使用新的 runner，并在自己的任务结束后执行同样的释放。

## 改动范围

修改任务收尾和释放入口：

- [pipelines.rs](</D:/Github/koharu-docker/crates/koharu-rpc/src/routes/pipelines.rs>)：在 `execute` 返回后的统一收尾路径调用任务级释放。
- [pipeline.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/pipeline.rs>)：为当前执行使用的 runner 提供检测、OCR、修补释放入口。
- [stage_runner.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stage_runner.rs>)：按固定顺序转发三个阶段的现有释放操作。
- [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs>)：在任务收尾清空本次任务的图像缓存。

不修改以下实现：

- [detection.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/detection.rs>) 的检测推理和结果写入。
- [ocr.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/ocr.rs>) 的 Baberu 推理和文本写入。
- [inpainting.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/inpainting.rs>) 的 LaMa 推理和图像写入。
- Torch、模型定义、权重加载、预处理、后处理和设备选择。

## 验证

增加聚焦测试：

- 一次包含检测、Baberu、LaMa 的任务完成后，三个模型持有位置都为空，图像缓存项数为 0。
- 任务失败和取消时也完成同样的释放。
- 连续执行两个任务时，第二个任务可以重新加载所需模型并完成。
- 相同输入在释放改动前后产生相同的检测结果、Baberu 文本和 LaMa 输出。

实现时按仓库规则运行一次 debug 级聚焦测试。不运行完整测试套件。

任务完成后重新运行 [audit_koharu_memory.py](</D:/Github/koharu-docker/scripts/audit_koharu_memory.py>)。记录 `SwapPss`、匿名映射、heap、`memfd` 和文件映射。目标是相对本次审计的 `5,196.9 MiB` `SwapPss` 下降。

## 约束

本次只处理默认检测、Baberu、LaMa。不调整其他模型。

“每个传来的任务”指一次 RPC `pipeline.execute` 调用，不是每个页面或每个阶段。

如果任务级释放后匿名交换没有下降，停止扩大代码改动，保留新的审计结果，重新判断内存归属。
