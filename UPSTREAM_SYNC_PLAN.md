# Upstream Headless/Core Synchronization Plan

## Scope and invariants

This plan updates the fork's `upstream` branch reference to
`mayocream/koharu:main`, then evaluates selected upstream commits from the
fork's `main` baseline. It does not merge the upstream tree into `main` and it
does not change the current working tree during the audit phase.

The synchronization target is headless/core functionality only. Exclude
Tauri, CEF, WebGPU/WASM canvas, desktop windows, frontend packages, IME,
updater, `koharu-app`, and `koharu-desktop` changes. Preserve every commit
that is unique to the fork's `main`; use a temporary integration branch for
all candidate work and apply only reviewed patches to `main` later.

Before execution, re-resolve all SHAs with `gh`. The 2026-08-20 audit snapshot
records fork `main` as `ddadafc9`, fork `upstream` as `77de3a4d`, and upstream
`main` as `d2f659a4`; upstream moved several times during this audit, so its SHA
must never be assumed.

## Phase 0: record and verify state

1. Confirm the clean worktree and current branch. The expected source of
   candidate work is `ternurarl/koharu-docker:main`, not the local `dev`
   branch.
2. Resolve the two branch SHAs and the common ancestor through GitHub API:
   `gh api repos/ternurarl/koharu-docker/branches/main` and
   `gh api repos/mayocream/koharu/branches/main`.
3. Save the initial fork `main` SHA, the fork `upstream` SHA, upstream `main`
   SHA, and the ancestor in the audit record. Do not force-push or rewrite
   `main`.
4. Run the smallest baseline check once in a temporary checkout:

   ```text
   cargo check -p koharu --no-default-features --features headless
   ```

   Record known baseline failures separately. The existing headless RPC path
   imports `koharu_renderer::RasterOptions` and calls `Renderer::rasterize`,
   while current upstream uses `koharu_rasterizer::{Rasterizer, RasterOptions}`
   and `Frame::raster_frame()`. This API mismatch is not permission to adopt
   the upstream GUI/renderer architecture.

## Phase 1: synchronize the fork `upstream` branch with `gh`

1. Fetch the source SHA and verify it is the intended `mayocream/koharu:main`
   commit immediately before the update.
2. Compare it with `ternurarl/koharu-docker:upstream`. If they differ, update
   only the fork branch ref through the GitHub API with a non-forced fast
   forward (`PATCH .../git/refs/heads/upstream`, `force=false`).
3. If the ref is not fast-forwardable or branch protection rejects the update,
   stop and create a reviewable GitHub PR/ref update; never force-update a
   branch without explicit approval.
4. Re-read both branch refs and record the resulting equality. This phase is
   complete before any candidate commit is considered.

## Phase 2: candidate application order

Start a disposable integration branch at the recorded fork `main` SHA. For
each group, inspect the patch file list, apply it with three-way context, and
run the focused headless check before continuing. Stop the group on a conflict,
GUI file, dependency drift, or a new failure whose cause is not understood.

### Group A: runtime and device capability

Apply independently, in this order:

- `b608a9f7` `perf(runtime): maximize download throughput` (conditional; see
  the follow-up gate below)
- `84ef3ee5` `fix(runtime): correct Linux cuDNN library manifest`
- `ce2e6fbe` `fix(ml): select precision by device capability`
- `0d206640` `fix(runtime): load CUDA Torch dependencies in order` (manual
  CUDA-only hunk if fork ROCm/layout context differs)

Review download/network behavior and CUDA package manifests on Linux before
accepting. Verify CPU and CUDA feature resolution without adding platform GUI
dependencies.

`b608a9f7` must not be accepted as a complete download solution without
reviewing its later follow-ups `22dab373` (bounded part concurrency and
manifest changes) and `1143fe77` (retry behavior and tests). Those follow-ups
may be evaluated as a separate, complete runtime group; do not silently mix
their dependency changes into the first patch.

