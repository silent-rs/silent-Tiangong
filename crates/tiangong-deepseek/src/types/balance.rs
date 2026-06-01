use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceResponse {
    pub is_available: bool,
    pub balance_infos: Vec<BalanceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceInfo {
    pub currency: Currency,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Currency {
    #[serde(rename = "CNY")]
    Cny,
    #[serde(rename = "USD")]
    Usd,
}
