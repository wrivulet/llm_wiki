/**
 * Web shim for @tauri-apps/plugin-autostart.
 * Autostart is meaningless in a browser; the server process is managed
 * by systemd/docker instead. All calls no-op.
 */

export async function enable(): Promise<void> {}

export async function disable(): Promise<void> {}

export async function isEnabled(): Promise<boolean> {
  return false
}
