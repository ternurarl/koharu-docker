# 上游同步审计 — koharu-translator（P0 翻译层）

日期：2026-10-15。只读审计，不实施移植。
枚举范围：`git log main..upstream/main -- crates/koharu-translator`（当前 `upstream/main` = 52340cbf）。

## 0. 基线核实与数量

- 计划称该 crate 有 26 个上游提交。在计划快照（upstream = 4df6a4f7）下核实为 26，数字与当时一致；当前 `upstream/main` 比该快照多 1 个提交 `ac2172b4`（2026-10-15，移除 koharu-metrics 遥测），故当前实际为 **27 个**。本清单列出全部 27 个。
- 已移植判定（fork 提交正文含 `Upstream: <上游 SHA>`）：fork `main` 上仅有 3 个移植提交——`e592ffb9`（上游 `f8a25e2a` 重许可）、`68650b09`（上游 `dceb8485` + `b87b93b2` 的 pipeline/ml 子集）、`03106e91`（上游 `6959411c`）。三者都不在这 27 个提交内。逐个 SHA 在 fork 历史中检索 `Upstream:` 行，27 个全部无匹配。**本清单已移植 = 0。**
- fork 独有、必须保留的 translator 改动（合并移植时不得覆盖）：
  - `f6bee459`：`crates/koharu-translator/src/lib.rs` 增加 `Translator::release()` 与测试 `remote_release_returns_false`。
  - `4ccad2d3`：`crates/koharu-translator/src/remote/openai_compatible.rs` 在 `compatible()` 中显式下发 `backend.thinking = Some("enabled"/"disabled")`。
- 重要状态变化（任务书基线已过期，如实记录）：任务书称 `openai_compatible.rs` 有未提交修改。审计期间 fork `main` 从 `a23301f5` 前进到 `9c290688`（新增 5 个提交：`5358464e`、`4ccad2d3`、`91ed2f2e`、`29507a65`、`9c290688`），该未提交修改已被提交为 `4ccad2d3`（正文无 `Upstream:` 行，属 fork 独有修复），当前工作树中该文件干净。第 5 节按提交后的状态分析相交点。
- 27 个上游提交中同时触及范围排除路径（`koharu-app`、`koharu-desktop`、`koharu-canvas`、`packages/*`、`koharu-metrics`、CI 与发布文件）的，均在下表「涉及文件」列写明排除项。

## 1. 逐提交清单

状态五档：已移植 / 可 cherry-pick / 手工移植 / 跳过-GUI / 暂缓。

