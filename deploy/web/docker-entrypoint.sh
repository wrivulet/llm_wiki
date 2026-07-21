#!/bin/sh
# Container entrypoint: optionally starts the remote-MCP HTTP sidecar
# (mcp-server/src/http.ts) in the background, then execs the main
# llm-wiki binary under xvfb-run as PID 1's foreground process.
#
# The background job stays a child of this script's PID even after
# exec replaces the script's own process image, so it's still reaped
# by the container's init (`--init` / compose `init: true`) — the one
# known gap is that xvfb-run doesn't forward SIGTERM to it, so on
# graceful shutdown it lives until the container's stop_grace_period
# expires and Docker sends SIGKILL. Harmless for a stateless HTTP
# sidecar; revisit with a proper supervisor if that ever matters.
set -e

if [ "${LLM_WIKI_MCP_ENABLE:-0}" = "1" ]; then
    # Swarm secrets land as files under /run/secrets, not env vars;
    # http.ts reads a plain LLM_WIKI_MCP_TOKEN env var, so bridge the two
    # here rather than teaching every consumer about the file path.
    if [ -z "${LLM_WIKI_MCP_TOKEN:-}" ] && [ -f /run/secrets/llm_wiki_mcp_token ]; then
        LLM_WIKI_MCP_TOKEN="$(cat /run/secrets/llm_wiki_mcp_token)"
        export LLM_WIKI_MCP_TOKEN
    fi
    echo "[entrypoint] starting MCP HTTP sidecar" >&2
    node /app/mcp-server/dist/src/http.js &
fi

exec xvfb-run -a /app/llm-wiki
