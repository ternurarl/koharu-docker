# 单模型翻译流水线计划

记录日期：2026-10-10
依据：[pipeline-exploration.md](</D:/Github/koharu-docker/pipeline-exploration.md>) 与当前索引源码。

## 目标

一次流水线运行期间，同一时刻只保留一个已加载模型。检测、OCR、本地翻译、修补各自完成其全部页面后，再加载下一个模型。

检测、OCR、翻译、修补四个阶段的模型、推理、后处理和场景写入保持现有实现。改动限制在调度顺序、活动页面数量和阶段切换时的模型释放。

远程翻译不在 `LoadedLocal` 中保存本地模型。选择远程翻译时，翻译阶段不加载本地模型，但仍作为独立阶段执行，使前后阶段的模型释放边界保持一致。

## 当前约束

当前调度器按页面窗口交错启动阶段。完整请求的窗口大小是 4，并且每个阶段同时只处理一页。第一页检测完成后，调度器可以同时启动第一页 OCR、第一页修补和第二页检测。依据见 [scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:80>)、[scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:145>) 和 [scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs:162>)。

阶段产物依赖固定为：

- OCR 读取检测写入的文本区域。
- 翻译读取 OCR 写入的源文本。
- 修补读取检测写入的区域，不读取 OCR 或翻译结果。

成功路径目前不卸载模型。`Stages::load` 在模型已存在时直接返回；`ModelCell::unload` 和 `Translator::unload` 都使用 `try_lock`，锁被占用时返回 `false`，不会等待并取出模型。依据见 [model_cell.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/model_cell.rs:16>)、[model_cell.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/model_cell.rs:32>) 和 [lib.rs](</D:/Github/koharu-docker/crates/koharu-translator/src/lib.rs:85>)。

因此，不能只在现有交错调度中追加卸载调用。必须先让一个阶段的全部工作结束，并确认该阶段没有仍在运行的任务，再释放模型。

## 执行顺序

调度单位从“受页面窗口限制的下一页阶段”改为“当前阶段的下一页”。请求包含的阶段仍由 `Operation::stages` 决定，不改变 `Full`、`Through`、`Only`、`Stages` 的阶段集合。

阶段按 `Stage::ALL` 中的固定顺序执行：

1. 对全部范围内页面执行检测，然后卸载检测模型。
2. 对全部范围内页面执行 OCR，然后卸载 OCR 模型。
3. 对全部范围内页面执行翻译，然后卸载本地翻译模型。
4. 对全部范围内页面执行修补，然后卸载修补模型。

请求未包含的阶段直接跳过，不加载其模型。例如 `Through { stage: Inpainting }` 只包含检测和修补，执行顺序是全部检测、卸载检测模型、全部修补、卸载修补模型。

同一阶段内仍按现有页面顺序逐页执行，不增加页面级并发。活动页面窗口改为 1，使 `Execution.images` 同时只保留当前页的 `ImageCache`。页面的该阶段完成后立即移除缓存，不等待该页的后续阶段。

停止、失败、提交、补丁变基和进度事件继续使用 `Execution` 的现有路径。阶段失败时保留现有错误卸载；阶段正常结束、用户停止或运行进入失败收尾时，再释放本次请求已经加载且尚未释放的模型。

## 模型释放

新增一个会等待模型锁的释放操作，供阶段边界使用。它在取出模型后返回，使检测网络、OCR 网络、本地 `Llm` 或修补网络离开 `ModelCell`、`Translator.local` 及其外层 `Arc`。现有 `try_lock` 卸载继续只服务于当前的失败路径和加速器内存恢复，不把它当作阶段切换的释放保证。

阶段边界的释放按以下条件执行：

- 当前阶段在本次请求中的全部页面都已完成、跳过或停止。
- `busy_stages` 不再包含该阶段。
- 该阶段对应的异步推理任务已经结束，因此模型锁可以被获取。

释放后才允许调度下一个阶段。这样下一个阶段调用 `load` 时，前一个模型已经从对应持有位置取出。

CPU 上的 `AcceleratorGate.lane` 仍保持 `None`。本次改动不依赖加速器许可来限制模型数量，也不改变非 CPU 设备的现有许可逻辑。

## 改动范围

修改以下调度和生命周期代码：

- [scheduler.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/scheduler.rs>)：按阶段批次选择下一页，并把活动页面窗口固定为 1。
- [execution.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/execution.rs>)：在阶段批次结束后调用等待式释放，再调度下一批次。
- [stage_runner.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stage_runner.rs>)：提供批次结束和运行收尾所需的阶段释放入口。
- [model_cell.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/model_cell.rs>)：增加等待锁后取出模型的操作。
- [stages/mod.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/mod.rs>)：把等待式释放转发到四个阶段。
- [lib.rs](</D:/Github/koharu-docker/crates/koharu-translator/src/lib.rs>)：为本地翻译模型增加等待式释放；远程翻译保持空释放。

不修改以下实现：

- [detection.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/detection.rs>) 的检测推理和区域写入。
- [ocr.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/ocr.rs>) 的 OCR 推理和文本写入。
- [translation.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/translation.rs>) 的请求构造和翻译结果写入。
- [inpainting.rs](</D:/Github/koharu-docker/crates/koharu-pipeline/src/stages/inpainting.rs>) 的修补推理和图像写入。
- 模型结构、权重加载、预处理、后处理和设备选择。

## 验证

更新调度器测试，使它们描述批次顺序：

- 两个页面的完整请求依次产生：第一页检测、第二页检测、第一页 OCR、第二页 OCR、第一页翻译、第二页翻译、第一页修补、第二页修补。
- 检测完成并释放前，不启动 OCR、翻译或修补。
- OCR 完成并释放前，不启动翻译。
- 只包含检测和修补的请求不加载 OCR 或翻译，并在两组修补任务开始前释放检测模型。
- 活动页面数量不超过 1，当前页缓存不保留到后续阶段。

增加模型单元测试，覆盖“锁占用时等待，任务结束后取出模型，并在下一次加载时重新创建模型”。调度测试使用替身阶段记录加载和释放顺序，不下载或加载真实权重。

按仓库验证规则，在实现时运行一次对应的 debug 级聚焦测试。不运行完整测试套件，也不把真实模型的端到端内存测量纳入本次代码改动。
