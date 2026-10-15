# 交换占用修复计划

本计划补充 `task-memory-release-plan.md` 的任务级资源释放要求，目标是让一次任务结束后，模型执行产生的匿名映射、交换页、`[heap]` 和 `memfd` 资源随任务边界回收。计划日期为 2026-10-14。下一阶段只编辑本计划列出的 Koharu 进程边界、任务调度、审计和验证文件，不直接修改 Torch、模型结构、权重加载、推理或后处理代码。

## 当前事实

- `task-memory-release-plan.md:38` 已定案释放顺序为 `Detection`、`Ocr`、`Inpainting`，最后清空本次任务的 `ImageCache`。
- `crates/koharu-pipeline/src/pipeline.rs:114` 的 `TaskExecution::release_resources` 调用模型释放入口，`crates/koharu-pipeline/src/pipeline.rs:117` 清空图像缓存。
- `crates/koharu-pipeline/src/stage_runner.rs:54` 的 `release_task_models` 按固定顺序释放三个模型持有位置。
- `crates/koharu-rpc/src/routes/pipelines.rs:159` 在写入最终任务状态前调用释放入口。
- `scripts/audit_koharu_memory.py:220` 记录 `SwapPss`、映射分组、交换映射和打开的模型文件。
- `crates/koharu-pipeline/src/tests.rs:131` 已检查模型释放顺序和 `ImageCache` 项数为 0。
- 运行中目标进程的审计显示，任务结束后的 `memory.swap.current` 仍为 `2,781.2 MiB`，且与 `memory.swap.peak` 相同；交换映射前 100 项仍包含 `[anonymous]`、`[heap]` 和 `/memfd:allocation fd (deleted)`。因此仅释放 Rust 任务对象不足以保证宿主内存回收。

## 仓库现状与先例

- `crates/koharu-pipeline/src/bin/run.rs:130` 是完整流水线的独立进程入口；该进程完成一次任务并退出，可作为任务进程生命周期的先例。
- `crates/koharu-pipeline/src/images.rs:47` 和 `crates/koharu-app/src/commands/output.rs:111` 已有后台工作入口，并处理工作入口意外结束的错误。
- 当前 API 调用位置是 `crates/koharu-rpc/src/routes/pipelines.rs:157` 和 `crates/koharu-app/src/commands/processing.rs:215`。
- 当前测试覆盖位于 `crates/koharu-pipeline/src/tests.rs:131`，但缺少“工作进程退出后宿主交换占用不增加”的运行时验证。

## 直接修复规格

一次 `pipeline.execute` 调用必须拥有一个任务级模型执行子进程。父进程只负责 API 会话、输入输出和最终状态；子进程负责 `Pipeline`、`StageRunner`、三个模型持有位置、`ImageCache` 以及模型运行时状态。子进程在提交任务结果、执行现有 `TaskExecution::release_resources` 后退出。子进程退出是最终资源边界，操作系统负责销毁该进程的匿名映射、`[heap]`、`memfd` 和交换页。

子进程边界内的模型实现保持原样。任务输入、场景快照、阶段输出和报告通过序列化数据传输；Torch `Tensor`、模型句柄和运行时指针不得跨进程传输。现有 `Committer` 行为必须保留，阶段输出仍按原顺序提交到父进程会话。

## 保留现状选项

保留当前同一进程内模型加载和释放，继续使用 `TaskExecution::release_resources`，只在任务收尾增加进程分配器的释放调用，并用前后审计验证匿名映射和交换页变化。这个选项不修改模型代码，改动较少；它的后果是无法保证 LibTorch、stable-diffusion.cpp 或 Vulkan 运行时持有的匿名映射会归还宿主，因此只有在前后审计满足验收判据时才可采用。任务级子进程方案仍是直接修复方案。

## 文件改动清单

- `crates/koharu-pipeline/src/pipeline.rs`：把当前进程内执行逻辑改为子进程边界内的本地执行入口，并让公开任务入口负责启动、等待和读取任务子进程；保留现有 `TaskExecution::release_resources` 调用。
- `crates/koharu-pipeline/src/stage_runner.rs`：保留 `release_task_models` 的 `Detection`、`Ocr`、`Inpainting` 顺序，不增加模型容器，不改变 `ModelCell` 锁行为。
- `crates/koharu-pipeline/src/execution.rs`：保留 `ImageCache` 清理和任务级释放结果，记录释放后的项数。
- `crates/koharu-rpc/src/routes/pipelines.rs`：把 RPC 任务收尾连接到任务级子进程入口，最终状态仍在释放完成后写入。
- `crates/koharu-app/src/commands/processing.rs`：桌面命令使用同一个任务级子进程入口，避免桌面和 RPC 形成两套生命周期。
- `crates/koharu-pipeline/src/bin/run.rs`：保留完整流水线 CLI 的独立进程先例，并改为使用同一任务边界；不把 CLI 文件当作长期运行的服务。
- `crates/koharu-pipeline/src/tests.rs`：增加成功、失败、取消和连续任务的释放顺序检查，并检查 `ImageCache` 项数为 0。
- `scripts/audit_koharu_memory.py`：保留现有只读审计格式，补充任务前后 cgroup `memory.current`、`memory.swap.current` 和目标 PID 生命周期的对比字段。
- `Dockerfile`：仅在新增独立工作进程二进制时复制该二进制；不改变 Torch、LibTorch 或模型包内容。

## 禁止修改范围

以下文件和目录不作为本次修复的编辑目标：

- `crates/koharu-torch/**`
- `crates/koharu-ml/**`
- `crates/koharu-diffusion/**`
- `crates/koharu-llama/**`
- 任何 `*_generated.rs`、生成源码或生成器输入
- `/home/koharu/.local/share/Koharu/packages/**` 中的 Torch、模型权重、LibTorch、stable-diffusion.cpp 和 Vulkan 运行时文件

## 验证步骤

1. 在任务开始前运行 `scripts/audit_koharu_memory.py`，记录父进程 PID、`memory.current`、`memory.swap.current`、`SwapPss`、`[anonymous]`、`[heap]`、`memfd` 和普通文件映射。
2. 使用下一次对话提供的真实图片输入执行一次完整任务，记录输入数量、`operation_id`、最终状态、输出数量和输出文件名。
3. 在父进程 `koharu` 仍运行、任务子进程已经退出时再次运行同一审计脚本。
4. 检查并要求模型释放顺序为 `Detection`、`Ocr`、`Inpainting`，`ImageCache` 项数为 0，`open_model_files` 为空，任务子进程 PID 已不存在。
5. 检查并要求任务后 `memory.swap.current` 不高于任务前数值，任务产生的 `[anonymous]`、`[heap]` 和 `memfd` 映射不保留在父进程中；普通文件映射只保留启动时已经存在的运行库和模型包映射。
6. 只运行聚焦的生命周期测试和一次真实任务，不运行完整测试套件。

## 验收结论

修复完成的判据是：每次任务结束后，模型和图像资源由任务子进程边界销毁，父进程的交换占用不因该任务累积；如果任务后仍有新增匿名映射或交换页，则修复未完成。
