#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
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
            tiangong_app::commands::synthesize_speech,
            tiangong_app::commands::has_tts_capability,
        ])
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