### Group B: OCR and translation inference

Apply in dependency order:

- `e47620c0` `perf(translator): optimize structured inference`
- `653eb14b` `feat(ocr): tighten PaddleOCR-VL sampling` (manual OCR-owned
  behavior only)
- `8ab7cde1` `fix(translator): use LM Studio vision capability` (manual; keep
  capabilities optional and default missing vision to false)
- `08601a8f` `feat(translator): refresh remote model catalogs`

After `e47620c0`, complete the manual `efb28e30` gate in Phase 3 before
applying `653eb14b`; if its manifest/dependency review is deferred, stop Group
B there. Check structured sampling, model capability selection, and catalog
behavior using headless translator/ML paths only. Do not import upstream
application or frontend consumers.

The translator audit found these additional gates: `e47620c0`, `653eb14b`,
`8ab7cde1`, and `08601a8f` pass `git apply --check` against a temporary fork
`main` snapshot when considered as Rust-only patches. `efb28e30` does not apply
as a complete patch because the fork's manifests/lockfile diverge; its Rust
prompt hunk does apply and remains a manifest-authority/manual-lockfile gate.
For `653eb14b`, keep the PaddleOCR-specific 512-token limit and 1.2 repetition
penalty, but omit the global `GenerationOptions::default()` change from 1.1 to
1.2 because it affects every local LLM and is not owned by the OCR model.
`8ab7cde1` assumes LM Studio always returns `capabilities.vision`; do not apply
that required-field contract unchanged. Later upstream corrected capabilities
to be optional. Port the vision decision with optional/default-false parsing
and require JSON fixtures for present, false, and missing capabilities.
`08601a8f` changes catalog API-failure behavior to an empty result; test both
successful and failed mock responses. Later `412822d2` changes several
catalogs to dynamic discovery; audit it separately if the target is current
upstream catalog behavior rather than the `08601a8f` snapshot.

### Group C: renderer and pipeline core

- `dceb8485` `fix: recover from an out-of-range detection threshold`: apply
  only `crates/koharu-pipeline/src/stages/detection.rs` and
  `crates/koharu-pipeline/src/stages/mod.rs`. Exclude the two frontend test/
  preference files in the commit.
- `d4c075fe` `fix(renderer): apply language-aware line breaking`

Validate line-breaking and detection threshold behavior at the core API level;
do not port the upstream rasterizer or desktop rendering architecture.
The two `dceb8485` Rust files are one unit: `Processor::new` becomes infallible
and its caller removes `?`. Apply it before `d4c075fe` to preserve upstream
topology, even though the commits modify disjoint files.

## Phase 3: manual ports requiring review

- `efb28e30` must be handled immediately after `e47620c0` and before
  `653eb14b`: port the Rust prompt implementation and review its `Cargo.toml`
  dependency changes. Change the manifest as the authoritative input, then
  regenerate/validate `Cargo.lock` with Cargo; do not hand-edit a generated
  lockfile. Keep only dependencies reachable from headless crates.
- `800e32bb`: port only Rust provider files (`crates/koharu-translator/src/bin/translate.rs`,
  `provider.rs`, `remote/{grok,minimax,mod}.rs`). Exclude `packages/bridge`,
  frontend locales, and README/docs unless a headless CLI contract requires a
  narrowly scoped documentation update.
- `c17d7a07`: port only `crates/koharu-translator/src/local/catalog.rs`;
  exclude README changes. Before accepting the entry, compare its generation
  parameters with later correction `9962e14c` instead of preserving stale
  values mechanically.
- `6959411c`: treat as one high-risk FFI group. Review diffusion/llama headers,
  bindings, and native package expectations together, then build against the
  supported native libraries on the target device. Do not accept this group
  on a Rust-only check. It couples C struct layouts, enum values, sampler
  signatures, Rust wrappers, runtime packages, and the pinned native releases
  `stable-diffusion.cpp-master-820-de298c2` and `llama.cpp-b10430`; do not split
  those changes or hand-edit generated bindings.

