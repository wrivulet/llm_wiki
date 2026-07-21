#!/usr/bin/env node
/**
 * Remote MCP entrypoint (Streamable HTTP transport), for clients that
 * can't spawn a local stdio subprocess — e.g. LibreChat's mcpServers
 * config pointing at this server over the network. Runs as a sibling
 * process inside the same container as the llm-wiki web build, talking
 * to the local api_server.rs HTTP API exactly like the stdio entrypoint
 * (index.ts) does — same tool definitions, same LlmWikiApiClient.
 *
 * Unlike stdio (trusted by construction: the client spawned the
 * process), this listens on the network, so it's fail-closed on auth:
 * refuses to start unless LLM_WIKI_MCP_TOKEN is set. Every request
 * must carry `Authorization: Bearer <token>`.
 *
 * Env:
 *   LLM_WIKI_MCP_TOKEN   (required) shared secret; LibreChat sends it
 *                        as a static header, no OAuth dance needed.
 *   LLM_WIKI_MCP_PORT    default 3939
 *   LLM_WIKI_MCP_BIND    default 0.0.0.0 (container-internal; the
 *                        stack publishes this port directly — see
 *                        deploy/web/docker-stack.yml)
 *   LLM_WIKI_MCP_TLS_CERT / LLM_WIKI_MCP_TLS_KEY
 *                        optional PEM paths. Required unless
 *                        LLM_WIKI_MCP_BIND is loopback — this port is
 *                        published straight to the host, bypassing
 *                        oauth2-proxy's TLS termination.
 */
import * as http from "node:http"
import * as https from "node:https"
import { readFileSync } from "node:fs"
import { timingSafeEqual } from "node:crypto"
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js"
import { createServer as createMcpServer } from "./tools.js"
import { VERSION } from "./version.js"

const MAX_BODY_BYTES = 10 * 1024 * 1024
const PORT = Number(process.env.LLM_WIKI_MCP_PORT ?? "3939")
const BIND = process.env.LLM_WIKI_MCP_BIND ?? "0.0.0.0"
const TOKEN = process.env.LLM_WIKI_MCP_TOKEN

function jsonRpcError(res: http.ServerResponse, status: number, message: string): void {
  if (res.headersSent) return
  res.writeHead(status, { "content-type": "application/json" })
  res.end(JSON.stringify({ jsonrpc: "2.0", error: { code: -32000, message }, id: null }))
}

function isAuthorized(req: http.IncomingMessage): boolean {
  const header = req.headers.authorization
  if (!header || !TOKEN) return false
  const expected = Buffer.from(`Bearer ${TOKEN}`)
  const actual = Buffer.from(header)
  // Constant-time compare so response timing can't leak the token;
  // lengths must match first since timingSafeEqual throws otherwise.
  return actual.length === expected.length && timingSafeEqual(actual, expected)
}

async function readJsonBody(req: http.IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = []
  let total = 0
  for await (const chunk of req) {
    total += (chunk as Buffer).length
    if (total > MAX_BODY_BYTES) throw new Error("Request body too large")
    chunks.push(chunk as Buffer)
  }
  const raw = Buffer.concat(chunks).toString("utf8")
  return raw.trim() === "" ? undefined : JSON.parse(raw)
}

async function handleMcpPost(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
  const server = createMcpServer()
  try {
    const body = await readJsonBody(req)
    const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined })
    await server.connect(transport)
    await transport.handleRequest(req, res, body)
    res.on("close", () => {
      transport.close()
      server.close()
    })
  } catch (err) {
    console.error("Error handling MCP request:", err)
    jsonRpcError(res, 500, err instanceof Error ? err.message : "Internal server error")
  }
}

const requestListener: http.RequestListener = (req, res) => {
  if (req.url !== "/mcp") {
    jsonRpcError(res, 404, "Not found")
    return
  }
  if (!isAuthorized(req)) {
    jsonRpcError(res, 401, "Unauthorized")
    return
  }
  if (req.method === "POST") {
    void handleMcpPost(req, res)
    return
  }
  // Stateless mode doesn't support the GET (SSE resume) / DELETE
  // (session close) verbs — matches the SDK's own stateless example.
  jsonRpcError(res, 405, "Method not allowed")
}

function isLoopback(host: string): boolean {
  return host === "127.0.0.1" || host === "localhost" || host === "::1"
}

function main(): void {
  if (!TOKEN) {
    console.error(
      "[MCP HTTP] LLM_WIKI_MCP_TOKEN is not set — refusing to start a network-reachable " +
        "MCP endpoint without a shared secret. Set it (and configure the same value in " +
        "LibreChat's mcpServers headers) to enable.",
    )
    process.exit(1)
  }

  const certPath = process.env.LLM_WIKI_MCP_TLS_CERT
  const keyPath = process.env.LLM_WIKI_MCP_TLS_KEY
  const useTls = Boolean(certPath && keyPath)
  if (!useTls && !isLoopback(BIND)) {
    console.error(
      `[MCP HTTP] Binding to ${BIND} without LLM_WIKI_MCP_TLS_CERT/KEY would send the ` +
        "bearer token in plaintext over the network. Refusing to start.",
    )
    process.exit(1)
  }

  const server = useTls
    ? https.createServer(
        { cert: readFileSync(certPath as string), key: readFileSync(keyPath as string) },
        requestListener,
      )
    : http.createServer(requestListener)

  server.listen(PORT, BIND, () => {
    console.error(
      `LLM Wiki MCP HTTP server v${VERSION} listening on ${useTls ? "https" : "http"}://${BIND}:${PORT}/mcp`,
    )
  })
}

main()
