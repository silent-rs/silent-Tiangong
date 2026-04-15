use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRole {
    #[default]
    Controller,
    Approver,
    Observer,
}

impl RemoteRole {
    pub fn can_send_message(&self) -> bool {
        matches!(self, Self::Controller)
    }

    pub fn can_manage_sessions(&self) -> bool {
        matches!(self, Self::Controller)
    }

    pub fn can_approve(&self) -> bool {
        matches!(self, Self::Controller | Self::Approver)
    }

    pub fn can_observe(&self) -> bool {
        true
    }

    pub fn can_cancel_task(&self) -> bool {
        matches!(self, Self::Controller)
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::Controller => "控制者",
            Self::Approver => "审批者",
            Self::Observer => "观察者",
        }
    }
}

impl std::fmt::Display for RemoteRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub id: String,
    pub connector: String,
    pub channel_id: String,
    pub sender_id: String,
    #[serde(default)]
    pub sender_role: RemoteRole,
    pub content: MessageContent,
    pub reply_to: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub content: MessageContent,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    Text(String),
    Image {
        url: String,
        caption: Option<String>,
    },
    File {
        url: String,
        name: String,
    },
    Audio {
        url: String,
        duration: Option<u32>,
    },
    Video {
        url: String,
        caption: Option<String>,
    },
}
