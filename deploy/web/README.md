# LLM Wiki Web 版部署(POC)

在服务器上以 headless 方式运行桌面应用,通过进程内 RPC 桥接把 Tauri IPC
暴露为 HTTP/SSE,前端用 shim 替换 `@tauri-apps/*` 后跑在浏览器里,认证由
oauth2-proxy 对接 internal-idp(OIDC)完成。

```
浏览器 ──HTTPS:7180──▶ oauth2-proxy(终结 TLS, OIDC ⇆ internal-idp)
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

## 真实部署值:模板文件 + .local 覆盖(此仓库是公开 fork)

`docker-stack.yml`、`oauth2-proxy.cfg`、`oauth2-clients-llm-wiki.yml` 里的域名
(`example.internal`)、IdP 项目名(`internal-idp`)等都是**占位符**——本仓库
是上游项目的公开 fork,不能把内部域名/主机名提交进去。

真正部署时:

```bash
cp deploy/web/docker-stack.yml deploy/web/docker-stack.local.yml
cp deploy/web/oauth2-proxy.cfg deploy/web/oauth2-proxy.local.cfg
cp deploy/web/oauth2-clients-llm-wiki.yml deploy/web/oauth2-clients-llm-wiki.local.yml
# 编辑这三个 .local.* 文件,把占位符换成真实值
```

`*.local.yml`/`*.local.cfg` 已加入 `.gitignore`,不会被提交。部署/运行时把
下文命令里的文件名换成对应的 `.local.*` 版本(如
`docker stack deploy -c deploy/web/docker-stack.local.yml llm-wiki`)。

**维护提醒**:以后这三个模板文件结构变化(新增/改名字段)时,记得手动把
同样的改动同步到你自己的 `.local.*` 文件里——这几个文件不支持自动合并,
`.local.*` 是完整复制而非增量补丁,改了模板不会自动带过去。

## Docker 部署(推荐)

```bash
# 构建镜像(仓库根目录;三阶段:前端 → Rust 编译 → xvfb 运行时)
docker build -f deploy/web/Dockerfile -t da2.example.internal:9350/llm-wiki-web:<version> .
docker push da2.example.internal:9350/llm-wiki-web:<version>

# 创建 Swarm secrets
printf '%s' '<OAuth2ClientRegistrar 打印的 client-secret>' | \
  docker secret create llm_wiki_oidc_client_secret -
# cookie secret 必须是 URL-safe base64(+/ 换成 -_),否则 oauth2-proxy 报长度错误
openssl rand -base64 32 | tr -d '\n' | tr -- '+/' '-_' | \
  docker secret create llm_wiki_cookie_secret -

# 部署 stack(先按环境改 docker-stack.yml 里的 issuer / redirect)
LLM_WIKI_IMAGE=da2.example.internal:9350/llm-wiki-web:<version> \
  docker stack deploy -c deploy/web/docker-stack.yml llm-wiki
