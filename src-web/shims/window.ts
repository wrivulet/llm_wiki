/**
 * Web shim for @tauri-apps/api/window.
 *
 * All call sites in src/ guard on isTauriRuntime() before touching the
 * native window, so these are compile-time stand-ins that no-op safely.
 */

export type Theme = "light" | "dark"

class WebWindow {
  async setTheme(_theme: Theme | null): Promise<void> {}
  async setBackgroundColor(_color: string): Promise<void> {}
  async theme(): Promise<Theme> {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
  }
}

const current = new WebWindow()

export function getCurrentWindow(): WebWindow {
  return current
}
