# syntax=docker/dockerfile:1.7
# Koharu headless server (Linux). Builds the Tauri-free headless feature so
# no GTK/CEF/WebKit/frontend is required at build or run time.
#
#   docker build -t koharu-headless .
#   docker run --rm -p 4000:4000 \
#     --security-opt seccomp=unconfined \
#     --device /dev/dri \
#     -v koharu-data:/home/koharu/.local/share/Koharu \
#     koharu-headless
#
# seccomp=unconfined is required because the providers that still read their
# API key from the Linux keyutils keyring call keyctl/add_key syscalls, and
# GET /llm/catalog reaches every provider. The deepseek and openai-compatible
# keys never touch the keyring: they arrive with the request and are dropped
# when the task ends. The image does
# NOT embed LibTorch or the llama.cpp / stable-diffusion.cpp binaries. The
# headless server calls Store::configure(data_dir/packages) in
# crates/koharu-rpc/src/server.rs, so the packages are downloaded on the
# first start into /home/koharu/.local/share/Koharu/packages, which is the
# koharu-data volume. That first start needs outbound network access.
# GPU note: the runtime downloads Vulkan-only llama.cpp / stable-diffusion.cpp
# assets, so a GPU-less container runs detection/OCR on CPU and remote-provider
# translation works out of the box. The default CMD enables the iGPU mode
# (--gpu), so pass --device /dev/dri to expose the Intel ANV Vulkan driver to
# stable-diffusion.cpp inpainting.
#
# Memory allocator: the runtime preloads jemalloc instead of relying on glibc
# malloc. Torch and the detection models allocate and free tensors of
# megabytes per page from several threads, and glibc malloc returns those to
# the kernel through per-thread arenas it grows but rarely trims, which shows up
# as a resident set that keeps climbing across pages; jemalloc reuses freed
# size classes instead.
#
# OpenMP: torch's intra-op pool defaults to one spinning thread per core and
# keeps spinning between calls. OMP_WAIT_POLICY=PASSIVE parks those threads
# once a kernel finishes, so the cores are free for the Rayon parallel work
# (mask resampling, cropping) that runs between Torch calls; OMP_PROC_BIND=CLOSE
# keeps each worker on one core rather than migrating. Torch's intra-op thread
# count is set at start-up from KOHARU_TORCH_THREADS (falling back to
# OMP_NUM_THREADS) in crates/koharu-ml/src/lib.rs.

FROM ubuntu:24.04 AS builder
ARG DEBIAN_FRONTEND=noninteractive

# build-essential: rustc links the release binaries through cc, and blake3,
#   zstd-sys, ring / aws-lc-sys and openssl-sys compile C.
# pkg-config: yeslogic-fontconfig-sys and openssl-sys locate their system libs.
# clang libclang-dev: koharu-bindgen runs bindgen for the koharu-torch-sys,
#   koharu-llama-sys and koharu-diffusion-sys build.rs files.
# libssl-dev: openssl-sys, pulled in by sentry's default "transport" feature.
# libfontconfig-dev: koharu-renderer -> fontique -> yeslogic-fontconfig-sys.
# cmake is deliberately omitted: the only CMake users are aws-lc-sys (a
#   fallback path that linux-gnu does not take) and cef-dll-sys (gui feature).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential pkg-config \
        clang libclang-dev \
        libssl-dev \
        libfontconfig-dev \
        curl ca-certificates git file \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal

WORKDIR /app
COPY . .

# target-cpu=x86-64-v3 is the baseline every supported x86-64 host CPU meets
# (AVX2, BMI2, FMA) and it is what Torch's own x86-64 wheels assume, so the
# compiler's autovectorized Rust code matches the SIMD width the native
# operators already use. Override it for a narrower target: --build-arg
# RUSTFLAGS="-C target-cpu=x86-64".
ARG RUSTFLAGS="-C target-cpu=x86-64-v3"
ENV RUSTFLAGS=${RUSTFLAGS}

# Concurrent rustc jobs. Unset keeps cargo's default (one per core), which is
# what a build host with memory to spare wants; a host that would swap passes a
# number, for example --build-arg CARGO_BUILD_JOBS=2. It reaches cargo as `-j`
# only when set, because cargo rejects an empty CARGO_BUILD_JOBS instead of
# treating it as unset.
ARG CARGO_BUILD_JOBS

# Frontend build is intentionally skipped: the headless feature does not
# embed a frontend (no tauri-build / generate_context!).

# Cache the cargo registry and the target directory. The build no longer
# downloads a LibTorch wheel; the native packages are fetched at runtime into
# <data_dir>/packages on the koharu-data volume.
RUN --mount=type=cache,target=/root/.cache/koharu \
    --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build -p koharu --release --locked --no-default-features --features headless \
        ${CARGO_BUILD_JOBS:+-j ${CARGO_BUILD_JOBS}} \
    && cargo build -p koharu-pipeline --release --locked --bin pipeline-task \
        ${CARGO_BUILD_JOBS:+-j ${CARGO_BUILD_JOBS}} \
    && mkdir -p /out \
    && cp /app/target/release/koharu /out/koharu \
    && cp /app/target/release/pipeline-task /out/pipeline-task

FROM ubuntu:24.04 AS runtime
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl libssl3 libgomp1 libfontconfig1 fonts-noto-cjk \
        mesa-vulkan-drivers libvulkan1 libjemalloc2 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash koharu \
    && install -d -o koharu -g koharu -m 755 /home/koharu/.local/share/Koharu

COPY --from=builder /out/koharu /usr/local/bin/koharu
COPY --from=builder /out/pipeline-task /usr/local/bin/pipeline-task

# jemalloc and the OpenMP wait policy are explained in the header comment.
ENV LD_PRELOAD=/usr/lib/x86_64-linux-gnu/libjemalloc.so.2 \
    OMP_PROC_BIND=CLOSE \
    OMP_WAIT_POLICY=PASSIVE

USER koharu
WORKDIR /home/koharu
ENV XDG_CACHE_HOME=/home/koharu/.cache

# Projects + model packages live under this directory.
VOLUME ["/home/koharu/.local/share/Koharu"]
EXPOSE 4000

ENTRYPOINT ["/usr/local/bin/koharu", "--headless"]
# mesa-vulkan-drivers provides the Intel ANV Vulkan driver; the container
# still needs --device /dev/dri (and /dev/dri/renderD* permissions) to see
# the iGPU. Use --cpu instead of --gpu for a GPU-less host.
CMD ["--host", "0.0.0.0", "--port", "4000", "--gpu"]
