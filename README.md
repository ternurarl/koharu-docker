<h1 align="center">Koharu</h1>

<p align="center">基于机器学习的漫画翻译工具，用 Rust 编写。</p>

> [!NOTE]
> Koharu 的视觉模型与 LLM 均在**本地**运行，确保你的数据私密、安全。

---

## 本仓库变更

本仓库是 [mayocream/koharu](https://github.com/mayocream/koharu) 0.66.6 的 fork。`dev` 分支将其改造为 headless、面向 Docker 部署的构建，主要变化：

- **恢复 headless 模式与 /api/v1 REST 服务**：新增 `crates/koharu-rpc`（基于 axum 的 HTTP 服务，暴露 `/api/v1` REST 端点与 `/events` SSE，在重建的 0.66.x 引擎上复用旧版 REST 风格）。
- **三种运行模式**：纯 CPU（`--cpu`，仅 Torch，跳过 llama/diffusion）、iGPU（`--gpu`，Torch + diffusion，远程翻译）、全量（额外初始化 llama.cpp 本地翻译）。
- **Cargo features**：`gui`（默认）与 `headless`（后者跳过 tauri-build、CEF 入口与前端嵌入）。
- **CLI**：`koharu --headless [--host 0.0.0.0] [--port 4000] [--data DIR] [--cpu|--gpu]`。
- **Dockerfile**：多阶段 Ubuntu 24.04 构建，默认 `CMD ["--host","0.0.0.0","--port","4000","--gpu"]`；运行镜像包含 `mesa-vulkan-drivers` 与 `libvulkan1`，用于 Vulkan 驱动的 iGPU 修复。

完整的 headless 文档（API 参考、pipeline 请求结构、SSE 协议与部署说明）见 [HEADLESS.md](HEADLESS.md)。

![screenshot](docs/screenshot.png)

> [!NOTE]
> 支持与讨论请前往 [Discord 服务器](https://discord.gg/mHvHkxGnUY)。

## 功能特性

- 自动检测文字区域、对话气泡与清理掩码
- 对漫画对白、旁白及其他页面文本进行 OCR
- 修复（Inpainting）移除页面上的原文文字
- 使用本地或远程 LLM 后端翻译
- 支持竖排 CJK 与 RTL 的高级文本渲染
- 可编辑文本的分层 PSD 导出

## GPU 加速

Koharu 支持 CUDA、ROCm / HIP、Metal 与 Vulkan。当加速路径不可用或配置成本不划算时，始终可以回退到 CPU。

### CUDA

Koharu 通过 CUDA 在 Windows 与 Linux 上支持 NVIDIA GPU。请确保已安装最新的 NVIDIA 驱动。

### HIP / ROCm

Koharu 通过 ROCm 与 HIP 在 Windows 上支持 AMD GPU。请确保已安装最新的 AMD 驱动。

### Metal

Koharu 在 Apple Silicon Mac 上支持 Metal。

### Vulkan

Koharu 亦在 Windows 与 Linux 上支持 Vulkan，作为 CUDA 与 HIP 的替代方案。

## 机器学习模型

Koharu 采用分阶段的视觉与语言模型组合，而非用单个网络处理整页内容。

### 计算机视觉模型

Koharu 使用多个预训练模型，各自针对页面流水线中的特定环节进行调优。

#### 检测与版面

Koharu 使用目标检测来寻找文字区域、对话气泡与分割掩码。

- [Koharu Layout RF-DETR Seg 2XL](https://huggingface.co/mayocream/koharu-layout-rfdetr-seg-2xl-1152)

#### OCR

这些模型在检测之后识别原文。

- [PaddleOCR VL 1.6](https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6)
- [Manga OCR](https://huggingface.co/mayocream/manga-ocr)
- [Baberu OCR](https://huggingface.co/genshiai-daichi/baberu-ocr)

#### 修复

这些模型在译后文本回填之前移除原文。

- [FLUX.2 Klein](https://huggingface.co/unsloth/FLUX.2-klein-4B-GGUF)
- [RORem mixed](https://huggingface.co/mayocream/RORem-mixed-GGUF)
- [LaMa](https://huggingface.co/mayocream/lama-manga)
- [AOT GAN](https://huggingface.co/mayocream/aot-inpainting)

## Headless 参数

### CLI

```
koharu --headless [--host <addr>] [--port <port>] [--data <dir>] [--cpu | --gpu]
```

三种运行模式：

- `--cpu` — 仅 Torch；修复使用 LaMa（Torch 后端）；翻译必须使用远程 provider。
- `--gpu` — Torch + stable-diffusion.cpp，跳过 llama.cpp；翻译必须使用远程 provider。
- 不带标志 — 全量模式；额外初始化 llama.cpp 以支持本地 LLM 翻译。

### REST API（全部位于 `/api/v1`）

- `GET /meta`
- `GET /operations` · `DELETE /operations/{id}`
- `GET /projects` · `POST /projects`（`{"name"}`）
- `GET /projects/current` · `PUT /projects/current`（body `{"name"}`）· `DELETE /projects/current`
- `DELETE /projects/{name}`
- `POST /projects/current/pages`（multipart 图片导入）
- `POST /projects/current/export`（body `{"pages"?, "format": "rendered"|"source"}`）
- `GET /pages/{id}/thumbnail`
- `GET /config` · `PATCH /config`
- `PUT /config/providers/{id}/secret` · `DELETE /config/providers/{id}/secret`
- `GET /llm/current` · `PUT /llm/current` · `DELETE /llm/current`
- `GET /llm/catalog`
- `POST /pipelines`
- `GET /events`（SSE）

### Pipeline 请求

```json
{
  "operation": {
    "operation": "full"
      | { "operation": "through", "stage": <s> }
      | { "operation": "only", "stage": <s> }
      | { "operation": "stages", "stages": [<s>, ...] }
  },
  "scope": { "scope": "project" } | { "scope": "pages", "value": [ids] } | region | entities
}
```

- `stage` 取值：`detection` | `ocr` | `translation` | `inpainting`（snake_case）
- 响应：`{"operationId": "..."}`

### 配置（`GET`/`PATCH /config`，三个 section）

**pipeline**

- `detection.model` — 仅 `"koharu-layout-rfdetr-seg-2xl"`
- `ocr.model` — `baberu-ocr` | `manga-ocr` | `paddleocr-vl-1.6`（`paddleocr-vl-1.6` 依赖 llama，`--gpu` 下不可用）
- `inpainting.model` — `lama` | `aot-inpainting` | `flux2-klein` | `rorem-mixed`
- `translation.model` — `{"provider","model","quantization","vision"}`（`quantization` 仅对 `local` 有意义，远程填 `null`）
- `translation.target_language` — BCP47 串：`zh-CN`/`zh`/`zh-Hans`/`zh-TW`/`en-US`/`ja-JP`/`fr-FR`/`pt-BR`/`es-ES`/`tr-TR`/…
- `translation.instructions` — `string | null`
- `translation.generation` — `{"temperature","top_k","top_p","min_p","max_tokens","repeat_penalty","frequency_penalty","presence_penalty": null|float, "thinking": bool}`
- `processor` — 每个模型独立调参（`rfdetr` 暴露 `text_threshold`/`bubble_threshold`/`panel_threshold`）

**providers** — 12 个 id：`local`、`atlas-cloud`、`openai`、`gemini`、`claude`、`deepseek`、`openai-compatible`、`openrouter`、`lm-studio`、`deepl`、`google-cloud-translation`、`caiyun`；多数 `settings` 为空对象 `{}` — `openai-compatible` = `{"base_url": url|null, "vision": false}`，`lm-studio` = `{"base_url": url|null}`，`deepl` = `{"base_url": url|null}`。

**typesetting** — `{"font_families": ["CCWildWords", "Adobe 黑体 Std"]}`

### 密钥

- `PUT /api/v1/config/providers/{id}/secret`，body `{"secret": "<key>"}` → `204`
- 密钥存于 Linux keyring，容器重启后丢失——每次重启后需重新 PUT。

### 翻译模型捷径

- `PUT /llm/current`，body `{"model": {"provider","model","quantization","vision"}, "instructions"?}` → `204`（与 `PATCH pipeline.translation.model` 写同一字段）
- `GET /llm/current` → `{"model": {...}, "targetLanguage": "..."}`（无 `status` 字段）
- `GET /llm/catalog` → `{"models": [{"provider","model","name","quantizations": [{"id","name"}], "vision"}]}`

### 注意事项

1. `PATCH /config` 仅在顶层稀疏合并——请求中出现的每个 section 会**整段替换**存储内容。只发 `pipeline.ocr` 会把 `translation` 重置回默认的 local gemma；务必携带完整 section。
2. `target_language` 使用 BCP47 字符串。

### Docker 运行（iGPU）

```bash
docker run -p 4000:4000 \
  --security-opt seccomp=unconfined \
  --device /dev/dri \
  -v koharu-data:/home/koharu/.local/share/Koharu \
  koharu-headless
```

默认 `CMD` 即 `--gpu`；无 GPU 的主机追加 `--host 0.0.0.0 --port 4000 --cpu`。

## 故障排查

你也可以将 `RUST_LOG` 环境变量设置为 `debug` 或 `trace` 以查看更详细的日志：

```bash
# macOS / Linux
RUST_LOG=debug koharu
# Windows (PowerShell)
$env:RUST_LOG="debug"; koharu.exe
```

## 开发

要从源码构建 Koharu，请按以下步骤操作。

### 前置依赖

- [Rust](https://www.rust-lang.org/tools/install) 1.95 或更高（Rust 2024 edition）
- [Bun](https://bun.sh/) 1.0 或更高
- [LLVM](https://llvm.org/) 15 或更高
- [ninja](https://ninja-build.org/) 1.11 或更高

### 安装依赖

```bash
bun install
```

### 开发

```bash
bun dev
```

### 构建

```bash
bun run build
```

构建产物输出到 `target/release`。

## Contributors ❤️

感谢所有帮助改进 Koharu 的贡献者！

<a href="https://github.com/mayocream/koharu/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=mayocream/koharu" />
</a>

## 许可证

Copyright 2025-2026 Mayo Takanashi 与 Koharu 贡献者。

Koharu 依据 [GNU General Public License version 3 only](LICENSE)（`GPL-3.0-only`）授权。
