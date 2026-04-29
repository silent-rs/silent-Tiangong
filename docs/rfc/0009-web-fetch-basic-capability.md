# RFC 0009：web_fetch 基础能力

> 状态：草稿
> 日期：2026-04-29
> 关联：`crates/tiangong-core`、`crates/tiangong-cli`、`crates/tiangong-gui`、`crates/tiangong-server`

---

## 1. 背景

当前天工已经具备本地文件、命令、MCP、Skill、Memory 与多媒体能力，但主链路缺少一个受控的网页读取能力。模型在需要获取网页内容、官方文档、公开资料或轻量页面信息时，只能依赖用户手动粘贴内容，无法形成稳定的工具闭环。

`web_fetch` 的目标是补齐最小可用的 HTTP/HTTPS 页面获取能力，使 Agent 可以在明确需要时读取指定 URL，并把提取后的页面内容作为工具结果进入会话上下文。

该能力也用于补足运行环境差异：当用户机器没有安装 `curl`、`wget` 等命令行下载工具，或命令执行权限不适合直接访问网络时，天工仍应具备一个内置、可治理、跨 CLI / GUI / Server 一致的网页获取能力。如果用户需要下载在线文件，本 RFC 当期即应提供受控文件下载语义。

该能力应定位为基础工具，而不是通用浏览器自动化系统。复杂登录、点击、动态交互和页面截图应由后续浏览器能力或 MCP 适配承载。

---

## 2. 目标

- 提供受控的 `web_fetch` 工具，支持读取公开 HTTP/HTTPS URL。
- 支持常见文本内容类型，包括 HTML、纯文本、JSON、Markdown 和 XML。
- 对 HTML 页面执行基础正文提取，返回标题、最终 URL、内容类型、状态码和正文文本。
- 支持超时、最大响应体大小、重定向次数和字符数预算限制。
- 工具结果以结构化 tool 消息进入会话层，不追加到稳定 system prompt。
- 网络访问必须经过运行时权限治理，能够限制协议、目标主机、私网地址和请求大小。
- CLI、GUI、Server 共享同一 Core 能力，不在前端重复实现抓取逻辑。
- 在用户环境缺少 `curl` / `wget` 等外部工具时，提供内置的基础网页读取替代路径。
- 支持受控在线文件下载，包括 URL 校验、网络权限、响应大小限制、内容类型识别和文件写入边界检查。

---

## 3. 非目标

- 不实现完整浏览器渲染、JavaScript 执行、点击、表单填写或截图。
- 不支持登录态 Cookie 管理、站点会话保持或浏览器 Profile 复用。
- 不把 `web_fetch` 做成搜索引擎；URL 必须由模型或用户明确给出。
- 不绕过网站访问控制、付费墙、robots 策略或反爬限制。
- 不在本 RFC 中实现网页内容长期索引；是否写入 Memory 由现有产物记忆规则后续判断。

---

## 4. 能力边界

### 4.1 输入

`web_fetch` 工具的最小输入：

```json
{
  "url": "https://example.com/page",
  "mode": "text",
  "max_chars": 12000
}
```

可选输入：

- `method`：首期仅允许 `GET`，未来可评估 `HEAD`。
- `mode`：执行模式，支持 `text` 和 `download`；缺省为 `text`。
- `headers`：首期只允许安全白名单 Header，例如 `Accept`、`Accept-Language`、`User-Agent`。
- `timeout_ms`：请求超时时间，必须受全局上限约束。
- `follow_redirects`：是否跟随重定向，默认开启。
- `extract_mode`：内容提取模式，支持 `auto`、`text`、`raw`。
- `output_path`：下载模式下的目标文件路径，必须通过现有文件写入边界检查。
- `overwrite`：下载模式下是否允许覆盖已有文件，默认 `false`。

### 4.2 输出

工具输出应保持结构化，至少包含：

```json
{
  "mode": "text",
  "url": "https://example.com/page",
  "final_url": "https://example.com/page",
  "status": 200,
  "content_type": "text/html; charset=utf-8",
  "title": "Example",
  "text": "页面正文...",
  "truncated": false,
  "bytes_read": 4096
}
```

