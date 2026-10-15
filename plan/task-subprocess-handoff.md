# 交接：任务子进程交换占用修复

日期：2026-10-14（第二段）。上一段停在 `cargo check` 不通过，本段把它做完，并把上一段列出的设计错误一并修掉。真实任务验证还没有跑完，见「还没做」。

## 目标

一次 `pipeline.execute` 的模型加载、推理、`ImageCache` 必须发生在子进程里。子进程提交结果、调用现有 `TaskExecution::release_resources` 后退出，由操作系统回收匿名映射、`[heap]`、`memfd` 和交换页。父进程只保留会话、提交和最终状态。

禁止改：`crates/koharu-torch/**`、`crates/koharu-ml/**` 的模型实现、`crates/koharu-diffusion/**`、`crates/koharu-llama/**`、任何生成源码、模型包。`release_task_models` 的 `Detection`、`Ocr`、`Inpainting` 顺序不动。

本段为了记录子进程该初始化哪些运行时，在 `crates/koharu-ml/src/lib.rs` 加了只读的 `RuntimeInit` 和 `runtime_init()`，没有改任何初始化行为。

## 编译错误（已解决）

根因是 `revision` 0.30 不能派生没有 `Revisioned` 的 `Stage`。传输结构里 `Stage` 全部改成 `String`，边界上用 `Stage` 已有的 `Display` 和 `FromStr`（`strum`，snake_case）互转：

- `WorkerProgress` 的 `stages` 和四个变体里的 `stage`
- `WorkerFailure::stage`
- `WorkerModelRelease::stage`
- 随之：`From<Progress> for WorkerProgress`、`TryFrom<WorkerProgress> for Progress`、`worker_releases`、`model_releases`、`WorkerOutcome::from_release`、`into_release`

`WorkerErrorKind` 是本文件自己的枚举，能派生，没动。

另外三处：

- `WorkerAssignment::from_request(request, project, base)` 和 `into_request()` 返回 `Result`，两个调用点都加了 `?`。
- `serde_json` 加进了 `crates/koharu-pipeline/Cargo.toml`（`operation` 和 `scope` 走 JSON 字符串）。
- `ProjectId` 的内部字段是 `pub(crate)`，`crates/koharu-scene/src/id.rs` 加了 `ProjectId::document()`，不公开字段。`crates/koharu-scene/src/lib.rs` 多导出 `DocumentId`。

解析收敛成一个 `parse_stage`，父进程读 patch 和四个进度变体都用它。

## 已修的设计错误（上一段的 1、2、6）

**1. 取消和读取循环的竞争。** 原来 `tokio::select!` 的 `cancel` 分支写成 `() = cancel => read.await?`，`read` 被两个分支同时持有。现在取消只负责把字节送出去：

```rust
tokio::spawn(cancel);
let finished = read.await?;
```

结果只从 `read` 来。`read` 在子进程报出 `Finished` 或关掉 stdout 时结束，取消不改变这一点。

**2. `PatchTransfer` 的 project 和 base。** 这是上一段说的「最大的洞」。修法是父进程在导出前把自己的 `project_id` 和 `revision` 放进 `WorkerAssignment`，子进程回传 patch 时用这两个值盖掉副本自己的：

```rust
// crates/koharu-pipeline/src/worker.rs 的 WorkerCommitter::commit
let mut transfer = output.patch.to_transfer(stage, page);
transfer.project = self.project;
transfer.base = *self.base;
...
*self.base = self.base.next()?;
```

`PatchTransfer` 的 `project` 和 `base` 因此改成公开字段（`crates/koharu-scene/src/patch.rs`）。

为什么不需要回传协议：`Session::commit` 用 `current.revision().next()` 推进 revision（`crates/koharu-scene/src/session.rs:147`），所以父进程的 revision 严格每次加一。子进程按「父进程连续提交、每次加一」推算是安全的，前提是任务期间没有别人写这个会话。`Pipeline::execute_in_subprocess` 已经持有 `self.execution` 锁，三个调用方也都走它。

**6. 子进程的 ML 初始化。** 原来固定 `koharu_ml::init()`，CPU 机器会在子进程里尝试初始化 llama 和 diffusion。现在父进程把初始化模式读出来放进 `WorkerAssignment`：

```rust
// crates/koharu-ml/src/lib.rs
pub enum RuntimeInit { Cpu, Gpu, Full }
pub fn runtime_init() -> Option<RuntimeInit>
```

子进程按 `WorkerRuntime::{Cpu,Gpu,Full}` 调 `init_cpu` / `init_gpu` / `init`，`device` 的 `cpu` 参数同样跟 `RuntimeInit::Cpu` 对齐。父进程初始化过的运行时，子进程只初始化同样的那几个。

## 上一段保留的可接受项（未改，理由）

