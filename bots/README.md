# 天工移动端控制 bot 制品

本目录存放各平台 bot 的**独立二进制源码**。每个子目录是一个独立编译的 `[[bin]]` crate，随主仓库 CI 交叉编译后上传到 GitHub Release，供天工主程序运行时下载、启动并监控。

## 设计

- bot 是**独立进程**，不是主程序的编译期依赖（主程序不引入各平台 SDK）。
- bot 通过 HTTP 与天工通信：收到 IM 消息后 POST 到天工本地 embedded server 的 `/api/v1/messages`。
- 平台凭证可由 bot 的扫码流程自行保存；手工配置的凭证仍可由主程序通过环境变量注入。
- 主程序负责：制品下载（SHA256 校验）→ spawn → 崩溃重启 → 日志捕获。

## 目录结构

```
bots/
  feishu/          # 飞书 bot（openlark WebSocket 长连接）
    Cargo.toml     # [[bin]] name = "tiangong-bot-feishu"，含 [workspace]（独立编译）
    src/main.rs
  weixin/          # 微信 bot（腾讯 iLink 协议长轮询）
    Cargo.toml     # [[bin]] name = "tiangong-bot-weixin"，含 [workspace]（独立编译）
    src/main.rs
  qq/              # QQ bot（QQ 开放平台 WebSocket 网关）
    Cargo.toml     # [[bin]] name = "tiangong-bot-qq"，含 [workspace]（独立编译）
    src/main.rs
```

## 新增一个 bot

1. 在 `bots/` 下新建目录，Cargo.toml 声明 `[[bin]]` + 空 `[workspace]` 表（避免被主 workspace 收编）。
2. 实现 `--describe` 输出配置 schema（单一真相来源），并可选实现 `--provision-begin`/`--provision-poll` 扫码配置。
3. 实现消息接收逻辑，把 IM 消息 POST 到 `TIANGONG_URL/api/v1/messages`。
4. 处理 SIGTERM/SIGINT 优雅退出（主程序 stop 时发送）。
5. 日志输出到 stderr（主程序捕获 tail 用于诊断）。
6. 在 `bots/<name>/bot.json` 补充 `name`/`description`/`min_app_version`/`config_schema`（发版元数据 + bots-index.json 预览）。

## bots-index.json

**每个 bot 独立发版**，各自有独立的 CI 工作流、tag 和 Release，互不影响。

- 每个 bot 对应一个 workflow：`.github/workflows/release-<bot-id>.yml`（如 `release-feishu.yml`、`release-weixin.yml`、`release-qq.yml`）。
- tag 约定：`bot-<bot-id>-v<version>`，如 `bot-feishu-v0.1.0`、`bot-weixin-v0.1.0`、`bot-qq-v0.1.0`。
- 新增 bot 时复制一个已有的 workflow，把 bot id、名称、描述改为新 bot 即可。
- 每个 bot 在 OSS 使用独立索引对象：`bots-index/<bot-id>.json`。发布流程只写自己的对象，不读取或覆盖其他 bot 的索引。
- `bots-index/catalog.json` 是索引目录，只列出各独立索引地址；主程序固定读取这一个目录，再读取并合并其中的索引。
- 索引目录的源码是 `bots/index-catalog.json`，由 `.github/workflows/publish-bots-catalog.yml` 独立发布。新增或下线 bot 时只更新目录，不需要发布主程序。
- GitHub Release 作为产物归档（人可查、CI 可用），每个 Release 只附带本 bot 的 `bots-index.json`。

### 发版流程

1. 推送 `bot-<bot-id>-v0.1.0` tag（如 `bot-weixin-v0.1.0`），或 Actions 页面手动触发对应 bot 的 workflow 并输入 tag。
2. CI 交叉编译该 bot 的 4 平台制品 → 上传到对应 Release。
3. `generate-bots-index` job 汇总该 bot 各平台 SHA256，生成只包含当前 bot 的索引并附到 Release。
4. `upload-to-oss` job 上传该 bot 制品到 `bots/bot-<bot-id>-v0.1.0/`，并把索引上传到 `bots-index/<bot-id>.json`。
5. 新增或下线 bot 时修改 `bots/index-catalog.json`；合并到主分支后，目录发布流程会更新 `bots-index/catalog.json`。

