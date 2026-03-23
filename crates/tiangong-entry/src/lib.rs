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
        Some(MainCommand::Cli) => tiangong_cli::run_cli(),
        None | Some(MainCommand::Ui) => Err(anyhow::anyhow!("UI 模式请通过 Tauri 桌面应用启动")),
    }
}
