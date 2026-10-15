# 并发阶段提交的乱序风险核查

日期：2026-10-15。对应 `plan/pipeline-stage-overlap-plan.md` 的「约束」#3 与「踩坑备忘」#18。

验证代码：`crates/koharu-pipeline/src/overlap_tests.rs`（`#[cfg(test)] mod overlap_tests`）。
运行方式：

```
CARGO_TARGET_DIR=D:/Github/koharu-docker/target/overlap cargo test -p koharu-pipeline overlap
```

结果：13 通过 / 0 失败（其中 11 个是本次新增的 overlap 测试）。`cargo test -p koharu-pipeline` 全量 75 通过 / 0 失败，未削弱任何既有测试。

## 测试的建模方式

patch 形状按生产代码逐条复刻，不是"大概像"：

- `stages/ocr.rs` 的 `Model::run` 尾部：`edit_as(generation(PRODUCER, model))`，`PRODUCER = "dev.koharu.pipeline.ocr"`；`observe_assets(page)`，每个结果 `observe::<Region>` / `observe::<Geometry>` / `observe::<SourceText>`；再 `set` `SourceText`（内容）与 `OcrAnalysis`（区域）。
- `stages/translation.rs` 的 `Processor::process` 尾部：`PRODUCER = "dev.koharu.pipeline.translation"`；每个文本层 `observe::<SourceText>` + `observe::<Translation>`；再 `set` `Translation`。
- `stages/inpainting.rs` 的 `Model::run` 尾部：`PRODUCER = "dev.koharu.pipeline.inpainting"`；`observe_assets(page)`，已有清理图层时再 `observe::<RasterLayer>(cleanup)` + `observe_assets(cleanup)`；然后 `add_entity(page, At::Start)` + `set` `RasterLayer` + `set_asset`。手动遮罩分支用 `scene.edit()`（无 generation），观测集与自动分支相同，也覆盖了。

简化之处与理由：

1. **不跑模型推理。** 测试直接写出推理后才会产生的文本与清理图层位图。被测对象是提交协议，不是模型。`stages/inpainting.rs` 的 `prepare` 只读场景与图像缓存，不产生 observation，所以跳过它不影响观测集。
2. **夹具的源文本由 Ocr 形状的 patch 写入，不是用户手写的。** `Edit::validate_origin_removal` 禁止 pipeline 编辑覆盖用户拥有的组件，`stages/ocr.rs` 也因此跳过 `Origin::User` 的目标。所以"已有识别结果、再跑一次 OCR"才是能与修补并发的真实形态，夹具就按这个形态建。
3. **`WorkerCommitter` 是 `crate::worker` 的私有类型，测试模块无法命名。** 子进程路径按它的三行盖章规则等价复现（`to_transfer` → 覆写 `project`/`base` → 父进程 `from_transfer`），没有为测试开可见性。
4. **快照推进用 `Snapshot::preview`**（与 `stages/mod.rs` 的既有测试一致）。`preview` 不推进 revision 号，所以 revision 盖章那部分改用真实 `Session::commit`，见测试 6。

## 一、逐项结论