| 上游 SHA | 标题 | 状态 | 涉及文件（translator 范围；排除项另注） | 备注 |
|---|---|---|---|---|
| 87442b49 | chore(deps): modernize workspace dependency configuration | 手工移植 | 白名单仅 `crates/koharu-translator/Cargo.toml`。排除：workspace `Cargo.toml`、`Cargo.lock`、`koharu-torch*` 等 16 个文件 | translator 切片只删 `[dev-dependencies] toml`（已核实 translator 源码无 `toml` 引用）。workspace 依赖布局重构与 fork 漂移，manifest 变更与 `efb28e30` 一并人工审定，不整取。 |
| c17d7a07 | feat(translator): add Qwen 3.8 27B model | 可 cherry-pick | `src/local/catalog.rs`；`README.md` 可选 | 新增条目自带生成参数为过期值。必须先于 `9962e14c` 应用，参数以 `9962e14c` 修正后的形态为准（计划硬约束）。 |
| 6269da2c | feat(translator): enforce keyed translation output | 暂缓 | `Cargo.toml`、`src/prompt.rs`、`src/remote/gemini.rs`、`src/remote/openai_compatible.rs` | 被 `1d168ff4` 全量 revert，净变更为零，不移植。 |
| 1d168ff4 | Revert "feat(translator): enforce keyed translation output" | 暂缓 | 同上 | 与 `6269da2c` 成对对消，不移植。 |
| 08601a8f | feat(translator): refresh remote model catalogs | 可 cherry-pick | `src/remote/{atlas_cloud,claude,deepseek,gemini,openai,openrouter}.rs` | 与 `412822d2` 按最终形态处理：先本提交，后 `412822d2`（计划硬约束）。 |
| 800e32bb | feat(translator): add Grok and MiniMax providers | 可 cherry-pick | `src/remote/grok.rs`（新）、`src/remote/minimax.rs`（新）、`src/remote/mod.rs`、`src/provider.rs`、`src/bin/translate.rs`。排除：`packages/*` locales、`packages/bridge/src/protocol.ts`；`README.md`、`docs/models/translation-providers.md` 可选 | 夹在 `08601a8f` 与 `412822d2` 之间，按此顺序应用。 |
| 412822d2 | feat(translator): discover provider model catalogs | 可 cherry-pick | `src/model.rs`、`src/remote/{claude,deepseek,gemini,minimax,mod,openai}.rs` | 目录动态发现的最终形态，覆盖 `08601a8f` 的静态目录刷新结果。 |
| e47620c0 | perf(translator): optimize structured inference | 可 cherry-pick | 仅取 `src/remote/lm_studio.rs`（`InputItem::Message` → `InputItem::Text`）。排除：`koharu-llama/src/{llguidance_sampler,model,sampling}.rs`、`koharu-ml/src/llm/model.rs`（归运行时轮） | translator 切片 6 行，与 fork 私有改动无交叠。 |
| efb28e30 | feat(translator): strengthen translation prompt | 手工移植 | `src/prompt.rs` 直接取；`crates/koharu-translator/Cargo.toml` 与 workspace `Cargo.toml` 的 `indoc` 依赖行人工审定。`Cargo.lock` 禁止手改 | 计划硬约束：只取 Rust prompt 实现。fork 现无 `indoc` 依赖；锁文件由 cargo 重新生成。 |
| 9962e14c | feat(translator): tune local model sampling | 可 cherry-pick | `src/local/catalog.rs`（106 增 106 删） | 采样参数的权威值，覆盖 `c17d7a07` 的过期参数（计划硬约束）。 |
| 8ab7cde1 | fix(translator): use LM Studio vision capability | 手工移植 | `src/remote/lm_studio.rs` | 计划硬约束：不取「`capabilities.vision` 必返」假设。按可选字段解析、缺失取 false；须补三态 JSON fixture。 |
| 5b6672cc | fix(translator): align reasoning capabilities | 手工移植 | `src/{model.rs,lib.rs,bin/translate.rs,local/mod.rs}`、`src/remote/{claude,deepseek,gemini,grok,lm_studio,minimax,openai,openai_compatible,openrouter}.rs`、`src/remote/atlas_cloud.rs`（若保留 Atlas Cloud）、`crates/koharu-pipeline/src/{stages/translation.rs,tests.rs}`。排除：`packages/bridge/src/protocol.ts`、`packages/koharu/**`、`packages/koharu/tests/**` | 引入 reasoning 能力协商，触及 14 个 translator 文件，与 `4ccad2d3`、`f6bee459` 相交，须手工合并。 |
| 87104fcf | feat(translator): support local reasoning | 手工移植 | `src/{local/catalog.rs,local/mod.rs,model.rs,remote/openai_compatible.rs}`、`crates/koharu-ml/src/llm/{mod.rs,model.rs}`。排除：`packages/*` locales、`packages/koharu/**` | 本地模型 reasoning 依赖 `koharu-ml` 切片同取。`openai_compatible.rs` 删除 `OpenAiCompatibleConfig.vision` 字段，与 fork 相交。 |
| f842172e | fix(translator): respect model capabilities | 手工移植 | 13 个 translator 文件（`src/model.rs`、`src/lib.rs`、`src/bin/translate.rs`、`src/local/mod.rs`、`src/remote/*`）、`crates/koharu-pipeline/src/{bin/run.rs,tests.rs}`。排除：`packages/bridge/src/protocol.ts`、`packages/koharu/**`、`packages/koharu/tests/**` | 核心改动：`GenerationConfig.thinking: bool` 重命名为 `reasoning: Option<bool>`，波及 `koharu-pipeline/src/config.rs` 等 fork 调用点，须全量改名（AGENTS.md 禁止保留旧形态）。 |
| 66e603d4 | feat: add native usage metrics | 暂缓 | `src/lib.rs`（metrics 埋点）。排除：`koharu-metrics/**`、`koharu-app/**`、`.github/workflows/release.yml` | `koharu-metrics` 在范围排除清单（Google Analytics 遥测）。translator 切片仅为遥测埋点，且被 `ac2172b4` 整体移除，不取。 |
| 3da33914 | fix(translator): decode optional Gemini thinking capability | 手工移植 | `src/remote/gemini.rs` | 依赖 `f842172e` 之后 `ListedModel.thinking` 字段形态；fork 的 `gemini.rs` 该结构体尚无 `thinking` 字段，不能单独 cherry-pick。与 `f53631b4` 成组处理。 |
| 5f87445e | refactor: refine metrics | 暂缓 | `src/lib.rs`（metrics span）。排除：`koharu-metrics/**`、`koharu-app/**`、`koharu-pipeline/**` | 同 `66e603d4`：遥测埋点，范围排除，不取。 |
| f53631b4 | fix(translator): map Gemini thinking controls | 手工移植 | `src/remote/gemini.rs` | `ThinkingConfig` 改为 `Option<thinking_budget>` + `Option<thinking_level>` 并按模型名分支映射；fork 现状仅对 `gemini-2.5-flash` 发固定 budget。替换为上游最终映射，测试一并取。 |
| e4c3d069 | feat(translator): add uncensored Qwen 3.8 model | 可 cherry-pick | `src/local/catalog.rs`；`README.md` 可选 | 排在 `9962e14c` 之后应用，目录行级依赖 `dbed34d8` 的话则改排其后（见顺序节）。 |
| dbed34d8 | refactor(runtime): require pinned Hugging Face revisions | 可 cherry-pick | `src/local/catalog.rs`（`LocalModelDescriptor` 增加 `revision` 字段与逐条 pin）。排除或延后：`crates/koharu-runtime/src/source/hugging_face.rs`（归运行时轮，冲突则弃） | fork 的 `catalog.rs` 现用 `HuggingFaceFile::latest`；fork runtime 已有 `HuggingFaceFile::pinned`，translator 切片可独立编译。 |
| bf516a01 | chore: update repository links | 可 cherry-pick | `src/language.rs`、`src/remote/{caiyun,claude,deepl,gemini,google_cloud,openai_compatible}.rs`（各 1 行注释链接） | 无行为变更。与 `4ccad2d3` 同文件但仅碰注释行，冲突可随手解。 |
| e6d330c8 | fix(translator): recover model responses the JSON repair layer rejects | 手工移植 | `src/prompt.rs`、`src/lib.rs`、`src/json.rs`（整文件删除）、`src/remote/{atlas_cloud,claude,deepseek,lm_studio,minimax,mod,openai,openai_compatible,openrouter}.rs` | 本清单最大单提交（1133 增 761 删）。JSON 修复层从独立 `json.rs` 并入 `prompt.rs` 与各 provider；`openai_compatible.rs` 重写移除 `ChatBackend`，与 `4ccad2d3` 直接相交（见第 5 节）。 |
| 69e67f96 | feat: export a project as a CBZ archive | 跳过-GUI | translator 切片仅 `src/local/mod.rs`（`spawn_blocking` → `tokio_rayon::spawn`）与 `Cargo.toml`（`tokio-rayon` 依赖）。排除：`koharu-app/**`、`koharu-desktop/**`、`packages/**` 及导出主体（`koharu-storage`、`koharu-psd`、`koharu-renderer`、`koharu-pipeline` 导出相关） | 主体为导出/GUI 功能。translator 切片是调度微优化，不单独移植；如未来需要，随 manifest 人工审定顺带取。 |
| d8a68ba2 | feat(ui): show download state for local translation models | 手工移植 | `src/local/catalog.rs`（`LocalModelDescriptor::file()`）、`src/local/mod.rs`、`src/model.rs`（`Quantization.downloaded`）、`crates/koharu-runtime/src/source/hugging_face.rs`（`HuggingFaceFile::exists()`）。排除：`packages/koharu/**`、`packages/bridge/src/protocol.ts` | 依赖 runtime 下载层形态（`dbed34d8`/`7834852e` 之后的 `pinned`/`exists` API）。fork runtime 的 `hugging_face.rs` 与上游已漂移，runtime 切片预计手改。 |
| 9e8698be | chore: remove Atlas Cloud provider and sponsorship | 暂缓 | translator 切片：`src/remote/atlas_cloud.rs`（整文件删除）、`src/provider.rs`、`src/remote/mod.rs`、`src/bin/translate.rs`。排除：`.github/sponsorkit/**`、`README.md`、`docs/**`、`packages/**` locales | 是否移除 Atlas Cloud provider 属产品裁决，待用户决定（fork 当前保留该 provider）。裁决前不动。 |
| bc1a51b6 | fix(translator): cache model-listing client with five-second timeout | 可 cherry-pick | `src/lib.rs`（`models()` 用 `OnceCell<reqwest::Client>` 5 秒超时客户端替代 `koharu_runtime::http_client()`） | 与 `f6bee459` 的 `lib.rs` 改动位于不同函数区域，预期可干净应用。 |
| ac2172b4 | chore: remove koharu-metrics telemetry | 暂缓 | translator 切片为移除 metrics 埋点（对 fork 是空操作，fork 从未引入）。排除：`koharu-metrics/**`、`koharu-app/**`、`koharu-pipeline/**`、`packages/docs/**` | fork 无 metrics 埋点，translator 侧无需任何改动。本提交其余为遥测 crate 移除，范围排除。 |

