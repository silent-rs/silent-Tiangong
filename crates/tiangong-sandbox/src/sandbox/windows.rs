//! Windows 沙箱状态（RFC 0017 S6：受限令牌，未实现）。
//!
//! Windows v1 维持 journal-only 弱档：快照恢复层可用（S1 已交付），
//! 进程级写白名单待受限令牌（WRITE_RESTRICTED + 独立受限用户）实验结论后实现。

/// Windows 平台当前不支持进程级沙箱包装。
pub const UNSUPPORTED_REASON: &str =
    "Windows 沙箱（受限令牌）尚未实现（RFC 0017 S6），当前仅有快照恢复保护";