| # | 测试 | 结论 |
|---|---|---|
| 1 | `ocr_and_inpainting_for_one_page_rebase_in_either_order` | **成立。** 同一页 Ocr ∥ Inpainting，两种顺序都能 `rebase_on` + `validate_on`，终态相同。自动 / 手动遮罩两种 patch 形态都试了；源文本"首次写入"与"覆盖上一次 OCR 结果"两种前置状态都试了。 |
| 1b | `ocr_and_inpainting_over_an_existing_cleanup_layer_rebase_in_either_order` | **成立。** 页面已有清理图层时 Inpainting 走"原地改资产"分支（观测 `RasterLayer`+`Assets`，不再 `add_entity`），仍与 Ocr 双向可换序。 |
| 2 | `ocr_translation_and_inpainting_interleave_in_every_producible_order` | **成立。** 在 `Scheduler::prerequisite` 允许的三种提交顺序下全部通过：`Ocr→Translation→Inpainting`、`Ocr→Inpainting→Translation`、`Inpainting→Ocr→Translation`。 |
| 2b | `translation_before_ocr_is_rejected_or_stale_and_needs_the_scheduler` | **部分成立，且有一处必须靠调度器兜底。** 见下文"最需要关注的一条"。 |
| 3 | `patches_for_different_pages_commute` | **成立。** 跨页 Ocr/Inpainting/Translation 的 4 组配对在两种顺序下终态逐组件相等。 |
| 4 | `two_inpainting_patches_for_different_pages_commute` | **成立。** 两页并发修补可换序。 |
| 4b | `two_inpainting_patches_for_the_same_page_conflict` | **按预期失败。** 同一页两个修补 patch 都观测 `Children { parent: page }`，第二个被拒，错误 `patch conflict: children of entity … changed`。这正是同页修补并发不该放开的理由。 |
| 5 | `stale_text_rewrites_are_still_rejected` | **成立。** 并发不削弱文本守卫。三处断言见下文。 |
| 6 | `transfer_forwards_out_of_order_stage_patches_in_lockstep` | **成立。** `to_transfer` / `from_transfer` 往返 + 预测 base 盖章，两种乱序都能按锁步提交。 |
| 6b | `transfer_rejects_a_base_stamp_out_of_lockstep` | **按预期失败。** 第二次转发仍用起始 revision 盖章时被拒（`Error::Storage`），即 revision 错位会被抓住而不是被静默接受。 |
| 6c | `transfer_rejects_an_observation_invalidated_by_an_earlier_commit` | **按预期失败。** base 盖章正确，但 observation 被前一次提交改掉时 `Patch::from_transfer` 同样拒绝：`patch conflict: observed component dev.koharu.text.source changed`。 |

### 最需要关注的一条

`translation_before_ocr_is_rejected_or_stale_and_needs_the_scheduler` 的三个断言：

1. **Ocr 先提交、再提交"提前生成的"翻译 patch → 拒绝。**
   `patch conflict: observed component dev.koharu.text.source changed`
   （翻译观测 `SourceText`，Ocr 改写了它。这是并发下真正会冲突的写入，守卫有效。）
2. **"Ocr 完成后才生成"的翻译 patch 抢在 Ocr 之前提交 → 同样拒绝。**
   同样的 `observed component … changed`。因为该 patch 的观测指纹已经记着 Ocr 之后的文本，拿它去对 Ocr 之前的场景校验必然不匹配。**这意味着"先提交翻译、再提交 Ocr"这条路即使调度器写错也走不通**——前提是翻译 patch 确实是 Ocr 之后构建的。
3. **"提前生成的"翻译 patch 先提交、Ocr 后提交 → 通过，但结果是陈旧的。**
   终态里 `SourceText = "recognized"`，而 `Translation = "EN<before>"`——译文对应的是被替换掉的旧原文。**patch 层完全不报错**，`Operation::ReplaceComponent` 的 `before` 检查也拦不住，因为 Ocr 与翻译写的不是同一个组件。

   换句话说：**"翻译必须晚于同页 Ocr"这条依赖在 patch 层是没有守卫的，唯一守卫是 `Scheduler::prerequisite()`（`Stage::Translation => Some(Stage::Ocr)`）。** 并发改动只要保留这条前置依赖就安全；如果为了重叠而放宽它（例如"OCR 一出结果就发翻译"实现在"提交之前"就发起翻译请求），就会出现上面这种静默陈旧译文。

   顺带说明：`prerequisite` 目前是"同页 Ocr 的作业 `Finished` 才派发翻译"，而 `Execution::apply_completion` 是先 `commit_patch` 再 `mark_page_stage`，所以翻译拿到的场景一定包含 Ocr 的提交。**这个"先提交后标记完成"的顺序是安全性的关键，不要在实现里倒过来。**

### 文本守卫仍然有效（测试 5）

