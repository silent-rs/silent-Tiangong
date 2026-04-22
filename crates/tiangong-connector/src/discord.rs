use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tiangong_types::{MessageContent, OutgoingMessage};

use crate::traits::{Connector, ConnectorStatus};

pub struct DiscordConnector {
    name: String,
    bot_token: String,
    running: bool,
    #[cfg(feature = "discord")]
    http: Option<std::sync::Arc<serenity::http::Http>>,
    #[cfg(feature = "discord")]
    shard_manager: Option<std::sync::Arc<serenity::gateway::ShardManager>>,
}

impl DiscordConnector {
    pub fn new(name: String, bot_token: String) -> Self {
        Self {
            name,
            bot_token,
            running: false,
            #[cfg(feature = "discord")]
            http: None,
            #[cfg(feature = "discord")]
            shard_manager: None,
        }
    }
}

#[cfg(feature = "discord")]
struct Handler {
    connector_name: String,
}

#[cfg(feature = "discord")]
#[async_trait]
impl serenity::client::EventHandler for Handler {
    async fn message(
        &self,
        _ctx: serenity::client::Context,
        msg: serenity::model::channel::Message,
    ) {
        // 忽略 bot 自身的消息
        if msg.author.bot {
            return;
        }
        tracing::info!(
            connector = %self.connector_name,
            channel_id = %msg.channel_id,
            author = %msg.author.name,
            content = %msg.content,
            "Discord 收到消息"
        );
        // TODO: 通过 Gateway 转发消息到核心处理
    }

    async fn ready(&self, _ctx: serenity::client::Context, ready: serenity::model::gateway::Ready) {
        tracing::info!(
            connector = %self.connector_name,
            user = %ready.user.name,
            "Discord bot 已就绪"
        );
    }
}

#[async_trait]
impl Connector for DiscordConnector {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> Result<()> {
        #[cfg(feature = "discord")]
        {
            use serenity::prelude::*;

            let handler = Handler {
                connector_name: self.name.clone(),
            };

            let intents = GatewayIntents::GUILD_MESSAGES
                | GatewayIntents::DIRECT_MESSAGES
                | GatewayIntents::MESSAGE_CONTENT;

            let mut client = Client::builder(&self.bot_token, intents)
                .event_handler(handler)
                .await
                .map_err(|e| anyhow!("创建 Discord client 失败: {e}"))?;

            self.http = Some(client.http.clone());
            self.shard_manager = Some(client.shard_manager.clone());

            let connector_name = self.name.clone();
            tokio::spawn(async move {
                if let Err(e) = client.start().await {
                    tracing::error!(
                        connector = %connector_name,
                        error = %e,
                        "Discord client 运行出错"
                    );
                }
            });
        }

        #[cfg(not(feature = "discord"))]
        {
            return Err(anyhow!("Discord connector 需要启用 'discord' feature"));
        }

        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        #[cfg(feature = "discord")]
        {
            if let Some(shard_manager) = self.shard_manager.take() {
                shard_manager.shutdown_all().await;
            }
            self.http = None;
        }

        tracing::info!(connector = %self.name, "Discord connector 已停止");
        self.running = false;
        Ok(())
    }

    async fn send_message(&self, channel_id: &str, message: &OutgoingMessage) -> Result<()> {
        #[cfg(feature = "discord")]
        {
            use serenity::model::id::ChannelId;

            let http = self
                .http
                .as_ref()
                .ok_or_else(|| anyhow!("Discord client 尚未启动"))?;

            let channel_id_num: u64 = channel_id
                .parse()
                .map_err(|_| anyhow!("无效的 Discord channel_id: {channel_id}"))?;

            let channel = ChannelId::new(channel_id_num);

            let text = match &message.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Image { url, caption } => {
                    // TODO: 使用 embed 发送图片
                    format!("{}\n{}", caption.as_deref().unwrap_or(""), url)
                }
                MessageContent::File { url, name } => {
                    // TODO: 使用附件发送文件
                    format!("[文件] {name}: {url}")
                }
                MessageContent::Audio { url, .. } => format!("[音频] {url}"),
                MessageContent::Video { url, caption } => {
                    format!("[视频] {}\n{}", url, caption.as_deref().unwrap_or(""))
                }
            };

            channel
                .say(http.as_ref(), &text)
                .await
                .map_err(|e| anyhow!("Discord 发送消息失败: {e}"))?;
        }

        #[cfg(not(feature = "discord"))]
        {
            let _ = (channel_id, message);
            return Err(anyhow!("Discord connector 需要启用 'discord' feature"));
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
