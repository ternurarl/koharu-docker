# Headless mode (REST + SSE) on Koharu 0.66.x

The upstream project dropped its headless server and the /api/v1 REST transport
in the 0.63.0 "rebuild" commit (d53d3d7). This branch restores a headless mode
on the *rebuilt* 0.66.x engine, re-establishing the legacy /api/v1 REST style
without pulling in Tauri, CEF, GTK, or a window server.

## What changed

- New crate crates/koharu-rpc: an axum HTTP server over the Tauri-free crates
  (koharu-pipeline, koharu-scene, koharu-config, koharu-secrets,
  koharu-translator, koharu-renderer, koharu-runtime, koharu-ml).
- The koharu binary gained two cargo features:
  - gui (default): the desktop app (unchanged behavior).
  - headless: compiles the server path; skips tauri-build, the frontend embed,
    and the CEF entrypoint entirely.
- CLI: koharu --headless [--host 0.0.0.0] [--port 4000] [--data DIR] [--cpu].

## Build (Docker)

    docker build -t koharu-headless .

## Build (Linux, local)

    # Requires: build-essential cmake clang libclang-dev libssl-dev libfontconfig-dev curl git
    cargo build -p koharu --release --locked --no-default-features --features headless

## Run

    docker run --rm -p 4000:4000 \
      --security-opt seccomp=unconfined \
      -v koharu-data:/home/koharu/.local/share/Koharu \
      koharu-headless

Notes:
- --data defaults to the platform data dir + "Koharu"
  (~/.local/share/Koharu on Linux). Projects live under data/projects, model
  packages under data/packages.
- Configuration stays at ~/.koharu/config.toml (koharu_config).
- seccomp=unconfined is required because koharu-secrets stores provider API
  keys in the Linux keyutils keyring (keyctl/add_key). Without it, provider
  secret endpoints fail with "failed to initialize Linux Keyutils".
- On first run the runtime downloads the CPU LibTorch wheel and (Vulkan-only)
  llama.cpp / stable-diffusion.cpp assets into data/packages. Local LLM
  translation and PNG export therefore require a Vulkan GPU; detection/OCR run
  on CPU, and remote-provider translation works without a GPU.

## REST API (all under /api/v1)

- GET  /meta                              -> { version, mlDevice }
- GET  /operations                        -> { operations: JobSummary[] }
- DELETE /operations/{id}                 -> 204 (cancels a running pipeline job)
- GET  /projects                          -> { projects: [{ name }] }
- POST /projects {name}                   -> creates + opens; returns ProjectInfo
- GET  /projects/current                  -> ProjectInfo (name, revision, activePage, pages)
- PUT  /projects/current {name}           -> open by name; returns ProjectInfo
- DELETE /projects/current                -> 204
- DELETE /projects/{name}                 -> 204
- POST /projects/current/pages            -> multipart image import (adds pages)
- POST /projects/current/export           -> { pages?, format: "rendered"|"source" }
- GET  /pages/{id}/thumbnail              -> image/webp (128px)
- GET  /config                            -> { pipeline, providers, typesetting }
- PATCH /config                           -> sparse replace of those three sections
- PUT/DELETE /config/providers/{id}/secret
- GET  /llm/current                       -> { model, targetLanguage, instructions }
- PUT  /llm/current {model, instructions?}-> select the translation model
- DELETE /llm/current                     -> reset to the default local model
- GET  /llm/catalog                       -> { models: Model[] }
- POST /pipelines                         -> start a run; returns { operationId }
- GET  /events                            -> SSE stream

### Pipeline request

POST /pipelines accepts the pipeline's own Operation and Scope, which are
internally tagged:

    { "operation": { "operation": "full" },
      "scope":    { "scope": "project" } }

Operation values: full | { operation: "through", stage } | { operation: "only",
stage } | { operation: "stages", stages: [...] }. Scope values: project | pages
| region | entities. See koharu_pipeline::{Operation, Scope} for the exact wire
shape.

### SSE contract

Frames carry no SSE event: name. The JSON "event" discriminator identifies the
type; live frames set id: to a monotonic sequence number. A fresh or
reconnecting client is first seeded with a snapshot frame, then receives the
live tail. Event shapes:

- {"event":"snapshot","jobs":[...]}
- {"event":"jobStarted","id","kind"}
- {"event":"jobProgress","id","status","stage","currentPage","completed","total","overallPercent"}
- {"event":"jobFinished","id","status","error"?}
- {"event":"jobWarning","id","message"}

JobStatus is snake_case: running | completed | cancelled | failed.

## Divergences from the legacy (0.61.2) contract

- The scene/op/blobs/fonts/ai(MCP) routes are not ported; the data model was
  rebuilt, so scene.json / history / Op no longer map 1:1. The restored surface
  covers meta, operations, projects, config, llm, pipelines, and events.
- /projects/import is replaced by /projects/current/pages (multipart images) -
  the .khr archive format no longer exists in the new engine.
- Export supports rendered (GPU) and source (CPU) PNG; psd/inpainted/khr are
  not yet ported.
- GET /config returns the new section-based config (pipeline/providers/
  typesetting) rather than the legacy AppConfig.

## Verification status

Cargo manifests and the feature graph resolve (cargo generate-lockfile
succeeded). A full cargo check requires the native -sys build scripts
(bindgen + the LibTorch C++ shim), so run it inside the Dockerfile's Ubuntu
builder.
