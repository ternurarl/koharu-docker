# 上游同步审计：推理运行时（koharu-runtime 及 torch/llama/diffusion 绑定）

日期：2026-10-15。审计对象：fork `D:\Github\koharu-docker`（`main` = `a23301f5`）与上游 remote-tracking ref `upstream/main`（`52340cbf`）之间的待同步提交，范围限定 `crates/koharu-runtime`、`crates/koharu-torch`、`crates/koharu-torch-sys`、`crates/koharu-llama`、`crates/koharu-llama-sys`、`crates/koharu-diffusion`、`crates/koharu-diffusion-sys`，外加 `plan/UPSTREAM_SYNC_PLAN.md` 点名的 `ce2e6fbe`、`874ed98c`（只改 `crates/koharu-ml`，但属于 ROCm/MIOpen 精度与加载链）。merge-base = `c79882d9`。本文件只给移植清单，不实施移植。

判定基线（已核实）：fork `main` 上 `crates/koharu-runtime/src/runtime/packages/` 与上游 `6959411c` 逐字节一致（`git diff 6959411c main -- crates/koharu-runtime/src/runtime/packages/` 为空）；fork 自 merge-base 起对本审计范围的自有改动只有 `4276ec48`（CPU-only 模式）与 `42624633`（iGPU 模式），二者只改 `crates/koharu-ml/src/lib.rs`、`crates/koharu-rpc/src/server.rs`、`Dockerfile`、`HEADLESS.md`、`crates/koharu/src/main.rs`，不碰 runtime packages。

状态词表：已移植（注明 fork 提交号）／可 cherry-pick／手工移植／跳过-GUI／暂缓（注明原因）。

---

## 1. 逐提交表格

体量列格式为「文件数 +增/−删」，只统计 `git show --stat` 的全量数字；「排除」列写该提交中不属 headless 范围、应用时必须丢弃的路径（对应 `plan/UPSTREAM_SYNC_PLAN.md` 的范围排除：`koharu-app`、`koharu-desktop`、`koharu-canvas`、`packages/*`、`koharu-metrics`、CI 与发布文件）。

