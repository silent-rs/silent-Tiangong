use crate::client::{DeepSeekClient, MultipartForm};
use crate::error::DeepSeekError;
use crate::types::{DeleteFileResponse, FileObject, ListFilesParams, ListFilesResponse};

/// Files API 文件有效期：1 小时至 30 天。
pub const MIN_EXPIRES_AFTER_SECONDS: u32 = 3600;
pub const MAX_EXPIRES_AFTER_SECONDS: u32 = 2_592_000;

pub struct Files<'c> {
    client: &'c DeepSeekClient,
}

impl<'c> Files<'c> {
    pub fn new(client: &'c DeepSeekClient) -> Self {
        Self { client }
    }

    /// 上传图片文件（JPEG/PNG/GIF/WebP，按文件实际内容判断，单文件最大 64 MiB）。
    ///
    /// `expires_after_seconds` 取值 3600–2592000（1 小时至 30 天），
    /// `None` 表示永久有效。上传须在 10 分钟内完成。
    pub async fn upload(
        &self,
        filename: &str,
        data: Vec<u8>,
        expires_after_seconds: Option<u32>,
    ) -> Result<FileObject, DeepSeekError> {
        let mut form = MultipartForm::new()
            .field("purpose", "user_data")
            .file("file", filename, data);
        if let Some(seconds) = expires_after_seconds {
            form = form
                .field("expires_after[anchor]", "created_at")
                .field("expires_after[seconds]", &seconds.to_string());
        }
        self.client.post_multipart("/files", &form).await
    }

    /// 列出当前 API key 下的文件，游标分页。
    pub async fn list(&self, params: ListFilesParams) -> Result<ListFilesResponse, DeepSeekError> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(after) = params.after {
            query.push(("after", after));
        }
        if let Some(limit) = params.limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(order) = params.order {
            query.push(("order", order.as_str().to_string()));
        }
        if let Some(purpose) = params.purpose {
            query.push(("purpose", purpose));
        }
        self.client.get_with_query("/files", &query).await
    }

    /// 查询单个文件信息。
    pub async fn retrieve(&self, file_id: &str) -> Result<FileObject, DeepSeekError> {
        self.client.get(&format!("/files/{file_id}")).await
    }

    /// 删除指定文件。
    pub async fn delete(&self, file_id: &str) -> Result<DeleteFileResponse, DeepSeekError> {
        self.client.delete(&format!("/files/{file_id}")).await
    }
}
