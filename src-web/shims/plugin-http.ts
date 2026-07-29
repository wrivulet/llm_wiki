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
  // Same reasoning as rpc.ts's rpcCall(): without a header telling
  // oauth2-proxy this is an API call, an expired session 302s to the
  // IdP login page, fetch() follows it into a cross-origin CORS dead
  // end, and the LLM call fails with a bare "Failed to fetch" that
  // looks exactly like the LLM endpoint being unreachable. Forwarded
  // to the target LLM too, but every real LLM API ignores unknown
  // headers, so this is harmless there.
  headers.set("X-Requested-With", "XMLHttpRequest")
  if (!headers.has("Accept")) headers.set("Accept", "application/json")

  // Tauri-specific options (connectTimeout, …) have no browser
  // equivalent and are dropped; signal/method/body pass through.
  const { connectTimeout: _ct, maxRedirections: _mr, ...rest } = init ?? {}
  return globalThis.fetch("/proxy", { ...rest, headers })
}
