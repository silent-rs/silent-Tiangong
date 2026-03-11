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

    // 先处理明确的命令
    match &args.command {
        Some(MainCommand::Mcp(_)) | Some(MainCommand::Skill(_)) => match args.command {
            Some(MainCommand::Mcp(args)) => return mcp::run_mcp_command(args),
            Some(MainCommand::Skill(args)) => return skill::run_skill_command(args),
            _ => unreachable!(),
        },
        #[cfg(feature = "cli")]
        Some(MainCommand::Cli) => {
            return crate::cli::run_cli();
        }
        #[cfg(not(feature = "cli"))]
        Some(MainCommand::Cli) => {
            return Err(anyhow::anyhow!(
                "CLI 模式未启用，请使用 --features cli 编译"
            ));
        }
        _ => {}
    }

    // 处理 UI 模式（None 或 Some(MainCommand::Ui)）
    run_ui_mode()
}

#[cfg(feature = "tui")]
fn run_ui_mode() -> anyhow::Result<()> {
    crate::ui::run()
}

#[cfg(all(not(feature = "tui"), feature = "cli"))]
fn run_ui_mode() -> anyhow::Result<()> {
    crate::cli::run_cli()
}

#[cfg(all(not(feature = "tui"), not(feature = "cli")))]
fn run_ui_mode() -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "UI 模式未启用，请使用 --features tui 或 --features cli 编译"
    ))
}
