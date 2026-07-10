# LLM Wiki Web 版部署(POC)

在服务器上以 headless 方式运行桌面应用,通过进程内 RPC 桥接把 Tauri IPC
暴露为 HTTP/SSE,前端用 shim 替换 `@tauri-apps/*` 后跑在浏览器里,认证由
oauth2-proxy 对接 internal-idp(OIDC)完成。

```
浏览器 ──HTTPS──▶ oauth2-proxy(:4180, OIDC ⇆ internal-idp)
                     │ 认证通过后反代
                     ▼
              RPC 桥接(127.0.0.1:19829,进程内)
              ├── /rpc/{command}   命令调用(POST, JSON)
              ├── /events?name=…   Tauri 事件 → SSE
              ├── /store/{name}    plugin-store 读写(桌面/Web 共享)
              ├── /asset?path=…    本地文件字节(convertFileSrc 等价物)
              └── /*               dist-web 静态文件(SPA fallback)
                     ▲
        llm_wiki Tauri 应用(xvfb-run,全部业务逻辑不变)
```

## 与上游同步的约定

- 除 `src-tauri/src/lib.rs` 的 3 行挂载代码和 `package.json` 的 2 个 script
  外,Web 版全部是新增文件(`src-web/`、`vite.config.web.ts`、
  `src-tauri/src/rpc_bridge.rs`、`deploy/web/`)。
- `main` 分支保持上游镜像(`git merge --ff-only upstream/main`),
  Web 工作在 `web` 分支,定期 `git merge main`。
- 上游改动命令签名时,`rpc_bridge.rs` 的分发表会编译失败,按编译错误跟进即可。

## 构建

```bash
# 前端(本机只需 Node)
npm ci
npm run build:web            # 输出 dist-web/

# 后端(需要 Rust 工具链 + Linux WebKit 依赖)
# dnf install gtk3-devel webkit2gtk4.1-devel libappindicator-gtk3-devel \
#             librsvg2-devel xorg-x11-server-Xvfb
npm run build:desktop        # 或 cargo build --release -p llm-wiki(见 src-tauri)
```

## 运行

```bash
LLM_WIKI_WEB_ENABLE=1 \
LLM_WIKI_WEB_DIST=/opt/llm-wiki/dist-web \
xvfb-run -a /opt/llm-wiki/llm-wiki
```

环境变量:

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LLM_WIKI_WEB_ENABLE` | 未启用 | 设为 `1` 才启动桥接,桌面用户不受影响 |
| `LLM_WIKI_WEB_PORT` | `19829` | 桥接端口 |
| `LLM_WIKI_WEB_BIND` | `127.0.0.1` | 绑定地址;**保持默认**,对外暴露只走 oauth2-proxy |
| `LLM_WIKI_WEB_DIST` | 未设置 | `dist-web` 目录路径,未设置则不托管静态文件 |

## OIDC 认证(internal-idp)

1. 在平台侧注册客户端:`deploy/web/oauth2-clients-llm-wiki.yml`
   (用 `OAuth2ClientRegistrar` CLI 执行,记下打印的 client_secret)。
2. 填好 `deploy/web/oauth2-proxy.cfg`(client_secret、cookie_secret、域名),
   启动 `oauth2-proxy --config deploy/web/oauth2-proxy.cfg`。
3. 外层再加 TLS(Caddy/nginx 反代 :4180,或让 oauth2-proxy 直接持证书)。

## 开发

```bash
LLM_WIKI_WEB_ENABLE=1 npm run tauri dev   # 起后端(桥接监听 :19829)
npm run dev:web                            # 起 web 前端,自动代理桥接端点
```

## POC 范围与已知限制

- 已桥接命令:文件系统全套、`open_project`/`create_project`、`search_project`。
  未桥接的命令返回 501(Agent 聊天流用的是 Tauri Channel,需要后续把
  `agent_start_turn_stream` 改走 SSE)。
- 文件选择对话框在 Web 端暂用 prompt 输入服务器路径,后续需要做
  服务器端文件浏览组件。
- 桥接本身无认证、接受绝对路径(与桌面 IPC 同权),**必须**只绑定
  127.0.0.1 并置于 oauth2-proxy 之后;这是单用户模型,不做多租户隔离。
- 浏览器对 HTTP/1.1 同源并发连接有 ~6 个上限,事件流较多时请确保
  外层代理启用 HTTP/2。