五档数量统计：已移植 0、可 cherry-pick 10、手工移植 10、跳过-GUI 1、暂缓 6，合计 27。

## 2. 推荐应用顺序与依赖关系

依赖依据：(a) 上游时间顺序（`git log --reverse`）；(b) 计划硬约束；(c) 同文件先后覆盖关系。

**批次 A — 本地模型目录（低冲突，先做）**

1. `c17d7a07` → 2. `9962e14c` → 3. `e4c3d069` → 4. `dbed34d8`
   依赖：`9962e14c` 覆盖 `c17d7a07` 的生成参数（必须先于其应用，否则取到过期值）；`dbed34d8` 给全部条目加 `revision` 字段，其 diff 上下文包含 `c17d7a07`/`9962e14c`/`e4c3d069` 新增行，故排在三者之后最稳。

**批次 B — 远程 provider 目录（成组，按序）**

5. `08601a8f` → 6. `800e32bb` → 7. `412822d2`
   依赖：三者同日线性演进；计划要求 `08601a8f` 与 `412822d2` 取最终形态，即完整按序应用，不拆中间态。`800e32bb`（Grok/MiniMax 新文件）必须在 `412822d2` 之前，后者改写 `minimax.rs` 目录逻辑。

**批次 C — 协议与能力对齐（高冲突面，逐个手工）**

