//! Screenshot Input 插件私有业务协议。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "screenshot-input";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SCREENSHOT_PROTOCOL_VERSION: u32 = 1;
pub const CAPTURE_OPERATION: &str = "capture";

pub trait ScreenshotOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub struct Capture;

impl ScreenshotOperation for Capture {
    const NAME: &'static str = CAPTURE_OPERATION;
    type Request = CaptureRequest;
    type Response = CaptureResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureRequest {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub cancelled: bool,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub original_name: Option<String>,
}
