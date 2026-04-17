//! IPC 模块骨架（Phase B：UDS IPC 服务端/客户端）
//!
//! Phase B 先实现进程内单 Leader 模式，UDS 多进程通信在后续补全。

pub(crate) mod protocol;

/// IPC 服务端骨架
#[allow(dead_code)]
pub(crate) struct IpcServer;

/// IPC 客户端骨架
#[allow(dead_code)]
pub(crate) struct IpcClient;
