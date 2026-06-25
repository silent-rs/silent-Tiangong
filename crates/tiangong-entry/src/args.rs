use clap::{Args, Parser, Subcommand, ValueEnum};

use tiangong_core::agent_config::McpTransportMode;

#[derive(Debug, Parser)]
#[command(
    name = "tiangong",
    disable_help_subcommand = true,
    arg_required_else_help = false,
    about = "天工应用入口"
)]
pub(crate) struct MainArgs {
    #[command(subcommand)]
    pub(crate) command: Option<MainCommand>,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum MainCommand {
    #[command(about = "启动桌面 UI")]
    Ui,
    #[command(about = "启动 CLI 模式")]
    Cli {
        /// 信任模式（full_trust / supervised）
        #[arg(long = "trust-mode", value_enum, help = "强制指定信任模式")]
        trust_mode: Option<TrustModeArg>,
    },
    #[command(about = "启动 Server 模式")]
    Server(ServerArgs),
    #[command(about = "MCP 配置管理")]
    Mcp(McpArgs),
    #[command(about = "Skill 配置管理")]
    Skill(SkillArgs),
    #[command(about = "检查并安装天工更新")]
    Update(UpdateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// 只检查更新，不安装
    #[arg(long, help = "只检查更新，不安装")]
    pub(crate) check: bool,
    /// 更新源地址，默认使用 GitHub Release updater JSON
    #[arg(long, help = "覆盖更新源地址")]
    pub(crate) endpoint: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ServerArgs {
    #[command(subcommand)]
    pub(crate) command: Option<ServerSubcommand>,
    /// 监听地址
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) host: String,
    /// 监听端口
    #[arg(long, default_value_t = 8080)]
    pub(crate) port: u16,
    /// API 认证 Token
    #[arg(long, help = "API 认证 Token")]
    pub(crate) token: Option<String>,
    /// 后台运行
    #[arg(short, long, help = "后台运行")]
    pub(crate) daemon: bool,
    /// 信任模式
    #[arg(long = "trust-mode", value_enum, help = "强制指定信任模式")]
    pub(crate) trust_mode: Option<TrustModeArg>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServerSubcommand {
    #[command(about = "停止后台 Server")]
    Stop,
}

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    pub(crate) command: McpSubcommand,
}

#[derive(Debug, Args)]
pub(crate) struct SkillArgs {
    #[command(subcommand)]
    pub(crate) command: SkillSubcommand,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustModeArg {
    /// 完全信任（工具自动执行，无审批）
    FullTrust,
    /// 监督模式（高风险工具需要用户审批）
    Supervised,
}

impl TrustModeArg {
    pub(crate) fn to_trust_mode(self) -> tiangong_core::permission::TrustMode {
        match self {
            TrustModeArg::FullTrust => tiangong_core::permission::TrustMode::FullTrust,
            TrustModeArg::Supervised => tiangong_core::permission::TrustMode::Supervised,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum McpTransportArg {
    Auto,
    Stdio,
    Http,
}

impl From<McpTransportArg> for McpTransportMode {
    fn from(value: McpTransportArg) -> Self {
        match value {
            McpTransportArg::Auto => McpTransportMode::Auto,
            McpTransportArg::Stdio => McpTransportMode::Stdio,
            McpTransportArg::Http => McpTransportMode::Http,
        }
    }
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum McpSubcommand {
    #[command(about = "查看全部 MCP server")]
    List,
    #[command(about = "查看指定 MCP server（不传 name 时等同 list）")]
    Show {
        #[arg(help = "MCP server 名称")]
        name: Option<String>,
    },
    #[command(about = "注册 MCP server")]
    Add {
        #[arg(help = "MCP server 名称（非 JSON 导入模式必填）")]
        name: Option<String>,
        #[arg(help = "MCP server 命令（如 npx；HTTP 可直接填 endpoint）")]
        command: Option<String>,
        #[arg(
            long,
            help = "通过 JSON 字符串导入（支持单对象或 {\"mcpServers\": {...}}）",
            conflicts_with = "json_file"
        )]
        json: Option<String>,
        #[arg(
            long = "json-file",
            value_name = "PATH",
            help = "通过 JSON 文件导入（支持单对象或 {\"mcpServers\": {...}}）",
            conflicts_with = "json"
        )]
        json_file: Option<String>,
        #[arg(long, default_value_t = false, help = "同名 server 存在时覆盖")]
        force: bool,
        #[arg(
            long = "arg",
            short = 'a',
            allow_hyphen_values = true,
            help = "命令参数，可重复，如 -a -y -a @modelcontextprotocol/server-browser"
        )]
        args: Vec<String>,
        #[arg(long, value_delimiter = ',', help = "标签列表，逗号分隔")]
        tags: Vec<String>,
        #[arg(long, value_enum, help = "传输类型（auto/stdio/http）")]
        transport: Option<McpTransportArg>,
        #[arg(long, help = "HTTP MCP endpoint（如 https://example.com/mcp）")]
        endpoint: Option<String>,
        #[arg(long, help = "HTTP MCP Bearer Token（不带 Bearer 前缀）")]
        auth_header: Option<String>,
        #[arg(
            long = "header",
            value_parser = parse_key_value,
            help = "HTTP header，格式 key=value，可重复"
        )]
        headers: Vec<(String, String)>,
        #[arg(
            long = "env",
            value_parser = parse_key_value,
            help = "stdio env，格式 key=value，可重复"
        )]
        env: Vec<(String, String)>,
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "CMDLINE",
            help = "通过 -- 传入完整命令，如 -- npx -y @modelcontextprotocol/server-filesystem /path"
        )]
        cmdline: Vec<String>,
        #[arg(long, default_value_t = true, help = "是否启用（true/false）")]
        enabled: bool,
    },
    #[command(about = "删除 MCP server")]
    Remove {
        #[arg(help = "MCP server 名称")]
        name: String,
    },
    #[command(about = "启用 MCP server")]
    Enable {
        #[arg(help = "MCP server 名称")]
        name: String,
    },
    #[command(about = "禁用 MCP server")]
    Disable {
        #[arg(help = "MCP server 名称")]
        name: String,
    },
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum SkillSubcommand {
    #[command(about = "查看全部 Skill")]
    List,
    #[command(about = "查看指定 Skill（不传 id 时等同 list）")]
    Show {
        #[arg(help = "Skill ID")]
        id: Option<String>,
    },
    #[command(
        about = "初始化 Skill 脚手架（生成 SKILL.md 与 skill.toml）",
        visible_alias = "create"
    )]
    Init {
        #[arg(help = "目标目录")]
        path: String,
        #[arg(long, help = "Skill 名称")]
        name: Option<String>,
        #[arg(long, help = "Skill ID")]
        id: Option<String>,
        #[arg(long, default_value_t = false, help = "存在同名文件时是否覆盖")]
        force: bool,
    },
    #[command(about = "安装 Skill（本地目录）", visible_alias = "add")]
    Install {
        #[arg(help = "Skill 本地目录")]
        path: String,
        #[arg(long, default_value_t = true, help = "是否启用（true/false）")]
        enabled: bool,
        #[arg(long, default_value_t = false, help = "是否启用外部 Skill 转换")]
        convert: bool,
    },
    #[command(about = "删除 Skill")]
    Remove {
        #[arg(help = "Skill ID")]
        id: String,
    },
    #[command(about = "启用 Skill")]
    Enable {
        #[arg(help = "Skill ID")]
        id: String,
    },
    #[command(about = "禁用 Skill")]
    Disable {
        #[arg(help = "Skill ID")]
        id: String,
    },
    #[command(about = "刷新 Skill 注册表（重扫 skills/<id>/）")]
    Refresh,
    #[command(about = "检测或清理孤儿 Skill 托管 MCP 配置")]
    Gc {
        #[arg(long, default_value_t = false, help = "执行清理；默认只报告不删除")]
        apply: bool,
    },
    #[command(about = "诊断 Skill 注册表、manifest 与托管 MCP 一致性")]
    Doctor,
    #[command(about = "校验配置")]
    Validate,
}

pub(crate) fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("参数格式无效（需 key=value）：{raw}"))?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() || value.is_empty() {
        return Err(format!("参数格式无效（key/value 不能为空）：{raw}"));
    }
    Ok((key.to_string(), value.to_string()))
}
