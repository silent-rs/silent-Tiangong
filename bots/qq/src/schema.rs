//! QQ bot 配置 schema——bot 二进制内单一真相来源。
//!
//! `--describe` 输出此 schema，主程序据此渲染表单、校验必填、注入环境变量。
//! 同一份 schema 由 CI 同步到 `bots/qq/bot.json`（供 bots-index.json 预览）。
//!
//! **加字段时只需修改本文件 + 对应 main.rs 的环境变量读取**，主程序零改动。

use serde_json::{Value, json};

/// `--describe` 输出的完整 schema JSON。
pub fn describe_output() -> Value {
    json!({
        "schema_version": 1,
        "artifact_id": "qq",
        "config_schema": [
            {
                "key": "provision",
                "label": "QQ 扫码授权",
                "field_type": { "kind": "barcode" },
                "required": false,
                "help": "使用手机 QQ 选择或创建机器人，确认后自动获取并保存凭证"
            },
            {
                "key": "app_id",
                "label": "App ID",
                "field_type": { "kind": "secret" },
                "required": false,
                "env": "TIANGONG_BOT_QQ_APP_ID",
                "help": "可选，仅用于手工配置已有 QQ 机器人的 AppID"
            },
            {
                "key": "app_secret",
                "label": "App Secret",
                "field_type": { "kind": "secret" },
                "required": false,
                "env": "TIANGONG_BOT_QQ_APP_SECRET",
                "help": "可选，仅用于手工配置已有 QQ 机器人的 ClientSecret"
            }
        ]
    })
}
