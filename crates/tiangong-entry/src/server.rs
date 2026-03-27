use anyhow::Result;

use crate::args::{ServerArgs, ServerSubcommand};

pub(crate) fn run_server_command(args: ServerArgs) -> Result<()> {
    // 处理 stop 子命令
    if let Some(ServerSubcommand::Stop) = args.command {
        return tiangong_server::stop_daemon();
    }

    // daemon 模式：后台启动并退出主进程
    if args.daemon {
        return tiangong_server::run_daemon(&args.host, args.port, args.token);
    }

    // 前台运行
    tiangong_server::run_server(&args.host, args.port, args.token)
}
