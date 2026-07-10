/** Web shim for @tauri-apps/plugin-opener */

export async function openUrl(url: string): Promise<void> {
  window.open(url, "_blank", "noopener,noreferrer")
}

export async function openPath(_path: string): Promise<void> {
  console.warn("[web-shim] openPath() cannot open server-side paths in a browser")
}
