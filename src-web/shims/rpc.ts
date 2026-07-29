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

/**
 * Thrown when oauth2-proxy has already rejected the request with a
 * clean 401 (see the X-Requested-With header below) — i.e. the
 * browser's session expired mid-use. Distinct from a generic RpcError
 * so callers/UI can tell "log in again" apart from "something broke."
 */
export const SESSION_EXPIRED_MESSAGE =
  "Your session has expired. Please refresh the page and sign in again."

export async function rpcCall<T>(path: string, body?: unknown, method = "POST"): Promise<T> {
  const res = await fetch(`${RPC_BASE}${path}`, {
    method,
    headers: {
      "Content-Type": "application/json",
      // Without these, an expired oauth2-proxy session 302s to the IdP
      // login page instead of returning 401. fetch() follows that
      // redirect by default, lands on a cross-origin page with no CORS
      // headers, and the promise rejects with a bare "Failed to fetch"
      // — indistinguishable from the target LLM endpoint being down,
      // and the reason a stale-session failure kept getting misread as
      // an LLM/embedding connectivity problem. These two headers are
      // the standard "this is an API call, not a browser navigation"
      // signal auth proxies (oauth2-proxy included) check before
      // deciding whether to redirect or just 401.
      "Accept": "application/json",
      "X-Requested-With": "XMLHttpRequest",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  })
  if (res.status === 401) {
    // This endpoint has no auth of its own (that's entirely
    // oauth2-proxy's job) — a 401 here can only be oauth2-proxy
    // rejecting an expired session, never something our own bridge
    // emits, so it's unambiguous.
    throw SESSION_EXPIRED_MESSAGE
  }
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
