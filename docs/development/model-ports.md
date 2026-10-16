---
title: Model Ports
description: Verify an upstream model port against the reference implementation, and avoid the failure modes that make a port look correct when it is not.
---

# Model Ports

Koharu reimplements upstream models in Rust against a pinned checkpoint. A port is correct only when it reproduces the reference implementation on identical inputs; a plausible-looking output is not evidence.

This page is the short form. The full treatment, with worked examples and the measured numbers behind each rule, is in [Simplified Chinese](/zh-CN/development/model-ports/).

## What must match

Checkpoint-affecting structure has to line up exactly: variable names and nesting, which layers carry biases, normalization epsilons, and activation variants. Read the released `model.safetensors` header rather than a guide or a doc comment — the checkpoint is authoritative.

`VarStore::load` walks registered variables and errors on ones missing from the file, but ignores extra keys. Auxiliary training heads are harmless and need no placeholder layer.

## Verifying against the reference

Compare intermediates, not just final text. A one-token difference in the output tells you almost nothing; the layer where values first diverge tells you where the bug is.

1. Dump each stage from the Rust side to `.npy` — preprocessed pixels, encoder output, projector output, rotary tables, prefill hidden states, per-step logits.
2. Load those same tensors into the reference implementation and compare stage by stage.
3. Read the magnitude at each stage: `1e-3` relative on an fp32 path is rounding, `1e-1` is a bug.

**Isolate preprocessing before blaming the model.** Run the reference's generation on our preprocessed pixels. If the reference reproduces our wrong output, the divergence is in preprocessing; if it is still correct, the divergence is inside the port. Do this before reasoning about architecture.

**Separate valid positions from padding.** Padded rows are usually discarded later. Diff the valid region and the padding region separately, or padding noise dominates the summary statistics and hides the divergence that matters.

## Failure modes

| Failure | What goes wrong |
| --- | --- |
| Trusting a comment that asserts a limitation | The claim is unverified. A note stating a binding lacks an operation may be false — check the generated bindings. Likewise, a "negligible difference" claim must be checked in the regime the workload produces, not an easy one. |
| Treating every divergence as a bug | Published benchmarks bound what the model can do. A release reporting 75% exact match will not match one crop in four. Distinguish in-envelope noise from systematic error by checking whether the reference reproduces it. |
| Treating the reference as a specification | The reference is code and carries artifacts. Where behavior and stated intent disagree, choose deliberately and record the choice in a comment. |
| Transcribing test cases by hand | Hand-written escapes mirroring a Python literal invent phantom bugs. Write the cases once to a shared file and have both sides read it; compare outputs programmatically. |

## Tooling hazards

- **Stale binaries.** `cargo test` rebuilds the library, not a previously built `target/debug/<bin>`. Rebuild the bin before concluding a fix failed.
- **Console encoding on Windows.** UTF-8 results piped through a non-UTF-8 console show replacement characters that are not in the data. Write results to a file and diff them.
- **Python isolation flags.** `-I` drops user site-packages; use `-E` when untrusted-input work still needs packages installed with `--user`.
- **Path syntax.** Windows Python does not resolve `/d/...` Git Bash paths; pass drive-letter paths.

## Local checkpoints

Place a pinned checkpoint in the runtime store so `resolve()` finds it without a download:

```
<store>/hugging-face/models/<owner>--<repo>/snapshots/<revision>/<file>
```

The store root is `dirs::cache_dir()/koharu/packages`, or the configured data directory when the host sets one.

## Checklist

- Read the checkpoint header and match names, biases, epsilons, and activations.
- Run the reference on our preprocessed inputs to isolate preprocessing from the model.
- Diff intermediates stage by stage, valid positions separate from padding.
- Reproduce the reference's post-processing contract from its release notes, not only its code.
- Rebuild the binary before judging a fix.
- Remove temporary dump code before committing.
