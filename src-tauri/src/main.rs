//! 天工统一入口
//!
//! 无参数 → 启动 GUI
//! 其他命令 → 委托给 tiangong_entry（cli/server/mcp/skill）
//!
//! DMG 安装后可通过 symlink 获得 CLI 能力：
//! ln -s /Applications/天工.app/Contents/MacOS/天工 /usr/local/bin/tiangong

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// 初始化日志（所有模式统一）
///
/// - 文件：~/.tiangong/logs/tiangong.log（按天滚动，始终写入）
/// - 终端：CLI 模式静默，其他模式输出到 stderr
fn init_logging(terminal_output: bool) -> anyhow::Result<tiangong_config::logging::WorkerGuard> {
    tiangong_config::logging::init_logging(tiangong_config::logging::LoggingOptions::desktop(
        terminal_output,
    ))
}

fn main() {
    if should_run_cli_update() {
        run_cli_update();
        return;
    }

    // 无参数 → GUI
    if std::env::args().len() <= 1 {
        run_gui();
        return;
    }

    // CLI 模式终端静默，其他模式终端输出
    let is_cli = std::env::args().nth(1).as_deref() == Some("cli");
    let _guard = init_logging(!is_cli).expect("failed to initialize logging");

    if let Err(err) = tiangong_entry::run() {
        eprintln!("错误：{err}");
        std::process::exit(1);
    }
}

fn should_run_cli_update() -> bool {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return false;
    };
    if command != "update" {
        return false;
    }
    !args.any(|arg| arg == "-h" || arg == "--help")
}

fn run_cli_update() {
    let _guard = init_logging(false).expect("failed to initialize logging");
    let options = match parse_cli_update_options() {
        Ok(options) => options,
        Err(err) => {
            eprintln!("错误：{err}");
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            use tauri::Manager;

            for (_, window) in app.webview_windows() {
                let _ = window.hide();
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match run_cli_update_async(handle.clone(), options).await {
                    Ok(CliUpdateOutcome::Done) => handle.exit(0),
                    Ok(CliUpdateOutcome::RestartRequested) => {}
                    Err(err) => {
                        eprintln!("错误：{err}");
                        handle.exit(1);
                    }
                }
            });
            Ok(())
        })
        .run(generate_tauri_context())
        .expect("error while running updater");

    drop(_guard);
}

#[derive(Debug)]
struct CliUpdateOptions {
    check_only: bool,
    endpoint: Option<String>,
}

fn parse_cli_update_options() -> anyhow::Result<CliUpdateOptions> {
    let mut check_only = false;
    let mut endpoint = None;
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        if arg == "--check" {
            check_only = true;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--endpoint=") {
            endpoint = Some(non_empty_update_endpoint(value)?);
            continue;
        }
        if arg == "--endpoint" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--endpoint 缺少地址"))?;
            endpoint = Some(non_empty_update_endpoint(&value)?);
            continue;
        }
        return Err(anyhow::anyhow!("不支持的 update 参数：{arg}"));
    }

    Ok(CliUpdateOptions {
        check_only,
        endpoint,
    })
}

fn non_empty_update_endpoint(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow::anyhow!("--endpoint 不能为空"));
    }
    Ok(value.to_string())
}

enum CliUpdateOutcome {
    Done,
    RestartRequested,
}

async fn run_cli_update_async(
    app: tauri::AppHandle,
    options: CliUpdateOptions,
) -> anyhow::Result<CliUpdateOutcome> {
    use std::io::Write;

    use tauri_plugin_updater::UpdaterExt;

    let updater = if let Some(endpoint) = options.endpoint.as_deref() {
        let endpoint = reqwest::Url::parse(endpoint)?;
        app.updater_builder()
            .endpoints(vec![endpoint])?
            .build()?
    } else {
        app.updater()?
    };
    let update = match updater.check().await {
        Ok(update) => update,
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
            println!("当前没有可用的在线更新发布。");
            return Ok(CliUpdateOutcome::Done);
        }
        Err(err) => return Err(err.into()),
    };
    let Some(update) = update else {
        println!("当前已是最新版本。");
        return Ok(CliUpdateOutcome::Done);
    };

    println!(
        "发现新版本：{} -> {}",
        update.current_version, update.version
    );
    if let Some(date) = update.date {
        println!("发布时间：{date}");
    }
    if let Some(body) = update.body.as_deref().filter(|value| !value.trim().is_empty()) {
        println!("\n更新说明：\n{}", body.trim());
    }

    if options.check_only {
        return Ok(CliUpdateOutcome::Done);
    }

    println!("\n开始下载并安装更新...");
    let mut downloaded = 0u64;
    let mut last_percent = 0u64;
    update
        .download_and_install(
            |chunk_len, content_len| {
                downloaded = downloaded.saturating_add(chunk_len as u64);
                if let Some(total) = content_len.filter(|value| *value > 0) {
                    let percent = downloaded.saturating_mul(100) / total;
                    if percent >= last_percent.saturating_add(10) || percent == 100 {
                        last_percent = percent;
                        print!("\r下载进度：{percent}%");
                        let _ = std::io::stdout().flush();
                    }
                }
            },
            || {
                println!("\n下载完成，正在安装...");
            },
        )
        .await?;

    println!("更新已安装，正在重启应用...");
    app.request_restart();
    Ok(CliUpdateOutcome::RestartRequested)
}

