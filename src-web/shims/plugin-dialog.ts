/**
 * Web shim for @tauri-apps/plugin-dialog.
 *
 * open() uses the browser's native file/folder picker, uploads the
 * selection to the bridge's /upload staging area on the server, and
 * returns the resulting server-side paths — so the import flow that
 * follows (copy into raw/sources, ingest queue) runs unchanged.
 * Folder picks preserve the directory structure via webkitRelativePath.
 *
 * Directory mode has two call-site semantics the API can't distinguish:
 * importing content (upload a local folder) vs choosing a server-side
 * location (open/create project, scheduled-import watch dir). We ask via
 * an in-page overlay — NOT window.confirm(): native modals consume the
 * transient user activation, after which input.click() is silently
 * ignored by the browser. Overlay button clicks are fresh gestures, so
 * the file picker must be triggered synchronously inside their handlers.
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

/**
 * Opens the native picker and uploads the selection. MUST be called
 * synchronously from a user-gesture handler (click), or the browser
 * ignores input.click().
 */
function pickAndUpload(
  options: OpenDialogOptions,
): Promise<string | string[] | null> {
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

/** In-page chooser for directory mode; overlay clicks are fresh gestures. */
function openDirectoryFlow(
  options: OpenDialogOptions,
): Promise<string | string[] | null> {
  return new Promise((resolve, reject) => {
    const overlay = document.createElement("div")
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,.55);z-index:2147483647;" +
      "display:flex;align-items:center;justify-content:center;font-family:system-ui,sans-serif"
    const box = document.createElement("div")
    box.style.cssText =
      "background:#fff;color:#1a1a1a;border-radius:10px;padding:20px 24px;" +
      "width:340px;max-width:90vw;font-size:14px;line-height:1.6;" +
      "box-shadow:0 10px 40px rgba(0,0,0,.35)"

    const title = document.createElement("div")
    title.textContent = options.title ?? "选择目录"
    title.style.cssText = "font-weight:600;font-size:15px;margin-bottom:12px"

    const makeButton = (label: string, primary: boolean) => {
      const btn = document.createElement("button")
      btn.type = "button"
      btn.textContent = label
      btn.style.cssText =
        "display:block;width:100%;margin-top:8px;padding:9px 12px;border-radius:8px;" +
        "font-size:14px;cursor:pointer;border:1px solid " +
        (primary
          ? "#2563eb;background:#2563eb;color:#fff"
          : "#d1d5db;background:#f9fafb;color:#1a1a1a")
      return btn
    }

    const uploadBtn = makeButton("上传本机文件夹(导入资料)", true)
    const serverBtn = makeButton("输入服务器目录路径(项目位置等)", false)
    const cancelBtn = makeButton("取消", false)

    const close = () => overlay.remove()

    uploadBtn.onclick = () => {
      close()
      // 同步调用:此刻处于按钮点击手势内,文件选择器才被允许弹出
      pickAndUpload(options).then(resolve, reject)
    }
    serverBtn.onclick = () => {
      close()
      const value = window.prompt(
        "请输入服务器上的目录绝对路径(位于 /data 下):",
        options.defaultPath ?? "/data/",
      )
      resolve(value ? value : null)
    }
    cancelBtn.onclick = () => {
      close()
      resolve(null)
    }
    overlay.onclick = (e) => {
      if (e.target === overlay) {
        close()
        resolve(null)
      }
    }

    box.append(title, uploadBtn, serverBtn, cancelBtn)
    overlay.append(box)
    document.body.appendChild(overlay)
  })
}

export async function open(
  options: OpenDialogOptions = {},
): Promise<string | string[] | null> {
  if (options.directory) return openDirectoryFlow(options)
  return pickAndUpload(options)
}

export async function message(
  text: string,
  _options?: string | MessageDialogOptions,
): Promise<void> {
  window.alert(text)
}
