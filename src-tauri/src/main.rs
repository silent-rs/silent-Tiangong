//! 天工统一入口
//!
//! 无参数 → 启动 GUI
//! 其他命令 → 委托给 tiangong_entry（cli/server/mcp/skill）
//!
//! DMG 安装后可通过 symlink 获得 CLI 能力：
//! ln -s /Applications/天工.app/Contents/MacOS/天工 /usr/local/bin/tiangong

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Emitter;
use tauri::Manager;
use tracing::{debug, info, warn};

const TRAY_SHOW_WINDOW_ID: &str = "show_window";
const TRAY_START_SERVER_ID: &str = "start_server";
const TRAY_STOP_SERVER_ID: &str = "stop_server";
const TRAY_STATUS_ID: &str = "server_status";
const TRAY_QUIT_ID: &str = "quit";

fn browser_events_to_feedback(
    events: Vec<tiangong_plugin_browser::types::BrowserEvent>,
) -> Option<(Vec<tiangong_plugin_browser::types::BrowserEvent>, String)> {
    let network_events: Vec<_> = events
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                tiangong_plugin_browser::types::BrowserEvent::NetworkResponse { .. }
            )
        })
        .collect();
    let feedback = tiangong_plugin_browser::types::format_browser_events(&network_events)?;
    Some((network_events, feedback))
}