- 有人改写了 `SourceText` 之后，旧翻译 patch 被拒：`patch conflict: observed component dev.koharu.text.source changed`（与 `stages/mod.rs` 的既有测试同一守卫，这里是生产形状的翻译 patch）。
- Ocr 改写 `SourceText` 之后，先建的翻译 patch 被拒，同一错误。
- Inpainting 与这两条守卫都可换序，不削弱它们。

### 两处值得记下的机制事实

1. **`Execution::commit_patch` 里的 `validate_on` 是空转。**
   代码是 `patch.rebase_on(&self.scene).and_then(|patch| { patch.validate_on(&self.scene)?; … })`。`rebase_on` 返回的 patch 的 `base_state` 就是 `self.scene.state` 的同一个 `Arc`，所以 `validate_on` → `rebase_on` 走 `Arc::ptr_eq` 快路径直接 `Ok`，**不会再次校验 observation**。真正的冲突检查全部在第一次 `rebase_on` 里。不要把 `validate_on` 当成第二道防线。
2. **`Operation::ReplaceComponent` 的 `before` 比对是第二道独立冲突检查**，即使某个组件没被 observe，两次写同一个组件后写的也会失败（`patch conflict: component … changed`）。`Edit::set` 不自动 observe 就是为了让"自己改过的"不变成与自己竞争的观测（见 `observe_component` 的注释）。测试 1/2 里 Ocr 与 Inpainting 的写入目标不相交，所以两道检查都不触发。

## 二、下游消费者

逐个读了 `crates/koharu-rpc/src/routes/pipelines.rs`、`crates/koharu-app/src/commands/processing.rs`、`crates/koharu-pipeline/src/bin/run.rs`、`crates/koharu-pipeline/src/worker.rs`。结论：**没有一处假设阶段顺序或进度事件顺序会被并发打破**，但有两处界面语义会变、一处既有假设值得记下来。`Operation` / `Scope` 的接口语义按计划保持不动，本次未改动。

### `crates/koharu-rpc/src/routes/pipelines.rs`

- 进度累计是纯计数器：`Started` 时算一次 `total = pages.len() * stages.len()`，`Finished` / `Skipped` 各 `+1`，`Running` 不计。**与事件顺序无关**，跨阶段交错不影响百分比。
- `JobSummary.stage` / `page` / `model` 只保存**最后一个事件**的值。并发后这三个字段会在 OCR / 修补 / 翻译之间来回跳，"当前阶段"不再是单调推进的一条线。**这是显示语义变化，不是正确性问题**；前端如果把 `stage` 当作"现在在跑哪一段"来渲染，会看到它反复横跳。不需要改代码，但要预期到。
- `SessionCommitter` 直接 `project.session.commit(output.patch)`，不自己 rebase；它依赖 `worker.rs` 里的 `Patch::from_transfer(&scene, transfer)` 已把 patch 重建到父进程正在跑的场景上。这一点与并发无关，且已由测试 6 覆盖。

### `crates/koharu-app/src/commands/processing.rs`

- 进度处理与 RPC 同构，同样是计数器，顺序无关。
- `PipelineCommitter.revisions` 收集每个非空提交的 revision，收尾 `project.record(revisions)` 合成一个撤销组。乱序后这个列表仍按 revision 升序且连续（`Session::commit` 每次 +1）。`Session::undo_many` 会自行降序排序并按 inverse 顺序回放，**不要求连续、不要求入参有序**，所以并发不影响撤销。
- 每次提交都会 `desktop.synchronize(...)` + 发一次 canvas 状态。并发后提交次数不变（每个 stage 输出一次），只是到达时间更分散，重绘次数不变。
- 仍然只允许一个任务（`stops` 非空即拒绝），所以不存在两个流水线同时写同一 session 的情形。

### `crates/koharu-pipeline/src/bin/run.rs`

- 进度回调只在 `Progress::Finished` 上打一行 `{stage} finished in {elapsed}`，顺序无关。
- 走 `execute_in_subprocess`，与 RPC/应用同一条路径。

### `crates/koharu-pipeline/src/worker.rs`