| 上游 SHA | 标题 | 状态 | 涉及文件（headless 部分加粗；排除见备注） | 体量 | 备注 |
|---|---|---|---|---|---|
| `6959411c` | feat: update diffusion and ggml FFI bindings | 已移植（fork `03106e91`） | 19 文件全量已进 fork，含 `crates/koharu-diffusion-sys/`、`crates/koharu-llama-sys/include/`、`crates/koharu-diffusion/src/{enums,params}.rs`、`crates/koharu-llama/src/{llguidance_sampler,model/params,sampling}.rs`、`crates/koharu-ml/src/{flux2_klein,llm}/`、`crates/koharu-runtime/src/runtime/packages/{diffusion,llama}.rs` | 19 +523/−88 | C ABI、Rust 包装、原生包版本钉三者绑定。残留差异见第 5 节 |
| `87442b49` | chore(deps): modernize workspace dependency configuration | 手工移植 | **`Cargo.toml`（workspace 依赖表）**、`crates/koharu-{torch-sys,llama-sys,diffusion-sys,torch,ml,pipeline,psd,renderer,translator}/Cargo.toml`、`crates/koharu-torch/src/tensor/npy.rs` | 16 +81/−223 | 排除 `crates/koharu-{app,canvas,desktop}/Cargo.toml`、`crates/koharu/Cargo.toml`。sys crate 侧实际改动只有 `cargo-machete` 的 `ignored` 列表，风险低 |
| `e8422a29` | chore(deps): update Rust dependencies | 手工移植 | **`Cargo.toml`/`Cargo.lock`（与 runtime 相关条目）**、`crates/koharu-runtime/src/hardware/{cuda,hip}.rs`、`crates/koharu-torch/src/tensor/safetensors.rs`、`crates/koharu-torch/src/vision/dataset.rs` | 8 +327/−311 | 排除 `crates/koharu/src/sentry.rs`（遥测）、`crates/koharu-bindgen/src/rewrite.rs`（若 bindgen 版本同步升级再一并取）。`hardware/hip.rs` 的改动是依赖 API 适配 |
| `4d9f3aca` | fix(runtime): disable broken gfx1032 Winograd solver | 暂缓 | （仅 `crates/koharu-runtime/src/runtime/packages/rocm.rs`） | 1 +10 | 只在 Windows gfx1032 是部署目标时才有意义。新事实：上游 `8818911f` 随后删除了本提交加入的整段环境变量代码，见第 3 节与第 5 节裁决点 |
| `8199a8b2` | fix(runtime): disable broken gfx1032 inpainting solver | 暂缓 | （仅 `crates/koharu-runtime/src/runtime/packages/rocm.rs`） | 1 +4/−3 | 该 diff 修改的代码块由 `4d9f3aca` 创建，`4d9f3aca` 不取时本提交无法干净应用。与 `4d9f3aca` 同进退。`plan/UPSTREAM_SYNC_PLAN.md` 把本提交列入 ROCm 整链，但其内容同样被 `8818911f` 删除 |
| `e47620c0` | perf(translator): optimize structured inference | 可 cherry-pick | **`crates/koharu-llama/src/{llguidance_sampler,model,sampling}.rs`**、**`crates/koharu-ml/src/llm/model.rs`**、`crates/koharu-translator/src/remote/lm_studio.rs` | 5 +42/−31 | 与 P0 翻译层审计范围重叠（`lm_studio.rs`）。两轮移植只做一次，建议归翻译层那轮 |
| `7462036c` | feat(runtime): support ROCm Torch on Linux | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +50/−35 | ROCm 加载链起点，与后续加载顺序提交构成一条线性依赖链，按序整链取 |
| `b608a9f7` | perf(runtime): maximize download throughput | 可 cherry-pick | **`crates/koharu-runtime/src/downloads.rs`**、**`crates/koharu-runtime/src/network.rs`** | 2 +165/−124 | 下载链第 1 环。不得单独作为完整下载方案接受 |
| `cf914e8b` | feat(runtime): add Linux CUDA and ROCm artifacts | 可 cherry-pick | **`crates/koharu-llama-sys/include/ggml.h`**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama}.rs`** | 3 +55/−6 | ROCm 加载链第 2 环 |
| `22dab373` | perf(runtime): accelerate multipart downloads | 可 cherry-pick | **`crates/koharu-runtime/src/downloads.rs`**、`Cargo.toml`/`Cargo.lock`/`crates/koharu-runtime/Cargo.toml`（`num_cpus`） | 4 +25/−19 | 下载链第 2 环 |
| `01bc9705` | fix(runtime): load ROCm Torch dependencies in order | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +1/−1 | ROCm 加载链第 3 环 |
| `84ef3ee5` | fix(runtime): correct Linux cuDNN library manifest | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/cuda.rs`** | 1 +1/−1 | CUDA 清单修正 |
| `1143fe77` | fix(runtime): retry interrupted download ranges | 可 cherry-pick | **`crates/koharu-runtime/src/downloads.rs`**、`Cargo.toml` 等（`backon`） | 4 +119/−3 | 下载链第 3 环，也是该演进序列的最终形态：multipart 加速 + 断点区间重试 |
| `ce2e6fbe` | fix(ml): select precision by device capability | 可 cherry-pick | **`crates/koharu-ml/src/backend.rs`**（`set_precision`） | 1 +20/−14 | fork 未改过 `backend.rs`，`Hardware::device()`、`Device::compute_capability()`、`Device::target()` 等调用所需的 API 在 fork 树中已存在（与 `ce2e6fbe` 父提交同内容） |
| `fb74b752` | fix(runtime): isolate bundled DRM symbols on Linux | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`**、`Cargo.toml`（`libloading` 相关） | 3 +14 | ROCm/Torch 加载链第 4 环（符号隔离） |
| `0d206640` | fix(runtime): load CUDA Torch dependencies in order | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +1/−1 | CUDA 加载顺序 |
| `6c21c9ad` | fix(runtime): isolate bundled ROCm libraries on Linux | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +11/−7 | ROCm 加载链第 5 环 |
| `c6dab0c0` | fix(runtime): share ROCm runtime on Linux | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/{diffusion,llama,rocm,torch}.rs`** | 4 +120/−109 | ROCm 加载链第 6 环 |
| `b830b495` | fix(runtime): preserve ROCm wheel layout | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +58/−77 | ROCm 加载链第 7 环 |
| `556485b1` | fix(runtime): load Linux cuFile for Torch | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/{cuda,torch}.rs`** | 2 +12/−2 | CUDA 依赖加载 |
| `1dda0844` | fix(runtime): load ROCm libraries globally | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/loader.rs`**、**`.../packages/rocm.rs`** | 2 +16/−7 | ROCm 加载链第 8 环（终点）。链内提交同改 `torch.rs`／`rocm.rs`，不可拆开单取 |
| `6df4ded5` | chore(runtime): update native runtimes | 手工移植 | **`crates/koharu-diffusion-sys/include/stable-diffusion.h`**、**`crates/koharu-llama-sys/include/{mtmd,mtmd-helper}.h`**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama}.rs`** | 8 +67/−9 | 排除 `.agents/skills/runtime/`（工具链脚本）。版本钉 `stable-diffusion.cpp-master-820-de298c2 → master-827-97d2990`、`llama.cpp-b10472 → b10488` |
| `dc2c0442` | fix(runtime): reject unsupported CUDA architectures | 可 cherry-pick | **`crates/koharu-runtime/src/hardware/mod.rs`** | 1 +3/−1 | 加速器发现 |
| `c1fce14c` | fix(runtime): isolate system HIP discovery | 可 cherry-pick | **`crates/koharu-runtime/src/hardware/hip.rs`** | 1 +24/−4 | 加速器发现 |
| `e7f0cbce` | fix(runtime): improve accelerator detection | 手工移植 | **`crates/koharu-runtime/src/hardware/**`（7 文件）**、**`crates/koharu-runtime/src/runtime/**`（10 文件）**、`crates/koharu-ml/src/*/mod.rs`（约 20 个模型目录的 `model_repository!` 宏路径修正） | 39 +792/−423 | 分歧点：`crates/koharu-ml/src/lib.rs` 被 fork 的 `4276ec48`/`42624633` 加入 `init_cpu()`/`init_gpu()`，与本提交对该文件的修改冲突，须手工合并。宏路径修正（`model_repository!` → `crate::model_repository!`）可整批替换 |
| `7834852e` | refactor(runtime): simplify downloads | 手工移植（建议排在下载链验证通过之后单独一轮） | **`crates/koharu-runtime/src/download.rs`（新建，+440）**、**`crates/koharu-runtime/src/downloads.rs`（删除，−448）**、**`crates/koharu-runtime/src/network.rs`**、**`crates/koharu-runtime/src/source/{hugging_face,pypi}.rs`**、**`crates/koharu-runtime/src/runtime/packages/*.rs`**、`Cargo.toml` | 15 +1213/−792 | 排除 `crates/koharu-app/src/app.rs`。本提交把 Hugging Face 客户端从 fork 现有的手写实现（`crates/koharu-runtime/src/source/hugging_face.rs` 268 行，走 `downloads::Transfer`）改写为 `hf-hub` crate（引入 `hf-hub = "=1.0.0"`）。计划点名的下载演进序列止于 `1143fe77`，本提交是其后的重构，取否见第 5 节裁决点 |
| `bcefc521` | fix: resolve clippy warnings | 手工移植 | **`crates/koharu-runtime/src/hardware/cuda.rs`**、`crates/koharu-ml/src/{comic_onomatopoeia,pp_ocr_v6}/detector/processor.rs` | 6 +20/−14 | 排除 `crates/koharu-rasterizer/src/{compositor,prepared}.rs` 与 `crates/koharu-renderer/src/layout.rs`（fork 在 renderer 有 CPU 导出 workaround `de9aae2d`，clippy 修正须逐处对照，不属本轮 runtime 范围） |
| `d3ce865b` | chore(runtime): update llama.cpp | 可 cherry-pick | **`crates/koharu-llama-sys/include/{ggml,llama,mtmd,mtmd-helper}.h`**、**`crates/koharu-runtime/src/runtime/packages/llama.rs`** | 5 +16/−3 | 版本钉 `llama.cpp-b10488 → b10603` |
| `12da3d94` | fix(ci): support PyTorch 2.13 Torch builds | 手工移植 | **`crates/koharu-torch-sys/libtch/torch_api_generated.{cpp,h}`**、**`crates/koharu-torch/src/wrappers/tensor{,_fallible}_generated.rs`** | 7 +86/−86（全量 7 +0/−86 视生成文件） | 排除 `.github/libtch/`、`.github/workflows/torch.yml`。生成文件适配 PyTorch 2.13 API。若采纳 `b5cfaba9` 的预编译方案，`libtch/*.cpp` 部分被吸收，Rust 侧生成文件仍须同步 |
| `ba313b47` | fix(torch): disambiguate module handle in C++20 | 手工移植 | **`crates/koharu-torch-sys/libtch/torch_api.cpp`** | 2 +10/−10 | 排除 `.github/libtch/torch_api.cpp`（同一补丁的 CI 镜像）。若 fork 保留源码编译 shim 则必须取；若走 `b5cfaba9` 预编译方案则该 C++ 修正由上游 CI 产物承担 |
| `9f73d2c0` | fix(torch): restore stream serialization | 手工移植 | **`crates/koharu-torch-sys/{build.rs,libtch/torch_api.{cpp,h},libtch/torch_api_generated.h,src/{io,lib}.rs}`**、**`crates/koharu-torch/src/wrappers/{mod,stream,tensor}.rs`** | 13 +607/−26 | 排除 `.github/libtch/`。Rust 流序列化包装是功能本体，必须取 |
| `bf930058` | test(torch): align serialization tests with upstream | 可 cherry-pick | **`crates/koharu-torch/src/wrappers/tensor.rs`**、**`crates/koharu-torch/tests/{serialization_tests,test_utils}.rs`** | 3 +245/−26 | |
| `dbed34d8` | refactor(runtime): require pinned Hugging Face revisions | 手工移植 | **`crates/koharu-runtime/src/source/hugging_face.rs`**、`crates/koharu-translator/src/local/catalog.rs` | 2 +43/−64 | 本提交修改的 `hugging_face.rs` 上下文基于 `7834852e` 之后的 `hf-hub` 实现，fork 当前是手写客户端，不能直接套用。`catalog.rs` 属翻译层审计范围。若不取 `7834852e`，本提交的语义（无钉死修订则拒绝下载）须以 fork 手写客户端为基线重写 |
| `38fc352f` | test(torch): remove runtime-dependent serialization tests | 可 cherry-pick | **`crates/koharu-torch/tests/serialization_tests.rs`**、**`crates/koharu-torch/tests/test_utils.rs`** | 2 −245 | |
| `8818911f` | fix(ml): disable MIOpen on Windows gfx10 | 手工移植 | **`crates/koharu-ml/src/backend.rs`**、**`crates/koharu-torch/src/nn/batch_norm.rs`**；`crates/koharu-runtime/src/runtime/packages/rocm.rs` 的删除块视情况 | 3 +14/−17 | 该提交的 `rocm.rs` hunk 删除 `4d9f3aca`/`8199a8b2` 加入的 gfx1032 Winograd 环境变量块；`4d9f3aca`/`8199a8b2` 暂缓时此 hunk 无对应内容，只取 `backend.rs` 与 `batch_norm.rs` |
| `874ed98c` | fix: disable MIOpen on Windows | 可 cherry-pick | **`crates/koharu-ml/src/backend.rs`** | 1 +7/−7 | 计划未点名。把 `8818911f` 的 gfx10 专属 MIOpen 关闭推广到 Windows 全部 ROCm 设备，是 MIOpen 处理的最终形态，须排在 `8818911f` 之后 |
| `b5cfaba9` | refactor(runtime): use bundled native releases | 手工移植（本组核心） | **`crates/koharu-torch-sys/{Cargo.toml,build.rs,src/lib.rs,libtch/**}`**、**`crates/koharu-runtime/src/runtime/packages/{cuda,diffusion,llama,rocm,torch}.rs`**、`Cargo.toml`/`Cargo.lock` | 24 +85/−32740 | 排除 `.github/workflows/build.yml`、`crates/koharu/{build.rs,tauri.*.conf.json,Cargo.toml}`、`crates/koharu-torch/README.md`。本提交把原生库来源从源码编译 shim（fork 现状：`libtch/torch_api.cpp` 经 CMake 编出 `koharu-torch.dll`）改为下载预编译产物（`koharu-org/{torch,llama,diffusion}` 发布仓库），`libtch` 目录只剩 `torch_api.h` 与 `torch_api_generated.h` 头文件。−32740 行主要是删除 vendored 的 `stb_image*.h` 与 shim 源码 |
| `922f0f48` | refactor(runtime): simplify Torch package variants | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +24/−46 | |
| `10c440aa` | fix(runtime): restore macOS Torch load order | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +3/−1 | macOS 修复；Docker 镜像用不到，但同一文件的小改动，取了无害 |
| `559aab7b` | fix(runtime): use koharu-rs release repositories | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/{diffusion,llama,torch}.rs`** | 3 +3/−3 | 下载域名 `koharu-org/* → koharu-rs/*`。必须排在 `b5cfaba9` 之后（`koharu-org/*` 域名由 `b5cfaba9` 引入） |
| `2f308fa2` | fix(runtime): update Torch release | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +8/−8 | 版本钉 `v2.13.0 → v2.13.0.2` |
| `fd0ca01e` | fix(runtime): update Torch to v2.13.0.3 | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +1/−1 | |
| `0147de13` | fix(runtime): update ROCm to 10.0 | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +31/−41 | ROCm `VERSION 7.14.0 → 10.0.0`，索引换到 `stable.repo.amd.com/rocm/core/whl-next` |
| `f282f225` | runtime: support every ROCm 10 device target | 可 cherry-pick | **`crates/koharu-ml/src/backend.rs`**、**`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 2 +33/−1 | |
| `01ba67a6` | fix(ml): pass ggml Metal backend name MTL0 to FLUX.2 | 可 cherry-pick | **`crates/koharu-runtime/src/device.rs`**、**`crates/koharu-ml/src/{flux2_klein,rorem_mixed}/model.rs`** | 3 +9/−33 | Metal 后端名修正，Docker GPU 场景用不到 Metal，但 `device.rs` 的后缀解析对全部后端生效，取了无害 |
| `ccfdf340` | runtime: limit ROCm devices to official ROCm 10 | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 −20 | |
| `1c736f70` | fix(runtime): update native dependencies | 可 cherry-pick | **`crates/koharu-llama-sys/include/{ggml,llama,mtmd-helper}.h`**、**`crates/koharu-llama/src/mtmd.rs`**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama,torch}.rs`** | 7 +62/−28 | 版本钉 `master-827-97d2990 → master-829-0a565f2.9`、`llama.cpp → b10690`、Torch `v2.13.0.3 → v2.13.0.4`。头文件与 Rust 包装同改 |
| `5b4b9b8e` | feat(llama): sync utilityai wrapper changes | 可 cherry-pick | **`crates/koharu-llama/src/{context,context/session,lib,llguidance_sampler,model,model/params,sampling}.rs`**、**`crates/koharu-ml/src/llm/model.rs`**、`crates/koharu-ml/Cargo.toml`、`Cargo.lock` | 10 +299/−85 | llama 包装层对齐 utilityai 上游 |
| `19385665` | feat(runtime): support all ROCm 10 targets | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +59/−9 | |
| `9152946a` | refactor(runtime): inline ROCm device library check | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +24/−21 | |
| `eb063470` | fix(runtime): use exact ROCm library paths | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +17/−19 | |
| `64173abb` | fix(runtime): drop unsupported gfx1250 | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +1/−8 | |
| `c3e79e68` | test(runtime): remove gfx1250 reference | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 −1 | 与 `64173abb` 成对 |
| `eadb7976` | fix(runtime): align ROCm targets with llama.cpp | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/rocm.rs`** | 1 +17/−2 | |
| `892db6bf` | feat(runtime): support Linux ARM64 CUDA | 可 cherry-pick | **`crates/koharu-diffusion-sys/include/stable-diffusion.h`**、**`crates/koharu-diffusion/src/enums.rs`**、**`crates/koharu-llama-sys/include/{ggml,ggml-backend}.h`**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama,torch}.rs`**、**`crates/koharu-runtime/src/source/pypi.rs`** | 8 +106/−30 | 头文件与版本绑定同改，属 `6959411c` 原则覆盖对象 |
| `e460a9bf` | fix(runtime): skip unsupported Linux ROCm targets | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/{diffusion,llama}.rs`** | 2 +2/−2 | |
| `7a6884a6` | fix(runtime): update Torch to v2.13.0.6 | 可 cherry-pick | **`crates/koharu-runtime/src/runtime/packages/torch.rs`** | 1 +1/−1 | |
| `087b40aa` | refactor(runtime): simplify accelerator discovery | 可 cherry-pick | **`crates/koharu-runtime/src/hardware/**`（5 文件）**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama,rocm,torch}.rs`**、`Cargo.toml`/`Cargo.lock`/`crates/koharu-runtime/Cargo.toml` | 12 +286/−486 | 加速器发现简化（计划点名项）。`hardware/hip/targets.rs` 净减约 200 行 |
| `ac5e9c24` | chore(deps): update Cargo, Bun, and native runtimes | 手工移植 | **`Cargo.toml`/`Cargo.lock`**、**`crates/koharu-diffusion-sys/include/stable-diffusion.h`**、**`crates/koharu-diffusion/src/{callbacks,enums,params}.rs`**、**`crates/koharu-llama-sys/include/{ggml,ggml-backend,mtmd,mtmd-helper}.h`**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama}.rs`**、**`crates/koharu-torch/src/vision/dataset.rs`** | 20 +802/−865 | 排除 `package.json`、`bun.lock`、`packages/**`、`.agents/skills/runtime/scripts/sync.sh`。版本钉 `master-841-6b3edaa.2 → master-849-d04e895.1`、`b10752 → b10872` |
| `67d507b4` | feat(runtime): update native runtimes and CUDA 13.3 | 手工移植 | **`crates/koharu-llama-sys/include/llama.h`**、**`crates/koharu-runtime/src/hardware/cuda.rs`**（`MIN_DRIVER_VERSION 13000 → 13030`）、**`crates/koharu-runtime/src/runtime/{graph.rs,packages/{cuda,diffusion,llama,torch}.rs}`** | 11 +67/−56 | 排除 `README.md`、`packages/docs/**`。CUDA 13.3（`nvidia-cuda-runtime/13.3.29` 等）+ Torch `v2.13.0.7`。计划点名项 |
| `d8a68ba2` | feat(ui): show download state for local translation models | 跳过-GUI | （headless 残余：`crates/koharu-runtime/src/source/hugging_face.rs`、`crates/koharu-translator/src/local/{catalog,mod}.rs`、`crates/koharu-translator/src/model.rs`） | 9 +181/−195 | 主体是 `packages/bridge` 与 `packages/koharu` 的界面改动。其 `hugging_face.rs` 改动基于 `7834852e` 之后的 `hf-hub` 实现（引入 `HFRepository`），fork 现状下无法单独应用；若将来取 `7834852e`，把其 `hugging_face.rs`／`catalog.rs` 部分并入那次手工移植 |
| `85557a25` | chore(deps): track hf-hub main branch | 暂缓 | （`Cargo.toml`、`Cargo.lock`、`crates/koharu-runtime/src/source/hugging_face.rs`） | 3 +5/−6 | 把 `hf-hub = "=1.0.0"` 改为跟随 `https://github.com/huggingface/hf-hub` 的 `branch = "main"`。与「HuggingFace 修订钉死」的可复现构建要求冲突；且依赖 `7834852e` 的 `hf-hub` 代码基线。裁决点见第 5 节 |
| `2762080e` | chore(runtime): update llama and diffusion releases | 可 cherry-pick | **`crates/koharu-diffusion-sys/include/stable-diffusion.h`**、**`crates/koharu-diffusion/src/{context,enums,params}.rs`**、**`crates/koharu-llama-sys/include/{ggml,llama,mtmd-helper}.h`**、**`crates/koharu-runtime/src/runtime/packages/{diffusion,llama}.rs`** | 9 +226/−29 | 版本钉终点：diffusion `master-929-3f8527a.3`、llama `b11317` |
| `57e9b65f` | fix(torch): replace deprecated float constants | 可 cherry-pick | **`crates/koharu-torch/src/wrappers/tensor_fallible_generated.rs`** | 1 +147/−147 | 纯机械替换 |
| `e2ea5e58` | chore: fix warning on macos | 可 cherry-pick | **`crates/koharu-runtime/src/hardware/hip/targets.rs`** | 1 +1 | |

统计（共 65 行：路径枚举 63 笔 + 计划外必要项 `ce2e6fbe`、`874ed98c`）：已移植 1；可 cherry-pick 46；手工移植 14；跳过-GUI 1；暂缓 3（`4d9f3aca`、`8199a8b2`、`85557a25`；其中 `8199a8b2` 与 `4d9f3aca` 同进退）。

---

## 2. 分组与推荐应用顺序

总体顺序按 `git rev-list --count <sha> ^c79882d9` 的拓扑距离排序（上游 `main` 为线性历史，该距离即应用顺序）。下列各组给出组内拓扑；组与组之间的推荐先后为：**FFI 残留组 → 下载链最终形态组 → 平台/工具链组 → 可复现构建组 → 加速器发现与精度组**，理由：FFI 头文件与版本钉是其余各组的编译基线；`b5cfaba9`（可复现构建组）会改写平台组提交触及的 `packages/*.rs`，因此平台组的版本钉类提交要排在它之后或与其最终形态对齐；加速器发现组（`e7f0cbce`、`087b40aa`）对 `hardware/**` 是整段重写，放在依赖稳定的最后。

### 组 0：FFI 残留组（`6959411c` 之后的绑定与版本钉演进）

组内拓扑（严格按序）：`6959411c`（已由 fork `03106e91` 完成）→ `d3ce865b` → `6df4ded5` → `1c736f70` → `ac5e9c24` → `67d507b4` → `2762080e`，中途穿插 `892db6bf`（ARM64 头文件，排在 `eadb7976` 之前、`1c736f70` 之后的平台组段）、`5b4b9b8e`（Rust 包装同步，排在 `1c736f70` 之后）。另一支是 Torch 生成绑定：`12da3d94` → `ba313b47` → `9f73d2c0` → `bf930058` → `38fc352f` → `57e9b65f`。

最终版本钉以 `2762080e` + `67d507b4` 为准：`koharu-rs/diffusion` 的 `master-929-3f8527a.3`、`koharu-rs/llama` 的 `b11317`、`koharu-rs/torch` 的 `v2.13.0.7`。中间版本钉提交可以顺序 cherry-pick（结构可追溯），但验证只对最终版本钉做一次。

### 组 1：下载链最终形态组

组内拓扑：`b608a9f7` → `22dab373` → `1143fe77`。三者依次修改 `crates/koharu-runtime/src/downloads.rs`，`1143fe77` 是最终形态（multipart 并行 + `backon` 区间重试）；`b608a9f7` 不得单独接受。`7834852e`（`downloads.rs` → `download.rs` 重写 + `hf-hub` 引入）是该序列之后的重构，列为可选的手工移植项，不并入本组。

### 组 2：平台/工具链组（CUDA／ROCm／ARM64）

组内拓扑（三条子链并行，同一子链内严格按序）：

- ROCm 运行时加载链（整链取，不拆开）：`7462036c` → `cf914e8b` → `01bc9705` → `fb74b752` → `6c21c9ad` → `c6dab0c0` → `b830b495` → `1dda0844`。
- ROCm 目标与版本链：`0147de13` → `f282f225` → `ccfdf340` → `19385665` → `9152946a` → `eb063470` → `64173abb` → `c3e79e68` → `eadb7976` → `e460a9bf`。计划点名的 `6c21c9ad`、`c6dab0c0`、`b830b495`、`1dda0844`、`cf914e8b`、`01bc9705`、`7462036c` 落在前一条子链，`8199a8b2` 落在暂缓区，此为核实后的完整拓扑。
- CUDA 子链：`dc2c0442` → `84ef3ee5` → `0d206640` → `556485b1` → `892db6bf` → `67d507b4`（CUDA 13.3）。
- MIOpen 子链（Windows）：`8818911f` → `874ed98c`（最终形态：Windows 上所有 ROCm 设备关闭 MIOpen）。
- macOS 小项：`10c440aa`、`e2ea5e58`，可随手带上。

### 组 3：可复现构建组

组内拓扑：`b5cfaba9`（bundled native releases，切换到预编译产物）→ `559aab7b`（发布仓库换到 `koharu-rs/*`）→ 版本钉提交（见组 0）。`dbed34d8`（HuggingFace 修订钉死）依赖 `7834852e` 的客户端基线，单独成项；`85557a25` 暂缓。

### 组 4：加速器发现与精度组

组内拓扑：`c1fce14c` → `ce2e6fbe` → `e7f0cbce` → `087b40aa`，设备后端小项 `01ba67a6`（ggml Metal 后端名）可随 `f282f225` 前后带上。`c1fce14c`、`dc2c0442` 是 `e7f0cbce` 的前置小修（`dc2c0442` 已列 CUDA 子链，任选一侧应用一次即可）。

---

## 3. 手工移植提交的文件白名单与分歧点

1. `e7f0cbce`（加速器检测改进）
   - 白名单：`crates/koharu-runtime/src/hardware/{mod,cuda,hip,hip/linux,hip/mod,hip/targets,hip/windows}.rs`、`crates/koharu-runtime/src/runtime/{mod,graph,loader}.rs`、`crates/koharu-runtime/src/runtime/packages/{cuda,diffusion,llama,rocm,torch}.rs`、`crates/koharu-runtime/Cargo.toml`、`crates/koharu-ml/src/*/mod.rs` 的宏路径行、`crates/koharu-ml/src/lib.rs`（只取与检测相关的行）。
   - 分歧点：fork `crates/koharu-ml/src/lib.rs` 含 `4276ec48` 的 `init_cpu()` 与 `42624633` 的 `init_gpu()`，上游该文件的对应改动是模型仓库宏与初始化顺序调整，合并时保留 `init_cpu()`/`init_gpu()` 的导出与调用约定（`crates/koharu-rpc/src/server.rs` 依赖这两个函数）。
2. `b5cfaba9`（预编译原生产物）
   - 白名单：`crates/koharu-torch-sys/{Cargo.toml,build.rs,src/lib.rs}`、`crates/koharu-torch-sys/libtch/`（按上游最终形态只留 `torch_api.h` 与 `torch_api_generated.h`，删除 `torch_api.cpp`、`torch_api_generated.cpp`、`CMakeLists.txt`、`stb_image*.h`）、`crates/koharu-runtime/src/runtime/packages/{cuda,diffusion,llama,rocm,torch}.rs`、`Cargo.toml` 与 `Cargo.lock` 中对应条目。
   - 分歧点一：fork 的 `crates/koharu-torch-sys/build.rs` 当前用 `cmake::Config` 编译 `libtch` 并产出 `koharu-torch.dll`／`libkoharu-torch.dylib`；上游最终形态改为下载 `koharu-rs/torch` 发布的预编译库。fork 的 `Dockerfile`（fork 私有文件）里凡是要么安装编译工具链要么拷贝 shim 产物的步骤，须同步改成下载与校验预编译产物。
   - 分歧点二：`koharu-org/*` 发布仓库域名由本提交引入，随后被 `559aab7b` 换成 `koharu-rs/*`，两笔一起落。
   - 分歧点三：排除清单里的 `crates/koharu/build.rs` 与 `tauri.*.conf.json` 不取。
3. `7834852e`（下载层重写，可选）
   - 白名单：`crates/koharu-runtime/src/download.rs`（新建）、`crates/koharu-runtime/src/downloads.rs`（删除）、`crates/koharu-runtime/src/network.rs`、`crates/koharu-runtime/src/lib.rs`、`crates/koharu-runtime/Cargo.toml`、`crates/koharu-runtime/src/source/{hugging_face,pypi}.rs`、`crates/koharu-runtime/src/runtime/packages/*.rs` 中调用点改名、`Cargo.toml`/`Cargo.lock` 的 `hf-hub = "=1.0.0"` 条目。
   - 分歧点：fork `crates/koharu-runtime/src/source/hugging_face.rs` 是手写 Hugging Face REST 客户端（268 行，自管 `REVISIONS` 表与 `downloads::Transfer`），上游该文件在本提交改写为 `hf-hub` crate。两套实现的重试、修订解析、进度语义不同，须以行为对照方式移植，不能只套 diff。排除 `crates/koharu-app/src/app.rs`。
4. `dbed34d8`（HuggingFace 修订钉死）
   - 白名单：`crates/koharu-runtime/src/source/hugging_face.rs`（按 fork 手写客户端重写「无钉死修订则拒绝」语义，或在取了 `7834852e` 后按上游 diff 应用）、`crates/koharu-translator/src/local/catalog.rs`（与 P0 翻译层审计共享）。
   - 分歧点：同上一条的客户端实现分歧；fork `Cargo.toml` 当前 `hf-hub = "0.5"` 且没有任何 `.rs` 文件引用它（未使用的工作区依赖），与上游的 `hf-hub 1.x` 代码无关。
5. `12da3d94`、`ba313b47`、`9f73d2c0`（Torch 生成绑定与 shim）
   - 白名单：`crates/koharu-torch-sys/libtch/*`、`crates/koharu-torch-sys/src/{io,lib}.rs`、`crates/koharu-torch/src/wrappers/*generated*.rs`、`crates/koharu-torch/src/wrappers/{mod,stream,tensor}.rs`。
   - 分歧点：三个提交都同时补丁 `.github/libtch/`（上游 CI 构建 shim 用的镜像副本），该目录属范围排除，不取。若采纳 `b5cfaba9`，`libtch/*.cpp` 与 `build.rs` 的 CMake 路径整体被删除，届时这三笔的 C++ 部分无落点，只保留 Rust 侧与头文件侧改动。
6. `6df4ded5`、`ac5e9c24`、`67d507b4`（原生版本钉 + 头文件）
   - 白名单：各 `*-sys/include/*.h`、`crates/koharu-diffusion/src/*.rs`、`crates/koharu-runtime/src/runtime/packages/{diffusion,llama,cuda,torch}.rs`、`crates/koharu-runtime/src/hardware/cuda.rs`、`crates/koharu-runtime/src/runtime/graph.rs`、`crates/koharu-torch/src/vision/dataset.rs`、`Cargo.toml`/`Cargo.lock`。
   - 分歧点：排除 `.agents/skills/runtime/**`、`README.md`、`packages/docs/**`、`package.json`、`bun.lock`、`packages/**`。
7. `bcefc521`（clippy）
   - 白名单：`crates/koharu-runtime/src/hardware/cuda.rs`、`crates/koharu-ml/src/comic_onomatopoeia/detector/processor.rs`、`crates/koharu-ml/src/pp_ocr_v6/det/processor.rs`。
   - 分歧点：排除 `crates/koharu-rasterizer/src/{compositor,prepared}.rs`、`crates/koharu-renderer/src/layout.rs`；fork 的 `de9aae2d`（CPU 导出 workaround）改过 renderer，clippy 修正若将来要取须逐行核对不覆盖该 workaround。
8. `87442b49`、`e8422a29`（依赖整理）
   - 白名单：`Cargo.toml` 工作区依赖表、`crates/koharu-{torch-sys,llama-sys,diffusion-sys,torch,runtime,ml}/` 下被触及的清单与源文件、`crates/koharu-runtime/src/hardware/{cuda,hip}.rs`。
   - 分歧点：排除 `crates/koharu-{app,canvas,desktop}/Cargo.toml`、`crates/koharu/Cargo.toml`、`crates/koharu/src/sentry.rs`（遥测）、`crates/koharu-bindgen/src/rewrite.rs`（除非 bindgen 同步升级）。
9. `8818911f`（MIOpen）
   - 白名单：`crates/koharu-ml/src/backend.rs`、`crates/koharu-torch/src/nn/batch_norm.rs`。
   - 分歧点：`crates/koharu-runtime/src/runtime/packages/rocm.rs` 的 hunk 是删除 `4d9f3aca`/`8199a8b2` 加入的块；这两笔暂缓时该 hunk 无内容可删，丢弃此 hunk。

---

## 4. 风险分级与验证方法

### 只需 `cargo check -p koharu --no-default-features --features headless` 的提交

下列提交不改 C ABI、不改原生库版本钉、不改动态库加载顺序，编译通过即可视为应用成功：`b608a9f7`、`22dab373`、`1143fe77`、`7834852e`（功能验证另做一次真实下载，见下）、`dc2c0442`、`c1fce14c`、`ce2e6fbe`、`874ed98c`、`087b40aa`、`e7f0cbce`、`0147de13`、`f282f225`、`ccfdf340`、`19385665`、`9152946a`、`eb063470`、`64173abb`、`c3e79e68`、`eadb7976`、`e460a9bf`、`922f0f48`、`10c440aa`、`57e9b65f`、`e2ea5e58`、`38fc352f`、`bf930058`、`87442b49`、`e8422a29`、`bcefc521`、`dbed34d8`（编译层面）。

下载链三笔（`b608a9f7`、`22dab373`、`1143fe77`）建议在编译检查之外补一次行为验证：用一个大于 32 MiB 的模型文件走 `crates/koharu-runtime` 的下载路径，中途用断网或进程暂停打断一次，确认恢复后字节区间续传完成且文件校验和一致。

### 必须在目标设备上用真实原生库（CUDA／ROCm）构建验证的提交

`6959411c` 原则：C ABI、Rust 包装、原生包版本绑定三者一体，不能只过 Rust 编译检查。下列提交命中该原则或改写了动态库加载顺序：

| 提交 | 验证方法 |
|---|---|
| `6959411c`（已移植）、`d3ce865b`、`6df4ded5`、`1c736f70`、`ac5e9c24`、`67d507b4`、`2762080e`、`892db6bf` | 在 CUDA 设备与 ROCm 设备各构建一次 Docker 镜像，启动 headless 服务后分别执行一次 llama.cpp 本地翻译与一次 stable-diffusion inpainting，确认 `crates/koharu-runtime/src/runtime/loader.rs` 加载 `libstable-diffusion.so`／`libllama.so`（Windows 为 `.dll`）无符号缺失；对照 `crates/koharu-*-sys/include/*.h` 与实际下载的 `master-929-3f8527a.3`／`b11317` 头文件逐项核对函数签名 |
| `12da3d94`、`ba313b47`、`9f73d2c0`、`5b4b9b8e`、`e47620c0` | 同上镜像里跑一次带 KV cache 保存/恢复的翻译请求（覆盖 stream 序列化路径）与一次 llguidance 语法约束采样；`5b4b9b8e` 额外核对 `crates/koharu-llama/src/context/session.rs` 新会话 API 与 `b11317` 头文件一致 |
| `b5cfaba9`、`559aab7b` | 在 Linux CUDA 与 Linux ROCm 两种镜像里确认运行时从 `koharu-rs/{torch,llama,diffusion}` 下载的预编译库能被进程加载（`torch.rs` 的加载顺序生效）；断网构建一次确认依赖的下载 URL 与版本钉在 `crates/koharu-runtime/src/runtime/packages/*.rs` 中固定可复现 |
| ROCm 加载链 `7462036c`→`cf914e8b`→`01bc9705`→`fb74b752`→`6c21c9ad`→`c6dab0c0`→`b830b495`→`1dda0844` | 在 Linux ROCm 设备上启动服务，执行一次 inpainting，确认 `rocm.rs` 与 `torch.rs` 的库搜索路径互不覆盖、`loader.rs` 的全局加载不与系统 HIP 冲突；`ldd`（或 `dumpbin /dependents`）检查最终进程的 ROCm 库解析到 bundled 目录而非系统目录 |
| CUDA 加载 `0d206640`、`556485b1`、`84ef3ee5`、`67d507b4`（CUDA 13.3） | Linux CUDA 设备：`nvidia-smi` 确认驱动 ≥ 13.3 对应版本（`hardware/cuda.rs` 的 `MIN_DRIVER_VERSION = 13030`），启动服务执行一次 Torch 推理 + 一次 inpainting；确认 cuFile、cuDNN 按清单加载 |
| `0147de13` 及 ROCm 目标链 | 在实际部署的 GPU 型号上（`rocm.rs` 的 `targets` 枚举与 `eadb7976` 对齐 llama.cpp）跑一次 inpainting；若部署含 Windows gfx10 设备，另验 MIOpen 关闭生效（`874ed98c` 后 Torch 不走 MIOpen） |

构建检查命令（供执行移植的一方使用，本审计不执行）：`cargo check -p koharu --no-default-features --features headless`；真机验证按上表在 Docker 镜像内进行。

---

## 5. 已移植项的残留差异清单

### `03106e91` 对上游 `6959411c`

已核实：fork `03106e91` 覆盖了 `6959411c` 的全部 19 个文件（523 增 / 88 删，与上游一致），`git diff 6959411c main` 在这 19 个文件上为空。树内没有内容残留差异。

版本绑定层面的残留（在 `6959411c` 之后上游继续演进，fork 仍停在 `6959411c` 的钉上）：

| 项 | fork 当前（`6959411c` 形态） | 上游 `upstream/main` 当前 | 对应上游提交 |
|---|---|---|---|
| `crates/koharu-runtime/src/runtime/packages/diffusion.rs` 的 `RELEASE` | `stable-diffusion.cpp-master-820-de298c2` | `master-929-3f8527a.3` | `6df4ded5`→`1c736f70`→`ac5e9c24`→`67d507b4`→`2762080e` |
| `crates/koharu-runtime/src/runtime/packages/llama.rs` 的 `RELEASE` | `llama.cpp-b10430` | `b11317` | 同上（经 `d3ce865b` 等） |
| 下载域名 | `https://github.com/mayocream/koharu/releases/download/...` | `https://github.com/koharu-rs/{diffusion,llama,torch}/releases/download/...` | `b5cfaba9`→`559aab7b` |
| Torch 包装编译方式 | 源码编译 `libtch` shim（`build.rs` + CMake） | 预编译产物 `v2.13.0.7` | `b5cfaba9` 起 |
| sys 头文件 | `6959411c` 时点快照 | 已多次刷新（`ggml.h`、`llama.h`、`stable-diffusion.h`、`mtmd*.h`） | `d3ce865b`、`6df4ded5`、`1c736f70`、`ac5e9c24`、`67d507b4`、`2762080e`、`892db6bf` |

处理方式：上述残留不是补丁遗漏，而是版本钉与打包方式的演进，按第 2 节组 0 与组 3 的顺序补齐；补齐后在目标设备按第 4 节表格对真实原生库验证一次。

### 其他已移植项

- `68650b09`（上游 `dceb8485` + `b87b93b2` 的 pipeline/ml 子集）只影响 `crates/koharu-pipeline` 与 `crates/koharu-ml/src/{koharu_layout_rfdetr_seg_2xl,bin}`，与本轮 runtime 范围无重叠文件；`e7f0cbce` 的手工移植时注意 `crates/koharu-ml/src/lib.rs` 同时叠着 `4276ec48`、`42624633`、`68650b09` 三笔 fork 改动。

### 需要用户裁决的点

1. `4d9f3aca` 与 `8199a8b2` 的取舍。新事实：上游 `8818911f` 会删除这两笔加入的 gfx1032 Winograd 环境变量块，MIOpen 处理的最终形态是 `874ed98c`（Windows 全 ROCm 关闭 MIOpen）。两个可选方案：方案一，两笔暂缓（维持 `plan/UPSTREAM_SYNC_PLAN.md` 的默认结论），`8818911f` 只手工取 `backend.rs` 与 `batch_norm.rs`，最终树与上游一致且无 gfx1032 专属代码；方案二，把两笔作为过渡态按序 cherry-pick 再由 `8818911f` 删除，机械冲突最少。两方案最终树相同，差异只在过程。
2. `7834852e` 取否。取则下载层与 Hugging Face 客户端对齐上游最终形态，但引入 `hf-hub` 依赖并要求 `dbed34d8`、`d8a68ba2` 的相关部分一并手工整合；不取则保留 fork 手写客户端与 `downloads.rs`，multipart 与断点重试在 `1143fe77` 形态下已具备。
3. `85557a25` 跟随 `hf-hub` git `main` 分支与「HuggingFace 修订钉死」冲突。若取 `7834852e`，建议把依赖钉到具体 commit（`Cargo.lock` 记录的 `e88c9efe6d38bb370008ba0f5281249210c52176`）而不是 `branch = "main"`。
4. `e47620c0` 同时触及 `crates/koharu-translator` 与 `crates/koharu-llama`，与 P0 翻译层审计范围重叠，须指定由哪一轮移植执行，避免两轮重复应用。