8. `e47620c0`（lm_studio 输入项文本化，独立小改）
9. `8ab7cde1`（vision 可选化，手工）— 依赖：无硬依赖，但与 10 同文件，建议相邻做。
10. `5b6672cc`（reasoning 能力协商）— 依赖：`8ab7cde1` 的 vision 形态由本提交最终确定（`capabilities.is_some_and`），若两提交连续手工，vision 可一步到位取最终形态。
11. `87104fcf`（本地 reasoning）— 依赖：`5b6672cc` 的能力字段形态；并需 `koharu-ml/llm` 切片同取。
12. `f842172e`（`GenerationConfig.thinking` → `reasoning: Option<bool>` 全量改名）— 依赖：须在 10、11 之后（上游顺序即如此），改名波及 `koharu-pipeline` 调用点，单独一次提交完成改名。
13. `3da33914` + 14. `f53631b4`（gemini thinking 解码与映射）— 依赖：`f842172e` 的 `reasoning` 字段与 `ListedModel.thinking` 形态；两提交同文件，成组手工。

**批次 D — 修复与结构（手工，次序关键）**

15. `efb28e30`（prompt 强化 + `indoc` manifest 人工审定）— 与 16 同碰 `prompt.rs`，必须先于 16。
16. `e6d330c8`（JSON 修复层重构，删除 `json.rs`）— 依赖：`efb28e30` 的 `prompt.rs` 形态；吸收 `4ccad2d3` 意图（见第 5 节）。**先于** `bc1a51b6`（后者碰 `lib.rs` 的 `mod` 区域上下文）。
17. `bc1a51b6`（models 客户端缓存）。
18. `87442b49` 的 manifest 切片（删 `toml` dev-dep）—— 随 15 的 manifest 审定一并做，不单独排队。

