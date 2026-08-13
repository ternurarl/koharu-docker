# syntax=docker/dockerfile:1.7
# Koharu headless server (Linux). Builds the Tauri-free headless feature so
# no GTK/CEF/WebKit/frontend is required at build or run time.
#
#   docker build -t koharu-headless .
#   docker run --rm -p 4000:4000 \
#     --security-opt seccomp=unconfined \
#     -v koharu-data:/home/koharu/.local/share/Koharu \
#     koharu-headless
#
# seccomp=unconfined is required because koharu-secrets stores provider API
# keys in the Linux keyutils keyring (keyctl/add_key syscalls). GPU note: the
# runtime downloads Vulkan-only llama.cpp / stable-diffusion.cpp assets; the
# build downloads the CPU LibTorch wheel, so a GPU-less container runs
# detection/OCR on CPU and remote-provider translation works out of the box.

FROM ubuntu:24.04 AS builder
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential pkg-config cmake \
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

# Cache the LibTorch CPU wheel download (~200 MB) and the cargo registry.
RUN --mount=type=cache,target=/root/.cache/koharu \
    --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build -p koharu --release --locked --no-default-features --features headless \
    && mkdir -p /out \
    && cp /app/target/release/koharu /out/koharu \
    && cp /app/target/release/libkoharu-torch.so /out/libkoharu-torch.so

FROM ubuntu:24.04 AS runtime
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl libssl3 libgomp1 libfontconfig1 fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash koharu \
    && install -d -o koharu -g koharu -m 755 /home/koharu/.local/share/Koharu

COPY --from=builder /out/koharu /usr/local/bin/koharu
COPY --from=builder /out/libkoharu-torch.so /usr/local/lib/libkoharu-torch.so
RUN ldconfig

USER koharu
WORKDIR /home/koharu
ENV XDG_CACHE_HOME=/home/koharu/.cache

# Projects + model packages live under this directory.
VOLUME ["/home/koharu/.local/share/Koharu"]
EXPOSE 4000

ENTRYPOINT ["/usr/local/bin/koharu", "--headless"]
CMD ["--host", "0.0.0.0", "--port", "4000", "--cpu"]
