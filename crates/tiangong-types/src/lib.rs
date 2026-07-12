//! tiangong-types：天工统一数据结构
//!
//! 所有 crate（core、cli、gui、server、connector）共用的数据类型。
//! 不包含业务逻辑，只有数据结构和序列化。

pub mod attachment;
pub mod event;
pub mod message;
pub mod process;
pub mod remote;
pub mod session;
pub mod status;
pub mod stream;
pub mod token;
pub mod trust_mode;

pub use attachment::{
    StoredAsset, content_blocks_are_empty, content_blocks_text, stable_content_blocks,
    validate_ready_content_blocks,
};
pub use event::{EventSource, RuntimeEvent, RuntimeEventType};
pub use message::{
    ContentBlock, DeferredToolInjection, MediaAsset, MediaKind, Message, MessagePhase, MessageRole,
    MessageToolCall, PendingAgentDelivery, TurnStatus, now_text,
};
pub use process::{configure_no_window, configure_tokio_no_window};
pub use remote::{IncomingMessage, MessageContent, OutgoingMessage, RemoteRole};
pub use session::Session;
pub use status::RunStatus;
pub use stream::{MemoryRecallHitSummary, SessionStreamEvent, StreamEvent, StreamToolCall};
pub use token::TokenUsage;
pub use trust_mode::TrustMode;

#[cfg(test)]
mod tests;
