---
title: Pipeline, Rendering, and Desktop Composition
description: Follow page processing from stage scheduling through retained rendering and native desktop composition.
---

# Pipeline, Rendering, and Desktop Composition

## Fixed pipeline

`koharu-pipeline` coordinates a small named workflow rather than a runtime graph:

```mermaid
flowchart LR
  detection["Detection"] --> ocr["OCR"] --> translation["Translation"]
  detection --> inpainting["Inpainting"]
```

An execution unit is one stage on one page. Pages stay in project order, but execution is batched by stage: every requested page finishes the current stage before the next stage starts. A two-page run therefore completes both detection jobs before either OCR job, and both OCR jobs before either translation job.

Each stage produces a semantic scene patch. The application commits it immediately, synchronizes the current canvas, and groups all revisions from one invocation into one undo step.

## Scheduling and residency

Accelerator inference uses one admission lane because representative heterogeneous overlap reduced throughput through compute and memory-bandwidth contention. CPU-only execution can retain independent per-model lanes.

Each requested stage loads its model for that batch and releases it before the next stage can load. Failure and stop close the unfinished jobs in the active batch, then release its model; the failure path may unload first, and the batch-end release then observes an empty model cell. Remote translation has no local model, so its release reports that nothing was resident.

A page image cache lives only for the active stage job. The cache is removed when the page finishes that stage or when the stage batch ends, rather than remaining until every later stage on the page has finished. CPU execution still has no accelerator admission lane.

Stopping is cooperative. No new stage starts after the stop request; an active native call returns safely, and its result is discarded if it has not committed.

## Retained rendering

`koharu-renderer::Renderer` turns a scene snapshot and page ID into one immutable `Frame`. The frame contains ordered retained layers, entity lookup, presentation metadata, dependencies, diagnostics, and vector scenes.

Scene translation is the only visible text source. Typography, fit relations, fonts, images, visibility, and opacity all come from scene data. The renderer loads resources and retains reusable nodes; callers do not provide an alternate document model.

The canvas, flattened PNG, layer crops, and PSD adapter consume this same frame. Incremental renderer updates reuse unchanged nodes but must produce the same result as a complete render.

## Desktop composition

The WebView draws interface chrome, menus, controls, and inspectors. It stays transparent over the canvas region. Native WGPU/Vello output is composed beneath it in the same desktop window.

Validate visual changes through the final desktop window. A WebView screenshot alone cannot prove that native canvas pixels are present, and a native render alone cannot prove that transparency and interface composition are correct.

On Windows, the development WebView2 endpoint is `http://127.0.0.1:4000`; use semantic CDP inspection for the web layer and native window capture for the final composite.