**批次 E — 依赖运行时下载层**

19. `d8a68ba2`（`downloaded` 状态）— 依赖：`HuggingFaceFile::exists()` 与 pinned 形态（`dbed34d8`、运行时轮的 `7834852e`）。可与运行时轮并行排期，translator 侧等 runtime API 就绪再落。

**批次 F — 待裁决**

20. `9e8698be`（Atlas Cloud 移除）— 裁决后执行；若移除，须回看 `e6d330c8`/`08601a8f`/`5b6672cc` 中对 `atlas_cloud.rs` 的改动是否保留。

**不进入队列**：`6269da2c`、`1d168ff4`（净零）、`66e603d4`、`5f87445e`、`ac2172b4`（metrics 范围排除）、`69e67f96`（跳过-GUI）。
`bf516a01` 无严格顺序，任意批次顺带。

## 3. 手工移植提交：文件白名单与分歧点

**87442b49**
- 白名单：`crates/koharu-translator/Cargo.toml`。
- 分歧点：workspace `Cargo.toml` 依赖表布局与 fork 漂移（fork 无 `indoc`、无 `tokio-rayon`）；只删 translator 的 `[dev-dependencies] toml` 一项（已核实 `crates/koharu-translator` 源码无 `toml` 引用），不取 workspace 重构与 `Cargo.lock`。

**8ab7cde1**
- 白名单：`crates/koharu-translator/src/remote/lm_studio.rs`。
- 分歧点：上游本提交把 vision 判定写成 `model.capabilities.vision`（假定字段必返）；上游后续 `5b6672cc` 已改为 `capabilities.is_some_and(|capabilities| capabilities.vision)`。fork 现状为 `vision: model.kind == "vlm"`。移植按可选字段解析、缺失取 false，不落中间形态。fixture 覆盖三种输入：`capabilities` 里 `vision` 为 true、为 false、整段缺失。

**5b6672cc**
- 白名单：`crates/koharu-translator/src/model.rs`、`src/lib.rs`、`src/bin/translate.rs`、`src/local/mod.rs`、`src/remote/{claude,deepseek,gemini,grok,lm_studio,minimax,openai,openai_compatible,openrouter}.rs`、（若保留 Atlas Cloud）`src/remote/atlas_cloud.rs`、`crates/koharu-pipeline/src/stages/translation.rs`、`crates/koharu-pipeline/src/tests.rs`。
- 排除文件：`packages/bridge/src/protocol.ts`、`packages/koharu/components/**`、`packages/koharu/lib/translation.ts`、`packages/koharu/public/locales/**`、`packages/koharu/tests/**`。
- 分歧点：(1) fork 的 `GenerationConfig` 仍为 `thinking: bool`，本提交起能力字段向 `reasoning` 形态演进（重命名在 `f842172e` 收敛），合并时以最终形态落；(2) `src/remote/openai_compatible.rs` 与 fork 独有提交 `4ccad2d3`（`compatible()` 显式下发 `backend.thinking`）相交，合并后保留「关闭 reasoning 时显式下发禁用值」的行为；(3) `src/lib.rs` 与 fork 独有提交 `f6bee459`（`Translator::release()`）相交，保留该方法与测试。

