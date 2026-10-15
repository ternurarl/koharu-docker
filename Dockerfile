# syntax=docker/dockerfile:1.7
# Koharu headless server (Linux). Builds the Tauri-free headless feature so
# no GTK/CEF/WebKit/frontend is required at build or run time.
#
#   docker build -t koharu-headless .
#   docker run --rm -p 4000:4000 \
#     --security-opt seccomp=unconfined \
#     --device /dev/dri \
#     -v koharu-data:/home/koharu/.local/share/Koharu \
#     -v koharu-cache:/home/koharu/.cache/koharu \
#     koharu-headless
#
# seccomp=unconfined is required because koharu-secrets stores provider API
# keys in the Linux keyutils keyring (keyctl/add_key syscalls). The image does
# NOT embed LibTorch or the llama.cpp / stable-diffusion.cpp binaries: the
# runtime downloads those native packages into Store::root(), which resolves to
# /home/koharu/.cache/koharu/packages inside this container (XDG_CACHE_HOME is
# set below). The first start therefore needs outbound network access; mount
# the koharu-cache volume to keep the packages across container rebuilds.
# GPU note: the runtime downloads Vulkan-only llama.cpp / stable-diffusion.cpp
# assets, so a GPU-less container runs detection/OCR on CPU and remote-provider
# translation works out of the box. The default CMD enables the iGPU mode
# (--gpu), so pass --device /dev/dri to expose the Intel ANV Vulkan driver to
# stable-diffusion.cpp inpainting.

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

# Frontend build is intentionally skipped: the headless feature does not
# embed a frontend (no tauri-build / generate_context!).

# Cache the cargo registry and the target directory. The build no longer
# downloads a LibTorch wheel; the native packages are fetched at runtime into
# Store::root(), i.e. /home/koharu/.cache/koharu/packages in this container.
RUN --mount=type=cache,target=/root/.cache/koharu \
    --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build -p koharu --release --locked --no-default-features --features headless \
    && cargo build -p koharu-pipeline --release --locked --bin pipeline-task \
    && mkdir -p /out \
    && cp /app/target/release/koharu /out/koharu \
    && cp /app/target/release/pipeline-task /out/pipeline-task

FROM ubuntu:24.04 AS runtime
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl libssl3 libgomp1 libfontconfig1 fonts-noto-cjk \
        mesa-vulkan-drivers libvulkan1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash koharu \
    && install -d -o koharu -g koharu -m 755 /home/koharu/.local/share/Koharu

COPY --from=builder /out/koharu /usr/local/bin/koharu
COPY --from=builder /out/pipeline-task /usr/local/bin/pipeline-task

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
