mod cli;
mod core;
mod entry;
mod ui;

fn main() -> anyhow::Result<()> {
    entry::run()
}
