use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::image::{
    GeneratedImage, ImageEditRequest, ImageGenRequest, ImageGenResponse, ImageGenerator,
};

/// OpenAI 图片生成后端（支持 DALL-E 3 / GPT-Image-1）
pub struct OpenAIImageGenerator {
    api_key: String,
    api_base: String,
    model: String,
    client: Client,
}

impl OpenAIImageGenerator {
    /// 创建 OpenAI 图片生成器
    ///
    /// - `api_key`: OpenAI API Key
    /// - `api_base`: API 基础地址，如 `https://api.openai.com`
    /// - `model`: 模型名称，如 `dall-e-3` 或 `gpt-image-1`
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        Self {
            api_key,
            api_base,
            model,
            client: Client::new(),
        }
    }
}

/// OpenAI /v1/images/generations 请求体
#[derive(Debug, Serialize)]
struct ImageGenBody {
    model: String,
    prompt: String,
    n: u32,
    size: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    style: Option<String>,
}

/// OpenAI 图片生成 API 响应
#[derive(Debug, Deserialize)]
struct OpenAIImageResponse {
    data: Vec<OpenAIImageData>,
}

#[derive(Debug, Deserialize)]
struct OpenAIImageData {
    url: Option<String>,
    b64_json: Option<String>,
    revised_prompt: Option<String>,
}

/// OpenAI 错误响应
#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: OpenAIError,
}

#[derive(Debug, Deserialize)]
struct OpenAIError {
    message: String,
}

#[async_trait]
impl ImageGenerator for OpenAIImageGenerator {
    fn name(&self) -> &str {
        "openai-image"
    }

    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse> {
        let size = format!("{}x{}", request.width, request.height);
        let body = ImageGenBody {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            prompt: request.prompt,
            n: request.num_images,
            size,
            style: request.style,
        };

        debug!("OpenAI 图片生成请求: {:?}", body);

        let url = format!("{}/images/generations", self.api_base.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("请求 OpenAI 图片生成 API 失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.context("读取响应体失败")?;

        if !status.is_success() {
            let err_msg = serde_json::from_str::<OpenAIErrorResponse>(&resp_text)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| resp_text.clone());
            return Err(anyhow!("OpenAI 图片生成失败 ({}): {}", status, err_msg));
        }

        let api_resp: OpenAIImageResponse =
            serde_json::from_str(&resp_text).context("解析 OpenAI 图片生成响应失败")?;

        let images = api_resp
            .data
            .into_iter()
            .map(|d| GeneratedImage {
                url: d.url,
                b64_data: d.b64_json,
                revised_prompt: d.revised_prompt,
            })
            .collect();

        Ok(ImageGenResponse { images })
    }

    async fn edit(&self, request: ImageEditRequest) -> Result<ImageGenResponse> {
        let url = format!("{}/images/edits", self.api_base.trim_end_matches('/'));
        let model = request.model.unwrap_or_else(|| self.model.clone());

        let image_part = reqwest::multipart::Part::bytes(request.image)
            .file_name("image.png")
            .mime_str("image/png")
            .context("构造 image 文件部分失败")?;

        let prompt_part = reqwest::multipart::Part::text(request.prompt);
        let model_part = reqwest::multipart::Part::text(model);
        let n_part = reqwest::multipart::Part::text(request.num_images.to_string());

        let mut form = reqwest::multipart::Form::new()
            .part("image", image_part)
            .part("prompt", prompt_part)
            .part("model", model_part)
            .part("n", n_part);

        if let Some(mask_data) = request.mask {
            let mask_part = reqwest::multipart::Part::bytes(mask_data)
                .file_name("mask.png")
                .mime_str("image/png")
                .context("构造 mask 文件部分失败")?;
            form = form.part("mask", mask_part);
        }

        debug!("OpenAI 图片编辑请求: url={}", url);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .context("请求 OpenAI 图片编辑 API 失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.context("读取响应体失败")?;

        if !status.is_success() {
            let err_msg = serde_json::from_str::<OpenAIErrorResponse>(&resp_text)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| resp_text.clone());
            return Err(anyhow!("OpenAI 图片编辑失败 ({}): {}", status, err_msg));
        }

        let api_resp: OpenAIImageResponse =
            serde_json::from_str(&resp_text).context("解析 OpenAI 图片编辑响应失败")?;

        let images = api_resp
            .data
            .into_iter()
            .map(|d| GeneratedImage {
                url: d.url,
                b64_data: d.b64_json,
                revised_prompt: d.revised_prompt,
            })
            .collect();

        Ok(ImageGenResponse { images })
    }
}