下载模式输出至少包含：

```json
{
  "mode": "download",
  "url": "https://example.com/file.zip",
  "final_url": "https://example.com/file.zip",
  "status": 200,
  "content_type": "application/zip",
  "file_path": "/workspace/downloads/file.zip",
  "bytes_written": 1048576,
  "sha256": "..."
}
```

错误输出必须能区分：

- URL 格式非法。
- 协议不允许。
- 目标被权限策略拒绝。
- DNS / 连接 / TLS / 超时错误。
- HTTP 状态码错误。
- 内容类型不支持。
- 响应体超过限制。
- 下载目标路径不允许。
- 下载目标文件已存在且未允许覆盖。
- 文件写入失败。
- 文本解析或编码转换失败。

---

## 5. 安全与权限

### 5.1 默认策略

- 仅允许 `http` 与 `https`。
- 默认拒绝 `file://`、`ftp://`、`data:`、`javascript:` 等非 HTTP 协议。
- 默认拒绝访问本机、私网、链路本地、保留地址和云元数据地址。
- 默认跟随重定向，但每次重定向后的目标必须重新通过权限检查。
- 默认限制最大响应体大小，避免内存膨胀。
- 默认限制最大下载文件大小，避免磁盘膨胀。
- 默认限制请求超时，避免工具长时间阻塞。
- 下载文件必须复用现有写入边界：只能写入当前工作空间、当前对话显式指定目录和 `~/.tiangong/skills` 等已允许范围。
- 下载目标文件名必须进行路径规范化，拒绝路径穿越、隐藏覆盖和符号链接绕过。

### 5.2 网络目标治理

`web_fetch` 必须接入现有权限治理模型，至少预留以下策略：

- `allow_hosts`：允许访问的域名或通配域名。
- `deny_hosts`：拒绝访问的域名或通配域名。
- `allow_private_network`：是否允许访问私网地址，默认关闭。
- `max_redirects`：最大重定向次数。
- `max_body_bytes`：最大响应体读取字节数。
- `max_download_bytes`：最大下载写入字节数。
- `timeout_ms`：全局最大超时时间。

当用户显式要求访问本地服务或私网地址时，必须由运行时权限策略明确放行，不能由模型自行绕过。

---

## 6. 内容处理

### 6.1 HTML 提取

首期只做基础 HTML 正文提取：

- 解析 `<title>` 作为标题。
- 移除 `script`、`style`、`noscript`、`svg` 等非正文节点。
- 将可见文本规整为空白稳定的纯文本。
- 保留链接文本，但不要求保留完整 DOM 结构。

如果引入第三方库，应优先选择轻量、维护活跃、纯 Rust 或成熟生态库，并避免把浏览器级依赖引入 Core。

### 6.2 非 HTML 文本

- `text/plain`、`application/json`、`application/xml`、`text/markdown` 可按文本返回。
- JSON 可保持原始文本，暂不强制格式化。
- 编码优先按响应 Header 判断，缺失时可尝试 UTF-8。

### 6.3 下载与二进制内容

图片、视频、音频、压缩包、PDF 等二进制内容不进入正文提取，但可以在 `mode = "download"` 时按文件下载语义处理：

- `mode = "text"` 时，返回不支持的内容类型错误，或只返回元数据。
- `mode = "download"` 时，按流式读取写入目标文件，避免完整文件常驻内存。
- 写入前必须检查目标路径、覆盖策略和可写边界。
- 写入完成后返回文件路径、字节数、内容类型和校验摘要。
- 下载产物应进入会话层结构化工具结果，供后续文件工具或 Memory 产物记忆使用。

---

## 7. 架构设计

### 7.1 Core 工具

`web_fetch` 应作为 Core 内置工具注册，工具实现位于 Core 能力层，前端只负责展示工具调用与结果。

建议拆分为：

- `WebFetchTool`：工具入口与参数校验。
- `WebFetchClient`：HTTP 请求、重定向、大小限制和超时控制。
- `WebFetchPolicy`：协议、主机、私网地址和请求预算检查。
- `WebFetchExtractor`：根据内容类型提取文本。
- `WebFetchDownloader`：下载模式下的路径校验、流式写入、覆盖控制和摘要计算。