- **进度只在提交点和任务结束前发出。** `WorkerCommitter` 只在 `commit` 时 `drain_progress`，长时间阶段中间父进程看不到 `Running`。RPC 的 `start_pipeline` 只用 `Started`/`Finished` 更新计数，桌面端的活动中心同样按阶段显示，都不依赖中间进度。
- **`Pipeline::from_config` 在子进程里会 spawn 一个配置热重载任务。** 一次性子进程没有意义，退出时直接杀掉，不影响正确性。
- **`from_checkpoint` 不核对 blob 覆盖。** 缺 blob 时 `save` 会在 `derive` 里失败，错误信息不直指缺哪个 blob。`export_scene` 是 `snapshot.blob_ids()` 全量导出，实际不会缺。

## 已做

- `crates/koharu-pipeline/src/worker.rs`、`crates/koharu-pipeline/Cargo.toml`、`crates/koharu-pipeline/src/lib.rs`、`crates/koharu-pipeline/src/pipeline.rs`、`crates/koharu-pipeline/src/request.rs`、`crates/koharu-pipeline/src/bin/pipeline-task.rs`、`crates/koharu-pipeline/src/bin/run.rs`
- `crates/koharu-scene/src/{patch,lib,session,snapshot,id}.rs`
- `crates/koharu-ml/src/lib.rs`（只读 `RuntimeInit` / `runtime_init()`）
- `crates/koharu-rpc/src/routes/pipelines.rs`、`crates/koharu-app/src/commands/processing.rs`、`crates/koharu-app/src/commands/agent/host.rs`（改成 `execute_in_subprocess`）
- `Dockerfile`：构建期 `cargo build -p koharu-pipeline --release --locked --bin pipeline-task`，运行期 `COPY --from=builder /out/pipeline-task /usr/local/bin/pipeline-task`。两个二进制必须在同一个目录，`worker_executable()` 才找得到。
- `scripts/audit_koharu_memory.py`：报告加 `anonymous_mappings_bytes`、`memory_current`、`memory_swap_current`、`swap_kib`、`task_children`；新增 `--compare`，读一份任务前的 `report.json`，输出验收结论（`memory.swap.current` 不涨、`[anonymous]`/`[heap]`/`memfd` 不涨、`pipeline-task` 已退出、`open_model_files` 为空）。

## 编译环境

Windows 上 `koharu-pipeline`、`koharu-scene` 能 `cargo check`，`koharu-rpc` 要在 VS 开发者环境里编（`vcvars64.bat`），`koharu-app` 在这台机器上编不了（CEF 需要的 SDK 版本不对，跟本次改动无关）。

Debian（`qinzhu@10.126.126.1`，别名 `fnnas`）上工具链在 `~/.cargo/bin`，需要自己 `export PATH="$HOME/.cargo/bin:$PATH"`。构建目录是 `~/koharu/build-subprocess`（从 `~/koharu/koharu-docker` 本地 clone 出来的，不影响原来的检出）。那里 `cargo check` 通过：`koharu-pipeline`、`koharu-rpc`、无头的 `koharu --headless`。`koharu-app` 缺 `libglib2.0-dev`，无头部署不需要。

`cargo test -p koharu-pipeline --lib task_memory_release_lifecycle` 在 Debian 上通过。

## 部署验证（2026-10-14 第三段）

容器：`koharu-headless`，`--network astrbot_network`，4001→4000，`koharu-data` + `koharu-config`，`--device /dev/dri`，`seccomp=unconfined`。构建目录 `~/koharu/build-subprocess`（Debian `fnnas`）。

**两个部署期缺陷，都是这次引入的：**

1. **补丁漏传 `crates/koharu-rpc`。** 同步到远端时 `git diff` 只带了 `koharu-ml`/`koharu-pipeline`/`koharu-scene`，导致镜像里 `pipelines.rs` 仍是 `.execute(...)`，整个任务跑在父进程内，`pipeline-task` 从未出现。判别方法：`grep -aoc "task subprocess" /usr/local/bin/koharu` 为 0 即未生效。已补传并重建。

2. **子进程未继承运行时包目录。** `Store::configure` 只在 `koharu-rpc/src/server.rs:30` 和 `koharu-app/src/app.rs:85`（父进程入口）调用，`worker::run` 没有。`Store::root()` 于是回落到 `dirs::cache_dir()/koharu/packages`，子进程把 Torch、stable-diffusion.cpp 和 HuggingFace 权重重新装了一份（容器内 `~/.cache/koharu` 实测 1.4 G，而父进程用的 `.../Koharu/packages` 是 1.5 G）。任务从 30 s 涨到 6 min 以上，且子进程线程全程阻塞在 I/O。修法：`WorkerAssignment.store` 传父进程的 `Store::root()`，子进程 `run_assignment` 先 `Store::configure` 再初始化 ML。已修。

**内存验收（`1.webp`，1201×2048）**，用 `python:3-slim --pid=host --cap-add=SYS_PTRACE` 跑 `scripts/audit_koharu_memory.py`（容器内读 `/proc` 需要 SYS_PTRACE）：

