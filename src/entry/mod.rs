mod args;
mod mcp;
mod skill;

use clap::Parser;
use clap::error::ErrorKind;

use self::args::{MainArgs, MainCommand};

pub fn run() -> anyhow::Result<()> {
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
        Some(MainCommand::Mcp(args)) => mcp::run_mcp_command(args),
        Some(MainCommand::Skill(args)) => skill::run_skill_command(args),
        #[cfg(feature = "cli")]
        Some(MainCommand::Cli) => {
            // CLI 模块在此 worktree 中不可用
            // 如需使用 CLI，请切换到主分支
            Err(anyhow::anyhow!("CLI 模式在当前版本中不可用"))
        }
        #[cfg(not(feature = "cli"))]
        Some(MainCommand::Cli) => {
            Err(anyhow::anyhow!("CLI 模式未启用，请使用 --features cli 编译"))
        }
        #[cfg(feature = "tui")]
        None | Some(MainCommand::Ui) => {
            Err(anyhow::anyhow!("TUI 模式暂未在此版本中实现"))
        }
        #[cfg(not(feature = "tui"))]
        None | Some(MainCommand::Ui) => {
            Err(anyhow::anyhow!("UI 模式未启用，请使用 --features tui 编译"))
        }
    }
}
