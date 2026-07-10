//! RPC bridge for the web build (POC).
//!
//! Exposes a subset of the Tauri commands over plain HTTP so the web
//! frontend (built with `npm run build:web`, using src-web/shims/*) can
//! run in a browser against this process:
//!
//!   POST /rpc/{command}      — invoke a command, JSON body = camelCase args
//!   GET  /events?name={ev}   — Server-Sent Events stream of one Tauri event
//!   GET  /store/{name}       — all entries of a tauri-plugin-store file
//!   POST /store/{name}       — {key, value, delete?} set/delete one entry
//!   GET  /asset?path={p}     — raw file bytes (convertFileSrc equivalent)
//!   GET  /*                  — static files from $LLM_WIKI_WEB_DIST (SPA fallback)
//!
//! Security model: the bridge itself performs NO authentication and, like
//! the Tauri IPC it mirrors, accepts absolute filesystem paths. It binds
//! to 127.0.0.1 by default and MUST only be exposed through an
//! authenticating reverse proxy (see deploy/web/). Enable with
//! LLM_WIKI_WEB_ENABLE=1; override port/bind with LLM_WIKI_WEB_PORT /
//! LLM_WIKI_WEB_BIND.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Listener};
use tauri_plugin_store::StoreExt;
use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::commands;
use crate::commands::search::SearchEmbeddingConfig;

const DEFAULT_PORT: u16 = 19829;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

pub fn start_rpc_bridge(app: AppHandle) {
    let host = std::env::var("LLM_WIKI_WEB_BIND").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("LLM_WIKI_WEB_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = format!("{host}:{port}");

    thread::spawn(move || {
        let server = match Server::http(&addr) {
            Ok(server) => server,
            Err(err) => {
                eprintln!("[RPC Bridge] failed to bind {addr}: {err}");
                return;
            }
        };
        eprintln!("[RPC Bridge] listening on http://{addr}");

        for request in server.incoming_requests() {
            let app = app.clone();
            thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_request(app, request);
                }));
                if let Err(payload) = result {
                    eprintln!("[RPC Bridge] request handler panicked: {payload:?}");
                }
            });
        }
    });
}

fn handle_request(app: AppHandle, mut request: tiny_http::Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (url.clone(), None),
    };

    match (&method, path.as_str()) {
        (Method::Post, p) if p.starts_with("/rpc/") => {
            let cmd = p.trim_start_matches("/rpc/").to_string();
            let body = match read_body(&mut request) {
                Ok(body) => body,
                Err(err) => return respond_error(request, 400, &err),
            };
            let args: Value = if body.trim().is_empty() {
                json!({})
            } else {
                match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(err) => return respond_error(request, 400, &format!("Invalid JSON: {err}")),
                }
            };
            match dispatch(&app, &cmd, args) {
                Ok(value) => respond_json(request, 200, value),
                Err(DispatchError::Unknown) => respond_error(
                    request,
                    501,
                    &format!("Command '{cmd}' is not bridged yet"),
                ),
                Err(DispatchError::Command(msg)) => respond_error(request, 400, &msg),
            }
        }
        (Method::Get, "/events") => handle_events(app, request, query.as_deref()),
        (Method::Get, p) if p.starts_with("/store/") => {
            let name = p.trim_start_matches("/store/").to_string();
            handle_store_get(app, request, &name)
        }
        (Method::Post, p) if p.starts_with("/store/") => {
            let name = p.trim_start_matches("/store/").to_string();
            let body = match read_body(&mut request) {
                Ok(body) => body,
                Err(err) => return respond_error(request, 400, &err),
            };
            handle_store_set(app, request, &name, &body)
        }
        (Method::Get, "/asset") => handle_asset(request, query.as_deref()),
        (Method::Get, _) => handle_static(request, &path),
        _ => respond_error(request, 405, "Method not allowed"),
    }
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

