# 计划：OCR 与修补并发，翻译流水线化

日期：2026-10-14。状态：并发调度已实现，见文末「实现记录」；性能与内存实测待做。

## 目标

一次多图翻译任务里，OCR、翻译、修补不再是三段线性执行。目标是让 **OCR 与修补并发运行**，同时翻译作为远端调用与本地计算重叠，从而提高多图任务的吞吐。

按原始设想：某一页的 OCR 一出结果就立刻发翻译，不必等其他页 OCR 完成；一页的修补也不必等其他页修补完成。三个阶段在页面维度上重叠。

## 为什么值得做

单页实测各阶段耗时（一次任务内）：

| 阶段 | 耗时 | 占比 | 资源 |
|---|---|---|---|
| 检测 | 8.58s | 29% | 本地模型 |
| OCR | 6.27s | 21% | 本地模型 |
| 翻译 | 9.49s | 32% | **远端网络，不占本地模型** |
| 修补 | 5.32s | 18% | 本地模型 |

翻译是单页最长的阶段，却是纯网络等待，现在和本地计算串行累加。4 页任务的粗略估算：

| 方案 | 估算总耗时 |
|---|---|
| 现状（阶段屏障，全串行） | ≈ 119s |
| 翻译与本地计算重叠 | ≈ 81s |
| 再放开 OCR 与修补并发 | ≈ 74s |

数字是估算，不是承诺，实现后要实测。

## 现状

流水线的执行由 `crates/koharu-pipeline/src/scheduler.rs` 和 `execution.rs` 决定：

- **阶段屏障**：`Scheduler` 的 `active_stage` 只派发当前阶段的作业，一个阶段的所有页面都 `Finished` 才前进到下一阶段。
- **阶段内串行**：`busy_stages` 限制同一阶段同时只有一个作业在跑，多页时逐页排队。
- **单模型驻留**：`Execution::release_active_batch` 在一个阶段的批抽干时释放该阶段模型。这是阶段屏障存在的直接原因 —— 屏障保证了任意时刻只有一个模型在内存里。
- `Execution::run` 用 `FuturesUnordered` 收集作业，但受上面两条限制，实际并发度是 1。
- `AcceleratorGate`（`accelerator.rs`）在加速设备上是 `Semaphore::new(1)`，注释写明异构 CUDA 模型并行反而慢 2.5–4 倍。

## 关键事实

**真实的依赖图不是 OCR → 翻译 → 修补 的线性链。**

`crates/koharu-pipeline/src/stages/inpainting.rs` 不读译文：它只用检测出的文字区域做遮罩把原文字抹掉，译文是渲染阶段画上去的。`scheduler.rs` 的 `prerequisite()` 也反映了这一点：

```
Detection(p) ──┬──> Ocr(p) ──> Translation(p)     远端，无本地模型
               └──> Inpainting(p)                  只依赖 Detection
```

Translation 与 Inpainting 是兄弟节点。修补不需要等翻译返回，这比原始设想的链式依赖更宽松，为并发留下了更大空间。

**翻译的请求粒度本来就是单页。** `Translation::process` 收单页 `StageInput`，把该页所有文字层的 source 打成一个 `TranslationRequest` 发出，收回来是逐段对应的数组。`context` 为空、`source_language` 为 `None`、图像只带当前页 —— 请求自包含，没有跨页依赖。所以「每页 OCR 完就发」不需要改请求的组包方式，只涉及何时发。

## 已测量的内存

子进程（`pipeline-task`）峰值 RSS，各阶段独立运行：

| 阶段 | 峰值 | 相对基线 | 权重文件 | 放大倍数 |
|---|---|---|---|---|
| 翻译（无本地模型） | 109 MiB | 基线 | — | — |
| 检测 | 887 MiB | +778 MiB | 154 MB | ~5× |
| OCR | 1,191 MiB | +1,082 MiB | 431 MB | ~2.5× |
| 修补 | 558 MiB | +449 MiB | 195 MB | ~2.3× |

宿主与容器：

| 项目 | 数值 |
|---|---|
| 物理内存 | 7,812 MiB |
| 当前可用 | 3,931 MiB |
| 交换区 | 16 GB，已用 4,390 MiB |
| 容器内存限额 | 无 |
| 当前完整任务子进程峰值 | 1,468 MiB |

由此的判断：**OCR 与修补并发是安全的**，估算峰值约 1.6–1.9 GB，相对现状增量约 30%，可用内存 3.8 GB 有余量。**检测不应参与并发** —— 它是 5 倍放大，性价比最差，而且是流水线第一段，没有并发收益。

