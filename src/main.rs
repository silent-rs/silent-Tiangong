mod cli;
mod core;
mod ui;

fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();

    match args.first().map(String::as_str) {
        None | Some("ui") => ui::run(),
        Some("cli") => cli::run_cli(),
        Some("-h") | Some("--help") => {
            print_usage();
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!(
            "未知命令：{other}\n\n用法：\n  tiangong            启动桌面 UI\n  tiangong ui         启动桌面 UI\n  tiangong cli        启动 CLI 模式\n  tiangong --help     查看帮助"
        )),
    }
}

fn print_usage() {
    println!("用法：");
    println!("  tiangong            启动桌面 UI");
    println!("  tiangong ui         启动桌面 UI");
    println!("  tiangong cli        启动 CLI 模式");
    println!("  tiangong --help     查看帮助");
}