enum DispatchError {
    Unknown,
    Command(String),
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, DispatchError> {
    serde_json::from_value(args).map_err(|e| DispatchError::Command(format!("Invalid args: {e}")))
}

fn ok<T: serde::Serialize>(value: T) -> Result<Value, DispatchError> {
    serde_json::to_value(value).map_err(|e| DispatchError::Command(e.to_string()))
}

fn run<T: serde::Serialize>(
    fut: impl std::future::Future<Output = Result<T, String>>,
) -> Result<Value, DispatchError> {
    match tauri::async_runtime::block_on(fut) {
        Ok(value) => ok(value),
        Err(err) => Err(DispatchError::Command(err)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadFileArgs {
    path: String,
    extract_images: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileArgs {
    path: String,
    contents: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteBase64Args {
    path: String,
    base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListDirectoryArgs {
    path: String,
    include_hidden: Option<bool>,
    max_depth: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceDestArgs {
    source: String,
    destination: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelatedPagesArgs {
    project_path: String,
    source_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectArgs {
    name: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchProjectArgs {
    project_path: String,
    query: String,
    top_k: Option<usize>,
    include_content: Option<bool>,
    query_embedding: Option<Vec<f32>>,
    embedding_config: Option<SearchEmbeddingConfig>,
}

fn dispatch(_app: &AppHandle, cmd: &str, args: Value) -> Result<Value, DispatchError> {
    use commands::{fs as cfs, project, search};
    match cmd {
        "read_file" => {
            let a: ReadFileArgs = parse(args)?;
            run(cfs::read_file(a.path, a.extract_images))
        }
        "write_file" => {
            let a: WriteFileArgs = parse(args)?;
            run(cfs::write_file(a.path, a.contents))
        }
        "write_file_atomic" => {
            let a: WriteFileArgs = parse(args)?;
            run(cfs::write_file_atomic(a.path, a.contents))
        }
        "write_file_base64" => {
            let a: WriteBase64Args = parse(args)?;
            run(cfs::write_file_base64(a.path, a.base64))
        }
        "list_directory" => {
            let a: ListDirectoryArgs = parse(args)?;
            run(cfs::list_directory(a.path, a.include_hidden, a.max_depth))
        }
        "copy_file" => {
            let a: SourceDestArgs = parse(args)?;
            run(cfs::copy_file(a.source, a.destination))
        }
        "copy_directory" => {
            let a: SourceDestArgs = parse(args)?;
            run(cfs::copy_directory(a.source, a.destination))
        }
        "preprocess_file" => {
            let a: PathArgs = parse(args)?;
            run(cfs::preprocess_file(a.path))
        }
        "delete_file" => {
            let a: PathArgs = parse(args)?;
            run(cfs::delete_file(a.path))
        }
        "create_directory" => {
            let a: PathArgs = parse(args)?;
            run(cfs::create_directory(a.path))
        }
        "file_exists" => {
            let a: PathArgs = parse(args)?;
            run(cfs::file_exists(a.path))
        }
        "get_file_modified_time" => {
            let a: PathArgs = parse(args)?;
            run(cfs::get_file_modified_time(a.path))
        }
        "get_file_size" => {
            let a: PathArgs = parse(args)?;
            run(cfs::get_file_size(a.path))
        }
        "get_file_md5" => {
            let a: PathArgs = parse(args)?;
            run(cfs::get_file_md5(a.path))
        }
        "read_file_as_base64" => {
            let a: PathArgs = parse(args)?;
            run(cfs::read_file_as_base64(a.path))
        }
        "find_related_wiki_pages" => {
            let a: RelatedPagesArgs = parse(args)?;
            run(cfs::find_related_wiki_pages(a.project_path, a.source_name))
        }
        "create_project" => {
            let a: CreateProjectArgs = parse(args)?;
            match project::create_project(a.name, a.path) {
                Ok(v) => ok(v),
                Err(e) => Err(DispatchError::Command(e)),
            }
        }
        "open_project" => {
            let a: PathArgs = parse(args)?;
            match project::open_project(a.path) {
                Ok(v) => ok(v),
                Err(e) => Err(DispatchError::Command(e)),
            }
        }
        "search_project" => {
            let a: SearchProjectArgs = parse(args)?;
            run(search::search_project(
                a.project_path,
                a.query,
                a.top_k,
                a.include_content,
                a.query_embedding,
                a.embedding_config,
            ))
        }
        _ => Err(DispatchError::Unknown),
    }
}

// ---------------------------------------------------------------------------
// Server-Sent Events: forward one Tauri event name per connection
// ---------------------------------------------------------------------------

/// Blocking reader that turns forwarded event payloads into an SSE byte
/// stream. Dropping it (client disconnect) unlistens from the app event.
struct SseReader {
    rx: mpsc::Receiver<String>,
    pending: Vec<u8>,
    app: AppHandle,
    event_id: tauri::EventId,
}

impl Drop for SseReader {
    fn drop(&mut self) {
        self.app.unlisten(self.event_id);
    }
}

impl Read for SseReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.rx.recv_timeout(SSE_KEEPALIVE) {
                Ok(payload) => {
                    self.pending = format!("data: {payload}\n\n").into_bytes();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // SSE comment as keepalive; also lets a dead socket
                    // surface as a write error so we can unlisten.
                    self.pending = b": keepalive\n\n".to_vec();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = self.pending.len().min(buf.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

fn handle_events(app: AppHandle, request: tiny_http::Request, query: Option<&str>) {
    let Some(name) = query_param(query, "name") else {
        return respond_error(request, 400, "Missing ?name= event name");
    };

    let (tx, rx) = mpsc::channel::<String>();
    let event_id = app.listen_any(name, move |event| {
        let _ = tx.send(event.payload().to_string());
    });

    let reader = SseReader {
        rx,
        pending: Vec::new(),
        app,
        event_id,
    };
    let response = Response::new(StatusCode(200), Vec::new(), reader, None, None)
        .with_header(header("Content-Type", "text/event-stream"))
        .with_header(header("Cache-Control", "no-cache"))
        .with_header(header("X-Accel-Buffering", "no"));
    let _ = request.respond(response);
}

// ---------------------------------------------------------------------------
// Store bridge (same plugin-store instance as the desktop UI)
// ---------------------------------------------------------------------------

fn valid_store_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.contains("..")
}

fn handle_store_get(app: AppHandle, request: tiny_http::Request, name: &str) {
    if !valid_store_name(name) {
        return respond_error(request, 400, "Invalid store name");
    }
    let store = match app.store(name) {
        Ok(store) => store,
        Err(err) => return respond_error(request, 500, &format!("Failed to open store: {err}")),
    };
    let mut entries = serde_json::Map::new();
    for key in store.keys() {
        if let Some(value) = store.get(&key) {
            entries.insert(key, value);
        }
    }
    respond_json(request, 200, Value::Object(entries));
}

#[derive(Deserialize)]
struct StoreSetBody {
    key: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    delete: bool,
}

fn handle_store_set(app: AppHandle, request: tiny_http::Request, name: &str, body: &str) {
    if !valid_store_name(name) {
        return respond_error(request, 400, "Invalid store name");
    }
    let parsed: StoreSetBody = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(err) => return respond_error(request, 400, &format!("Invalid JSON: {err}")),
    };
    let store = match app.store(name) {
        Ok(store) => store,
        Err(err) => return respond_error(request, 500, &format!("Failed to open store: {err}")),
    };
    if parsed.delete {
        store.delete(&parsed.key);
    } else {
        store.set(&parsed.key, parsed.value);
    }
    if let Err(err) = store.save() {
        return respond_error(request, 500, &format!("Failed to save store: {err}"));
    }
    respond_json(request, 200, json!({ "ok": true }));
}

// ---------------------------------------------------------------------------
// Assets and static files
// ---------------------------------------------------------------------------

fn handle_asset(request: tiny_http::Request, query: Option<&str>) {
    let Some(path) = query_param(query, "path") else {
        return respond_error(request, 400, "Missing ?path=");
    };
    serve_file(request, Path::new(&path));
}

fn handle_static(request: tiny_http::Request, url_path: &str) {
    let Some(dist) = std::env::var_os("LLM_WIKI_WEB_DIST") else {
        return respond_error(
            request,
            404,
            "Static serving disabled (set LLM_WIKI_WEB_DIST to the dist-web directory)",
        );
    };
    let dist = PathBuf::from(dist);
    let rel = url_path.trim_start_matches('/');
    let candidate = if rel.is_empty() { dist.join("index.html") } else { dist.join(rel) };

    let resolved = candidate
        .canonicalize()
        .ok()
        .filter(|p| p.is_file())
        .filter(|p| dist.canonicalize().map(|d| p.starts_with(d)).unwrap_or(false));

    match resolved {
        Some(file) => serve_file(request, &file),
        // SPA fallback: unknown non-file routes get index.html
        None => serve_file(request, &dist.join("index.html")),
    }
}

fn serve_file(request: tiny_http::Request, path: &Path) {
    match fs::read(path) {
        Ok(bytes) => {
            let response = Response::from_data(bytes)
                .with_header(header("Content-Type", content_type_for(path)));
            let _ = request.respond(response);
        }
        Err(err) => respond_error(request, 404, &format!("Cannot read file: {err}")),
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "woff2" => "font/woff2",
        "md" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Small HTTP helpers
// ---------------------------------------------------------------------------

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header")
}

fn read_body(request: &mut tiny_http::Request) -> Result<String, String> {
    let mut limited = request.as_reader().take(MAX_BODY_BYTES as u64 + 1);
    let mut body = String::new();
    limited
        .read_to_string(&mut body)
        .map_err(|e| format!("Failed to read body: {e}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err("Request body too large".to_string());
    }
    Ok(body)
}

fn respond_json(request: tiny_http::Request, status: u16, body: Value) {
    let data = body.to_string();
    let response = Response::from_string(data)
        .with_status_code(StatusCode(status))
        .with_header(header("Content-Type", "application/json"));
    let _ = request.respond(response);
}

fn respond_error(request: tiny_http::Request, status: u16, message: &str) {
    respond_json(request, status, json!({ "error": message }));
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return percent_decode(v);
        }
    }
    None
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hex = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}