**87104fcf**
- 白名单：`crates/koharu-translator/src/local/catalog.rs`、`src/local/mod.rs`、`src/model.rs`、`src/remote/openai_compatible.rs`、`crates/koharu-ml/src/llm/mod.rs`、`crates/koharu-ml/src/llm/model.rs`。
- 排除文件：`packages/bridge/src/protocol.ts`、`packages/koharu/**`、`packages/koharu/tests/**`、各 locales。
- 分歧点：(1) `openai_compatible.rs` 删除 `OpenAiCompatibleConfig.vision` 配置字段、`models()` 改为恒定 `vision: true, reasoning: true`，与 `4ccad2d3` 同文件相交；(2) `koharu-ml/src/llm` 切片与 fork 的 CPU-only（`4276ec48`）/iGPU（`42624633`）改动可能相交，执行前用 `git diff` 核对该目录的 fork 私有改动，确保 CPU-only 路径不被破坏（本次审计未逐行核对 `koharu-ml` 的 fork 差异，列为执行前检查点）。

**f842172e**
- 白名单：`crates/koharu-translator/src/model.rs`、`src/lib.rs`、`src/bin/translate.rs`、`src/local/mod.rs`、`src/remote/{atlas_cloud,claude,deepseek,gemini,grok,lm_studio,minimax,openai,openai_compatible,openrouter}.rs`（按届时文件集合）、`crates/koharu-pipeline/src/bin/run.rs`、`crates/koharu-pipeline/src/tests.rs`。
- 排除文件：`packages/bridge/src/protocol.ts`、`packages/koharu/**`、`packages/koharu/tests/**`。
- 分歧点：核心为 `GenerationConfig.thinking: bool` → `reasoning: Option<bool>`。fork 侧引用点至少包括 `crates/koharu-pipeline/src/config.rs`、`crates/koharu-pipeline/src/bin/run.rs`、fork 私有的 `src/remote/openai_compatible.rs`（`4ccad2d3` 读 `generation.thinking`）；按 AGENTS.md「不留向后兼容」要求全量改名，不保留 `thinking` 别名。

**3da33914 + f53631b4**（成组）
- 白名单：`crates/koharu-translator/src/remote/gemini.rs`。
- 分歧点：fork 的 `gemini.rs` 中 `ThinkingConfig` 只有 `thinking_budget: i32`，且 `thinking_config` 仅对 `gemini-2.5-flash` 发出；模型列表结构体尚无 `thinking` 字段（`3da33914` 的 `#[serde(default)]` 附着点）。上游最终形态：`ListedModel.thinking` 可选解码（缺省 false），`ThinkingConfig` 为 `Option<thinking_budget>` + `Option<thinking_level>`，按 `gemini-2.5*`（budget：-1/128/0，2.5 Pro 最小 128）与 `gemma-4`（`thinking_level: "minimal"`）等模型名分支映射。两提交按最终形态一次落入，测试 `reads_optional_thinking_capability_from_model_list` 与 `serializes_model_specific_thinking_controls` 一并移植。

**efb28e30**
- 白名单：`crates/koharu-translator/src/prompt.rs`（Rust prompt 实现，直接取）；`crates/koharu-translator/Cargo.toml` 与 workspace `Cargo.toml` 中 `indoc = "2.0.7"` / `indoc = { workspace = true }` 两行（人工审定后加入）。
- 分歧点：fork 现无 `indoc` 依赖；`Cargo.lock` 禁止手改，由 cargo 重新生成（计划硬约束）。manifest 与 fork 依赖布局漂移，只加依赖行，不带上游 workspace 重构。

**e6d330c8**
- 白名单：`crates/koharu-translator/src/prompt.rs`、`src/lib.rs`、`src/json.rs`（删除）、`src/remote/{atlas_cloud,claude,deepseek,lm_studio,minimax,mod,openai,openai_compatible,openrouter}.rs`（全部在 crate 内，无排除文件）。
- 分歧点：(1) 删除 `src/json.rs`（390 行）与 `lib.rs` 的 `mod json`，JSON 修复逻辑并入 `prompt.rs` 与各 provider 的响应恢复路径；fork 的 `json.rs` 未被 fork 私有改动触及，可整文件删除；(2) `openai_compatible.rs` 彻底重写：移除 `ChatBackend`/`ResponseMode` 抽象，改为直接构造 `ChatRequest` 并带 `reasoning_effort: generation.reasoning.map(|enabled| if enabled { "medium" } else { "none" })`。该形态在 reasoning 关闭时显式下发 `"none"`，已覆盖 fork 独有提交 `4ccad2d3` 的意图，合并时以 `e6d330c8` 形态为准，在 `compatible()` 旁注释说明分歧被上游形态吸收；(3) `atlas_cloud.rs` 的 217 行增改是否保留，取决于 `9e8698be` 的 Atlas Cloud 裁决——若裁决移除该 provider，此文件的增改随之作废。

