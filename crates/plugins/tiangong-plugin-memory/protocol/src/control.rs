use serde::{Deserialize, Serialize};

use crate::{Ack, Empty, MemoryOperation};

pub const ENABLE_OPERATION: &str = "enable";
pub const DISABLE_OPERATION: &str = "disable";
pub const STATUS_OPERATION: &str = "status";
pub const TEST_OPERATION: &str = "test";

pub struct Enable;
pub struct Disable;
pub struct Status;
pub struct Test;

impl MemoryOperation for Enable {
    const NAME: &'static str = ENABLE_OPERATION;
    type Request = Empty;
    type Response = EnableResponse;
}

impl MemoryOperation for Disable {
    const NAME: &'static str = DISABLE_OPERATION;
    type Request = Empty;
    type Response = EnableResponse;
}

impl MemoryOperation for Status {
    const NAME: &'static str = STATUS_OPERATION;
    type Request = Empty;
    type Response = StatusResponse;
}

impl MemoryOperation for Test {
    const NAME: &'static str = TEST_OPERATION;
    type Request = Empty;
    type Response = TestResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnableResponse {
    pub ok: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub model: String,
    pub base_url: String,
    pub configured: bool,
    #[serde(default)]
    pub dimension: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub disabled: bool,
    pub vector_mode: String,
    pub llm: Option<ModelStatus>,
    pub embedding: Option<ModelStatus>,
    pub rerank: Option<ModelStatus>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestResponse {
    pub ok: bool,
    #[serde(default)]
    pub issues: Vec<String>,
}

pub type AckResponse = Ack;