async fn observe_browser_snapshot_for_injection(
    app: tauri::AppHandle,
) -> Option<tiangong_plugin_browser::types::BrowserPageSnapshot> {
    let manager = {
        let state = app.state::<tiangong_plugin_browser::BrowserPluginState>();
        state.manager.clone()
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::task::spawn_blocking(move || manager.current_snapshot_without_events(12000)),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .flatten()
}

async fn ack_browser_events(
    app: tauri::AppHandle,
    events: Vec<tiangong_plugin_browser::types::BrowserEvent>,
) {
    let removed = {
        let state = app.state::<tiangong_plugin_browser::BrowserPluginState>();
        state.manager.ack_events(&events)
    };
    debug!(
        ack_count = events.len(),
        removed, "browser events acknowledged after session injection"
    );
}

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
        app.updater_builder().endpoints(vec![endpoint])?.build()?
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
    if let Some(body) = update
        .body
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
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
        .setup(|app| {
            use tauri::Listener;

            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);
            setup_tray(app)?;

            // Desktop 端独立初始化调度器（不依赖 server）
            let state = app.state::<tiangong_app::TiangongApp>();

            // 注入浏览器 Plugin 的 PageFetcher 和工具覆盖到 TiangongApp
            if let Some(fetcher) = tiangong_plugin_browser::get_page_fetcher(app.handle()) {
                state.set_page_fetcher(fetcher);
            }
            if let Some(handler) = tiangong_plugin_browser::get_tool_override(app.handle()) {
                state.register_tool_override("web_fetch", handler.clone());
                state.register_tool_override("web_browse", handler.clone());
                state.register_tool_override("web_form_extract", handler.clone());
                state.register_tool_override("web_form_fill", handler.clone());
                state.register_tool_override("web_click", handler.clone());
                state.register_tool_override("web_query_dom", handler.clone());
                state.register_tool_override("web_locate_element", handler);
            }

            // 注入终端 Plugin 的 TerminalProvider / ToolOverride / ToolSpecProvider / PromptSectionProvider
            if let Some(provider) = tiangong_plugin_terminal::get_terminal_provider(app.handle()) {
                state.set_terminal_provider(provider.clone());
                let terminal_override = tiangong_plugin_terminal::get_tool_override(app.handle());
                if let Some(handler) = terminal_override {
                    state.register_tool_override("run_shell", handler.clone());
                    state.register_tool_override("terminal_output", handler.clone());
                    state.register_tool_override("terminal_input", handler.clone());
                    state.register_tool_override("terminal_reset", handler);
                }
                state.register_tool_spec_provider(
                    tiangong_plugin_terminal::get_tool_spec_provider(),
                );
                state.register_prompt_section_provider(
                    tiangong_plugin_terminal::get_prompt_section_provider(),
                );
            }

            // 监听浏览器页面加载事件，自动注入内容到当前活跃会话
            let inject_handle = app.handle().clone();
            app.listen("browser:page_loaded", move |event| {
                let payload = event.payload().to_string();
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&payload) {
                    let url = data["url"].as_str().unwrap_or("").to_string();
                    if url.is_empty() {
                        return;
                    }
                    let browser_state =
                        inject_handle.state::<tiangong_plugin_browser::BrowserPluginState>();
                    if !browser_state.manager.is_visible() {
                        return;
                    }
                    let title = data["title"].as_str().unwrap_or("").to_string();
                    let text = data["text"].as_str().unwrap_or("").to_string();
                    let app_handle = inject_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app_handle.state::<tiangong_app::TiangongApp>();
                        let _ = state
                            .inject_browser_content(title, url, text, vec![], None, None)
                            .await;
                    });
                }
            });

            // 监听浏览器网络响应事件，主动注入到当前 Agent 会话。
            // 这覆盖页面 JS 自行发起 XHR/fetch、且 DOM 没有明显变化的场景。
            let event_inject_handle = app.handle().clone();
            app.listen("browser:events", move |event| {
                let browser_state =
                    event_inject_handle.state::<tiangong_plugin_browser::BrowserPluginState>();
                if !browser_state.manager.is_visible() {
                    return;
                }
                let payload = event.payload().to_string();
                let Ok(events) = serde_json::from_str::<
                    Vec<tiangong_plugin_browser::types::BrowserEvent>,
                >(&payload) else {
                    warn!("浏览器事件 payload 解析失败");
                    return;
                };
                let total_count = events.len();
                let Some((network_events, feedback)) = browser_events_to_feedback(events) else {
                    debug!(total_count, "浏览器事件无网络响应，跳过主动注入");
                    return;
                };
                let network_count = network_events.len();
                let app_handle = event_inject_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let snapshot = observe_browser_snapshot_for_injection(app_handle.clone()).await;
                    let title = snapshot
                        .as_ref()
                        .map(|s| s.title.clone())
                        .unwrap_or_default();
                    // 优先用 snapshot URL，其次用网络事件 URL，最后用 WebView 当前 URL
                    let url = snapshot
                        .as_ref()
                        .map(|s| s.url.clone())
                        .filter(|u| !u.is_empty())
                        .or_else(|| {
                            network_events.iter().find_map(|event| match event {
                                tiangong_plugin_browser::types::BrowserEvent::NetworkResponse {
                                    url,
                                    ..
                                } => Some(url.clone()),
                                _ => None,
                            })
                        })
                        .unwrap_or_default();
                    let text = snapshot
                        .as_ref()
                        .map(|s| s.text.clone())
                        .unwrap_or_default();
                    let tabs = snapshot
                        .as_ref()
                        .map(|s| {
                            s.tabs
                                .iter()
                                .map(|tab| (tab.id.clone(), tab.url.clone(), tab.title.clone()))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let active_tab_id = snapshot.as_ref().and_then(|s| s.active_tab_id.clone());
                    let mut page_url = url;
                    if page_url.is_empty() {
                        // 尝试从活跃标签的 WebView 获取当前页面 URL
                        let browser_state =
                            app_handle.state::<tiangong_plugin_browser::BrowserPluginState>();
                        page_url = browser_state.manager.current_url().unwrap_or_default();
                    }
                    if page_url.is_empty() {
                        warn!(
                            total_count,
                            network_count, "浏览器网络事件缺少页面 URL，无法注入"
                        );
                        return;
                    }

                    let state = app_handle.state::<tiangong_app::TiangongApp>();
                    let injected = state
                        .inject_browser_content(
                            title,
                            page_url.clone(),
                            text,
                            tabs,
                            active_tab_id,
                            Some(feedback),
                        )
                        .await;
                    info!(
                        url = %page_url,
                        total_count, network_count, injected, "浏览器网络事件注入检查完成"
                    );
                    if injected {
                        ack_browser_events(app_handle, network_events).await;
                    }
                });
            });

            let scheduler_ctx = state.create_scheduler_context();
            tauri::async_runtime::spawn(async move {
                tiangong_scheduler::executor::restore_cron_jobs(scheduler_ctx).await;
                silent::Scheduler::schedule(silent::SCHEDULER.clone()).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let plugin_state = window.state::<tiangong_plugin_browser::BrowserPluginState>();
                plugin_state.manager.set_visible(false);
                let _ = window.hide();
            }
        })
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
            tiangong_app::commands::get_models_config,
            tiangong_app::commands::set_models_config,
            tiangong_app::commands::get_memory_config,
            tiangong_app::commands::set_memory_config,
            tiangong_app::commands::list_memory_nodes,
            tiangong_app::commands::count_memory_nodes,
            tiangong_app::commands::upsert_manual_memory,
            tiangong_app::commands::set_memory_node_status,
            tiangong_app::commands::list_memory_relations,
            tiangong_app::commands::list_memory_relations_batch,
            tiangong_app::commands::upsert_memory_relation,
            tiangong_app::commands::delete_memory_relation,
            tiangong_app::commands::test_memory_recall,
            tiangong_app::commands::list_workspace_indexes,
            tiangong_app::commands::delete_workspace_index,
            tiangong_app::commands::rebuild_workspace_index,
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
            tiangong_app::commands::get_reasoning_effort,
            tiangong_app::commands::set_reasoning_effort,
            tiangong_app::commands::get_provider_balance,
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
            tiangong_app::commands::job_list,
            tiangong_app::commands::job_create,
            tiangong_app::commands::job_update,
            tiangong_app::commands::job_delete,
            tiangong_app::commands::job_trigger,
            tiangong_app::commands::job_list_runs,
            tiangong_app::commands::webhook_list,
            tiangong_app::commands::webhook_create,
            tiangong_app::commands::webhook_update,
            tiangong_app::commands::webhook_delete,
            tiangong_app::commands::webhook_trigger,
            tiangong_app::commands::webhook_list_runs,
        ])
        .plugin(tiangong_plugin_browser::init())
        .plugin(tiangong_plugin_terminal::init(
            scru128::new().to_string(),
            std::env::var("TIANGONG_WORKSPACE")
                .ok()
                .or_else(|| std::env::var("HOME").ok())
                .unwrap_or_else(|| "/tmp".to_string()),
        ))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(generate_tauri_context())
        .expect("error while building tauri application")
        .run(|handle, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main_window(handle);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (handle, event);
        });

    drop(_guard);
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    let app_handle = app.handle().clone();
    let status_item = MenuItemBuilder::with_id(TRAY_STATUS_ID, tray_server_status(&app_handle).0)
        .enabled(false)
        .build(app)?;
    let show_item = MenuItemBuilder::with_id(TRAY_SHOW_WINDOW_ID, "显示天工").build(app)?;
    let start_item = MenuItemBuilder::with_id(TRAY_START_SERVER_ID, "启动 Server").build(app)?;
    let stop_item = MenuItemBuilder::with_id(TRAY_STOP_SERVER_ID, "停止 Server").build(app)?;
    let quit_item = MenuItemBuilder::with_id(TRAY_QUIT_ID, "退出天工").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&status_item)
        .separator()
        .item(&show_item)
        .item(&start_item)
        .item(&stop_item)
        .separator()
        .item(&quit_item)
        .build()?;

    let status_item_for_menu = status_item.clone();
    let start_item_for_menu = start_item.clone();
    let stop_item_for_menu = stop_item.clone();
    let icon_normal = load_icon_rgba(include_bytes!("../icons/32x32.rgba"));
    let icon_running = load_icon_rgba(include_bytes!("../icons/32x32-running.rgba"));
    let icon_running_for_menu = icon_running.clone();
    let icon_normal_for_menu = icon_normal.clone();
    let mut tray = TrayIconBuilder::with_id("tiangong")
        .menu(&menu)
        .tooltip("天工")
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            handle_tray_menu_event(
                app.clone(),
                event.id().as_ref(),
                status_item_for_menu.clone(),
                start_item_for_menu.clone(),
                stop_item_for_menu.clone(),
                icon_normal_for_menu.clone(),
                icon_running_for_menu.clone(),
            );
        });
    if let Some(icon) = icon_normal.clone() {
        tray = tray.icon(icon);
    }
    let tray = tray.build(app)?;
    refresh_tray_server_status(
        &app_handle,
        &tray,
        &status_item,
        &start_item,
        &stop_item,
        &icon_normal,
        &icon_running,
    );
    start_tray_status_refresh(
        app_handle.clone(),
        tray,
        status_item,
        start_item,
        stop_item,
        icon_normal,
        icon_running,
    );

    // 自动拉起：如果上次退出时 Server 是开启的，自动启动嵌入式 Server
    auto_start_embedded_server(&app_handle);

    Ok(())
}

