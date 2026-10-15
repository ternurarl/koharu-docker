# 管线探索记录

记录日期：2026-10-10
范围：只读取当前仓库索引和源码，未修改任何现有源文件。

## 当前执行模型

完整操作依次包含 `Detection`、`Ocr`、`Translation`、`Inpainting`。定义位于 [stage.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stage.rs:22>)。

调度器按页面顺序创建工作项，但不是“一张图片完成四个阶段后再处理下一张”。`Scheduler::start_next` 同时满足三个条件才会启动工作项：

- 同一 `Stage` 不在 `busy_stages` 中，因此每个阶段同时只处理一页。
- 当前阶段的前置阶段已经完成。
- 新页面不能超过活动页面窗口。窗口大小等于本次请求包含的阶段数，完整请求是 4。

依据位于 [scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:37>)、[scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:80>)、[scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:91>) 和 [scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:145>)。

测试 `starts_pages_in_order_and_models_independently` 明确展示：第一页完成检测后，可以同时进入第一页 OCR、第一页修补和第二页检测，见 [scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:162>)。

执行器使用 `FuturesUnordered` 并发等待这些已启动工作项，见 [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs:94>)。

## 模型生命周期

每次阶段工作都会调用 `Stages::load`，但底层 `ModelCell::ensure` 在模型已存在时直接返回，不会重新加载。只有 `ModelCell::unload` 成功取出模型后，下一次 `ensure` 才会重新加载。依据见 [stage_runner.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stage_runner.rs:81>) 和 [model_cell.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/model_cell.rs:16>)。

普通成功路径没有卸载模型。当前调用 `unload` 的位置是：

- 阶段加载或处理失败后的 `StageRunner::stage_error`，见 [stage_runner.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stage_runner.rs:128>)。
- 内存不足恢复时，`AcceleratorGate::recover` 调用 `unload_other_models`，卸载除当前重试阶段以外的模型，见 [accelerator.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/accelerator.rs:39>) 和 [accelerator.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/accelerator.rs:62>)。

因此，“每张图片都加载并卸载检测模型，再加载并卸载修补模型”的猜测与当前成功路径不符。模型默认跨页驻留。

`AcceleratorGate` 只在设备不是 CPU 时创建容量为 1 的信号量。CPU 设备的 `lane` 是 `None`，`acquire` 直接返回空许可，见 [accelerator.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/accelerator.rs:14>)。

## 单次输入边界

检测、OCR 和修补的 `process` 都接收一个 `StageInput`，其中只有一个 `page`，见 [stages/mod.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/mod.rs:18>)。

检测阶段读取该页的 `source` 图像，再调用一次 `network.inference_with_thresholds`，见 [detection.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/detection.rs:143>)。

翻译阶段也接收单页输入，但会收集该页文本组中的多个文本，组成一个 `TranslationRequest` 后调用一次 `Translator::translate`，见 [translation.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/translation.rs:37>)。

## 已确认的内存驻留点

- `Execution` 持有当前 `Snapshot`，每次提交后替换为新的场景快照，见 [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs:23>) 和 [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs:207>)。
- 每个活动页面有一个 `ImageCache`。缓存按图像 blob 保存 `Arc<DynamicImage>`，没有字节上限，见 [images.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/images.rs:9>)。
- 页面的全部阶段完成后，`mark_complete` 才从 `Execution.images` 移除该页缓存，见 [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs:219>)。
- 完整请求的活动页面窗口是 4，因此调度上最多可同时保留 4 个页面缓存条目。
- 检测、OCR、修补模型由各自 `ModelCell` 持有；翻译器的本地模型由 `Translator.local` 持有。远程翻译不会在 `LoadedLocal` 中保存本地模型，但 `load_model` 的远程分支仍需由方案设计代理核对，见 [translator/lib.rs](</D:/Github/koharu-docker/crates/koharu-translator/src/lib.rs:26>)。

## 现有内存采样

`ResourceMonitor` 已有后台采样任务，采样间隔是 100 毫秒，但主机内存和当前进程只在每 5 次循环刷新一次，因此 RSS 更新间隔是 500 毫秒。进程内存字段是 `ResourceSnapshot.process_memory_bytes`，来源是 `sysinfo::Process::memory`，见 [resources.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/resources.rs:48>) 和 [resources.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/resources.rs:101>)。

`Execution::run` 在调度前调用 `resources.start()` 和 `wait_for_sample()`，见 [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs:94>)。当前没有在加载前、加载后、处理前、处理后输出 RSS 日志。

`StageRunner::new` 收到 `Arc<ResourceMonitor>`，但只把它传给 `AcceleratorGate`，`StageRunner` 自身没有保存该句柄，见 [stage_runner.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stage_runner.rs:23>)。

