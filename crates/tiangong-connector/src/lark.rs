use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tiangong_types::{MessageContent, OutgoingMessage};

use crate::traits::{Connector, ConnectorStatus};

/// 飞书/Lark Connector，通过 HTTP API 直接对接
pub struct LarkConnector {
    name: String,
    app_id: String,
    app_secret: String,
    running: bool,
    /// 缓存的 tenant_access_token
    #[cfg(feature = "lark")]
    access_token: Option<String>,
}

impl LarkConnector {
    pub fn new(name: String, app_id: String, app_secret: String) -> Self {
        Self {
            name,
            app_id,
            app_secret,
            running: false,
            #[cfg(feature = "lark")]
            access_token: None,
        }
    }

    /// 获取或刷新 tenant_access_token
    #[cfg(feature = "lark")]
    async fn refresh_access_token(&mut self) -> Result<String> {
        let client = reqwest::Client::new();
        let resp = client
            .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
            .json(&serde_json::json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret,
            }))
            .send()
            .await
            .map_err(|e| anyhow!("飞书获取 access_token 请求失败: {e}"))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("飞书 access_token 响应解析失败: {e}"))?;

        let code = body["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            let msg = body["msg"].as_str().unwrap_or("未知错误");
            return Err(anyhow!(
                "飞书获取 access_token 失败: code={code}, msg={msg}"
            ));
        }

        let token = body["tenant_access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("飞书响应缺少 tenant_access_token 字段"))?
            .to_string();

        self.access_token = Some(token.clone());
        Ok(token)
    }

    /// 获取当前有效的 access_token
    #[cfg(feature = "lark")]
    async fn get_access_token(&mut self) -> Result<String> {
        // 简单策略：如果有缓存就用缓存，否则刷新
        // TODO: 增加过期时间判断，token 有效期通常为 2 小时
        if let Some(ref token) = self.access_token {
            return Ok(token.clone());
        }
        self.refresh_access_token().await
    }
}

#[async_trait]
impl Connector for LarkConnector {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> Result<()> {
        #[cfg(feature = "lark")]
        {
            // 初始化时获取 access_token 验证配置是否正确
            let token = self.refresh_access_token().await?;
            tracing::info!(
                connector = %self.name,
                token_len = token.len(),
                "飞书 Connector 已启动，access_token 获取成功"
            );
        }

        #[cfg(not(feature = "lark"))]
        {
            return Err(anyhow!("Lark connector 需要启用 'lark' feature"));
        }

        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        #[cfg(feature = "lark")]
        {
            self.access_token = None;
        }

        tracing::info!(connector = %self.name, "飞书 Connector 已停止");
        self.running = false;
        Ok(())
    }

    async fn send_message(&self, channel_id: &str, message: &OutgoingMessage) -> Result<()> {
        #[cfg(feature = "lark")]
        {
            let token = self
                .access_token
                .as_ref()
                .ok_or_else(|| anyhow!("飞书 access_token 未初始化，请先调用 start()"))?;

            let (msg_type, content) = match &message.content {
                MessageContent::Text(t) => ("text", serde_json::json!({ "text": t })),
                MessageContent::Image { url, caption } => {
                    // 飞书发送图片需要先上传获取 image_key，这里暂用文本替代
                    // TODO: 实现图片上传再发送
                    let text = format!("[图片] {}\n{}", url, caption.as_deref().unwrap_or(""));
                    ("text", serde_json::json!({ "text": text }))
                }
                MessageContent::File { url, name } => {
                    // TODO: 实现文件上传再发送
                    let text = format!("[文件] {name}: {url}");
                    ("text", serde_json::json!({ "text": text }))
                }
            };

            let client = reqwest::Client::new();
            let resp = client
                .post("https://open.feishu.cn/open-apis/im/v1/messages")
                .bearer_auth(token)
                .query(&[("receive_id_type", "chat_id")])
                .json(&serde_json::json!({
                    "receive_id": channel_id,
                    "msg_type": msg_type,
                    "content": content.to_string(),
                }))
                .send()
                .await
                .map_err(|e| anyhow!("飞书发送消息请求失败: {e}"))?;

            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| anyhow!("飞书发送消息响应解析失败: {e}"))?;

            let code = body["code"].as_i64().unwrap_or(-1);
            if code != 0 {
                let msg = body["msg"].as_str().unwrap_or("未知错误");
                return Err(anyhow!("飞书发送消息失败: code={code}, msg={msg}"));
            }

            tracing::info!(
                connector = %self.name,
                channel = %channel_id,
                "飞书消息发送成功"
            );
        }

        #[cfg(not(feature = "lark"))]
        {
            let _ = (channel_id, message);
            return Err(anyhow!("Lark connector 需要启用 'lark' feature"));
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<ConnectorStatus> {
        Ok(if self.running {
            ConnectorStatus::Running
        } else {
            ConnectorStatus::Stopped
        })
    }
}
