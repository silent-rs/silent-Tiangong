//! tiangong-types：天工统一数据结构
//!
//! 所有 crate（core、cli、gui、server、connector）共用的数据类型。
//! 不包含业务逻辑，只有数据结构和序列化。

pub mod message;
pub mod remote;
pub mod session;
pub mod status;
pub mod stream;
pub mod token;

pub use message::{MediaAsset, MediaKind, Message, MessageRole, now_text};
pub use remote::{IncomingMessage, MessageContent, OutgoingMessage, RemoteRole};
pub use session::Session;
pub use status::RunStatus;
pub use stream::{SessionStreamEvent, StreamEvent};
pub use token::TokenUsage;

#[cfg(test)]
mod tests;
