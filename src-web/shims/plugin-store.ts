/**
 * Web shim for @tauri-apps/plugin-store.
 *
 * Backed by the bridge's /store endpoints, which go through the same
 * tauri-plugin-store instance the desktop app uses — settings and the
 * recent-projects list stay consistent between desktop and web sessions.
 */
import { rpcCall } from "./rpc"

export interface StoreOptions {
  autoSave?: boolean | number
  defaults?: Record<string, unknown>
}

export class Store {
  private cache: Record<string, unknown> = {}
  private loaded: Promise<void>

  constructor(private name: string, defaults?: Record<string, unknown>) {
    this.loaded = rpcCall<Record<string, unknown>>(
      `/store/${encodeURIComponent(name)}`,
      undefined,
      "GET",
    ).then((entries) => {
      this.cache = { ...(defaults ?? {}), ...entries }
    })
  }

  async get<T>(key: string): Promise<T | undefined> {
    await this.loaded
    return this.cache[key] as T | undefined
  }

  async set(key: string, value: unknown): Promise<void> {
    await this.loaded
    this.cache[key] = value
    await rpcCall<void>(`/store/${encodeURIComponent(this.name)}`, { key, value })
  }

  async delete(key: string): Promise<boolean> {
    await this.loaded
    const existed = key in this.cache
    delete this.cache[key]
    await rpcCall<void>(`/store/${encodeURIComponent(this.name)}`, { key, value: null, delete: true })
    return existed
  }

  async keys(): Promise<string[]> {
    await this.loaded
    return Object.keys(this.cache)
  }

  async save(): Promise<void> {
    // set/delete persist immediately through the bridge
  }
}

const stores = new Map<string, Store>()

export async function load(name: string, options?: StoreOptions): Promise<Store> {
  let store = stores.get(name)
  if (!store) {
    store = new Store(name, options?.defaults)
    stores.set(name, store)
  }
  return store
}
