mod cli;
mod core;
mod ui;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args();
    let _bin = args.next();

    match args.next().as_deref() {
        None | Some("ui") => ui::run(),
        Some("chat") => cli::run_chat(),
        Some("-h") | Some("--help") => {
            print_usage();
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!(
            "未知命令：{other}\n\n用法：\n  tiangong            启动桌面 UI\n  tiangong ui         启动桌面 UI\n  tiangong chat       启动终端对话模式\n  tiangong --help     查看帮助"
        )),
    }
}

fn print_usage() {
    println!("用法：");
    println!("  tiangong            启动桌面 UI");
    println!("  tiangong ui         启动桌面 UI");
    println!("  tiangong chat       启动终端对话模式");
    println!("  tiangong --help     查看帮助");
}