- 子进程确实出现：`pid=916650 ppid=910235 rss=1.11 G comm=pipeline-task`，模型内存在子进程
- 父进程任务前后：VmRSS 133 312 → 135 100 KB（+1.8 MB），RssAnon 63 332 → 64 724 KB（+1.4 MB），VmSwap 0 → 0，`[heap]` 不变
- `open_model_files` 为空，任务后无 `pipeline-task` 残留
- 对照旧的进程内路径：父进程 RssAnon 63 MB → 1.93 GB，VmSwap +53 MB，`[anonymous]`/`[heap]` 都增长

`[anonymous]` 虚拟映射会涨（预留地址空间），判据要看 `RssAnon` 和 `VmSwap`，不要只看映射大小。

**死锁（真根因，第三段发现）**：子进程任务跑完后永不退出，父进程 `child.wait()` 永久挂起。用 gdb 取栈定位：

```
tokio::runtime::blocking::pool::BlockingPool::shutdown
tokio::runtime::blocking::shutdown::Receiver::wait
pipeline_task::main
```

`main` 已返回，卡在 tokio 运行时关闭。链路是：父进程把 `ChildStdin` 放进 `tokio::spawn(cancel)`，该任务只在 `stop.cancelled()` 时结束，于是 **stdin 永不关闭**；子进程的取消读取是 `tokio::io::stdin()` 的 `read_exact`，跑在 tokio 阻塞池里，只在收到字节或 EOF 时返回；`cancel.abort()` 打不断 `spawn_blocking` 里的阻塞 read；运行时 drop 时 `BlockingPool::shutdown` 等它，进程无法退出。

注意第一次「慢了 6 分钟」其实不是慢，是同一个死锁（审计循环超时时仍是 `status=running`，我当时误判成下载慢）。

修法（两处，缺一不可）：

- 父进程 `worker::execute`：`read` 用 `tokio::pin!` 固定后 `select!`（取消分支只送字节，结果仍来自 `read`），拿到结果后 `drop(stdin)` 让子进程 read 收到 EOF。
- 子进程 `worker::run`：取消读取改成 `std::thread::spawn` + `std::io::stdin()`，不占用 tokio 阻塞池，`main` 返回时随进程一起结束。

**性能代价**：每次任务在全新进程里重载模型是方案固有的（内存靠进程退出回收，不可能同时让模型常驻）。修掉包目录后预计仍有每任务的模型加载开销。若不可接受，可选：常驻 worker 定期回收（N 个任务或 T 分钟后重启），或按父进程 RSS 阈值决定是否走子进程。

## 还没做

- **真实任务验证。** 计划第 1–5 步要求：任务前审计、用 `~/koharu/koharu-docker/待翻译.jpg`（2894×4093）跑一次完整任务、任务后审计并 `--compare`。接口是 `POST /projects`、`POST /projects/current/pages`（multipart）、`POST /pipelines`、`GET /events`（SSE）、`POST /projects/current/export`。
- **镜像构建。** 远端容器 `koharu-headless`（`koharu-headless:latest`，`--gpu`，`koharu-data` + `koharu-config` 两个卷，`--device /dev/dri`，`seccomp=unconfined`，`unless-stopped`，4001→4000）跑的是旧二进制。新镜像 `koharu-headless:subprocess-test` 正在构建，构建命令是两包分开编（`--bin` 按包解析，`pipeline-task` 在 `koharu-pipeline` 里）。
- 镜像构建完成后的替换：停掉旧容器、改 `koharu-headless:latest`、按同样的 `docker run` 参数起新容器，再做上面那轮验证。

## 不要动的工作区改动

与本修复无关：`.gitignore`、`AGENTS.md`、`Cargo.lock`（本修复改 `Cargo.toml` 之后它加了 `revision`/`serde_json`/`tempfile` 三行，就这几行）、`crates/koharu-renderer/src/renderer.rs`、`crates/koharu-translator/src/remote/openai_compatible.rs`，以及被删除的 `RENDER_BACKGROUND_LOSS_REPORT.md`、`UPSTREAM_SYNC_PLAN.md`、`task-memory-release-verification-report.md`、`task-memory-swap-retention-fix-plan.md`。未跟踪的 `.codegraph/`、`.mcp.json`、`koharu-memory-audit-*`、`plan/`、`scripts/__pycache__/` 也不是这次的产物。`_w.b64` 是上一段失败写入留下的空文件，可以删。

## 验收

计划的判据：任务结束后模型释放顺序是 `Detection`、`Ocr`、`Inpainting`，`ImageCache` 项数是 0，`open_model_files` 为空，任务子进程 PID 不存在，父进程 `memory.swap.current` 不高于任务前，父进程里没有这次任务新增的 `[anonymous]`、`[heap]`、`memfd`。做不到就说明修复没完成。

前两条由 `task_memory_release_lifecycle` 覆盖（进程内路径，不经过子进程），后几条只能靠 `scripts/audit_koharu_memory.py` 的前后对比。
