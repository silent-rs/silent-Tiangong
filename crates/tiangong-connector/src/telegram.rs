use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tiangong_types::{MessageContent, OutgoingMessage};

use crate::traits::{Connector, ConnectorStatus};

pub struct TelegramConnector {
    name: String,
    bot_token: String,
    running: bool,
    #[cfg(feature = "telegram")]
    bot: Option<teloxide::Bot>,
    #[cfg(feature = "telegram")]
    shutdown_token: Option<teloxide::dispatching::ShutdownToken>,
}

impl TelegramConnector {
    pub fn new(name: String, bot_token: String) -> Self {
        Self {
            name,
            bot_token,
            running: false,
            #[cfg(feature = "telegram")]
            bot: None,
            #[cfg(feature = "telegram")]
            shutdown_token: None,
        }
    }
}

#[async_trait]
impl Connector for TelegramConnector {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> Result<()> {
        #[cfg(feature = "telegram")]
        {
            use teloxide::prelude::*;

            let bot = teloxide::Bot::new(&self.bot_token);
            self.bot = Some(bot.clone());

            let connector_name = self.name.clone();

            // 启动 polling，使用 tokio::spawn 避免阻塞
            let mut dispatcher = teloxide::dispatching::Dispatcher::builder(
                bot,
                Update::filter_message().endpoint(|msg: Message| async move {
                    let text = msg.text().unwrap_or("[非文本消息]");
                    let chat_id = msg.chat.id;
                    tracing::info!(
                        chat_id = %chat_id,
                        text = %text,
                        "Telegram 收到消息"
                    );
                    // TODO: 通过 Gateway 转发消息到核心处理
                    Ok(())
                }),
            )
            .build();

            let shutdown_token = dispatcher.shutdown_token();
            self.shutdown_token = Some(shutdown_token);

            tokio::spawn(async move {
                tracing::info!(connector = %connector_name, "Telegram polling 已启动");
                dispatcher.dispatch().await;
            });
        }

        #[cfg(not(feature = "telegram"))]
        {
            return Err(anyhow!("Telegram connector 需要启用 'telegram' feature"));
        }

        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        #[cfg(feature = "telegram")]
        {
            if let Some(token) = self.shutdown_token.take() {
                token
                    .shutdown()
                    .map_err(|_| anyhow!("Telegram shutdown 失败"))?;
            }
            self.bot = None;
        }

        tracing::info!(connector = %self.name, "Telegram connector 已停止");
        self.running = false;
        Ok(())
    }

    async fn send_message(&self, channel_id: &str, message: &OutgoingMessage) -> Result<()> {
        #[cfg(feature = "telegram")]
        {
            use teloxide::prelude::*;
            use teloxide::types::ChatId;

            let bot = self
                .bot
                .as_ref()
                .ok_or_else(|| anyhow!("Telegram bot 尚未启动"))?;

            let chat_id: i64 = channel_id
                .parse()
                .map_err(|_| anyhow!("无效的 Telegram chat_id: {channel_id}"))?;

            let text = match &message.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Image { url, caption } => {
                    // TODO: 使用 send_photo API 发送图片
                    format!("[图片] {}\n{}", url, caption.as_deref().unwrap_or(""))
                }
                MessageContent::File { url, name } => {
                    // TODO: 使用 send_document API 发送文件
                    format!("[文件] {name}: {url}")
                }
            };

            bot.send_message(ChatId(chat_id), text).await?;
        }

        #[cfg(not(feature = "telegram"))]
        {
            let _ = (channel_id, message);
            return Err(anyhow!("Telegram connector 需要启用 'telegram' feature"));
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