**d8a68ba2**
- 白名单：`crates/koharu-translator/src/local/catalog.rs`、`src/local/mod.rs`、`src/model.rs`、`crates/koharu-runtime/src/source/hugging_face.rs`（`HuggingFaceFile::exists()` 及 pinned 调用形态）。
- 排除文件：`packages/bridge/src/protocol.ts`、`packages/koharu/**`（`ModelPicker.tsx`、`InferenceControl.tsx`、`TranslationPreferences.tsx`、`lib/translation.ts`）。
- 分歧点：translator 侧为 `Quantization` 增加 `downloaded: bool`、`LocalModelDescriptor::file()` 辅助与 `models()` 汇总判定，机械改动；runtime 侧上游 diff 重排了 `hugging_face.rs` 的 `Repository` 结构（64 行删除），fork 的该文件已有本地演进（`Store`/`download`/`network` 接线），预计不能整文件套用，只补 `exists()` 与 `file()` 所需 API。

## 4. 最小验证与测试补充

最小验证命令（移植批次每个提交后执行一次）：

```
cargo check -p koharu --no-default-features --features headless
```

针对行为变更需补的最小测试或 fixture（按提交）：

| 提交 | 测试 / fixture |
|---|---|
| 8ab7cde1 | `lm_studio` 模型发现测试改写：JSON fixture 三态覆盖 `capabilities.vision` 为 true、为 false、字段缺失；断言缺失时 `vision == false`。 |
| 5b6672cc | 各 provider 的能力协商单测（上游 `claude`/`openrouter`/`lm_studio` 测试随切片取）；`GenerationConfig` 改名后以编译期全覆盖为准，运行 `cargo test -p koharu-translator`。 |
| 87104fcf | `models()` 对 reasoning 字段的断言（随上游测试取）；若 fork 有本地模型配置测试，补一条「本地模型 reasoning 开关传入 `enable_thinking`」。 |
| f842172e | 上游 `lm_studio`/`openai_compatible` 侧 `reasoning_effort` 序列化测试随切片取（如 `reasoning_effort` 为 `"none"` 的断言）。 |
| 3da33914 | 移植上游 `reads_optional_thinking_capability_from_model_list`（两个条目：`thinking: true` 与字段缺失）。 |
| f53631b4 | 移植上游 `serializes_model_specific_thinking_controls`（`gemini-2.5-pro` 最小 budget 128、`gemma-4` → `thinking_level: "minimal"`、None 时不序列化 `thinkingConfig`）。 |
| e47620c0 | 更新 `lm_studio` 输入项序列化断言为 `{"type":"text","content":...}`。 |
| efb28e30 | 无独立行为面；跑 `cargo test -p koharu-translator` 确认 prompt 单测通过。 |
| e6d330c8 | 移植上游 `prompt.rs` 的扁平化测试（换行/制表符/控制字符转义、超长片段截断）；补一条「JSON 修复层此前拒绝、现可恢复」的响应解析用例。 |
| c17d7a07 / 9962e14c / e4c3d069 | 可选：catalog 完整性测试（id 无重复、`DEFAULT_MODEL` 可解析）。9962e14c 应用后断言 Qwen 3.8 27B 条目参数为修正值。 |
| dbed34d8 | 可选：断言每条目 `revision` 为 40 位十六进制。 |
| d8a68ba2 | `models()` 下载状态测试：临时缓存目录 fixture，断言文件齐全时 `downloaded == true`、缺 projector 时为 false。 |
| bc1a51b6 | 行为面为客户端缓存与 5 秒超时，最小验证以现有测试通过为准；如需覆盖，用慢速 mock endpoint 断言二次调用不重建客户端（可选）。 |
| 800e32bb / 08601a8f / 412822d2 | 随 `412822d2` 的目录发现测试取（上游后续提交带 mock 列表测试）；Grok/MiniMax 的 `models()` 映射断言。 |

