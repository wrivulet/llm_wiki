//! RPC bridge for the web build.
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
//!   *    /proxy              — streaming HTTP proxy to the URL in the
//!                              x-llmwiki-target-url header (web equivalent of
//!                              tauri-plugin-http: CORS-hostile LLM endpoints)
//!   POST /upload             — receive one browser-picked file into a
//!                              server-side staging dir; the dialog shim then
//!                              returns that path so the desktop import flow
//!                              runs unchanged
//!   GET  /*                  — static files from $LLM_WIKI_WEB_DIST (SPA fallback)
//!
//! Server: axum on tauri's tokio runtime. (First cut used tiny_http, whose
//! per-connection BufWriter never flushes until a response *ends* — fatal
//! for SSE and streamed proxy bodies.)
//!
//! Security model: the bridge itself performs NO authentication and, like
//! the Tauri IPC it mirrors, accepts absolute filesystem paths. It binds
//! to 127.0.0.1 by default and MUST only be exposed through an
//! authenticating reverse proxy (see deploy/web/). Enable with
//! LLM_WIKI_WEB_ENABLE=1; override port/bind with LLM_WIKI_WEB_PORT /
//! LLM_WIKI_WEB_BIND.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Listener, Manager};
use tauri_plugin_store::StoreExt;

use crate::commands;
use crate::commands::search::SearchEmbeddingConfig;

const DEFAULT_PORT: u16 = 19829;
const MAX_BODY_BYTES: usize = 40 * 1024 * 1024;
const MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const UPLOAD_PRUNE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn start_rpc_bridge(app: AppHandle) {
    let host = std::env::var("LLM_WIKI_WEB_BIND").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("LLM_WIKI_WEB_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = format!("{host}:{port}");

    let router = Router::new()
        .route("/rpc/{command}", post(handle_rpc))
        .route("/events", get(handle_events))
        .route("/store/{name}", get(handle_store_get).post(handle_store_set))
        .route("/asset", get(handle_asset))
        .route("/upload", post(handle_upload))
        .route("/proxy", any(handle_proxy))
        .fallback(get(handle_static))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES as usize))
        .with_state(app.clone());

    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("[RPC Bridge] failed to bind {addr}: {err}");
                return;
            }
        };
        eprintln!("[RPC Bridge] listening on http://{addr}");
        if let Err(err) = axum::serve(listener, router).await {
            eprintln!("[RPC Bridge] server error: {err}");
        }
    });
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
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

fn args_field(args: Value, key: &str) -> Result<Value, DispatchError> {
    args.get(key)
        .cloned()
        .ok_or_else(|| DispatchError::Command(format!("Missing field `{key}`")))
}

fn done<T: serde::Serialize>(result: Result<T, String>) -> Result<Value, DispatchError> {
    match result {
        Ok(value) => ok(value),
        Err(err) => Err(DispatchError::Command(err)),
    }
}

