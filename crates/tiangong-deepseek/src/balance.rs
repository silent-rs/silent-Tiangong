use crate::client::DeepSeekClient;
use crate::error::DeepSeekError;
use crate::types::BalanceResponse;

pub struct Balance<'c> {
    client: &'c DeepSeekClient,
}

impl<'c> Balance<'c> {
    pub fn new(client: &'c DeepSeekClient) -> Self {
        Self { client }
    }

    pub async fn get(&self) -> Result<BalanceResponse, DeepSeekError> {
        self.client.get("/user/balance").await
    }
}