**叠加效应需要注意**：模型并发与多页并发是乘法关系。两个模型同驻再叠加多页同时跑，2 页约 3.5 GB（临界），3 页约 5 GB（会触发交换）。多页并发不在本计划范围内。

## 约束

实现时不能破坏以下几点：

1. **模型释放顺序**必须保持 `Detection` → `Ocr` → `Inpainting`（`TaskExecution::release_resources` → `release_task_models`），`ImageCache` 项数归零。这是既有验收判据。
2. **父进程内存不能累积。** 任务子进程的资源边界是刚落地的修复（见 `plan/task-subprocess-handoff.md`）。验收判据：任务前后父进程 `VmSwap` 不涨、`[heap]` 不变、`open_model_files` 为空、子进程 PID 已退出。并发改动不得使父进程重新持有模型或图像缓存。
3. **提交顺序与 revision。** 父进程按到达顺序提交 patch，revision 每次加一，子进程按预测的 base 给 transfer 盖章（`WorkerCommitter::commit`）。乱序产生的 patch 必须仍然能通过 `Patch::from_transfer` 的 observations 校验和 `validate_commit`。
4. **禁止改动**：`crates/koharu-torch/**`、`crates/koharu-diffusion/**`、`crates/koharu-llama/**`、任何生成源码、模型包。`crates/koharu-ml/**` 只允许增加只读查询（已有 `RuntimeInit` / `runtime_init()` 先例）。
5. **加速设备的串行语义**：`AcceleratorGate` 在 GPU 上是有意串行的，并发策略不得让两个模型同时抢加速器而复现注释里提到的 2.5–4 倍退化。

## 风险

- **峰值内存上升**：两个模型同驻是本方案的固有代价。任务进行中的交换风险回来了（虽然子进程退出后整体回收）。
- **乱序提交冲突**：不同页的 patch observations 互斥，但同一页内跨阶段乱序可能冲突，需要验证而非假定。
- **远端限流**：多个翻译请求同时在飞是否触发服务商限流，取决于配置的 provider。
- **失败语义**：并发后单阶段失败的传播、取消（`StopToken`）的响应时机、以及 `release_resources` 的时序都需要重新核对。
- **回归风险**：`task_memory_release_lifecycle` 覆盖释放顺序，但走的是进程内路径；子进程路径的等价覆盖目前不足。

## 验收

- **正确性**：多图任务的最终场景与线性执行一致（同样的检测区域、同样的文字、同样的修补结果）。单页任务结果不变。
- **性能**：多图任务总耗时有可测量的下降。2 页以上才能看出差异，1 页任务本就无重叠空间。
- **内存**：父进程判据全部保持（`VmSwap` 0、`[heap]` 不变、无残留子进程、无打开的模型文件）；子进程峰值落在估算区间内，不触发交换。
- **释放顺序**：`Detection` → `Ocr` → `Inpainting`，`ImageCache` 为 0，`task_memory_release_lifecycle` 通过。
- **取消**：任务中途取消能在合理时间内结束并释放资源。

## 范围外

- 检测阶段的并发（放大倍数最高，收益为零）。
- 多页并发与并发度配置（与模型并发叠加后内存乘法放大，需单独评估）。
- 翻译请求的合并或拆分策略变更（当前单页粒度不变）。
- 服务端 `Operation` / `Scope` 的接口语义变更。

## 测量方法

以上数字的获取方式，便于实现后复测对比：

- 模型权重：容器内 `find .../hugging-face -name '*.safetensors'` 后 `du`。
- 各阶段峰值内存：用 `Operation::Only { stage }` 单独跑一个阶段，轮询 `ps -eo comm,rss` 取 `pipeline-task` 的最大值。
- 任务前后内存对比：`scripts/audit_koharu_memory.py` 加 `--compare`（读 `/proc` 需 `python:3-slim --pid=host --cap-add=SYS_PTRACE`）。
- 阶段耗时：容器日志的 TimingLayer 分段输出。

## 踩坑备忘

以下是本项目已经定位过的问题。重新发现它们会浪费大量时间与 token，实现前先读这一节。

### 一、验证与部署（最容易白跑一整轮）

**1. 同步代码到远端构建时，必须覆盖所有改动的 crate。**
本项目的子进程修复曾经只传了 `koharu-ml`/`koharu-pipeline`/`koharu-scene`，漏掉 `crates/koharu-rpc`，结果镜像里调用点仍是旧的 `execute`，整个任务跑在父进程内，`pipeline-task` 从未出现。更糟的是当时的内存数据看起来「很好」——因为测的根本是旧路径，据此得出的结论全部无效，白跑了一整轮验收。

