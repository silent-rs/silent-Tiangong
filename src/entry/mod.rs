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
        None | Some(MainCommand::Ui) => crate::ui::run(),
        Some(MainCommand::Cli) => crate::cli::run_cli(),
        Some(MainCommand::Mcp(args)) => mcp::run_mcp_command(args),
        Some(MainCommand::Skill(args)) => skill::run_skill_command(args),
    }
}
