/** Web shim for @tauri-apps/api/core */
import { rpcCall } from "./rpc"

export type InvokeArgs = Record<string, unknown>

export interface InvokeOptions {
  headers?: Record<string, string>
}

export async function invoke<T>(cmd: string, args?: InvokeArgs, _options?: InvokeOptions): Promise<T> {
  return rpcCall<T>(`/rpc/${encodeURIComponent(cmd)}`, args ?? {})
}

/**
 * Desktop builds turn absolute local paths into asset: URLs; the web
 * build serves the same bytes through the bridge's /asset endpoint.
 */
export function convertFileSrc(filePath: string, _protocol = "asset"): string {
  return `/asset?path=${encodeURIComponent(filePath)}`
}
