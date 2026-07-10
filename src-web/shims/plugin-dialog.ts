/**
 * Web shim for @tauri-apps/plugin-dialog.
 *
 * The desktop file dialog picks paths on the machine where the backend
 * runs. In the web build the backend filesystem lives on the server, so
 * for the POC we ask for a server-side path via prompt(). A proper
 * server-side file browser component is a follow-up milestone.
 */

export interface DialogFilter {
  name: string
  extensions: string[]
}

export interface OpenDialogOptions {
  title?: string
  directory?: boolean
  multiple?: boolean
  filters?: DialogFilter[]
  defaultPath?: string
}

export interface MessageDialogOptions {
  title?: string
  kind?: "info" | "warning" | "error"
  okLabel?: string
}

export async function open(
  options: OpenDialogOptions = {},
): Promise<string | string[] | null> {
  const kind = options.directory ? "目录" : "文件"
  const hint = options.filters?.length
    ? `(${options.filters.map((f) => f.extensions.join("/")).join(", ")})`
    : ""
  const value = window.prompt(
    `${options.title ?? "选择" + kind}${hint}\n请输入服务器上的绝对路径:`,
    options.defaultPath ?? "",
  )
  if (!value) return null
  return options.multiple ? [value] : value
}

export async function message(
  text: string,
  _options?: string | MessageDialogOptions,
): Promise<void> {
  window.alert(text)
}
