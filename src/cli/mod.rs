use anyhow::Result;

mod tui;

pub fn run_cli() -> Result<()> {
    tui::run_cli()
}

pub fn run_chat() -> Result<()> {
    run_cli()
}