改完代码必须验证**构建产物**里确实有你的改动，而不是只看源码。判别方法：用特征字符串搜二进制，或跑一次真实任务看目标进程是否出现。

**2. `grep -c` 对二进制统计的是「匹配的行数」，不是「出现次数」。**
一个没有换行的二进制整块算一行，`grep -c` 只会返回 0 或 1；而字符串常量可能被编译器拆分。它只能回答「在不在」，不能回答「有多少」。要精确计数用 `grep -ao` 配合 `-o`，或直接功能验证。

**3. 容器必须挂在 `astrbot_network`。**
AstrBot 插件通过 Docker 容器名 `koharu-headless` 解析 Koharu，这个名字只有在共享网络里才解析得到。用默认 `bridge` 起容器会导致 `getaddrinfo` 报 `Name or service not known`，插件侧表现为 `Koharu is not ready within 60.0s`。完整启动参数见 `plan/task-subprocess-handoff.md`。

**4. `cargo build` 的 `--bin` 是按包解析的。**
`pipeline-task` 属于 `koharu-pipeline` 包，不是 `koharu`。写成 `cargo build -p koharu ... --bin koharu --bin pipeline-task` 会报 `no bin target named 'pipeline-task' in 'koharu' package`，两个包要分开编。

**5. 构建目录在远端是 `~/koharu/build-subprocess`**（从 `~/koharu/koharu-docker` 本地 clone 出来的，不污染原检出）。Debian 上工具链在 `~/.cargo/bin`，要自己 `export PATH`。Windows 侧 `koharu-app` 编不了（CEF 需要的 SDK 版本不对），无头部署不依赖它。

### 二、运行时死锁（已修，但别改回去）

**6. 子进程的取消读取不能跑在 tokio 阻塞池里。**
曾经的死锁：父进程把 `ChildStdin` 放进 `tokio::spawn(cancel)`，那个任务只在 `stop.cancelled()` 时结束，于是 stdin 永不关闭；子进程用 `tokio::io::stdin().read_exact()` 读取消字节，跑在 `spawn_blocking` 里，只在收到字节或 EOF 时返回；`cancel.abort()` **打不断 `spawn_blocking` 里的阻塞 read**；子进程 `main` 返回后 `BlockingPool::shutdown()` 等这个 read，进程永远不退出，父进程 `child.wait()` 也永远等不到。

症状极具误导性：任务显示 4/4 完成、日志无任何错误、进程都活着、线程全在 futex。**看起来像「慢」，其实是死锁。** 当时被误判成模型重下载，浪费了一轮。

修法是双保险：父进程拿到结果后 `drop(stdin)` 让子进程 read 收到 EOF；子进程的取消读取改用 `std::thread::spawn` + `std::io::stdin()`，不占 tokio 阻塞池，`main` 返回时随进程一起结束。

**7. `pipeline-task` 没有初始化 tracing，子进程的 span 全部不可见。**
排查子进程问题时不能依赖日志 —— 容器日志里只有父进程的 span。`▶ execute ... 100%` 这类输出缺失**不能**用来推断子进程卡在哪一步。要定位子进程必须用调试器（见下）。

**8. 用 `ps`/`pgrep` 监控子进程会误匹配自己的命令行。**
如果你的监控脚本源码里包含 `pipeline-task` 这个字符串，`ps -eo args` 会把你自己的进程匹配进去，看起来像是抓到了 27 次子进程，实则全是自己。用 `pgrep -x pipeline-task`（精确进程名）或匹配 `ps` 的 `comm` 字段（第 4 列），不要匹配 `args`。

### 三、定位挂起进程的方法（已验证可用）

**9. 读别的用户的 `/proc/PID/maps` 需要 `CAP_SYS_PTRACE`。**
普通用户和没有该能力的容器 root 都会 `Permission denied`。可用的组合：

```bash
docker run --rm --pid=host --cap-add=SYS_PTRACE -v /path/audit.py:/audit.py:ro \
  -v /sys/fs/cgroup:/sys/fs/cgroup:ro python:3-slim python /audit.py --pid <PID>
```

**10. 取真实调用栈用 gdb + 显式二进制。**
直接 `gdb -p` 跨 mount namespace 读不到 `/proc/PID/exe`，栈会全变成 `??`。把二进制从镜像里 `docker cp` 出来再喂给 gdb 即可解出符号：