`4d9f3aca` (ROCm gfx1032 Winograd) is deferred unless ROCm gfx1032 is an
explicit deployment target. The ROCm artifact/download chain
(`7462036c`, `cf914e8b`, `01bc9705`, `fb74b752`, `8199a8b2`, `1143fe77`,
`22dab373`) must be ported and validated as a complete dependency set, never
cherry-picked piecemeal.

Even that set does not establish parity with current upstream Linux ROCm
behavior. Later fixes `6c21c9ad`, `c6dab0c0`, `b830b495`, and `1dda0844`
change library isolation, shared-runtime ownership, wheel layout, and global
loading. Audit those before claiming ROCm support complete.

## Verification and acceptance gates

After every candidate group, run the focused debug check once:

```text
cargo check -p koharu --no-default-features --features headless
```

For changed behavior, add the smallest available targeted Rust test or fixture
check. For FFI/runtime changes, also verify the relevant CPU/CUDA/ROCm package
path on the actual target; report device, input size, result, and any
correctness difference. Classify failures as baseline, patch-induced, or
environmental before proceeding.

Before any eventual merge to fork `main`:

- review `git diff` and the changed-file list for GUI/frontend paths;
- confirm all fork-only commits remain in history;
- confirm no generated files, model weights, credentials, or machine-specific
  artifacts are present;
- retain intentional divergence comments where the headless fork cannot use
  an upstream GUI path;
- merge only individually accepted core commits, never the whole upstream
  `main`.

This document records a plan only. No cherry-pick, merge, branch-ref update, or
source modification is performed by the audit step.

## Dispatch and audit record

The plan is split into three independent, read-only review packages. A
subagent must return changed-file evidence, dependency/ancestry facts, GUI
exclusions, and the smallest verification command; it must not edit the shared
worktree, update a Git ref, or apply a patch.

| Work package | Assigned responsibility | Output gate | Audit status |
| --- | --- | --- | --- |
| Group A: `runtime_ml_audit` | `b608a9f7`, `84ef3ee5`, `ce2e6fbe`, `0d206640`; inspect runtime/ML file drift and CUDA/ROCm follow-ups | exact file list, patch ancestry, device matrix, download-risk decision | Complete |
| Group B: `translator_ocr_audit` | `e47620c0`, `653eb14b`, `8ab7cde1`, `08601a8f`; manually review `efb28e30`, `800e32bb`, `c17d7a07` | Rust-only file allowlist, manifest/dependency edges, provider/catalog exclusions | Complete |
| Group C: `pipeline_ffi_audit` | `d4c075fe`, selective `dceb8485`, `6959411c`, `4d9f3aca`, and ROCm chain | core-only file allowlist, native ABI risk, topological application order | Complete |

The current audit evidence corrects the candidate order in Group A: use
`b608a9f7 -> 84ef3ee5 -> ce2e6fbe -> 0d206640` (or review each independently).
`ce2e6fbe` is earlier in the upstream ancestry than `0d206640`, and the fork's
`torch.rs` has ROCm/layout drift, so the CUDA ordering line must be ported
manually and verified rather than assumed to cherry-pick cleanly.

`b608a9f7` is not a self-contained download solution. Its later upstream
follow-ups `22dab373` (bounded part concurrency and manifest changes) and
`1143fe77` (retry behavior and tests) must be reviewed as a unit before the
performance patch is accepted.

The ROCm candidates are a linear dependency chain, not the thematic order
listed above. If ROCm support is explicitly enabled, review in this topology:

```text
6959411c -> 4d9f3aca -> 8199a8b2 -> d4c075fe -> 7462036c -> cf914e8b
-> 22dab373 -> 01bc9705 -> 1143fe77 -> fb74b752
```

Do not apply an isolated member of that chain. The three subagent reports are
advisory evidence for the main agent; acceptance still requires the per-group
headless check and the GUI-file audit described above.