/// 检查配置中 enabled 标记，自动启动嵌入式 Server
fn auto_start_embedded_server(app: &tauri::AppHandle) {
    let config = tiangong_server::config::load_server_config();
    if !config.enabled {
        return;
    }
    let state = app.state::<tiangong_app::TiangongApp>();
    match state.start_embedded_server(&config.host, config.port, config.auth_token.clone()) {
        Ok(()) => {
            info!(host = %config.host, port = config.port, "自动启动嵌入式 Server");
        }
        Err(err) => {
            warn!(error = %err, "自动启动嵌入式 Server 失败");
            // 启动失败时重置标记，避免下次启动继续失败
            let mut config = config;
            config.enabled = false;
            let _ = tiangong_server::config::save_server_config(&config);
        }
    }
}

fn start_tray_status_refresh(
    app: tauri::AppHandle,
    tray: tauri::tray::TrayIcon,
    status_item: tauri::menu::MenuItem<tauri::Wry>,
    start_item: tauri::menu::MenuItem<tauri::Wry>,
    stop_item: tauri::menu::MenuItem<tauri::Wry>,
    icon_normal: Option<tauri::image::Image<'static>>,
    icon_running: Option<tauri::image::Image<'static>>,
) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        refresh_tray_server_status(
            &app,
            &tray,
            &status_item,
            &start_item,
            &stop_item,
            &icon_normal,
            &icon_running,
        );
    });
}

