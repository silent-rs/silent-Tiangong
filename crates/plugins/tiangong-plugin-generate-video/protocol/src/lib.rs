//! Generate-Video 插件私有业务协议。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "generate-video";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const VIDEO_PROTOCOL_VERSION: u32 = 1;

pub const TOOL_GENERATE_VIDEO: &str = "generate_video";

pub trait VideoOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub const GENERATE_OPERATION: &str = "generate";

pub struct Generate;

impl VideoOperation for Generate {
    const NAME: &'static str = GENERATE_OPERATION;
    type Request = GenerateRequest;
    type Response = GenerateResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub prompt: String,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub resolution: Option<String>,
}

/// 视频生成状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoStatus {
    Completed {
        video_url: String,
        duration: Option<f64>,
    },
    Pending {
        task_id: String,
    },
    Processing {
        task_id: String,
        progress: Option<f64>,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub status: VideoStatusWrapper,
    pub model: String,
}

/// serde 友好的 VideoStatus 包装（Default）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoStatusWrapper {
    pub completed: bool,
    pub pending: bool,
    pub processing: bool,
    pub failed: bool,
    pub video_url: Option<String>,
    pub task_id: Option<String>,
    pub progress: Option<f64>,
    pub duration: Option<f64>,
    pub error: Option<String>,
}

impl Default for VideoStatusWrapper {
    fn default() -> Self {
        Self {
            completed: false,
            pending: false,
            processing: false,
            failed: false,
            video_url: None,
            task_id: None,
            progress: None,
            duration: None,
            error: None,
        }
    }
}
