//! tiangong-types：天工统一数据结构
//!
//! 所有 crate（core、cli、gui、server、connector）共用的数据类型。
//! 不包含业务逻辑，只有数据结构和序列化。

pub mod message;
pub mod session;
pub mod stream;

pub use message::{Message, MessageRole};
pub use session::Session;
pub use stream::StreamEvent;
