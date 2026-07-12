/**
 * Shared transport for the web build. Every Tauri IPC call becomes an
 * HTTP request against the in-app RPC bridge (src-tauri/src/rpc_bridge.rs),
 * same-origin in production (behind oauth2-proxy) and proxied by the Vite
 * dev server during development.
 */

export const RPC_BASE = ""

/**
 * The desktop app polls its local Web-Clipper daemon (127.0.0.1:19827)
 * with plain fetch every 3s. In the web build that hits the *user's*
 * machine, which runs nothing — endless console errors. Intercept that
 * host and answer with an empty-but-well-formed payload so the watcher
 * idles silently. (Clipper capture is a desktop-only feature.)
 */
const CLIP_SERVER_PREFIX = "http://127.0.0.1:19827/"
const nativeFetch = globalThis.fetch.bind(globalThis)
globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
  const url =
    typeof input === "string"
      ? input
      : input instanceof URL
        ? input.toString()
        : input.url
  if (url.startsWith(CLIP_SERVER_PREFIX)) {
    return Promise.resolve(
      new Response(JSON.stringify({ ok: false, clips: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    )
  }
  return nativeFetch(input as RequestInfo, init)
}) as typeof globalThis.fetch

export class RpcError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "RpcError"
  }
}

export async function rpcCall<T>(path: string, body?: unknown, method = "POST"): Promise<T> {
  const res = await fetch(`${RPC_BASE}${path}`, {
    method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  })
  const text = await res.text()
  if (!res.ok) {
    let message = text
    try {
      const parsed = JSON.parse(text) as { error?: string }
      if (parsed && typeof parsed.error === "string") message = parsed.error
    } catch {
      // keep raw text
    }
    // Tauri's invoke() rejects with the command's Err(String) payload;
    // callers throughout src/ rely on catching plain strings.
    throw message
  }
  if (text === "") return undefined as T
  return JSON.parse(text) as T
}
