/**
 * Web shim for @tauri-apps/plugin-http.
 *
 * Desktop builds route user-configured HTTP (LLM endpoints, embedding,
 * web search) through the Rust HTTP plugin to sidestep CORS. The web
 * build achieves the same by forwarding through the bridge's /proxy
 * endpoint: the browser talks same-origin, the backend performs the
 * real request and streams the response back (SSE chat included).
 */

export async function fetch(
  input: RequestInfo | URL,
  init?: RequestInit & { connectTimeout?: number; maxRedirections?: number },
): Promise<Response> {
  const url =
    typeof input === "string"
      ? input
      : input instanceof URL
        ? input.toString()
        : input.url

  const headers = new Headers(
    init?.headers ?? (input instanceof Request ? input.headers : undefined),
  )
  headers.set("x-llmwiki-target-url", url)

  // Tauri-specific options (connectTimeout, …) have no browser
  // equivalent and are dropped; signal/method/body pass through.
  const { connectTimeout: _ct, maxRedirections: _mr, ...rest } = init ?? {}
  return globalThis.fetch("/proxy", { ...rest, headers })
}
