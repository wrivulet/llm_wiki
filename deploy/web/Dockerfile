# LLM Wiki Web 版镜像(POC)
#
# 从仓库根目录构建:
#   docker build -f deploy/web/Dockerfile -t llm-wiki-web:0.6.0 .
#
# 三阶段:Node 构建前端(桌面 dist 供 cargo 编译期嵌入 + Web dist-web)
#        → Rust 编译 Tauri 二进制 → 精简运行时(xvfb headless)。
# 运行时安全模型见 deploy/web/README.md:桥接无认证,
# 只能通过 oauth2-proxy 暴露(见 docker-stack.yml)。

# --- 阶段 1:前端 -----------------------------------------------------------
FROM node:22-bookworm-slim AS frontend
WORKDIR /build

COPY package.json package-lock.json ./
# 上游锁文件缺平台可选依赖时 npm ci 会失败,回退 npm install
RUN npm ci --no-audit --no-fund || npm install --no-audit --no-fund

# mcp-server:tauri 构建脚本校验 tauri.linux.conf.json 声明的资源路径
# (../mcp-server/{package.json,dist,node_modules})必须存在
COPY mcp-server ./mcp-server
RUN (npm --prefix mcp-server ci --no-audit --no-fund || npm --prefix mcp-server install --no-audit --no-fund) \
    && npm --prefix mcp-server run build \
    && npm --prefix mcp-server prune --omit=dev

COPY index.html vite.config.ts vite.config.web.ts components.json ./
COPY tsconfig.json tsconfig.app.json tsconfig.node.json ./
COPY src ./src
COPY src-web ./src-web

# 桌面 dist:cargo 编译期嵌入需要;dist-web:浏览器端静态文件
RUN npx vite build && npx vite build --config vite.config.web.ts

# --- 阶段 2:Rust 二进制 ----------------------------------------------------
FROM rust:1-bookworm AS backend
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
COPY src-tauri ./src-tauri
# file_sync.rs 通过 include_str! 引用前端树里的 src/lib/source-watch-defaults.json
COPY src ./src
COPY --from=frontend /build/dist ./dist
COPY --from=frontend /build/mcp-server ./mcp-server
# 上游 release profile 是 fat LTO + codegen-units=1,链接期峰值内存极高
# (小内存构建机上会被 OOM 杀掉);容器构建降级为 thin LTO,不改上游 Cargo.toml
ENV CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
RUN cargo build --release --manifest-path src-tauri/Cargo.toml

# --- 阶段 3:运行时 ---------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-0 \
        libgtk-3-0 \
        libayatana-appindicator3-1 \
        librsvg2-2 \
        xvfb \
        xauth \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --home-dir /data app

COPY --from=backend /build/src-tauri/target/release/llm-wiki /app/llm-wiki
COPY --from=frontend /build/dist-web /app/dist-web
COPY src-tauri/pdfium/libpdfium.so /app/pdfium/libpdfium.so

# LLM_WIKI_WEB_BIND=0.0.0.0 仅面向容器内部网络(供 oauth2-proxy 访问),
# 切勿直接 publish 19829。WEBKIT_DISABLE_* 规避容器内无 GPU 的渲染初始化问题。
ENV LLM_WIKI_WEB_ENABLE=1 \
    LLM_WIKI_WEB_BIND=0.0.0.0 \
    LLM_WIKI_WEB_PORT=19829 \
    LLM_WIKI_WEB_DIST=/app/dist-web \
    PDFIUM_DYNAMIC_LIB_PATH=/app/pdfium/libpdfium.so \
    HOME=/data \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 \
    WEBKIT_DISABLE_COMPOSITING_MODE=1

USER app
# 项目数据与应用状态(plugin-store 等)都落在 /data
VOLUME /data
EXPOSE 19829

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:19829/ > /dev/null || exit 1

ENTRYPOINT ["xvfb-run", "-a", "/app/llm-wiki"]
