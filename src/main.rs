mod cli;
mod core;
mod ui;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

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
    }
}
