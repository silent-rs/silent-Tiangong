mod cli;
mod core;
mod ui;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::core::agent_config::McpTransportMode;
use crate::core::app_state::{RegisterMcpServerOptions, TiangongState};

#[derive(Debug, Parser)]
#[command(
    name = "tiangong",
    disable_help_subcommand = true,
    arg_required_else_help = false,
    about = "天工应用入口"
)]
struct MainArgs {
    #[command(subcommand)]
    command: Option<MainCommand>,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum MainCommand {
    #[command(about = "启动桌面 UI")]
    Ui,
    #[command(about = "启动 CLI 模式")]
    Cli,
    #[command(about = "MCP 配置管理")]
    Mcp(McpArgs),
}

#[derive(Debug, Args)]
struct McpArgs {
    #[command(subcommand)]
    command: McpSubcommand,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum McpTransportArg {
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
enum McpSubcommand {
    #[command(about = "查看全部 MCP server")]
    List,
    #[command(about = "查看指定 MCP server（不传 name 时等同 list）")]
    Show {
        #[arg(help = "MCP server 名称")]
        name: Option<String>,
    },
    #[command(about = "注册 MCP server")]
    Add {
        #[arg(help = "MCP server 名称")]
        name: String,
        #[arg(help = "MCP server 命令（如 npx；HTTP 可直接填 endpoint）")]
        command: Option<String>,
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
        #[arg(long, help = "stdio 工作目录")]
        cwd: Option<String>,
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

fn main() -> anyhow::Result<()> {
    let args = match MainArgs::try_parse() {
        Ok(args) => args,
        Err(err) => {
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                print!("{err}");
                return Ok(());
            }
            return Err(anyhow::anyhow!(err.to_string()));
        }
    };

    match args.command {
        None | Some(MainCommand::Ui) => ui::run(),
        Some(MainCommand::Cli) => cli::run_cli(),
        Some(MainCommand::Mcp(mcp_args)) => run_mcp_command(mcp_args),
    }
}

fn run_mcp_command(args: McpArgs) -> anyhow::Result<()> {
    let mut state = TiangongState::load_or_default();
    match args.command {
        McpSubcommand::List => {
            println!("{}", state.mcp_server_summary(None));
        }
        McpSubcommand::Show { name } => {
            if let Some(name) = name {
                println!("{}", state.mcp_server_detail(Some(&name)));
            } else {
                println!("{}", state.mcp_server_summary(None));
            }
        }
        McpSubcommand::Add {
            name,
            command,
            args,
            tags,
            transport,
            endpoint,
            auth_header,
            headers,
            env,
            cwd,
            cmdline,
            enabled,
        } => {
            let (command, args) = resolve_mcp_add_command(command, args, cmdline)?;
            let msg = state.register_mcp_server(
                &name,
                &command,
                args,
                tags,
                enabled,
                RegisterMcpServerOptions {
                    transport: transport.map(Into::into),
                    endpoint,
                    auth_header,
                    headers,
                    env,
                    cwd,
                },
            )?;
            println!("{msg}");
        }
        McpSubcommand::Remove { name } => {
            let msg = state.remove_mcp_server(&name)?;
            println!("{msg}");
        }
        McpSubcommand::Enable { name } => {
            let msg = state.set_mcp_server_enabled(&name, true)?;
            println!("{msg}");
        }
        McpSubcommand::Disable { name } => {
            let msg = state.set_mcp_server_enabled(&name, false)?;
            println!("{msg}");
        }
    }
    Ok(())
}

fn parse_key_value(raw: &str) -> Result<(String, String), String> {
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

fn resolve_mcp_add_command(
    command: Option<String>,
    mut args: Vec<String>,
    cmdline: Vec<String>,
) -> anyhow::Result<(String, Vec<String>)> {
    let command = command
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut cmdline = cmdline
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if cmdline.is_empty() {
        return Ok((command.unwrap_or_default(), args));
    }

    if let Some(command) = command {
        args.extend(cmdline);
        return Ok((command, args));
    }

    if cmdline.first().is_some_and(|item| item.starts_with('-')) {
        return Err(anyhow::anyhow!(
            "命令缺少可执行项，请在 -- 后提供 command，如 -- npx -y ..."
        ));
    }

    let command = cmdline.remove(0);
    args.extend(cmdline);
    Ok((command, args))
}
