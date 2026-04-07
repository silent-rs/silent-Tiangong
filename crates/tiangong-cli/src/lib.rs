#[allow(dead_code)]
mod commands;
mod completion;
mod input;
#[allow(dead_code)]
mod modal;
mod output;
mod repl;

pub fn run_cli() -> anyhow::Result<()> {
    repl::run()
}