## 第三方 Bot 接入

第三方 Bot 的源码和发布流程可以完全放在自己的仓库中，也不限制实现语言。接入时需要满足以下约定：

1. 提供 Windows、macOS 和 Linux 对应平台的独立可执行文件及 SHA-256。
2. 可执行文件支持 `--describe`，输出天工可识别的配置 schema；需要扫码时可选支持 `--provision-begin` 和 `--provision-poll`。
3. 正常运行时读取天工注入的 `TIANGONG_URL`、`TIANGONG_TOKEN` 及 schema 声明的配置环境变量，并通过 Server API 收发消息。
4. 在第三方自己的 HTTPS 地址发布独立 `bots-index.json`；索引中的 `id` 必须与 `--describe` 的 `artifact_id` 一致、全局唯一，并符合小写字母和数字组成、总长 1～64 位的规则。
5. 向 `bots/index-catalog.json` 提交该索引地址。目录合并并发布后，天工会自动发现该 Bot；后续版本只需更新第三方自己的索引，不需要再次修改天工或发布主程序。

### 独立索引示例

第三方索引至少包含一个 Bot 和一个平台制品。平台键使用 `darwin-aarch64`、`darwin-x86_64`、`linux-aarch64`、`linux-x86_64`、`windows-aarch64` 或 `windows-x86_64`。

```json
{
  "version": 1,
  "bots": [
    {
      "id": "examplebot",
      "name": "Example Bot",
      "version": "0.1.0",
      "description": "Example 平台连接器",
      "config_schema": [],
      "platforms": {
        "linux-x86_64": {
          "url": "https://example.com/releases/0.1.0/examplebot-linux-x86_64",
          "checksum": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }
      },
      "min_app_version": "0.12.3"
    }
  ]
}
```

### 贡献到官方目录

第三方只需要贡献索引地址，不需要把 Bot 源码或二进制提交到天工仓库：

1. 在自己的仓库或网站发布 Bot、SHA-256 和独立索引，并确保索引可以通过公网 HTTPS 直接读取。
2. Fork 天工仓库，在 `bots/index-catalog.json` 的 `indexes` 数组末尾添加自己的索引 URL，不修改或重排已有地址。
3. 提交 PR，并在说明中提供 Bot 源码地址、许可证、支持平台、需要的外部权限及一次实际运行结果。
4. `Publish Bots Catalog / Validate bots catalog` 会检查目录格式、索引可访问性、Bot ID 冲突、版本格式、HTTPS 制品地址和 SHA-256 格式。
5. 审核通过并合并后，发布任务自动更新 OSS 上的 `bots-index/catalog.json`。以后发布新版本只更新第三方自己的索引，不再提交目录 PR。

PR 校验任务不读取 OSS 密钥，也不会上传文件；只有主分支更新或维护者手工触发时才执行发布。

官方目录中的第三方 Bot 需要经过代码与制品来源审核。允许用户自行添加未经审核索引源属于另一项安全边界更大的功能，不在当前发布流程内。
目录 CI 会拒绝重复的索引地址和 Bot ID，第三方不能使用已有官方 Bot 的 ID。

## 凭证注入约定

主程序 spawn bot 时按 schema 注入手工配置的环境变量，并始终自动注入天工连接信息：

| 平台   | 环境变量                                                                 |
| ------ | ------------------------------------------------------------------------ |
| feishu | `TIANGONG_BOT_FEISHU_APP_ID` / `TIANGONG_BOT_FEISHU_APP_SECRET`（仅手工配置） |
| weixin | `TIANGONG_BOT_WEIXIN_TOKEN`（仅手工配置，扫码所得凭证由 bot 自行保存）   |
| qq     | `TIANGONG_BOT_QQ_APP_ID` / `TIANGONG_BOT_QQ_APP_SECRET`（可选手工配置，扫码所得凭证由 bot 自行保存） |
| 通用   | `TIANGONG_URL`（embedded server 地址）/ `TIANGONG_TOKEN`（认证 token）   |

约定见 `crates/tiangong-bots/src/runtime.rs::bot_env`。
