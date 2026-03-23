mod commands;
mod completion;
mod input;
mod modal;
mod output;
mod repl;

pub fn run_cli() -> anyhow::Result<()> {
    repl::run()
}
