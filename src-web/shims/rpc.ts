/**
 * Shared transport for the web build. Every Tauri IPC call becomes an
 * HTTP request against the in-app RPC bridge (src-tauri/src/rpc_bridge.rs),
 * same-origin in production (behind oauth2-proxy) and proxied by the Vite
 * dev server during development.
 */

export const RPC_BASE = ""

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
