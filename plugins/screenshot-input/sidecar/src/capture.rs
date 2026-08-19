use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use tiangong_plugin_screenshot_input_protocol::CaptureResponse;

use crate::platform::{CaptureOutcome, capture_to_file};

const MAX_BASE64_BYTES: usize = 50 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

struct TemporaryCapture {
    directory: PathBuf,
    image: PathBuf,
}

impl TemporaryCapture {
    fn create() -> Result<Self> {
        let directory = std::env::temp_dir()
            .join("tiangong-screenshot-input")
            .join(scru128::new().to_string());
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("创建截图临时目录失败: {}", directory.display()))?;
        Ok(Self {
            image: directory.join("screenshot.png"),
            directory,
        })
    }
}

impl Drop for TemporaryCapture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub fn capture_region() -> Result<CaptureResponse> {
    let capture = TemporaryCapture::create()?;
    if matches!(
        capture_to_file(&capture.image).map_err(anyhow::Error::msg)?,
        CaptureOutcome::Cancelled
    ) {
        return Ok(CaptureResponse {
            cancelled: true,
            source: None,
            original_name: None,
        });
    }

    let bytes = std::fs::read(&capture.image)
        .with_context(|| format!("读取截图失败: {}", capture.image.display()))?;
    if !bytes.starts_with(PNG_SIGNATURE) {
        bail!("截图工具未生成有效的 PNG 图片");
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    if encoded.len() > MAX_BASE64_BYTES {
        bail!("截图超过 50MB 限制");
    }
    Ok(CaptureResponse {
        cancelled: false,
        source: Some(format!("data:image/png;base64,{encoded}")),
        original_name: Some(format!("screenshot-{}.png", scru128::new())),
    })
}
