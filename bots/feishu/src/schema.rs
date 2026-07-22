//! 飞书 bot 配置 schema——bot 二进制内单一真相来源。
//!
//! `--describe` 输出此 schema，主程序据此渲染表单、校验必填、注入环境变量。
//! 同一份 schema 由 CI 同步到 `bots/feishu/bot.json`（供 bots-index.json 预览）。
//!
//! **加字段时只需修改本文件 + 对应 main.rs 的环境变量读取**，主程序零改动。

use serde_json::{json, Value};

/// `--describe` 输出的完整 schema JSON。
pub fn describe_output() -> Value {
    json!({
        "schema_version": 1,
        "artifact_id": "feishu",
        "config_schema": [
            {
                "key": "app_id",
                "label": "App ID",
                "field_type": { "kind": "barcode" },
                "required": true,
                "env": "TIANGONG_BOT_FEISHU_APP_ID",
                "help": "飞书应用凭证，可通过扫码创建应用自动获取"
            },
            {
                "key": "app_secret",
                "label": "App Secret",
                "field_type": { "kind": "barcode" },
                "required": true,
                "env": "TIANGONG_BOT_FEISHU_APP_SECRET"
            }
        ]
    })
}