- `WorkerCommitter::commit` 用 `*self.base` 给 transfer 盖 `project`/`base`，提交后 `*self.base = self.base.next()`。这是**精确预测**，前提是父进程每转发一次提交，session 的 revision 恰好 +1。空 patch 不会走到这里（`stage_runner.rs` 把 `patch.is_empty()` 转成 `StageOutcome::Skipped`，不进 `commit_patch`、不发 transfer），所以计数不会被空提交带偏——测试 6 的锁步往返验证了这条。
- `Execution::validate_commit` 只断言 `next.revision() > previous.revision()`（严格大于），不等于 +1。若父进程 session 在任务期间被别的写入者插队，预测值就会错位，下一次 `from_transfer` 以 `RevisionConflict` 失败。**`worker::execute` 的注释写明"任务期间没有别的东西写它的 session"，这是既有假设，并发没有改变它，也没有测试覆盖它**（见"缺口"）。
- `WorkerEvent::Progress` / `Patch` 是两个独立 frame，交错发送没有顺序依赖；父进程 `read` 循环按到达顺序处理，顺序与子进程 `WorkerCommitter` 写出顺序一致，所以 base 计数两端同步。
- `WorkerReport.base` / `final_revision` 只是回报值，RPC 与应用都不用它们做判断。

## 三、缺口

1. **"翻译必须晚于同页 Ocr"只靠 `Scheduler::prerequisite`，没有 patch 层兜底。** 见上文测试 2b 第 3 条：提前构建的翻译 patch 先提交会被接受，留下陈旧译文。实现时如果改动了派发时机（尤其是"OCR 一出结果就发翻译"这句原始设想，如果实现成"在 Ocr 提交之前就用某个中间结果发请求"），这里会静默出错。**建议保留"先 `commit_patch`、后 `mark_page_stage`"的顺序，并把 `prerequisite` 当作不可放宽的硬约束。** 本次没有为它写回归测试，因为它属于调度器行为（`scheduler.rs` 由并行改动负责）。
2. **父进程 session 在任务期间被其他写入者插队的行为未验证。** 应用侧有 `stops` 互斥保证同一时刻只有一个任务，但用户在任务进行中编辑项目仍会写同一 session。这会让 `Session::commit` 或 `from_transfer` 以 `RevisionConflict` 失败，任务整体报错。这是并发之前就存在的假设，本次只做了静态阅读，没有写测试（需要伪造一个并发写入者，超出本次范围）。
3. **`Snapshot::preview` 不推进 revision。** 测试 1–5 用 preview 推进状态，因此没有覆盖 revision 序列本身；测试 6 用真实 `Session::commit` 覆盖了 revision 盖章。两者对 observation 冲突的判定是一致的（都走 `Patch::rebase_on`），但"preview 与 session 提交在边界行为上完全等价"这一点没有被证明。
4. **同页 Inpainting ∥ Inpainting 被证否**（测试 4b），所以计划里"一页的修补不必等其他页修补完成"是安全的，"同一页两个修补并发"不安全。计划本身没要求后者，这里只是把边界钉住。
5. **检测阶段、跨页同阶段并发度、内存叠加**都不在本次范围（对应计划的"范围外"）。测试只覆盖 patch 提交协议，不覆盖 `AcceleratorGate` 的串行语义（约束 #5）、模型释放顺序（约束 #1）、父进程内存判据（约束 #2）。这些由并行改动自带的测试（`release_plan_*`、`task_memory_release_lifecycle` 等）负责。
6. **`WorkerCommitter` 本身没有被测试直接驱动**（私有类型）。它的盖章逻辑只有三行且被等价复现，但如果它将来改了（例如改成按 patch 自带的 `base_revision` 盖章），本文件的测试 6 不会跟着变红。要真正锁住它，需要把 `WorkerCommitter` 的盖章抽成一个 `pub(crate)` 可测的函数，或者把 `worker` 模块的测试挪进 `worker.rs` 自己的 `#[cfg(test)]`。**这是一处"测试与实现可能漂移"的已知缺口。**
