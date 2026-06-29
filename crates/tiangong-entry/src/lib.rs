mod args;
mod config;
mod configure;
mod doctor;
mod interactive;
mod mcp;
mod memory;
mod model;
mod prompt;
mod secrets;
mod server;
mod skill;
mod update;

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
        Some(MainCommand::Server(args)) => server::run_server_command(args),
        Some(MainCommand::Mcp(args)) => mcp::run_mcp_command(args),
        Some(MainCommand::Model(args)) => model::run_model_command(args),
        Some(MainCommand::Memory(args)) => memory::run_memory_command(args),
        Some(MainCommand::Skill(args)) => skill::run_skill_command(args),
        Some(MainCommand::Prompt(args)) => prompt::run_prompt_command(args),
        Some(MainCommand::Config(args)) => config::run_config_command(args),
        Some(MainCommand::Doctor(args)) => doctor::run_doctor_command(args),
        Some(MainCommand::Update(args)) => update::run_update_command(args),
        Some(MainCommand::Cli { trust_mode }) => {
            tiangong_cli::run_cli_with_trust_mode(trust_mode.map(|m| m.to_trust_mode()))
        }
        None | Some(MainCommand::Ui) => Err(anyhow::anyhow!("UI 模式请通过 Tauri 桌面应用启动")),
    }
}