## 5. 冲突面：`crates/koharu-translator/src/remote/openai_compatible.rs`

任务书称该文件在主工作树有未提交修改。审计期间该修改已提交为 fork 独有提交 `4ccad2d3`（"fix(translator): send explicit thinking setting to compatible endpoints"，正文无 `Upstream:` 行），当前 `git diff` 对该文件为空。

**本移植清单是否触及该文件：是。** 触及的上游提交共 6 个（净效果 4 个）：

1. `e6d330c8` — 重写约 246 行，移除 `ChatBackend`/`ResponseMode`，改直构 `ChatRequest`（相交最深）。
2. `5b6672cc` — reasoning 能力协商，34 行。
3. `87104fcf` — 删除 `OpenAiCompatibleConfig.vision`，`models()` 判定改写。
4. `f842172e` — `generation.thinking` → `generation.reasoning` 引用改名。
5. `bf516a01` — 1 行注释链接（低风险）。
6. `6269da2c` / `1d168ff4` — 净零对消，忽略。

相交点说明：

- `4ccad2d3` 修改的函数是 `compatible()`：在构造 `ChatBackend` 后设置 `backend.thinking = Some("enabled")` 或 `Some("disabled")`，使「reasoning 关闭」被显式下发而非省略（兼容端点在省略时会自行开启 reasoning）。
- `e6d330c8` 的最终形态把 `compatible()`/`ChatBackend` 整体替换，并用 `reasoning_effort: generation.reasoning.map(|enabled| if enabled { "medium" } else { "none" })` 在关闭时显式下发 `"none"`。行为意图与 `4ccad2d3` 一致（关闭时显式禁用），因此移植 `e6d330c8` 时以 `e4c3d069` 之后上游形态为准，把 `4ccad2d3` 的分歧在 `compatible()` 旧位置（或新 `translate()` 函数旁）注释记录「fork 的显式 thinking 下发已被上游 `reasoning_effort` 显式映射吸收」，不再单独保留 `backend.thinking` 字段。
- 需人工确认的一处语义差：`4ccad2d3` 发送 `"enabled"`/`"disabled"`（`thinking` 字段），上游最终发送 `reasoning_effort: "medium"`/`"none"`。若 fork 部署的兼容端点（LM Studio、Ollama 等）只认 `thinking` 不认 `reasoning_effort`，需在移植后用真实端点验证关闭 reasoning 时不再被自动开启；验证不通过则在 `ChatRequest` 中补 `thinking` 字段并在代码旁注明分歧。此项列为执行时验证点，不阻塞清单其余部分。
- `bf516a01` 与 `5b6672cc`/`87104fcf`/`f842172e` 的冲突均为行级上下文冲突，按批次 C、D 顺序手工合并即可。

## 6. 任务硬约束逐条核对

1. `efb28e30` 只取 Rust prompt 实现；manifest 人工审定；`Cargo.lock` 由 cargo 重新生成、禁止手改 —— 清单第 3 节已按此拆分白名单，未违反。
2. `8ab7cde1` 按可选字段解析、缺失取 false，fixture 覆盖三态 —— 清单状态为手工移植，测试要求三态，未违反。
3. `c17d7a07` 生成参数以 `9962e14c` 为准 —— 推荐顺序 `c17d7a07` 先于 `9962e14c`，备注已标明不取过期值，未违反。
4. `08601a8f` 与 `412822d2` 按最终形态合并处理 —— 推荐顺序 `08601a8f` → `800e32bb` → `412822d2` 完整按序应用，未违反。
5. 范围排除（`koharu-app`、`koharu-desktop`、`koharu-canvas`、`packages/*`、`koharu-metrics`、CI 与发布文件）—— 12 个混合提交的排除文件已在第 1 节逐条写明，未违反。
