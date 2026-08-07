//! 天工统一入口
//!
//! 无参数 → 启动 GUI
//! 其他命令 → 委托给 tiangong_entry（cli/server/mcp/skill）
//!
//! DMG 安装后可通过 symlink 获得 CLI 能力：
//! ln -s /Applications/tiangong.app/Contents/MacOS/tiangong-app /usr/local/bin/tiangong

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
    session_id: String,
) -> Option<tiangong_plugin_browser::types::BrowserPageSnapshot> {
    let manager = {
        let state = app.state::<tiangong_plugin_browser::BrowserPluginState>();
        let browser_state = state.registry.existing_session_state(&session_id)?;
        tiangong_plugin_browser::manager::BrowserManager::from_state(browser_state)
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
    session_id: String,
    events: Vec<tiangong_plugin_browser::types::BrowserEvent>,
) {
    let removed = {
        let state = app.state::<tiangong_plugin_browser::BrowserPluginState>();
        state
            .registry
            .existing_session_state(&session_id)
            .map(tiangong_plugin_browser::manager::BrowserManager::from_state)
            .map(|manager| manager.ack_events(&events))
            .unwrap_or(0)
    };
    debug!(
        session_id,
        ack_count = events.len(),
        removed,
        "browser events acknowledged after session injection"
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

fn run_gui() {
    let _guard = init_logging(true).expect("failed to initialize logging");

    // 旧版工作区 Tab 必须在 App State 加载/恢复前迁入各插件与薄布局存储，
    // 否则启动恢复可能先重写 Session JSON，永久丢失迁移输入。
    tiangong_app::workspace_tabs::migrate_legacy_tabs()
        .expect("旧工作区标签页迁移失败，为避免覆盖旧数据已停止启动");

    tauri::Builder::default()
        .manage(tiangong_app::TiangongApp::new())
        .setup(|app| {
            use tauri::Listener;

            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);
            setup_tray(app)?;

            // 注入 app_handle（builder 链构造时尚无 handle，setup 阶段补入）。
            let state = app.state::<tiangong_app::TiangongApp>();
            state.set_app_handle(app.handle().clone());

            // 启动阶段一次性预加载插件快照。后续状态查询、设置页和 Core 创建
            // 只复用该快照，不隐式扫描、编译或热加载插件。
            {
                let storage_root = tiangong_config::io::storage_root();
                tiangong_plugin_runtime::registry::preload_installed_plugins(&storage_root);
            }

            // Core 插件仍由 ensure_core 现场构造，确保每个 Core 持有独立实例
            //（隔离 per-session 状态如 workspace / recall_attempted / turn_count）。

            // 将 workspace 目录设为系统 PTY 默认 cwd（独立于插件实例化的全局状态）。
            let app_handle = app.handle().clone();
            let core_state = state.state.clone();
            tauri::async_runtime::spawn(async move {
                let workspace = {
                    let guard = core_state.lock().await;
                    guard.workspace_dir.clone()
                };
                tiangong_plugin_terminal::set_cwd(&app_handle, workspace).await;
            });

            // 媒体生成/转换插件（generate_image / generate_video / text_to_speech /
            // speech_to_text）在 app.rs 的 create_core_if_absent 中统一组装（与其他
            // 进程内插件一致），此处不再单独注册。

            // 注意：GUI 不注册 fetch / command 插件。web_fetch 由 browser 插件提供
            // （内嵌浏览器渲染），run_command / run_shell 由 terminal 插件提供（PTY 执行）。
            // CLI / Server 才注册 fetch / command（基础 reqwest 获取 + 子进程执行）。

            // 启动工具消息注入消费者任务（插件 push → 消费者统一处理）
            let injection_tx = state.tool_injection_tx();
            state.start_tool_injection_consumer(app.handle().clone());

            // 监听浏览器页面加载事件，push 到注入 channel（消费者统一处理）
            let tx1 = injection_tx.clone();
            let inject_handle = app.handle().clone();
            app.listen("browser:page_loaded", move |event| {
                let payload = event.payload().to_string();
                let Ok(data) = serde_json::from_str::<
                    tiangong_plugin_browser::types::BrowserPageLoadedEvent,
                >(&payload) else {
                    warn!("浏览器页面事件 payload 解析失败");
                    return;
                };
                if data.session_id.trim().is_empty() || data.url.is_empty() {
                    return;
                }
                let browser_state =
                    inject_handle.state::<tiangong_plugin_browser::BrowserPluginState>();
                let Some(source_state) = browser_state
                    .registry
                    .existing_session_state(&data.session_id)
                else {
                    return;
                };
                if !tiangong_plugin_browser::manager::BrowserManager::from_state(source_state)
                    .is_visible()
                {
                    return;
                }
                use tiangong_plugin_browser::page_fetcher::BrowserContent;
                let _ = tx1.send(tiangong_app::ToolInjection {
                    session_id: Some(data.session_id),
                    tool: Box::new(BrowserContent {
                        title: data.title,
                        url: data.url,
                        text: data.text,
                        tabs: vec![],
                        active_tab_id: None,
                        feedback: None,
                    }),
                });
            });

            // 监听浏览器网络响应事件，push 到注入 channel（消费者统一处理）。
            // 这覆盖页面 JS 自行发起 XHR/fetch、且 DOM 没有明显变化的场景。
            let event_inject_handle = app.handle().clone();
            let tx2 = injection_tx.clone();
            app.listen("browser:events", move |event| {
                let payload = event.payload().to_string();
                let Ok(payload) = serde_json::from_str::<
                    tiangong_plugin_browser::types::BrowserEventsEvent,
                >(&payload) else {
                    warn!("浏览器事件 payload 解析失败");
                    return;
                };
                let browser_state =
                    event_inject_handle.state::<tiangong_plugin_browser::BrowserPluginState>();
                let Some(source_state) = browser_state
                    .registry
                    .existing_session_state(&payload.session_id)
                else {
                    return;
                };
                if !tiangong_plugin_browser::manager::BrowserManager::from_state(source_state)
                    .is_visible()
                {
                    return;
                }
                let session_id = payload.session_id;
                let events = payload.events;
                let total_count = events.len();
                let Some((network_events, feedback)) = browser_events_to_feedback(events) else {
                    debug!(total_count, "浏览器事件无网络响应，跳过主动注入");
                    return;
                };
                let network_count = network_events.len();
                let app_handle = event_inject_handle.clone();
                let injection_tx = tx2.clone();
                tauri::async_runtime::spawn(async move {
                    let snapshot = observe_browser_snapshot_for_injection(
                        app_handle.clone(),
                        session_id.clone(),
                    )
                    .await;
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
                        page_url = browser_state
                            .registry
                            .existing_session_state(&session_id)
                            .map(tiangong_plugin_browser::manager::BrowserManager::from_state)
                            .and_then(|manager| manager.current_url())
                            .unwrap_or_default();
                    }
                    if page_url.is_empty() {
                        warn!(
                            total_count,
                            network_count, "浏览器网络事件缺少页面 URL，无法注入"
                        );
                        return;
                    }

                    use tiangong_plugin_browser::page_fetcher::BrowserContent;
                    let queued = injection_tx
                        .send(tiangong_app::ToolInjection {
                            session_id: Some(session_id.clone()),
                            tool: Box::new(BrowserContent {
                                title,
                                url: page_url.clone(),
                                text,
                                tabs,
                                active_tab_id,
                                feedback: Some(feedback),
                            }),
                        })
                        .is_ok();
                    info!(
                        session_id,
                        url = %page_url,
                        total_count, network_count, queued, "浏览器网络事件注入检查完成"
                    );
                    if queued {
                        ack_browser_events(app_handle, session_id, network_events).await;
                    }
                });
            });

            // 监听终端用户命令提交事件，push 到注入 channel（消费者统一处理 + 刷新前端）。
            let tx3 = injection_tx.clone();
            app.listen("terminal:user_command", move |event| {
                let payload = event.payload().to_string();
                let Ok(data) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    return;
                };
                let command = data["command"].as_str().unwrap_or("").to_string();
                if command.trim().is_empty() {
                    return;
                }
                use tiangong_plugin_terminal::collaboration::TerminalUserInput;
                let _ = tx3.send(tiangong_app::ToolInjection {
                    session_id: data["session_id"].as_str().map(|s| s.to_string()),
                    tool: Box::new(TerminalUserInput { command }),
                });
            });

            // 定时任务的 cron 调度已下沉到 scheduler sidecar 进程，本进程不再恢复
            // cron job 或启动 silent scheduler 循环。

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(auto_start_server_and_bots(app_handle));

            #[cfg(debug_assertions)]
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match tokio::signal::ctrl_c().await {
                        Ok(()) => {
                            info!("收到 Ctrl+C，正在正常退出天工");
                            app_handle.exit(0);
                        }
                        Err(error) => {
                            warn!(%error, "监听 Ctrl+C 失败");
                        }
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let plugin_state = window.state::<tiangong_plugin_browser::BrowserPluginState>();
                plugin_state.manager().set_visible(false);
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            tiangong_app::commands::get_sessions,
            tiangong_app::commands::get_session_tabs,
            tiangong_app::commands::set_session_tabs,
            tiangong_app::commands::switch_session,
            tiangong_app::commands::load_session,
            tiangong_app::commands::delete_session,
            tiangong_app::commands::delete_sessions_by_cwd,
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
            tiangong_app::commands::get_input_cache,
            tiangong_app::commands::set_input_cache,
            tiangong_app::commands::new_session_id,
            tiangong_app::commands::remove_input_cache,
            tiangong_app::commands::get_workspace_dir,
            tiangong_app::commands::set_session_cwd,
            tiangong_app::commands::set_workspace_dir,
            tiangong_app::commands::get_mcp_servers,
            tiangong_app::commands::get_mcp_health,
            tiangong_app::commands::probe_mcp_server,
            tiangong_app::commands::register_mcp_server,
            tiangong_app::commands::update_mcp_server,
            tiangong_app::commands::remove_mcp_server,
            tiangong_app::commands::set_mcp_server_enabled,
            tiangong_app::commands::get_server_config,
            tiangong_app::commands::set_server_config,
            tiangong_app::commands::start_server,
            tiangong_app::commands::stop_server,
            tiangong_app::commands::get_models_config,
            tiangong_app::commands::set_models_config,
            tiangong_app::commands::prewarm_workspace_index,
            tiangong_app::commands::get_model_capabilities,
            tiangong_app::commands::get_model_list,
            tiangong_app::commands::fetch_provider_models,
            tiangong_app::commands::probe_embedding_dimension,
            tiangong_app::commands::append_message,
            tiangong_app::commands::edit_and_resend,
            tiangong_app::commands::respond_approval,
            tiangong_app::commands::list_plugin_contributions,
            tiangong_app::commands::list_plugins,
            tiangong_app::commands::list_available_plugins,
            tiangong_app::commands::import_local_plugin,
            tiangong_app::commands::install_plugin,
            tiangong_app::commands::upgrade_plugin,
            tiangong_app::commands::set_plugin_enabled,
            tiangong_app::commands::rollback_plugin,
            tiangong_app::commands::uninstall_plugin,
            tiangong_app::commands::reload_plugin,
            tiangong_app::commands::plugin_open_view,
            tiangong_app::commands::plugin_call,
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
            tiangong_app::commands::webhook_list,
            tiangong_app::commands::webhook_create,
            tiangong_app::commands::webhook_update,
            tiangong_app::commands::webhook_delete,
            tiangong_app::commands::webhook_trigger,
            tiangong_app::commands::webhook_list_runs,
            tiangong_app::commands::bot_list,
            tiangong_app::commands::bot_health,
            tiangong_app::commands::bot_log,
            tiangong_app::commands::bot_config_schema,
            tiangong_app::commands::bot_provision_begin,
            tiangong_app::commands::bot_provision_poll,
            tiangong_app::commands::bot_available,
            tiangong_app::commands::bot_scan_local,
            tiangong_app::commands::bot_push_targets,
            tiangong_app::commands::bot_delete_push_target,
            tiangong_app::commands::bot_register_mcp,
            tiangong_app::commands::bot_register,
            tiangong_app::commands::bot_update,
            tiangong_app::commands::bot_remove,
            tiangong_app::commands::bot_install,
            tiangong_app::commands::bot_start,
            tiangong_app::commands::bot_stop,
            tiangong_app::commands::bot_check_update,
            tiangong_app::commands::bot_upgrade,
            tiangong_app::commands::resolve_model_context_window,
        ])
        .plugin(tiangong_plugin_browser::init())
        .plugin(tiangong_plugin_terminal::init(
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
            // Desktop 退出时停止自己 supervisor 管理的 bot（仅 entries，不影响 CLI 独立启动的）。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(app_state) = handle.try_state::<tiangong_app::TiangongApp>() {
                    let runtime = app_state.bot_runtime.clone();
                    tauri::async_runtime::block_on(async move {
                        runtime.stop_all().await;
                    });
                }
                // 逐个停止所有 sidecar（它们经 setsid 独立运行，不会随宿主自动退出）。
                tiangong_plugin_runtime::registry::shutdown_all_sidecars();
            }
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main_window(handle);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = handle;
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

    Ok(())
}

async fn auto_start_server_and_bots(app: tauri::AppHandle) {
    let state = app.state::<tiangong_app::TiangongApp>();
    // 方案：bot 独立运行，不自动启动 Server。仅启动 enabled 且 PID 不存活的 bot。
    let has_enabled_bot = state.bot_store.list().iter().any(|bot| bot.enabled);
    if !has_enabled_bot {
        return;
    }
    // Server 地址/Token 从配置读取注入（供 bot 回连），但不强制 Server 运行。
    let server_config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await
        .unwrap_or_default();
    let extra_env = tiangong_app::commands::bot_server_env(&server_config);
    state.bot_runtime.start_enabled(&extra_env).await;
    // MCP 注册需要 Server 运行；Server 未运行时跳过（bot 仍独立运行）。
    let server_ok = tiangong_app::commands::server_health_check(&server_config);
    if !server_ok {
        info!("Server 未运行，已启动的 bot 将独立运行；启动 Server 后可恢复 Agent 调用");
        return;
    }
    for bot in state.bot_store.list().into_iter().filter(|bot| bot.enabled) {
        if !matches!(
            state.bot_runtime.health(&bot.id).await,
            tiangong_bots::BotHealth::Running
        ) {
            continue;
        }
        if let Err(register_error) =
            tiangong_app::commands::ensure_bot_mcp_registered(&bot.id, state.inner()).await
        {
            warn!(
                bot_id = %bot.id,
                error = %register_error,
                "bot 自动启动后注册 MCP 失败"
            );
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
    browser_state.manager().set_visible(true);

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