fn handle_tray_menu_event(
    app: tauri::AppHandle,
    menu_id: &str,
    status_item: tauri::menu::MenuItem<tauri::Wry>,
    start_item: tauri::menu::MenuItem<tauri::Wry>,
    stop_item: tauri::menu::MenuItem<tauri::Wry>,
    icon_normal: Option<tauri::image::Image<'static>>,
    icon_running: Option<tauri::image::Image<'static>>,
) {
    match menu_id {
        TRAY_SHOW_WINDOW_ID => {
            info!("tray: 显示天工 clicked");
            show_main_window(&app);
        }
        TRAY_START_SERVER_ID => {
            let app_clone = app.clone();
            let status_item = status_item.clone();
            let start_item = start_item.clone();
            let stop_item = stop_item.clone();
            let icon_normal = icon_normal.clone();
            let icon_running = icon_running.clone();
            let tray = app.tray_by_id("tiangong");
            std::thread::spawn(move || {
                let _ = status_item.set_text("Server 状态：启动中");
                let config = tiangong_server::config::load_server_config();
                let state = app_clone.state::<tiangong_app::TiangongApp>();
                match state.start_embedded_server(
                    &config.host,
                    config.port,
                    config.auth_token.clone(),
                ) {
                    Ok(()) => {
                        info!(host = %config.host, port = config.port, "Server 已启动");
                        let mut config = config;
                        config.enabled = true;
                        let _ = tiangong_server::config::save_server_config(&config);
                    }
                    Err(err) => {
                        warn!(error = %err, "菜单栏启动 Server 失败");
                    }
                }
                if let Some(tray) = tray.as_ref() {
                    refresh_tray_server_status(
                        &app_clone,
                        tray,
                        &status_item,
                        &start_item,
                        &stop_item,
                        &icon_normal,
                        &icon_running,
                    );
                }
            });
        }
        TRAY_STOP_SERVER_ID => {
            let app_clone = app.clone();
            let status_item = status_item.clone();
            let start_item = start_item.clone();
            let stop_item = stop_item.clone();
            let icon_normal = icon_normal.clone();
            let icon_running = icon_running.clone();
            let tray = app.tray_by_id("tiangong");
            std::thread::spawn(move || {
                let _ = status_item.set_text("Server 状态：停止中");
                let state = app_clone.state::<tiangong_app::TiangongApp>();
                if let Err(err) = state.stop_embedded_server() {
                    warn!(error = %err, "菜单栏停止 Server 失败");
                } else {
                    info!("Server 已停止");
                    let mut config = tiangong_server::config::load_server_config();
                    config.enabled = false;
                    let _ = tiangong_server::config::save_server_config(&config);
                }
                if let Some(tray) = tray.as_ref() {
                    refresh_tray_server_status(
                        &app_clone,
                        tray,
                        &status_item,
                        &start_item,
                        &stop_item,
                        &icon_normal,
                        &icon_running,
                    );
                }
            });
        }
        TRAY_QUIT_ID => app.exit(0),
        _ => {}
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;

    let browser_state = app.state::<tiangong_plugin_browser::BrowserPluginState>();
    browser_state.manager.set_visible(true);

    let Some(window) = app.get_webview_window("main") else {
        // 尝试用 get_window 作为后备
        if let Some(win) = app.get_window("main") {
            let _ = win.show();
            let _ = win.set_focus();
        }
        return;
    };
    let _ = window.show();
    let _ = window.set_focus();
    // 延迟通知前端恢复浏览器 WebView 位置（等窗口渲染完成）
    let app_clone = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = app_clone.emit("browser:restore", ());
    });
}

