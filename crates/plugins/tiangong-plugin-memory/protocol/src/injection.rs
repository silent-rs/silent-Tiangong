use serde::{Deserialize, Serialize};

use crate::MemoryOperation;

pub const LOAD_OPERATION: &str = "load_injection";

pub struct LoadInjection;

impl MemoryOperation for LoadInjection {
    const NAME: &'static str = LOAD_OPERATION;
    type Request = LoadInjectionRequest;
    type Response = LoadInjectionResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadInjectionRequest {
    pub session_id: String,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadInjectionResponse {
    #[serde(default)]
    pub items: Vec<String>,
}
