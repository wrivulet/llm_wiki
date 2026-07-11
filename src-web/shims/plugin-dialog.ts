/**
 * Web shim for @tauri-apps/plugin-dialog.
 *
 * open() uses the browser's native file/folder picker, uploads the
 * selection to the bridge's /upload staging area on the server, and
 * returns the resulting server-side paths — so the import flow that
 * follows (copy into raw/sources, ingest queue) runs unchanged.
 * Folder picks preserve the directory structure via webkitRelativePath.
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

interface UploadResult {
  path: string
  root: string
}

async function uploadFile(token: string, file: File): Promise<UploadResult> {
  const rel = (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name
  const res = await fetch("/upload", {
    method: "POST",
    headers: {
      "content-type": "application/octet-stream",
      "x-llmwiki-upload-dir": token,
      "x-llmwiki-upload-name": encodeURIComponent(rel),
    },
    body: file,
    credentials: "same-origin",
  })
  if (!res.ok) {
    throw new Error(`上传 ${rel} 失败: ${await res.text()}`)
  }
  return (await res.json()) as UploadResult
}

export async function open(
  options: OpenDialogOptions = {},
): Promise<string | string[] | null> {
  // 目录模式的两种语义:
  //  - 导入内容(sources 的 Import Folder):上传本机文件夹 ✓
  //  - 选择服务器位置(打开/新建项目、定时导入监控目录):必须是服务器路径
  // 调用方无法区分,交给用户选择。
  if (options.directory) {
    const uploadLocal = window.confirm(
      `${options.title ?? "选择目录"}\n\n` +
        "「确定」= 选择并上传本机文件夹(用于导入资料)\n" +
        "「取消」= 手动输入服务器上的目录路径(用于打开/新建项目等)",
    )
    if (!uploadLocal) {
      const value = window.prompt("请输入服务器上的目录绝对路径(位于 /data 下):", options.defaultPath ?? "/data/")
      return value ? value : null
    }
  }
  return new Promise((resolve, reject) => {
    const input = document.createElement("input")
    input.type = "file"
    if (options.directory) {
      input.setAttribute("webkitdirectory", "")
    } else {
      if (options.multiple) input.multiple = true
      const extensions = options.filters?.flatMap((f) => f.extensions) ?? []
      if (extensions.length) {
        input.accept = extensions.map((e) => `.${e}`).join(",")
      }
    }
    input.style.display = "none"
    document.body.appendChild(input)

    const finish = <T>(fn: (value: T) => void, value: T) => {
      input.remove()
      fn(value)
    }

    input.addEventListener("cancel", () => finish(resolve, null))
    input.addEventListener("change", () => {
      void (async () => {
        const files = Array.from(input.files ?? [])
        if (files.length === 0) return finish(resolve, null)
        const token = crypto.randomUUID()
        try {
          const results: UploadResult[] = []
          for (const file of files) {
            results.push(await uploadFile(token, file))
          }
          if (options.directory) {
            // 服务器暂存目录 + 所选文件夹顶层名 = 等价的"所选目录"路径
            const first = files[0] as File & { webkitRelativePath?: string }
            const top = (first.webkitRelativePath || first.name).split("/")[0]
            finish(resolve, `${results[0].root}/${top}`)
          } else {
            const paths = results.map((r) => r.path)
            finish(resolve, options.multiple ? paths : paths[0])
          }
        } catch (err) {
          finish(reject, err)
        }
      })()
    })

    input.click()
  })
}

export async function message(
  text: string,
  _options?: string | MessageDialogOptions,
): Promise<void> {
  window.alert(text)
}