### 7.2 会话上下文

`web_fetch` 结果必须作为工具结果进入消息链：

- 不追加到 system prompt。
- 不作为稳定上下文缓存块。
- 需要截断时在工具结果中标记 `truncated = true`。
- 需要摘要时复用现有上下文压缩能力，而不是由 `web_fetch` 工具私自改写历史。

### 7.3 配置

首期配置可挂在 Core 配置下：

```toml
[tools.web_fetch]
enabled = true
timeout_ms = 15000
max_redirects = 5
max_body_bytes = 1048576
max_download_bytes = 104857600
default_max_chars = 12000
allow_private_network = false
```

如项目现有配置格式不使用 TOML，实现时应映射到对应配置结构，字段语义保持一致。

---

## 8. 前端与接口表现

### 8.1 CLI

- 工具调用时展示 URL、状态和是否截断。
- 错误时展示可读错误原因。
- 不在 CLI 中重复做网络请求。

### 8.2 GUI

- 工具调用卡片展示请求 URL、最终 URL、状态码、标题和截断状态。
- 正文内容默认折叠，避免长网页内容冲击对话视图。
- 不额外实现网页预览浏览器。

### 8.3 Server

- Server 模式复用 Core 工具。
- 远程请求触发 `web_fetch` 时仍受 Server 侧运行时权限策略约束。
- 观察者角色不能直接触发工具执行，保持现有远程角色语义。

---

## 9. 里程碑

### Phase A：RFC 与需求冻结

- 接受本 RFC。
- 在 `docs/requirements.md` 中补充 `web_fetch` 基础能力要求。
- 在 `PLAN.md` / `TODO.md` 中切换或新增当前任务条目。

### Phase B：Core 基础实现

- 增加 `web_fetch` 工具定义与参数结构。
- 实现 URL 校验、权限策略、超时、重定向和响应体大小限制。
- 实现 HTML / 文本内容提取。
- 实现下载模式的目标路径校验、覆盖控制、流式写入、大小限制和摘要计算。
- 将结果以结构化工具结果返回。

### Phase C：运行时与前端接入

- 接入工具注册与 Agent 调用链路。
- CLI / GUI / Server 展示工具调用结果。
- 配置默认值与关闭开关生效。

### Phase D：验证与收口

- 使用 `cargo check --workspace` 验证。
- 对公开网页、重定向、超时、私网拒绝和超大响应进行手动验证。
- 对在线文件下载、文件已存在、非法写入路径和超大下载进行手动验证。
- 根据实际实现更新 `TODO.md` 完成状态。

---

## 10. 验收标准

- Agent 可以调用 `web_fetch` 读取一个公开 HTTPS 文本页面，并基于返回正文回答问题。
- `web_fetch` 结果不污染 system prompt，只作为工具结果参与当前上下文。
- 非 HTTP/HTTPS 协议被拒绝。
- 默认策略下私网、本机和云元数据地址被拒绝。
- 超时、重定向次数和响应体大小限制生效。
- HTML 页面返回可读正文，不包含大量脚本和样式内容。
- 二进制内容不会被误当作正文注入上下文。
- 在线文件可在 `download` 模式下保存到允许写入目录，并返回结构化下载结果。
- 下载模式拒绝写入工作空间边界外路径。
- 下载模式默认不覆盖已有文件，除非用户或工具参数显式允许。
- CLI、GUI、Server 通过同一 Core 工具链路获得一致行为。

---

## 11. 待决问题

- 是否需要在首期支持 `HEAD` 请求。
- 是否需要记录 robots 策略检查结果，或仅依赖用户/模型使用规范。
- 是否需要为常见官方文档站点提供更高的默认字符预算。
- 是否需要为下载文件名提供基于 `Content-Disposition` 的自动推断。
- 是否将成功抓取的页面摘要写入 Memory，以及写入粒度如何控制。
- 是否需要为 `web_fetch` 单独增加审计事件，记录 URL、状态码和拒绝原因。
