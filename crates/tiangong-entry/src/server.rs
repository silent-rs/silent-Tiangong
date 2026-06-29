use anyhow::{Result, anyhow};

use crate::args::{ServerArgs, ServerConfigSubcommand, ServerSubcommand, ServerTokenSubcommand};

pub(crate) fn run_server_command(args: ServerArgs) -> Result<()> {
    match args.command {
        // —— 子命令：配置/状态/Token 管理 ——
        Some(ServerSubcommand::Status) => return run_status(),
        Some(ServerSubcommand::Config { command }) => return run_config(command),
        Some(ServerSubcommand::Token { command }) => return run_token(command),
        // —— 子命令：停止后台进程 ——
        Some(ServerSubcommand::Stop) => return tiangong_server::stop_daemon(),
        // 无子命令：进入启动分支（下方处理 host/port/token/daemon）
        None => {}
    }

    // daemon 模式：后台启动并退出主进程
    if args.daemon {
        return tiangong_server::run_daemon(&args.host, args.port, args.token);
    }

    // 前台运行
    tiangong_server::run_server(&args.host, args.port, args.token)
}

fn run_config(command: ServerConfigSubcommand) -> Result<()> {
    use tiangong_config::{load_server_config, save_server_config};
    match command {
        ServerConfigSubcommand::Show => {
            let config = load_server_config();
            println!("host: {}", config.host);
            println!("port: {}", config.port);
            println!("auth_token: {}", config.masked_auth_token());
        }
        ServerConfigSubcommand::Set { host, port } => {
            if host.is_none() && port.is_none() {
                return Err(anyhow!("请至少指定 --host 或 --port"));
            }
            let mut config = load_server_config();
            if let Some(host) = host {
                config.host = host;
            }
            if let Some(port) = port {
                config.port = port;
            }
            save_server_config(&config)?;
            println!("Server 配置已更新：{}:{}", config.host, config.port);
        }
    }
    Ok(())
}

fn run_token(command: ServerTokenSubcommand) -> Result<()> {
    use tiangong_config::{generate_token, load_server_config, save_server_config};
    match command {
        ServerTokenSubcommand::Show => {
            let config = load_server_config();
            println!("{}", config.masked_auth_token());
        }
        ServerTokenSubcommand::Set { token } => {
            let mut config = load_server_config();
            config.auth_token = Some(token);
            save_server_config(&config)?;
            println!("Server Token 已设置：{}", config.masked_auth_token());
        }
        ServerTokenSubcommand::Generate { length } => {
            let token = generate_token(length);
            let mut config = load_server_config();
            config.auth_token = Some(token);
            save_server_config(&config)?;
            println!("已生成新的 Server Token：{}", config.masked_auth_token());
            println!("（完整 Token 已写入 server.json）");
        }
    }
    Ok(())
}

fn run_status() -> Result<()> {
    use tiangong_config::load_server_config;
    let config = load_server_config();
    let status = tiangong_server::daemon::server_status(&config.host, config.port);

    println!("Server 配置：{}:{}", config.host, config.port);
    println!("Token：{}", config.masked_auth_token());
    println!(
        "PID 文件：{}",
        if status.pid_file_exists {
            "存在"
        } else {
            "不存在"
        }
    );
    if let Some(pid) = status.pid {
        println!("记录 PID：{pid}");
    }
    println!(
        "进程状态：{}",
        if status.process_alive {
            "\x1b[32m运行中\x1b[0m"
        } else {
            "\x1b[31m未运行\x1b[0m"
        }
    );
    println!(
        "端口监听：{}",
        if status.port_listening {
            "\x1b[32m是\x1b[0m"
        } else {
            "\x1b[31m否\x1b[0m"
        }
    );
    Ok(())
}
