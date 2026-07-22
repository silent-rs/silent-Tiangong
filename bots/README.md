# 天工移动端控制 bot 制品

本目录存放各平台 bot 的**独立二进制源码**。每个子目录是一个独立编译的 `[[bin]]` crate，随主仓库 CI 交叉编译后上传到 GitHub Release，供天工主程序运行时下载、启动并监控。

## 设计

- bot 是**独立进程**，不是主程序的编译期依赖（主程序不引入各平台 SDK）。
- bot 通过 HTTP 与天工通信：收到 IM 消息后 POST 到天工本地 embedded server 的 `/api/v1/messages`。
- 凭证由主程序通过环境变量注入（spawn 时传入）。
- 主程序负责：制品下载（SHA256 校验）→ spawn → 崩溃重启 → 日志捕获。

## 目录结构

```
bots/
  feishu/          # 飞书 bot（openlark WebSocket 长连接）
    Cargo.toml     # [[bin]] name = "tiangong-bot-feishu"，含 [workspace]（独立编译）
    src/main.rs
```

## 新增一个 bot

1. 在 `bots/` 下新建目录，Cargo.toml 声明 `[[bin]]` + 空 `[workspace]` 表（避免被主 workspace 收编）。
2. 实现消息接收逻辑，把 IM 消息 POST 到 `TIANGONG_URL/api/v1/messages`（凭证从环境变量读）。
3. 处理 SIGTERM/SIGINT 优雅退出（主程序 stop 时发送）。
4. 日志输出到 stderr（主程序捕获 tail 用于诊断）。
5. 在 `crates/tiangong-bots/src/config.rs` 的 `BotType` 枚举追加变体 + `config_schema()`。
6. 在 `.github/workflows/release.yml` 的 `publish-bots` job 追加构建目标。
7. 在 `bots-index.json` 生成逻辑里登记该 bot 的制品信息。

## bots-index.json

bot 与主程序**独立发版**（CI 工作流 `.github/workflows/release-bots.yml`，tag 前缀 `bots-v*`）。
`bots-index.json` 以**阿里云 OSS 根目录为权威源**（`silent-tiangong.oss-cn-hangzhou.aliyuncs.com/bots-index.json`），
因为 GitHub 的 `releases/latest` 指向主程序 tag，无法用于解析 bot 的最新版本。
GitHub Release 作为产物归档（人可查、CI 可用）。格式见 `crates/tiangong-bots/src/manifest.rs`。

### 发版流程

1. 推送 `bots-v0.1.0` tag，或 Actions 页面手动触发 `Release Bots` 工作流
2. CI 交叉编译 4 平台制品 → 上传 `bots-v0.1.0` Release
3. `generate-bots-index` job 汇总各平台 SHA256 → 生成 `bots-index.json`（URL 指向 OSS 版本化路径）→ 附到 Release
4. `upload-to-oss` job 上传制品到 `bots/bots-v0.1.0/` + `bots-index.json` 到 OSS 根

## 凭证注入约定

主程序 spawn bot 时按 `BotType` 注入对应环境变量：

| 平台   | 环境变量                                                                 |
| ------ | ------------------------------------------------------------------------ |
| feishu | `TIANGONG_BOT_FEISHU_APP_ID` / `TIANGONG_BOT_FEISHU_APP_SECRET`          |
| 通用   | `TIANGONG_URL`（embedded server 地址）/ `TIANGONG_TOKEN`（认证 token）   |

约定见 `crates/tiangong-bots/src/runtime.rs::bot_env`。