fn refresh_tray_server_status(
    app: &tauri::AppHandle,
    tray: &tauri::tray::TrayIcon,
    status_item: &tauri::menu::MenuItem<tauri::Wry>,
    start_item: &tauri::menu::MenuItem<tauri::Wry>,
    stop_item: &tauri::menu::MenuItem<tauri::Wry>,
    icon_normal: &Option<tauri::image::Image<'static>>,
    icon_running: &Option<tauri::image::Image<'static>>,
) {
    let (text, running) = tray_server_status(app);
    let _ = status_item.set_text(text);
    let _ = start_item.set_enabled(!running);
    let _ = stop_item.set_enabled(running);
    let icon = if running {
        icon_running.as_ref().or(icon_normal.as_ref())
    } else {
        icon_normal.as_ref()
    };
    if let Some(icon) = icon {
        let _ = tray.set_icon(Some(icon.clone()));
    }
}

fn tray_server_status(app: &tauri::AppHandle) -> (String, bool) {
    use tauri::Manager;
    let app_state = app.state::<tiangong_app::TiangongApp>();
    let config = tiangong_server::config::load_server_config();
    let running = app_state.is_embedded_server_running()
        || tiangong_app::commands::is_server_running(&config);
    if running {
        ("Server 状态：运行中".to_string(), true)
    } else {
        ("Server 状态：未运行".to_string(), false)
    }
}

fn generate_tauri_context() -> tauri::Context<tauri::Wry> {
    tauri::generate_context!()
}

fn load_icon_rgba(data: &'static [u8]) -> Option<tauri::image::Image<'static>> {
    if data.len() < 8 {
        return None;
    }
    let width = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let rgba = data[8..].to_vec();
    Some(tauri::image::Image::new_owned(rgba, width, height))
}
