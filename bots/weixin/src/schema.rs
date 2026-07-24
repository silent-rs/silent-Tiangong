//! 微信 bot 配置 schema——bot 二进制内单一真相来源。
//!
//! `--describe` 输出此 schema，主程序据此渲染表单、校验必填、注入环境变量。
//! 同一份 schema 由 CI 同步到 `bots/weixin/bot.json`（供 bots-index.json 预览）。
//!
//! **加字段时只需修改本文件 + 对应 main.rs 的环境变量读取**，主程序零改动。

use serde_json::{Value, json};

/// `--describe` 输出的完整 schema JSON。
pub fn describe_output() -> Value {
    json!({
        "schema_version": 1,
        "artifact_id": "weixin",
        "config_schema": [
            {
                "key": "provision",
                "label": "微信扫码授权",
                "field_type": { "kind": "barcode" },
                "required": false,
                "help": "扫码所得 bot_token 由微信 bot 自行保存"
            },
            {
                "key": "bot_token",
                "label": "Bot Token",
                "field_type": { "kind": "secret" },
                "required": false,
                "env": "TIANGONG_BOT_WEIXIN_TOKEN",
                "help": "可选，仅用于手工配置已有 iLink bot_token"
            }
        ],
        "capabilities": {
            "mcp": {
                "protocol_version": 1
            }
        }
    })
}
