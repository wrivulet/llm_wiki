# Rust 检查/构建环境镜像 —— 与 deploy/web/Dockerfile 的 backend 阶段同一套
# 依赖。构建机(Rocky 8)没有 webkit2gtk4.1 且无 sudo,原生工具链无法编译
# Tauri 2,用本镜像 + 缓存卷跑快速 cargo check(见 cargo-check.sh)。
#
#   docker build -f deploy/web/Dockerfile.builder -t llm-wiki-builder .
FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev \
        libgtk-3-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        libssl-dev \
        pkg-config \
        protobuf-compiler \
        libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
