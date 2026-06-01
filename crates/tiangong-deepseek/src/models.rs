use crate::client::DeepSeekClient;
use crate::error::DeepSeekError;
use crate::types::ListModelsResponse;

pub struct Models<'c> {
    client: &'c DeepSeekClient,
}

impl<'c> Models<'c> {
    pub fn new(client: &'c DeepSeekClient) -> Self {
        Self { client }
    }

    pub async fn list(&self) -> Result<ListModelsResponse, DeepSeekError> {
        self.client.get("/models").await
    }
}
