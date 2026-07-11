#!/usr/bin/env bash
# 在容器里对 src-tauri 跑 cargo check(或传入其他 cargo 子命令),
# registry 与 target 走命名卷缓存,预热后增量检查只需几十秒。
#
#   deploy/web/cargo-check.sh              # cargo check
#   deploy/web/cargo-check.sh clippy       # cargo clippy
#
# 前置:根目录 dist/ 与 mcp-server/dist、node_modules 需存在
# (tauri 构建脚本校验资源路径):
#   npx vite build && npm --prefix mcp-server install && npm --prefix mcp-server run build
set -euo pipefail

cd "$(dirname "$0")/../.."

SUBCOMMAND="${1:-check}"
shift || true

docker image inspect llm-wiki-builder >/dev/null 2>&1 || {
  echo "[cargo-check] builder 镜像不存在,先构建(仅首次)…"
  docker build -f deploy/web/Dockerfile.builder -t llm-wiki-builder .
}

exec docker run --rm \
  -v "$PWD":/build \
  -v llm-wiki-cargo-registry:/usr/local/cargo/registry \
  -v llm-wiki-check-target:/build/src-tauri/target \
  -w /build \
  llm-wiki-builder \
  cargo "$SUBCOMMAND" --manifest-path src-tauri/Cargo.toml "$@"
