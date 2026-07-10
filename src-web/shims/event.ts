/**
 * Web shim for @tauri-apps/api/event.
 *
 * Tauri events are forwarded by the RPC bridge as Server-Sent Events,
 * one EventSource per distinct event name, shared across listeners via
 * refcounting. EventSource reconnects automatically on network hiccups.
 *
 * Note: browsers cap concurrent connections per origin on HTTP/1.1 (~6);
 * deploy behind an HTTP/2-capable reverse proxy if many event streams
 * are active at once.
 */

export type UnlistenFn = () => void

export interface Event<T> {
  event: string
  id: number
  payload: T
}

export type EventCallback<T> = (event: Event<T>) => void

interface Channel {
  source: EventSource
  handlers: Set<(payload: string) => void>
}

const channels = new Map<string, Channel>()
let nextEventId = 1

function acquireChannel(name: string): Channel {
  let ch = channels.get(name)
  if (ch) return ch
  const source = new EventSource(`/events?name=${encodeURIComponent(name)}`)
  const handlers = new Set<(payload: string) => void>()
  source.onmessage = (msg) => {
    for (const handler of handlers) handler(msg.data)
  }
  ch = { source, handlers }
  channels.set(name, ch)
  return ch
}

export async function listen<T>(event: string, callback: EventCallback<T>): Promise<UnlistenFn> {
  const ch = acquireChannel(event)
  const handler = (raw: string) => {
    let payload: T
    try {
      payload = JSON.parse(raw) as T
    } catch {
      payload = raw as unknown as T
    }
    callback({ event, id: nextEventId++, payload })
  }
  ch.handlers.add(handler)
  return () => {
    ch.handlers.delete(handler)
    if (ch.handlers.size === 0) {
      ch.source.close()
      channels.delete(event)
    }
  }
}

export async function emit(_event: string, _payload?: unknown): Promise<void> {
  console.warn("[web-shim] emit() is not supported in the web build")
}