```bash
docker cp koharu-headless:/usr/local/bin/pipeline-task /tmp/pt.bin
docker run --rm --pid=host --cap-add=SYS_PTRACE --network host -v /tmp/pt.bin:/pt.bin:ro \
  python:3-slim bash -c "apt-get update -qq && apt-get install -y -qq gdb && \
  gdb /pt.bin -p <PID> -batch -ex 'thread apply all bt 16'"
```

本次死锁就是这样定位到 `tokio::runtime::blocking::pool::BlockingPool::shutdown` 的。辅助手段：`/proc/PID/task/TID/syscall` 的第 9 个字段是 `pc`（返回地址），配合 `/proc/PID/maps` 可以映射到库和偏移；`wchan` 看内核侧等待点。

### 四、测量与判据（结论错在判据上最不值）

**11. `[anonymous]` 的映射大小不是泄漏指标。**
glibc 分配器会预先保留地址空间，虚拟映射可以涨几百 MB 而常驻内存纹丝不动。判据要看 `RssAnon`、`VmSwap`、`[heap]`，不要看映射尺寸。`scripts/audit_koharu_memory.py` 的 `verdict()` 已按此修正，有 `RSS_ANON_SLACK_KIB` 容差。

**12. 审计脚本里 `pwd.getpwuid` 在异构用户库下会崩。**
从 python 容器里审宿主进程时，容器内没有宿主的 `/etc/passwd` 项。已加 `user_name()` 回退到数字 uid。

**13. `read_text` 的错误占位串会被当成数据解析。**
`/proc` 不可读时返回的 `<unreadable: ...>` 恰好能骗过按空白分列的长度检查。解析 `/proc/*/maps` 必须先用地址区间的正则过滤，只接受 `^[0-9a-f]+-[0-9a-f]+\s` 形态的行。

**14. `Operation` / `Scope` 的 serde 编码容易写错。**
`Operation` 是内部标签（`tag = "operation"`），而请求字段也叫 `operation`，所以要嵌一层：

```json
{"operation": {"operation": "only", "stage": "detection"}}
```

`Scope` 是邻接标签（`tag = "scope", content = "value"`）。两者都有 `#[serde(default)]`，发空 body `{}` 等价于 `Full` + `Project`。单阶段跑 `Only` 可以用来隔离测量某个模型的内存。

### 五、代码层面的既有约束

**15. `Stage` 没有实现 `revision::Revisioned`。**
任何 `#[revision::revisioned]` 的传输结构里不能直接放 `Stage`，要用 `String`，在边界上用 `Stage` 已有的 `Display`/`FromStr`（`strum`，snake_case）互转。`WorkerErrorKind` 这类本文件内定义的枚举可以正常派生。`revision` 0.30 对普通枚举和结构体的派生是可用的（已用最小用例验证过）。

**16. `ProjectId` 的内部字段是 `pub(crate)`。**
要拿 `DocumentId` 用 `ProjectId::document()`，不要访问 `.0`。`koharu-scene` 已重新导出 `DocumentId`。

**17. `ModelCell::release()` 返回 `false` 不代表失败。**
它表示「调用时已经没有模型可释放」。因为 `Execution::release_active_batch` 会在阶段批抽干时就释放过一次，任务收尾的 `release_task_models` 三个阶段都返回 `false` 是正常现象。看顺序要看返回的元组顺序，看终态要看模型是否还在。

**18. 提交顺序与 revision 是硬约束。**
父进程按到达顺序提交 patch，revision 每次加一；子进程的 `WorkerCommitter` 按预测值给 `PatchTransfer` 盖 `project`/`base`。乱序提交时 `Patch::from_transfer` 的 observations 校验和 `validate_commit` 可能拒绝 —— 这是并发改动最可能踩的坑，要实测验证而不是假定安全。

**19. `AcceleratorGate` 在加速设备上是有意串行的。**
`accelerator.rs` 的 `Semaphore::new(1)` 和注释写明：异构 CUDA 模型并行比串行慢 2.5–4 倍。并发策略在 GPU 上不应让两个模型抢加速器。CPU 上没有这道门。

## 实现记录（2026-10-15）

已实现并入库到工作区（未提交）：`crates/koharu-pipeline/src/{scheduler,execution,stage_runner,accelerator,stages/mod,tests}.rs`，加新增 `overlap_tests.rs`。`cargo test -p koharu-pipeline` 75 项全绿。

与本计划的三处出入，都是实现时确定的：