fn run_gui() {
    let _guard = init_logging(true).expect("failed to initialize logging");

    tauri::Builder::default()
        .manage(tiangong_app::TiangongApp::new())
        .invoke_handler(tauri::generate_handler![
            tiangong_app::commands::get_sessions,
            tiangong_app::commands::create_session,
            tiangong_app::commands::switch_session,
            tiangong_app::commands::delete_session,
            tiangong_app::commands::update_session_title,
            tiangong_app::commands::request_desktop_notification_permission,
            tiangong_app::commands::send_desktop_notification,
            tiangong_app::commands::send_message,
            tiangong_app::commands::send_message_with_media,
            tiangong_app::commands::read_attachment_as_data_url,
            tiangong_app::commands::cancel_turn,
            tiangong_app::commands::cancel_agent,
            tiangong_app::commands::get_background_tasks,
            tiangong_app::commands::cancel_background_task,
            tiangong_app::commands::get_run_snapshot,
            tiangong_app::commands::get_input_draft,
            tiangong_app::commands::set_input_draft,
            tiangong_app::commands::get_session_cwd,
            tiangong_app::commands::get_workspace_dir,
            tiangong_app::commands::set_session_cwd,
            tiangong_app::commands::set_workspace_dir,
            tiangong_app::commands::get_mcp_servers,
            tiangong_app::commands::get_mcp_health,
            tiangong_app::commands::register_mcp_server,
            tiangong_app::commands::remove_mcp_server,
            tiangong_app::commands::set_mcp_server_enabled,
            tiangong_app::commands::get_skills,
            tiangong_app::commands::refresh_skills,
            tiangong_app::commands::gc_skills,
            tiangong_app::commands::get_skill_detail,
            tiangong_app::commands::inspect_skill,
            tiangong_app::commands::install_skill,
            tiangong_app::commands::get_skill_env,
            tiangong_app::commands::set_skill_env,
            tiangong_app::commands::remove_skill,
            tiangong_app::commands::set_skill_enabled,
            tiangong_app::commands::get_server_config,
            tiangong_app::commands::set_server_config,
            tiangong_app::commands::start_server,
            tiangong_app::commands::stop_server,
            tiangong_app::commands::get_connectors,
            tiangong_app::commands::set_connector_enabled,
            tiangong_app::commands::get_models_config,
            tiangong_app::commands::set_models_config,
            tiangong_app::commands::get_memory_config,
            tiangong_app::commands::set_memory_config,
            tiangong_app::commands::list_memory_nodes,
            tiangong_app::commands::upsert_manual_memory,
            tiangong_app::commands::set_memory_node_status,
            tiangong_app::commands::list_memory_relations,
            tiangong_app::commands::list_memory_relations_batch,
            tiangong_app::commands::upsert_memory_relation,
            tiangong_app::commands::delete_memory_relation,
            tiangong_app::commands::test_memory_recall,
            tiangong_app::commands::get_model_capabilities,
            tiangong_app::commands::get_model_list,
            tiangong_app::commands::fetch_provider_models,
            tiangong_app::commands::probe_embedding_dimension,
            tiangong_app::commands::append_message,
            tiangong_app::commands::edit_and_resend,
            tiangong_app::commands::respond_approval,
            tiangong_app::commands::get_trust_mode,
            tiangong_app::commands::set_trust_mode,
            tiangong_app::commands::get_default_trust_mode,
            tiangong_app::commands::set_default_trust_mode,
            tiangong_app::commands::get_custom_system_prompt,
            tiangong_app::commands::set_custom_system_prompt,
            tiangong_app::commands::get_session_cost,
            tiangong_app::commands::list_workers,
            tiangong_app::commands::synthesize_speech,
            tiangong_app::commands::has_model_capability,
            tiangong_app::commands::has_tts_capability,
            tiangong_app::commands::has_stt_capability,
            tiangong_app::commands::get_available_capabilities,
            tiangong_app::commands::transcribe_speech,
            tiangong_app::commands::list_tts_voices,
            tiangong_app::commands::play_audio_file,
            tiangong_app::commands::stop_audio,
            tiangong_app::commands::get_mention_candidates,
            tiangong_app::commands::compress_context,
            tiangong_app::commands::reset_context,
        ])
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .run(generate_tauri_context())
        .expect("error while running tauri application");

    drop(_guard);
}

fn generate_tauri_context() -> tauri::Context<tauri::Wry> {
    tauri::generate_context!()
}
