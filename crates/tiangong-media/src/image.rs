use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 图片生成请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    /// 生成提示词
    pub prompt: String,
    /// 负面提示词
    pub negative_prompt: Option<String>,
    /// 图片宽度
    pub width: u32,
    /// 图片高度
    pub height: u32,
    /// 指定模型
    pub model: Option<String>,
    /// 风格
    pub style: Option<String>,
    /// 生成数量
    pub num_images: u32,
}

/// 图片编辑请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEditRequest {
    /// 原始图片数据
    pub image: Vec<u8>,
    /// 遮罩图片数据（用于指定编辑区域）
    pub mask: Option<Vec<u8>>,
    /// 编辑提示词
    pub prompt: String,
    /// 指定模型
    pub model: Option<String>,
    /// 生成数量
    pub num_images: u32,
}

/// 图片生成响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenResponse {
    /// 生成的图片列表
    pub images: Vec<GeneratedImage>,
}

/// 单张生成的图片
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// 图片 URL（远程存储）
    pub url: Option<String>,
    /// 图片 Base64 编码数据
    pub b64_data: Option<String>,
    /// 修订后的提示词（部分模型会返回）
    pub revised_prompt: Option<String>,
}

/// 图片生成能力 trait
#[async_trait]
pub trait ImageGenerator: Send + Sync {
    /// 生成器名称
    fn name(&self) -> &str;

    /// 根据提示词生成图片
    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse>;

    /// 编辑已有图片
    async fn edit(&self, request: ImageEditRequest) -> Result<ImageGenResponse>;
}
