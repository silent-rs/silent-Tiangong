mod cli;
mod core;
mod ui;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand};

use crate::core::app_state::TiangongState;

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

#[derive(Debug, Subcommand)]
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
        #[arg(help = "MCP server 命令（如 npx）")]
        command: String,
        #[arg(
            long = "arg",
            short = 'a',
            help = "命令参数，可重复，如 -a -y -a @modelcontextprotocol/server-browser"
        )]
        args: Vec<String>,
        #[arg(long, value_delimiter = ',', help = "标签列表，逗号分隔")]
        tags: Vec<String>,
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
            println!("{}", state.mcp_server_summary(name.as_deref()));
        }
        McpSubcommand::Add {
            name,
            command,
            args,
            tags,
            enabled,
        } => {
            let msg = state.register_mcp_server(&name, &command, args, tags, enabled)?;
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