```

要点:

- `llm-wiki` 服务不发布端口,只在 overlay 网络内被 oauth2-proxy 反代;
  对外只暴露 oauth2-proxy 的 7180(HTTPS,直接终结 TLS,证书走 Swarm
  secrets `llm_wiki_tls_cert`/`llm_wiki_tls_key`,PEM 格式含完整证书链)。
- 有状态单用户应用,`replicas` 必须为 1;项目数据和应用状态都在
  `llm-wiki-data` 卷的 `/data` 下,Web 端"打开项目"填的路径也应在
  `/data` 下(如 `/data/projects/my-wiki`)。本地卷需固定 placement 节点。
- 镜像内置 pdfium(`PDFIUM_DYNAMIC_LIB_PATH`)和 dist-web 静态托管;
  未包含 Node 运行时与 mcp-server(容器场景用不到桌面侧 MCP 分发)。
- 处理中文 PDF 需要渲染字体时,可在运行时阶段追加 `fonts-noto-cjk`。
- **容器必须带 init 进程**(stack 已配 `init: true`):`xvfb-run` 作为 PID 1 时
  Xvfb 的 SIGUSR1 就绪握手失效,会永远卡在等待、应用不启动且无任何日志。
- 本地(非 Swarm)验证:`docker run --rm --init -p 127.0.0.1:19829:19829 -v llm-wiki-data:/data <image>`,
  浏览器直接访问 http://127.0.0.1:19829(仅限本机调试,无认证;注意 `--init` 必不可少)。
- 镜像构建用 thin LTO 覆盖上游的 fat LTO profile(Dockerfile 内 `CARGO_PROFILE_RELEASE_*`
  环境变量),否则链接期在小内存构建机上会被 OOM 杀掉;二进制略增大,服务端无感。

## 手工构建(不用 Docker 时)

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
3. oauth2-proxy 直接持证书终结 TLS(Swarm 部署见 docker-stack.yml;
   手工部署见 oauth2-proxy.cfg 的 tls_cert_file/tls_key_file)。

## 开发

```bash
LLM_WIKI_WEB_ENABLE=1 npm run tauri dev   # 起后端(桥接监听 :19829)
npm run dev:web                            # 起 web 前端,自动代理桥接端点
```

## 远程 MCP(LibreChat 等)

`Settings → API + MCP → Enable MCP access` 里的入口路径只对**桌面版**有意义
(指向本机 stdio 子进程,供本机 Claude Desktop 配置用)。Web 版走的是
另一个东西:`mcp-server` 增加了 Streamable HTTP 传输
([mcp-server/src/http.ts](../../mcp-server/src/http.ts)),作为容器内的
Node 子进程运行,供 LibreChat 之类的远程 Agent 平台通过网络接入,
工具集与桌面/本地 MCP 完全一致(`llm_wiki_search`、`llm_wiki_chat`、
`llm_wiki_graph` 等 9 个工具)。

这是 `llm-wiki` 服务唯一直接发布端口的例外——鉴权用的是服务间静态
令牌(`Authorization: Bearer`),不是浏览器 OIDC 登录,所以没有接入
oauth2-proxy;`mcp-server/src/http.ts` 自己校验令牌,自带 TLS(复用同一
张服务证书)。**没设令牌会拒绝启动;非回环地址绑定又没配 TLS 也会
拒绝启动**——fail-closed,不会出现"忘了设密钥就裸奔"的情况。

启用步骤:

```bash
# 1. 建令牌 secret(LibreChat 侧原样使用这个值)
openssl rand -hex 32 | docker secret create llm_wiki_mcp_token -

# 2. 再建一个内部令牌:mcp-server 要转调本机 api_server.rs(:19828,只回环
#    可达、从不对外发布)才能真正执行工具,而那个 API 默认要求鉴权。
#    这个值只在容器内部使用,LibreChat 不需要知道它。
openssl rand -hex 32 | docker secret create llm_wiki_api_token -

# 3. docker-stack.yml 里给 llm-wiki 服务:
#    - 取消 `ports: - "7189:3939"` 的注释
#    - LLM_WIKI_MCP_ENABLE 改成 "1"
#    (TLS 证书/密钥、两个 secret 的挂载已经写好,不用再改)

docker stack deploy -c deploy/web/docker-stack.yml llm-wiki
```

漏配 `llm_wiki_api_token` 的典型症状:LibreChat 能成功连上并列出工具
(`tools/list` 正常),但实际调用 `llm_wiki_chat` 等工具时报
`LLM Wiki API 401: Unauthorized`——说明请求已经到达 mcp-server,只是它
自己回环调用 api_server.rs 时没带对上的令牌。

LibreChat 侧(`librechat.yaml`):

```yaml
mcpServers:
  llm-wiki:
    type: streamable-http
    url: https://dbp.test.example.internal:7189/mcp
    headers:
      Authorization: "Bearer <与 llm_wiki_mcp_token 相同的值>"
```

本地验证过:容器内启动日志出现 `listening on https://0.0.0.0:3939/mcp`,
从宿主机通过发布端口以 HTTPS + Bearer token 访问,`tools/list` 正确
返回全部工具。

## POC 范围与已知限制

- 已桥接命令覆盖核心链路:文件系统全套、项目管理、搜索、embedding、
  向量库、外部搜索、图片提取、Agent 聊天(含流式,走 `/events` SSE)、
  文件监控、文件历史。桌面语义命令(打开系统文件管理器、本地 CLI 集成、
  窗口关闭行为)保留 501,浏览器场景下没有对应物。
- 文件选择对话框:浏览器原生选择器上传到服务器暂存目录
  (`src-web/shims/plugin-dialog.ts`);目录模式弹出内嵌选择框,
  区分"上传本机文件夹"与"输入服务器路径"两种语义。
- 桥接本身(RPC/SSE/proxy/upload,19829 端口)无认证、接受绝对路径
  (与桌面 IPC 同权),**必须**只绑定 127.0.0.1 并置于 oauth2-proxy 之后;
  远程 MCP(3939 端口)是唯一的例外,见上一节。这是单用户模型,
  不做多租户隔离(多知识库场景请每个子集单独部署一套实例)。
- 浏览器对 HTTP/1.1 同源并发连接有 ~6 个上限,事件流较多时请确保
  外层代理启用 HTTP/2。
