pub use tiangong_scheduler::webhook::model::*;

use silent::prelude::*;

use super::store::WebhookStore;

pub fn open_store() -> Result<WebhookStore> {
    WebhookStore::open().map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("打开 webhook 存储失败：{e}"),
        )
    })
}