async fn handle_rpc(
    State(app): State<AppHandle>,
    UrlPath(command): UrlPath<String>,
    body: String,
) -> Response {
    if body.len() > MAX_BODY_BYTES {
        return error_response(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    }
    let args: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(err) => {
                return error_response(StatusCode::BAD_REQUEST, &format!("Invalid JSON: {err}"))
            }
        }
    };
    match dispatch(&app, &command, args).await {
        Ok(value) => axum::Json(value).into_response(),
        Err(DispatchError::Unknown) => error_response(
            StatusCode::NOT_IMPLEMENTED,
            &format!("Command '{command}' is not bridged yet"),
        ),
        Err(DispatchError::Command(msg)) => error_response(StatusCode::BAD_REQUEST, &msg),
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
struct WatcherArgs {
    project_id: String,
    project_path: String,
    source_watch_config: Option<commands::file_sync::SourceWatchConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPathArgs {
    project_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileTaskArgs {
    project_id: String,
    project_path: String,
    task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileHistoryArgs {
    project_path: String,
    file_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestoreHistoryArgs {
    project_path: String,
    file_path: String,
    entry_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddingFetchArgs {
    text: String,
    cfg: SearchEmbeddingConfig,
    max_retries: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebSearchArgs {
    query: String,
    config: crate::agent::tools::WebSearchConfig,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnyTxtSearchArgs {
    query: String,
    config: crate::agent::tools::AnyTxtConfig,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageArgs {
    project_path: String,
    page_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VectorUpsertArgs {
    project_path: String,
    page_id: String,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VectorSearchArgs {
    project_path: String,
    query_embedding: Vec<f32>,
    top_k: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkUpsertArgs {
    project_path: String,
    page_id: String,
    chunks: Vec<commands::vectorstore::ChunkUpsertInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtractSaveArgs {
    source_path: String,
    dest_dir: String,
    rel_to: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentTurnArgs {
    project_id: String,
    request: crate::agent::AgentChatRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentCancelArgs {
    project_id: String,
    session_id: String,
    run_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentSessionArgs {
    project_id: String,
    session_id: String,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectIdArgs {
    project_id: String,
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

async fn dispatch(app: &AppHandle, cmd: &str, args: Value) -> Result<Value, DispatchError> {
    use commands::{file_history, file_sync, fs as cfs, project, search};
    match cmd {
        "read_file" => {
            let a: ReadFileArgs = parse(args)?;
            done(cfs::read_file(a.path, a.extract_images).await)
        }
        "write_file" => {
            let a: WriteFileArgs = parse(args)?;
            done(cfs::write_file(a.path, a.contents).await)
        }
        "write_file_atomic" => {
            let a: WriteFileArgs = parse(args)?;
            done(cfs::write_file_atomic(a.path, a.contents).await)
        }
        "write_file_base64" => {
            let a: WriteBase64Args = parse(args)?;
            done(cfs::write_file_base64(a.path, a.base64).await)
        }
        "list_directory" => {
            let a: ListDirectoryArgs = parse(args)?;
            done(cfs::list_directory(a.path, a.include_hidden, a.max_depth).await)
        }
        "copy_file" => {
            let a: SourceDestArgs = parse(args)?;
            done(cfs::copy_file(a.source, a.destination).await)
        }
        "copy_directory" => {
            let a: SourceDestArgs = parse(args)?;
            done(cfs::copy_directory(a.source, a.destination).await)
        }
        "preprocess_file" => {
            let a: PathArgs = parse(args)?;
            done(cfs::preprocess_file(a.path).await)
        }
        "delete_file" => {
            let a: PathArgs = parse(args)?;
            done(cfs::delete_file(a.path).await)
        }
        "create_directory" => {
            let a: PathArgs = parse(args)?;
            done(cfs::create_directory(a.path).await)
        }
        "file_exists" => {
            let a: PathArgs = parse(args)?;
            done(cfs::file_exists(a.path).await)
        }
        "get_file_modified_time" => {
            let a: PathArgs = parse(args)?;
            done(cfs::get_file_modified_time(a.path).await)
        }
        "get_file_size" => {
            let a: PathArgs = parse(args)?;
            done(cfs::get_file_size(a.path).await)
        }
        "get_file_md5" => {
            let a: PathArgs = parse(args)?;
            done(cfs::get_file_md5(a.path).await)
        }
        "read_file_as_base64" => {
            let a: PathArgs = parse(args)?;
            done(cfs::read_file_as_base64(a.path).await)
        }
        "find_related_wiki_pages" => {
            let a: RelatedPagesArgs = parse(args)?;
            done(cfs::find_related_wiki_pages(a.project_path, a.source_name).await)
        }
        "create_project" => {
            let a: CreateProjectArgs = parse(args)?;
            done(project::create_project(a.name, a.path))
        }
        "open_project" => {
            let a: PathArgs = parse(args)?;
            done(project::open_project(a.path))
        }
        "search_project" => {
            let a: SearchProjectArgs = parse(args)?;
            done(
                search::search_project(
                    a.project_path,
                    a.query,
                    a.top_k,
                    a.include_content,
                    a.query_embedding,
                    a.embedding_config,
                )
                .await,
            )
        }
        "list_file_history" => {
            let a: FileHistoryArgs = parse(args)?;
            done(file_history::list_file_history(a.project_path, a.file_path).await)
        }
        "restore_file_history" => {
            let a: RestoreHistoryArgs = parse(args)?;
            done(
                file_history::restore_file_history(a.project_path, a.file_path, a.entry_id).await,
            )
        }
        "start_project_file_watcher" => {
            let a: WatcherArgs = parse(args)?;
            done(file_sync::start_project_file_watcher(
                app.clone(),
                app.state(),
                a.project_id,
                a.project_path,
                a.source_watch_config,
            ))
        }
        "stop_project_file_watcher" => done(file_sync::stop_project_file_watcher(app.state())),
        "rescan_project_files" => {
            let a: WatcherArgs = parse(args)?;
            done(file_sync::rescan_project_files(
                app.clone(),
                a.project_id,
                a.project_path,
                a.source_watch_config,
            ))
        }
        "get_file_change_queue" => {
            let a: ProjectPathArgs = parse(args)?;
            done(file_sync::get_file_change_queue(a.project_path))
        }
        "retry_file_change_task" => {
            let a: FileTaskArgs = parse(args)?;
            done(file_sync::retry_file_change_task(
                app.clone(),
                a.project_id,
                a.project_path,
                a.task_id,
            ))
        }
        "ignore_file_change_task" => {
            let a: FileTaskArgs = parse(args)?;
            done(file_sync::ignore_file_change_task(
                app.clone(),
                a.project_id,
                a.project_path,
                a.task_id,
            ))
        }
        "embedding_fetch" => {
            let a: EmbeddingFetchArgs = parse(args)?;
            done(search::embedding_fetch(a.text, a.cfg, a.max_retries).await)
        }
        "web_search" => {
            let a: WebSearchArgs = parse(args)?;
            done(commands::external_search::web_search(a.query, a.config, a.max_results).await)
        }
        "anytxt_search" => {
            let a: AnyTxtSearchArgs = parse(args)?;
            done(commands::external_search::anytxt_search(a.query, a.config, a.max_results).await)
        }
        "vector_upsert" => {
            let a: VectorUpsertArgs = parse(args)?;
            done(commands::vectorstore::vector_upsert(a.project_path, a.page_id, a.embedding).await)
        }
        "vector_search" => {
            let a: VectorSearchArgs = parse(args)?;
            done(
                commands::vectorstore::vector_search(a.project_path, a.query_embedding, a.top_k)
                    .await,
            )
        }
        "vector_delete" => {
            let a: PageArgs = parse(args)?;
            done(commands::vectorstore::vector_delete(a.project_path, a.page_id).await)
        }
        "vector_count" => {
            let a: ProjectPathArgs = parse(args)?;
            done(commands::vectorstore::vector_count(a.project_path).await)
        }
        "vector_upsert_chunks" => {
            let a: ChunkUpsertArgs = parse(args)?;
            done(
                commands::vectorstore::vector_upsert_chunks(a.project_path, a.page_id, a.chunks)
                    .await,
            )
        }
        "vector_search_chunks" => {
            let a: VectorSearchArgs = parse(args)?;
            done(
                commands::vectorstore::vector_search_chunks(
                    a.project_path,
                    a.query_embedding,
                    a.top_k,
                )
                .await,
            )
        }
        "vector_delete_page" => {
            let a: PageArgs = parse(args)?;
            done(commands::vectorstore::vector_delete_page(a.project_path, a.page_id).await)
        }
        "vector_count_chunks" => {
            let a: ProjectPathArgs = parse(args)?;
            done(commands::vectorstore::vector_count_chunks(a.project_path).await)
        }
        "vector_clear_chunks" => {
            let a: ProjectPathArgs = parse(args)?;
            done(commands::vectorstore::vector_clear_chunks(a.project_path).await)
        }
        "vector_optimize_chunks" => {
            let a: ProjectPathArgs = parse(args)?;
            done(commands::vectorstore::vector_optimize_chunks(a.project_path).await)
        }
        "vector_legacy_row_count" => {
            let a: ProjectPathArgs = parse(args)?;
            done(commands::vectorstore::vector_legacy_row_count(a.project_path).await)
        }
        "vector_drop_legacy" => {
            let a: ProjectPathArgs = parse(args)?;
            done(commands::vectorstore::vector_drop_legacy(a.project_path).await)
        }
        "extract_pdf_images_cmd" => {
            let a: PathArgs = parse(args)?;
            done(commands::extract_images::extract_pdf_images_cmd(a.path).await)
        }
        "extract_office_images_cmd" => {
            let a: PathArgs = parse(args)?;
            done(commands::extract_images::extract_office_images_cmd(a.path).await)
        }
        "extract_and_save_pdf_images_cmd" => {
            let a: ExtractSaveArgs = parse(args)?;
            done(
                commands::extract_images::extract_and_save_pdf_images_cmd(
                    a.source_path,
                    a.dest_dir,
                    a.rel_to,
                )
                .await,
            )
        }
        "extract_and_save_office_images_cmd" => {
            let a: ExtractSaveArgs = parse(args)?;
            done(
                commands::extract_images::extract_and_save_office_images_cmd(
                    a.source_path,
                    a.dest_dir,
                    a.rel_to,
                )
                .await,
            )
        }
        // Agent 聊天:流式版立即返回 run_id,过程通过全局 "agent-event"
        // 事件广播,经 /events SSE 到达前端;与桌面版链路一致
        "agent_start_turn" => {
            let a: AgentTurnArgs = parse(args)?;
            done(crate::agent_start_turn(app.clone(), a.project_id, a.request).await)
        }
        "agent_start_turn_stream" => {
            let a: AgentTurnArgs = parse(args)?;
            done(crate::agent_start_turn_stream(app.clone(), a.project_id, a.request).await)
        }
        "agent_cancel_turn" => {
            let a: AgentCancelArgs = parse(args)?;
            done(crate::agent_cancel_turn(
                app.clone(),
                a.project_id,
                a.session_id,
                a.run_id,
            ))
        }
        "agent_get_session" => {
            let a: AgentSessionArgs = parse(args)?;
            done(crate::agent_get_session(
                app.clone(),
                a.project_id,
                a.session_id,
                a.limit,
            ))
        }
        "agent_list_sessions" => {
            let a: ProjectIdArgs = parse(args)?;
            done(crate::agent_list_sessions(app.clone(), a.project_id))
        }
        "agent_list_skills" => {
            let a: ProjectPathArgs = parse(args)?;
            ok(crate::agent::skills::agent_list_skills(a.project_path))
        }
        // 全局出站代理设置(Settings → Proxy);注意 /proxy 的 reqwest 客户端
        // 是首次使用时构建的,改代理后需重启进程才对桥接代理生效
        "set_proxy_env" => {
            let config: crate::proxy::ProxyConfig = parse(args_field(args, "config")?)?;
            ok(crate::proxy::apply_proxy_env(&config))
        }
        // 状态类小命令:设置页与状态栏轮询,桥接为只读透传
        "clip_server_status" => ok(crate::clip_server::get_daemon_status().to_string()),
        "api_server_status" => ok(crate::api_server::get_api_status().to_string()),
        "api_server_reload_config" => {
            crate::api_server::invalidate_config_cache();
            ok("ok".to_string())
        }
        _ => Err(DispatchError::Unknown),
    }
}

// ---------------------------------------------------------------------------
// Server-Sent Events: forward one Tauri event name per connection
// ---------------------------------------------------------------------------

/// Unlistens from the Tauri event when the SSE connection drops.
struct ListenGuard {
    app: AppHandle,
    id: tauri::EventId,
}

impl Drop for ListenGuard {
    fn drop(&mut self) {
        self.app.unlisten(self.id);
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    name: String,
}

async fn handle_events(
    State(app): State<AppHandle>,
    Query(query): Query<EventsQuery>,
) -> Response {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let id = app.listen_any(query.name, move |event| {
        let _ = tx.send(event.payload().to_string());
    });
    let guard = ListenGuard {
        app: app.clone(),
        id,
    };

    let stream = futures::stream::unfold((rx, guard), |(mut rx, guard)| async move {
        let payload = rx.recv().await?;
        Some((
            Ok::<_, std::convert::Infallible>(SseEvent::default().data(payload)),
            (rx, guard),
        ))
    });

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response()
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

async fn handle_store_get(
    State(app): State<AppHandle>,
    UrlPath(name): UrlPath<String>,
) -> Response {
    if !valid_store_name(&name) {
        return error_response(StatusCode::BAD_REQUEST, "Invalid store name");
    }
    let store = match app.store(&name) {
        Ok(store) => store,
        Err(err) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to open store: {err}"),
            )
        }
    };
    let mut entries = serde_json::Map::new();
    for key in store.keys() {
        if let Some(value) = store.get(&key) {
            entries.insert(key, value);
        }
    }
    axum::Json(Value::Object(entries)).into_response()
}

#[derive(Deserialize)]
struct StoreSetBody {
    key: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    delete: bool,
}

async fn handle_store_set(
    State(app): State<AppHandle>,
    UrlPath(name): UrlPath<String>,
    axum::Json(body): axum::Json<StoreSetBody>,
) -> Response {
    if !valid_store_name(&name) {
        return error_response(StatusCode::BAD_REQUEST, "Invalid store name");
    }
    let store = match app.store(&name) {
        Ok(store) => store,
        Err(err) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to open store: {err}"),
            )
        }
    };
    if body.delete {
        store.delete(&body.key);
    } else {
        store.set(&body.key, body.value);
    }
    if let Err(err) = store.save() {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to save store: {err}"),
        );
    }
    axum::Json(json!({ "ok": true })).into_response()
}

// ---------------------------------------------------------------------------
// Streaming HTTP proxy — web equivalent of tauri-plugin-http. Lets the
// browser frontend reach CORS-hostile LLM/search endpoints through the
// backend, mirroring the desktop trust model (any URL, user-configured).
// Auth is the reverse proxy's job; this port must stay unpublished.
// ---------------------------------------------------------------------------

const TARGET_HEADER: &str = "x-llmwiki-target-url";
/// Inbound headers never forwarded upstream: connection metadata plus the
/// oauth2-proxy session cookie (must not leak to third-party endpoints).
const SKIP_REQUEST_HEADERS: &[&str] = &[
    TARGET_HEADER,
    "host",
    "cookie",
    "connection",
    "content-length",
    "accept-encoding",
    "origin",
    "referer",
];
const SKIP_RESPONSE_HEADERS: &[&str] = &[
    "transfer-encoding",
    "connection",
    "content-length",
    "set-cookie",
];

fn proxy_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut builder = reqwest::Client::builder().connect_timeout(PROXY_CONNECT_TIMEOUT);
        // 企业内网的 LLM/搜索端点常由内部 CA 签发;rustls 默认只信任
        // webpki 公共根。通过 PEM bundle 追加信任锚(须为 CA 证书)。
        if let Ok(path) = std::env::var("LLM_WIKI_PROXY_EXTRA_CA") {
            match fs::read(&path) {
                Ok(pem) => match reqwest::Certificate::from_pem_bundle(&pem) {
                    Ok(certs) => {
                        for cert in certs {
                            builder = builder.add_root_certificate(cert);
                        }
                        eprintln!("[RPC Bridge] loaded extra proxy CA bundle from {path}");
                    }
                    Err(err) => {
                        eprintln!("[RPC Bridge] invalid extra CA bundle {path}: {err}")
                    }
                },
                Err(err) => eprintln!("[RPC Bridge] cannot read extra CA {path}: {err}"),
            }
        }
        builder
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

async fn handle_proxy(
    method: axum::http::Method,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(target) = headers
        .get(TARGET_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Missing x-llmwiki-target-url header",
        );
    };
    if !target.starts_with("http://") && !target.starts_with("https://") {
        return error_response(StatusCode::BAD_REQUEST, "Target must be an http(s) URL");
    }

    let method = match reqwest::Method::from_bytes(method.as_str().as_bytes()) {
        Ok(method) => method,
        Err(_) => return error_response(StatusCode::METHOD_NOT_ALLOWED, "Unsupported method"),
    };

    let mut upstream = proxy_client().request(method, &target);
    for (name, value) in headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if SKIP_REQUEST_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            upstream = upstream.header(name.as_str(), value);
        }
    }
    if !body.is_empty() {
        upstream = upstream.body(body);
    }

    let resp = match upstream.send().await {
        Ok(resp) => resp,
        Err(err) => {
            // reqwest 的 Display 只有一句概述;把 source 链拼进来,
            // 让 DNS 解析失败/连接被拒/TLS 校验失败等根因直接可见
            let mut detail = err.to_string();
            let mut source = std::error::Error::source(&err);
            while let Some(inner) = source {
                detail.push_str(&format!(": {inner}"));
                source = inner.source();
            }
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("Proxy request failed: {detail}"),
            );
        }
    };

    let mut builder = Response::builder().status(resp.status().as_u16());
    for (name, value) in resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if SKIP_RESPONSE_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|err| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Proxy response error: {err}"),
            )
        })
}

// ---------------------------------------------------------------------------
// Browser upload staging — backs the web dialog shim's file/folder picker.
// Each picker session gets a token dir; files land under it preserving
// relative paths, and the shim hands those server paths to the normal
// import flow (which copies them into the project's raw/sources).
// ---------------------------------------------------------------------------

fn upload_base() -> PathBuf {
    if let Ok(dir) = std::env::var("LLM_WIKI_WEB_UPLOAD_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(".llm-wiki-web-uploads")
}

fn valid_upload_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Relative path from the browser (file name or webkitRelativePath).
/// Reject anything that could escape the session dir.
fn sanitize_upload_rel(rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.len() > 1024 {
        return None;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::Normal(part) => clean.push(part),
            _ => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

/// Best-effort removal of stale staging dirs from earlier sessions.
fn prune_stale_uploads(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > UPLOAD_PRUNE_AGE);
        if stale {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

async fn handle_upload(headers: HeaderMap, body: Body) -> Response {
    let header_value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let Some(token) = header_value("x-llmwiki-upload-dir").filter(|t| valid_upload_token(t))
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Missing or invalid x-llmwiki-upload-dir",
        );
    };
    let Some(rel) = header_value("x-llmwiki-upload-name")
        .and_then(|v| percent_decode(&v))
        .and_then(|v| sanitize_upload_rel(&v))
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Missing or invalid x-llmwiki-upload-name",
        );
    };

    let base = upload_base();
    prune_stale_uploads(&base);

    let session_root = base.join(&token);
    let dest = session_root.join(&rel);
    if let Some(parent) = dest.parent() {
        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Cannot create staging dir: {err}"),
            );
        }
    }

    let mut file = match tokio::fs::File::create(&dest).await {
        Ok(file) => file,
        Err(err) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Cannot create file: {err}"),
            )
        }
    };

    use tokio::io::AsyncWriteExt;
    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                let _ = tokio::fs::remove_file(&dest).await;
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &format!("Upload stream error: {err}"),
                );
            }
        };
        written += chunk.len() as u64;
        if written > MAX_UPLOAD_BYTES {
            let _ = tokio::fs::remove_file(&dest).await;
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "File exceeds upload size limit",
            );
        }
        if let Err(err) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&dest).await;
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Upload failed: {err}"),
            );
        }
    }
    if let Err(err) = file.flush().await {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Upload flush failed: {err}"),
        );
    }

    axum::Json(json!({
        "path": dest.to_string_lossy(),
        "root": session_root.to_string_lossy(),
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Assets and static files
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AssetQuery {
    path: String,
}

async fn handle_asset(Query(query): Query<AssetQuery>) -> Response {
    serve_file(Path::new(&query.path)).await
}

async fn handle_static(uri: axum::http::Uri) -> Response {
    let Some(dist) = std::env::var_os("LLM_WIKI_WEB_DIST") else {
        return error_response(
            StatusCode::NOT_FOUND,
            "Static serving disabled (set LLM_WIKI_WEB_DIST to the dist-web directory)",
        );
    };
    let dist = PathBuf::from(dist);
    let rel = uri.path().trim_start_matches('/');
    let candidate = if rel.is_empty() {
        dist.join("index.html")
    } else {
        dist.join(rel)
    };

    let resolved = candidate
        .canonicalize()
        .ok()
        .filter(|p| p.is_file())
        .filter(|p| dist.canonicalize().map(|d| p.starts_with(d)).unwrap_or(false));

    match resolved {
        Some(file) => serve_file(&file).await,
        // SPA fallback: unknown non-file routes get index.html
        None => serve_file(&dist.join("index.html")).await,
    }
}

async fn serve_file(path: &Path) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, content_type_for(path))],
            bytes,
        )
            .into_response(),
        Err(err) => error_response(StatusCode::NOT_FOUND, &format!("Cannot read file: {err}")),
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
