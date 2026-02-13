use anyhow::Result;

mod tui;

pub fn run_cli() -> Result<()> {
    tui::run_cli()
}
