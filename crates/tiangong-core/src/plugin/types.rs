use serde::{Deserialize, Serialize};

/// 插件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PluginType {
    /// 标准 MCP server（工具/资源）
    Mcp,
    /// Skill 插件
    Skill,
    /// 自定义能力插件
    Custom,
}

/// 插件传输方式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PluginTransport {
    /// 标准输入输出（子进程）
    Stdio,
    /// HTTP 通信
    Http,
}

/// 插件清单（元数据）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 插件唯一标识
    pub id: String,
    /// 插件名称
    pub name: String,
    /// 插件版本
    pub version: String,
    /// 插件描述
    pub description: String,
    /// 插件类型
    pub plugin_type: PluginType,
    /// 传输方式
    pub transport: PluginTransport,
    /// 声明的能力列表
    pub capabilities: Vec<String>,
}

/// 插件运行状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginStatus {
    /// 已注册但未启动
    Registered,
    /// 正在启动
    Starting,
    /// 运行中
    Running,
    /// 已停止
    Stopped,
    /// 启动失败
    Failed,
}

/// 插件实例（运行时信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstance {
    /// 插件清单
    pub manifest: PluginManifest,
    /// 运行状态
    pub status: PluginStatus,
    /// 是否启用
    pub enabled: bool,
}