1. **翻译不占加速器门。** 原计划没写到这一层：`StageRunner::run_with_recovery` 对**所有**阶段都抢 `AcceleratorGate` 的 `Semaphore(1)`，远程翻译会把加速器信号量占满整个网络等待，本地计算完全无法重叠。现在只有做设备计算的阶段持门（`Stages::needs_accelerator`：`Translation` 仅在 `Provider::Local` 时为真），加速器上的串行语义未动。
2. **检测是全局屏障。** 计划说检测不参与并发，实现成硬屏障：检测阶段列表里存在且未全部完成时，不派发任何非检测作业。屏障之后按页级依赖派发。
3. **同页解码图跨阶段常驻。** 并发下不能在阶段抽干时 `clear_images()`（会把别处还在用的页缓存清掉、触发重复解码），改成页面完成时清、收尾/中止时全清。代价是同一页的解码源图从检测活到修补，**不在本计划的内存估算里**，实测时单独看。

翻译的在飞上限是常量 `TRANSLATION_CONCURRENCY = 2`（`scheduler.rs`），不做配置项。本地三阶段仍是同阶段串行 1（多页并发在范围外）。

## 六、并发改动实测到的机制事实

用 `overlap_tests.rs` 的 11 个测试实测，不是推断：

**20. 「翻译必须晚于同页 Ocr」在 patch 层没有任何守卫，唯一守卫是 `Scheduler::prerequisite()`。**
实测三种情形：Ocr 先提交、再提交提前构建的翻译 patch → 拒绝（`observed component dev.koharu.text.source changed`）；Ocr 之后构建的翻译 patch 抢在 Ocr 前提交 → 同样拒绝；**提前构建的翻译 patch 先提交、Ocr 后提交 → 通过，结果静默陈旧**：终态 `SourceText` 是新识别的文本，`Translation` 还对应被替换掉的旧原文，patch 层完全不报错。`Operation::ReplaceComponent` 的 `before` 检查也拦不住，因为两者写的不是同一组件。

因此 `Execution::apply_completion` 的**「先 `commit_patch` 后 `mark_page_stage`」是安全性关键**，两处都已加注释。不要为了提前发翻译请求而把顺序倒过来，也不要放宽 `prerequisite()` 去重叠翻译请求与 OCR 阶段。

**21. `Execution::commit_patch` 里的 `validate_on` 是空转。**
`rebase_on` 返回的 patch 的 `base_state` 与 `self.scene.state` 是同一个 `Arc`，`validate_on` 内部走 `Arc::ptr_eq` 快路径直接 `Ok`，不会再次校验 observation。真正的检查全在第一次 `rebase_on` 里，别把它当第二道防线。

**22. 同页两个修补 patch 会冲突，这是对的。**
两者都观测 `Children{parent:page}`，第二个报 `patch conflict: children of entity … changed`。同页修补并发本来就不该放开（`concurrency(Inpainting) == 1`）。跨页修补、跨页任意组合、以及同页 Ocr ∥ Inpainting（含已有清理图层的就地改写分支）都双向可换序，已测。

**23. 下游消费者对顺序不敏感，但有一处显示语义会变。**
`koharu-rpc/src/routes/pipelines.rs` 和 `koharu-app/src/commands/processing.rs` 的进度都是纯计数器，与事件顺序无关；`PipelineCommitter.revisions` 收集的 revision 仍升序连续，`Session::undo_many` 自行降序重放，撤销不受影响。但 `JobSummary.stage/page/model` 只存最后一个事件的值，并发后会在 OCR/修补/翻译之间来回跳。这是显示语义变化，不是正确性问题，前端要有预期。

**24. `WorkerCommitter` 的盖章测试是等价复刻，会漂移。**
`WorkerCommitter` 是 `crate::worker` 的私有类型，测试驱动不到，`overlap_tests.rs` 复现了它三行盖章逻辑。它将来若改逻辑（例如改用 patch 自带的 `base_revision`），那批测试不会跟着变红。要锁住需把盖章抽成 `pub(crate)` 可测函数，或把测试挪进 `worker.rs` 自己的 `#[cfg(test)]`。

## 还没做

- **性能与内存实测。** 本计划的 119s / 81s / 74s 全是估算。需要远端 `fnnas` 重建镜像、跑 2 页以上的真实任务，用 TimingLayer 分段输出和 `scripts/audit_koharu_memory.py --compare` 复测。1 页任务看不出差异。
- **父进程判据复核。** 并发改动不碰 `worker.rs`、`pipeline.rs`，父进程内存边界理论上未变，但仍需按约束 2 做一轮前后对比。
- **远端翻译限流。** 2 个请求同时在飞是否触发服务商限流，取决于配置的 provider，未验证。

