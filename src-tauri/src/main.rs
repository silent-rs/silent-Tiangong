//! 天工统一入口
//!
//! 无参数或 `ui` → 启动 GUI
//! `cli` → CLI 模式
//! 其他命令 → 委托给 tiangong_entry（server/mcp/skill）
//!
//! DMG 安装后可通过 symlink 获得 CLI 能力：
//! ln -s /Applications/天工.app/Contents/MacOS/天工 /usr/local/bin/tiangong

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 检查第一个参数决定模式
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str());

    match command {
        // CLI 模式
        Some("cli") => run_cli(),
        // 帮助
        Some("-h") | Some("--help") => print_help(),
        Some("-V") | Some("--version") => print_version(),
        // 其他命令委托给 tiangong_entry（server/mcp/skill）
        Some("server") | Some("mcp") | Some("skill") => run_entry(),
        // 无参数或 "ui" → 启动 GUI
        None | Some("ui") => run_gui(),
        // 未知命令
        Some(cmd) => {
            eprintln!("未知命令：{cmd}");
            print_help();
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!("天工 — 全功能个人智能终端");
    println!();
    println!("用法：tiangong [命令]");
    println!();
    println!("命令：");
    println!("  (无)      启动桌面 GUI（默认）");
    println!("  ui        启动桌面 GUI");
    println!("  cli       启动 CLI 交互模式");
    println!("  server    启动 HTTP/WS Server");
    println!("  mcp       MCP 配置管理");
    println!("  skill     Skill 配置管理");
    println!();
    println!("选项：");
    println!("  -h, --help     显示帮助");
    println!("  -V, --version  显示版本");
    println!();
    println!("CLI 安装（macOS DMG 安装后）：");
    println!("  ln -s /Applications/天工.app/Contents/MacOS/天工 /usr/local/bin/tiangong");
}

fn print_version() {
    println!("天工 {}", env!("CARGO_PKG_VERSION"));
}

fn run_cli() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    if let Err(err) = tiangong_cli::run_cli() {
        eprintln!("CLI 错误：{err}");
        std::process::exit(1);
    }
}

fn run_entry() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    if let Err(err) = tiangong_entry::run() {
        eprintln!("错误：{err}");
        std::process::exit(1);
    }
}

fn run_gui() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tauri::Builder::default()
        .manage(tiangong_app::TiangongApp::new())
        .invoke_handler(tauri::generate_handler![
            tiangong_app::commands::get_sessions,
            tiangong_app::commands::create_session,
            tiangong_app::commands::switch_session,
            tiangong_app::commands::delete_session,
            tiangong_app::commands::update_session_title,
            tiangong_app::commands::send_message,
            tiangong_app::commands::cancel_turn,
            tiangong_app::commands::get_background_tasks,
            tiangong_app::commands::cancel_background_task,
            tiangong_app::commands::get_run_snapshot,
            tiangong_app::commands::get_input_draft,
            tiangong_app::commands::set_input_draft,
            tiangong_app::commands::get_session_cwd,
            tiangong_app::commands::set_session_cwd,
            tiangong_app::commands::get_mcp_servers,
            tiangong_app::commands::get_mcp_health,
            tiangong_app::commands::register_mcp_server,
            tiangong_app::commands::remove_mcp_server,
            tiangong_app::commands::set_mcp_server_enabled,
            tiangong_app::commands::get_skills,
            tiangong_app::commands::inspect_skill,
            tiangong_app::commands::install_skill,
            tiangong_app::commands::get_skill_env,
            tiangong_app::commands::set_skill_env,
            tiangong_app::commands::remove_skill,
            tiangong_app::commands::set_skill_enabled,
            tiangong_app::commands::get_server_config,
            tiangong_app::commands::set_server_config,
            tiangong_app::commands::get_connectors,
            tiangong_app::commands::set_connector_enabled,
            tiangong_app::commands::get_models_config,
            tiangong_app::commands::set_models_config,
            tiangong_app::commands::get_model_capabilities,
            tiangong_app::commands::get_model_list,
            tiangong_app::commands::fetch_provider_models,
            tiangong_app::commands::append_message,
            tiangong_app::commands::respond_approval,
            tiangong_app::commands::get_trust_mode,
            tiangong_app::commands::set_trust_mode,
            tiangong_app::commands::get_session_cost,
            tiangong_app::commands::list_workers,
            tiangong_app::commands::synthesize_speech,
            tiangong_app::commands::has_tts_capability,
            tiangong_app::commands::has_stt_capability,
            tiangong_app::commands::transcribe_speech,
            tiangong_app::commands::list_tts_voices,
            tiangong_app::commands::play_audio_file,
            tiangong_app::commands::stop_audio,
            tiangong_app::commands::get_mention_candidates,
        ])
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
