use components::{Button, Container, Input, SelectBuilder, SelectOption, SelectValue, Text};
use hetu::prelude::*;

use crate::core::app_state::TiangongState;
use crate::core::runtime::{RunSnapshot, RunStatus};
use crate::core::session::{MessageRole, now_text};

#[derive(Debug)]
struct TiangongApp {
    session_seed: WidgetId,
    new_session_btn: WidgetId,
    sidebar_settings_btn: WidgetId,
    header_settings_btn: WidgetId,
    session_title_input: WidgetId,
    session_title_save_btn: WidgetId,
    session_delete_btn: WidgetId,
    model_select: WidgetId,
    send_btn: WidgetId,
    input_widget: WidgetId,
}

impl Default for TiangongApp {
    fn default() -> Self {
        Self {
            session_seed: WidgetId::new(),
            new_session_btn: WidgetId::new(),
            sidebar_settings_btn: WidgetId::new(),
            header_settings_btn: WidgetId::new(),
            session_title_input: WidgetId::new(),
            session_title_save_btn: WidgetId::new(),
            session_delete_btn: WidgetId::new(),
            model_select: WidgetId::new(),
            send_btn: WidgetId::new(),
            input_widget: WidgetId::new(),
        }
    }
}

impl Component for TiangongApp {
    fn build(
        &mut self,
        tree: &mut UiTree,
        handlers: &mut UiHandlers<StateMap>,
        states: &mut StateMap,
        cx: AppCtx,
    ) {
        states.ensure::<TiangongState>().poll_pending_turn();
        let state = states.ensure::<TiangongState>();
        let active_session_id = state.active_session_id().to_string();
        let provider_label = state.provider_label();
        let draft = state.input_draft.clone();
        let has_pending_turn = state.has_pending_turn();
        let run_snapshot = state.run.clone();
        let current_model = state.current_model().to_string();
        let model_list = state.model_list().to_vec();
        let session_title_draft = state.session_title_draft().to_string();

        let sessions_snapshot = state
            .sessions()
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    session.title.clone(),
                    session.messages.len(),
                )
            })
            .collect::<Vec<_>>();

        let (active_title, active_messages) = if let Some(session) = state.active_session() {
            (session.title.clone(), session.messages.clone())
        } else {
            ("未找到会话".to_string(), Vec::new())
        };

        let sidebar_title = Text::new("天工")
            .class("sidebar_title")
            .mount(tree, handlers);
        let sidebar_subtitle = Text::new("桌面智能体")
            .class("sidebar_subtitle")
            .mount(tree, handlers);
        let sidebar_brand = Container::new(vec![sidebar_title, sidebar_subtitle])
            .class("sidebar_brand")
            .mount(tree, handlers);

        let new_session_btn = Button::new("+ 新建会话")
            .widget_id(self.new_session_btn)
            .class("session_new_btn")
            .on_click(|states: &mut StateMap, _ctx| {
                states.ensure::<TiangongState>().create_session();
            })
            .mount(tree, handlers);

        let sidebar_settings_btn = Button::new("LLM 设置")
            .widget_id(self.sidebar_settings_btn)
            .class("settings_open_btn")
            .on_click(|states: &mut StateMap, _ctx| {
                open_provider_settings_window(states);
            })
            .mount(tree, handlers);

        let mut session_items = Vec::new();

        for (session_id, session_title, message_count) in sessions_snapshot {
            let btn_label = format!("{} ({})", session_title, message_count);
            let is_active = session_id == active_session_id;
            let mut session_button = Button::new(btn_label)
                .widget_id(derive_id_from_key(self.session_seed, &session_id))
                .class("session_btn")
                .on_click({
                    let session_id = session_id.clone();
                    move |states: &mut StateMap, _ctx| {
                        states.ensure::<TiangongState>().switch_session(&session_id);
                    }
                });

            if is_active {
                session_button = session_button.class("session_btn_active");
            }

            session_items.push(session_button.mount(tree, handlers));
        }

        let session_section = Container::new(session_items)
            .class("session_section")
            .mount(tree, handlers);

        let provider_node = Text::new(provider_label.clone())
            .class("sidebar_provider")
            .mount(tree, handlers);

        let sidebar_footer = Container::new(vec![provider_node, sidebar_settings_btn])
            .class("sidebar_footer")
            .mount(tree, handlers);

        let sidebar_node = Container::new(vec![
            sidebar_brand,
            new_session_btn,
            session_section,
            sidebar_footer,
        ])
        .class("sidebar")
        .mount(tree, handlers);

        let heading_title = Text::new(active_title)
            .class("main_title")
            .mount(tree, handlers);
        let heading_subtitle = Text::new(format!("{} 条消息", active_messages.len()))
            .class("main_subtitle")
            .mount(tree, handlers);
        let heading = Container::new(vec![heading_title, heading_subtitle])
            .class("main_heading")
            .mount(tree, handlers);

        let model_pill = Text::new(format!("API：{provider_label}"))
            .class("model_pill")
            .mount(tree, handlers);
        let header_settings_btn = Button::new("设置")
            .widget_id(self.header_settings_btn)
            .class("top_settings_btn")
            .on_click(|states: &mut StateMap, _ctx| {
                open_provider_settings_window(states);
            })
            .mount(tree, handlers);
        let top_actions = Container::new(vec![model_pill, header_settings_btn])
            .class("top_actions")
            .mount(tree, handlers);
        let top_bar = Container::new(vec![heading, top_actions])
            .class("top_bar")
            .mount(tree, handlers);

        let session_title_input = Input::new()
            .widget_id(self.session_title_input)
            .text(session_title_draft.clone())
            .placeholder("会话标题")
            .class("session_title_input")
            .on_input(|states, ictx| {
                states
                    .ensure::<TiangongState>()
                    .update_session_title_draft(ictx.value);
            })
            .on_submit(|states, sctx| {
                let state = states.ensure::<TiangongState>();
                state.update_session_title_draft(sctx.value);
                if let Err(err) = state.save_active_session_title() {
                    state.run = RunSnapshot {
                        status: RunStatus::Failed,
                        summary: "会话重命名失败".to_string(),
                        last_session_id: state.run.last_session_id.clone(),
                        last_task_id: state.run.last_task_id.clone(),
                        last_duration_ms: state.run.last_duration_ms,
                        last_result: state.run.last_result.clone(),
                        last_plan: state.run.last_plan.clone(),
                        last_tool_result: state.run.last_tool_result.clone(),
                        last_error: Some(err.to_string()),
                        last_usage: state.run.last_usage.clone(),
                        updated_at: now_text(),
                    };
                }
            })
            .mount(tree, handlers, cx);

        let save_session_title_btn = Button::new("保存标题")
            .widget_id(self.session_title_save_btn)
            .class("session_title_save_btn")
            .disabled(session_title_draft.trim().is_empty())
            .on_click(|states: &mut StateMap, _ctx| {
                let state = states.ensure::<TiangongState>();
                if let Err(err) = state.save_active_session_title() {
                    state.run = RunSnapshot {
                        status: RunStatus::Failed,
                        summary: "会话重命名失败".to_string(),
                        last_session_id: state.run.last_session_id.clone(),
                        last_task_id: state.run.last_task_id.clone(),
                        last_duration_ms: state.run.last_duration_ms,
                        last_result: state.run.last_result.clone(),
                        last_plan: state.run.last_plan.clone(),
                        last_tool_result: state.run.last_tool_result.clone(),
                        last_error: Some(err.to_string()),
                        last_usage: state.run.last_usage.clone(),
                        updated_at: now_text(),
                    };
                }
            })
            .mount(tree, handlers);

        let delete_session_btn = Button::new("删除会话")
            .widget_id(self.session_delete_btn)
            .class("session_delete_btn")
            .on_click(|states: &mut StateMap, _ctx| {
                let state = states.ensure::<TiangongState>();
                if let Err(err) = state.delete_active_session() {
                    state.run = RunSnapshot {
                        status: RunStatus::Failed,
                        summary: "删除会话失败".to_string(),
                        last_session_id: state.run.last_session_id.clone(),
                        last_task_id: state.run.last_task_id.clone(),
                        last_duration_ms: state.run.last_duration_ms,
                        last_result: state.run.last_result.clone(),
                        last_plan: state.run.last_plan.clone(),
                        last_tool_result: state.run.last_tool_result.clone(),
                        last_error: Some(err.to_string()),
                        last_usage: state.run.last_usage.clone(),
                        updated_at: now_text(),
                    };
                }
            })
            .mount(tree, handlers);

        let session_actions = Container::new(vec![
            session_title_input,
            save_session_title_btn,
            delete_session_btn,
        ])
        .class("session_actions")
        .mount(tree, handlers);

        let mut message_children = Vec::new();
        let visible_messages = active_messages.iter().collect::<Vec<_>>();
        if visible_messages.is_empty() {
            let empty_title = Text::new("开始一个新对话")
                .class("message_empty_title")
                .mount(tree, handlers);
            let empty_desc = Text::new("输入任务后按 Enter，天工会生成执行结果。")
                .class("message_empty_desc")
                .mount(tree, handlers);
            message_children.push(
                Container::new(vec![empty_title, empty_desc])
                    .class("message_empty")
                    .mount(tree, handlers),
            );
        } else {
            for msg in visible_messages {
                let row_class = match msg.role {
                    MessageRole::System => "message_row_system",
                    MessageRole::Assistant => "message_row_assistant",
                    MessageRole::User => "message_row_user",
                };
                let content_class = match msg.role {
                    MessageRole::System => "message_content_system",
                    MessageRole::Assistant => "message_content_assistant",
                    MessageRole::User => "message_content_user",
                };
                let block_class = match msg.role {
                    MessageRole::System => "message_block_system",
                    MessageRole::Assistant => "message_block_assistant",
                    MessageRole::User => "message_block_user",
                };

                let content = Text::new(msg.content.clone())
                    .class("message_content")
                    .class(content_class)
                    .mount(tree, handlers);
                let block = Container::new(vec![content])
                    .class("message_block")
                    .class(block_class)
                    .mount(tree, handlers);
                message_children.push(
                    Container::new(vec![block])
                        .class("message_row")
                        .class(row_class)
                        .mount(tree, handlers),
                );
            }
        }

        let message_list_node = Container::new(message_children)
            .class("message_list")
            .mount(tree, handlers);

        let run_status_node = build_run_status_node(tree, handlers, &run_snapshot);

        let input_node = Input::new()
            .widget_id(self.input_widget)
            .text(draft.clone())
            .placeholder("给天工发送消息")
            .class("composer_input")
            .on_input(|states, ictx| {
                states.ensure::<TiangongState>().update_draft(ictx.value);
            })
            .on_submit(|states, sctx| {
                let state = states.ensure::<TiangongState>();
                state.update_draft(sctx.value);
                if let Err(err) = state.send_current_input() {
                    state.run = RunSnapshot {
                        status: RunStatus::Failed,
                        summary: "发送失败".to_string(),
                        last_session_id: state.run.last_session_id.clone(),
                        last_task_id: state.run.last_task_id.clone(),
                        last_duration_ms: state.run.last_duration_ms,
                        last_result: state.run.last_result.clone(),
                        last_plan: state.run.last_plan.clone(),
                        last_tool_result: state.run.last_tool_result.clone(),
                        last_error: Some(err.to_string()),
                        last_usage: state.run.last_usage.clone(),
                        updated_at: now_text(),
                    };
                }
                let windows = states.ensure::<WindowStateManager>();
                windows.request_redraw(sctx.app_ctx.window_id);
            })
            .mount(tree, handlers, cx);

        let mut model_options = model_list
            .into_iter()
            .map(|model| SelectOption::new(model.clone(), model))
            .collect::<Vec<_>>();
        if model_options.is_empty() && !current_model.trim().is_empty() {
            model_options.push(SelectOption::new(
                current_model.clone(),
                current_model.clone(),
            ));
        }

        let selected_model = if current_model.trim().is_empty() {
            None
        } else {
            Some(current_model.clone())
        };

        let composer_model_label = Text::new("模型")
            .class("composer_model_label")
            .mount(tree, handlers);
        let composer_model_select = SelectBuilder::new()
            .id(self.model_select)
            .options(model_options)
            .placeholder("选择模型")
            .value(SelectValue::Single(selected_model))
            .on_change(|states, sctx| {
                let model = match sctx.value {
                    SelectValue::Single(Some(value)) => value,
                    _ => return,
                };

                let select_result = states.ensure::<TiangongState>().select_model(&model);
                if let Err(err) = select_result {
                    let state = states.ensure::<TiangongState>();
                    state.run = RunSnapshot {
                        status: RunStatus::Failed,
                        summary: "模型切换失败".to_string(),
                        last_session_id: state.run.last_session_id.clone(),
                        last_task_id: state.run.last_task_id.clone(),
                        last_duration_ms: state.run.last_duration_ms,
                        last_result: state.run.last_result.clone(),
                        last_plan: state.run.last_plan.clone(),
                        last_tool_result: state.run.last_tool_result.clone(),
                        last_error: Some(err.to_string()),
                        last_usage: state.run.last_usage.clone(),
                        updated_at: now_text(),
                    };
                }

                let windows = states.ensure::<WindowStateManager>();
                windows.request_redraw(sctx.app_ctx.window_id);
                if let Some(settings_id) = windows.find_window("provider-settings") {
                    windows.request_redraw(settings_id);
                }
            })
            .build()
            .mount(tree, handlers, cx);

        let send_button = Button::new("发送")
            .widget_id(self.send_btn)
            .class("send_btn")
            .disabled(draft.trim().is_empty() || has_pending_turn)
            .on_click(|states: &mut StateMap, ctx| {
                let state = states.ensure::<TiangongState>();
                if let Err(err) = state.send_current_input() {
                    state.run = RunSnapshot {
                        status: RunStatus::Failed,
                        summary: "发送失败".to_string(),
                        last_session_id: state.run.last_session_id.clone(),
                        last_task_id: state.run.last_task_id.clone(),
                        last_duration_ms: state.run.last_duration_ms,
                        last_result: state.run.last_result.clone(),
                        last_plan: state.run.last_plan.clone(),
                        last_tool_result: state.run.last_tool_result.clone(),
                        last_error: Some(err.to_string()),
                        last_usage: state.run.last_usage.clone(),
                        updated_at: now_text(),
                    };
                }
                let windows = states.ensure::<WindowStateManager>();
                windows.request_redraw(ctx.app_ctx.window_id);
            })
            .mount(tree, handlers);

        let composer_model_row = Container::new(vec![composer_model_label, composer_model_select])
            .class("composer_model_row")
            .mount(tree, handlers);
        let composer_input_row = Container::new(vec![input_node, send_button])
            .class("composer_input_row")
            .mount(tree, handlers);

        let composer_node = Container::new(vec![composer_model_row, composer_input_row])
            .class("composer")
            .mount(tree, handlers);

        let conversation_node = Container::new(vec![message_list_node, composer_node])
            .class("conversation")
            .mount(tree, handlers);

        let main_panel = Container::new(vec![
            top_bar,
            session_actions,
            conversation_node,
            run_status_node,
        ])
        .class("main_panel")
        .mount(tree, handlers);

        let root = Container::new(vec![sidebar_node, main_panel])
            .size(Dimension::Percent(1.0), Dimension::Percent(1.0))
            .class("app_root")
            .mount(tree, handlers);

        tree.root_mut().children.push(root);

        if has_pending_turn {
            let windows = states.ensure::<WindowStateManager>();
            windows.request_redraw(cx.window_id);
        }
    }
}

fn open_provider_settings_window(states: &mut StateMap) {
    states.ensure::<TiangongState>().open_provider_settings();

    let settings_window_id = {
        let windows = states.ensure::<WindowStateManager>();
        if let Some(id) = windows.find_window("provider-settings") {
            let _ = windows.show_window(id);
            id
        } else {
            windows.open_window_with_options(
                "provider-settings",
                ProviderSettingsWindow::default(),
                WindowOptions {
                    title: "天工 - 模型供应商配置".to_string(),
                    width_px: 560,
                    height_px: 620,
                    visible: true,
                    ..Default::default()
                },
            )
        }
    };

    let windows = states.ensure::<WindowStateManager>();
    windows.request_redraw(settings_window_id);
}

#[derive(Debug)]
struct ProviderSettingsWindow {
    api_auth_token_input: WidgetId,
    api_base_url_input: WidgetId,
    api_timeout_ms_input: WidgetId,
    api_model_input: WidgetId,
    refresh_models_btn: WidgetId,
    model_seed: WidgetId,
    save_btn: WidgetId,
    cancel_btn: WidgetId,
}

impl Default for ProviderSettingsWindow {
    fn default() -> Self {
        Self {
            api_auth_token_input: WidgetId::new(),
            api_base_url_input: WidgetId::new(),
            api_timeout_ms_input: WidgetId::new(),
            api_model_input: WidgetId::new(),
            refresh_models_btn: WidgetId::new(),
            model_seed: WidgetId::new(),
            save_btn: WidgetId::new(),
            cancel_btn: WidgetId::new(),
        }
    }
}

impl Component for ProviderSettingsWindow {
    fn build(
        &mut self,
        tree: &mut UiTree,
        handlers: &mut UiHandlers<StateMap>,
        states: &mut StateMap,
        cx: AppCtx,
    ) {
        let state = states.ensure::<TiangongState>();
        let current_cfg = state.model_config().clone();
        let draft_api_auth_token = state.settings_api_auth_token_draft().to_string();
        let draft_api_base_url = state.settings_api_base_url_draft().to_string();
        let draft_api_timeout_ms = state.settings_api_timeout_ms_draft().to_string();
        let draft_api_model = state.settings_api_model_draft().to_string();
        let model_list = state.settings_model_list().to_vec();

        let title = Text::new("模型供应商配置")
            .class("settings_title")
            .mount(tree, handlers);

        let current = Text::new(format!(
            "{{\n  \"API_AUTH_TOKEN\": \"{}\",\n  \"API_BASE_URL\": \"{}\",\n  \"API_TIMEOUT_MS\": \"{}\",\n  \"API_MODEL\": \"{}\"\n}}",
            current_cfg.masked_auth_token(),
            current_cfg.api_base_url,
            current_cfg.api_timeout_ms,
            current_cfg.api_model
        ))
        .class("settings_current")
        .mount(tree, handlers);

        let api_auth_token_label = Text::new("API_AUTH_TOKEN")
            .class("settings_label")
            .mount(tree, handlers);

        let api_auth_token_input = Input::new()
            .widget_id(self.api_auth_token_input)
            .text(draft_api_auth_token)
            .placeholder("例如 sk-ant-xxxx")
            .class("settings_input")
            .on_input(|states, ictx| {
                states
                    .ensure::<TiangongState>()
                    .update_settings_api_auth_token_draft(ictx.value);
            })
            .mount(tree, handlers, cx);

        let api_base_url_label = Text::new("API_BASE_URL")
            .class("settings_label")
            .mount(tree, handlers);

        let api_base_url_input = Input::new()
            .widget_id(self.api_base_url_input)
            .text(draft_api_base_url)
            .placeholder("例如 https://open.bigmodel.cn/api/paas/v4")
            .class("settings_input")
            .on_input(|states, ictx| {
                states
                    .ensure::<TiangongState>()
                    .update_settings_api_base_url_draft(ictx.value);
            })
            .mount(tree, handlers, cx);

        let api_timeout_ms_label = Text::new("API_TIMEOUT_MS")
            .class("settings_label")
            .mount(tree, handlers);

        let api_timeout_ms_input = Input::new()
            .widget_id(self.api_timeout_ms_input)
            .text(draft_api_timeout_ms)
            .placeholder("例如 3000000")
            .class("settings_input")
            .on_input(|states, ictx| {
                states
                    .ensure::<TiangongState>()
                    .update_settings_api_timeout_ms_draft(ictx.value);
            })
            .mount(tree, handlers, cx);

        let api_model_label = Text::new("API_MODEL")
            .class("settings_label")
            .mount(tree, handlers);

        let api_model_input = Input::new()
            .widget_id(self.api_model_input)
            .text(draft_api_model.clone())
            .placeholder("例如 glm-4-plus 或 gpt-4o-mini")
            .class("settings_input")
            .on_input(|states, ictx| {
                states
                    .ensure::<TiangongState>()
                    .update_settings_api_model_draft(ictx.value);
            })
            .mount(tree, handlers, cx);

        let refresh_models_btn = Button::new("更新模型列表")
            .widget_id(self.refresh_models_btn)
            .class("settings_refresh_btn")
            .on_click(|states, ctx| {
                let refresh_result = states.ensure::<TiangongState>().refresh_model_list();

                let state = states.ensure::<TiangongState>();
                match refresh_result {
                    Ok(count) => {
                        state.run = RunSnapshot {
                            status: RunStatus::Idle,
                            summary: format!("模型列表已更新：{count} 项"),
                            last_session_id: state.run.last_session_id.clone(),
                            last_task_id: state.run.last_task_id.clone(),
                            last_duration_ms: state.run.last_duration_ms,
                            last_result: state.run.last_result.clone(),
                            last_plan: state.run.last_plan.clone(),
                            last_tool_result: state.run.last_tool_result.clone(),
                            last_error: None,
                            last_usage: state.run.last_usage.clone(),
                            updated_at: now_text(),
                        };
                    }
                    Err(err) => {
                        state.run = RunSnapshot {
                            status: RunStatus::Failed,
                            summary: "更新模型列表失败".to_string(),
                            last_session_id: state.run.last_session_id.clone(),
                            last_task_id: state.run.last_task_id.clone(),
                            last_duration_ms: state.run.last_duration_ms,
                            last_result: state.run.last_result.clone(),
                            last_plan: state.run.last_plan.clone(),
                            last_tool_result: state.run.last_tool_result.clone(),
                            last_error: Some(err.to_string()),
                            last_usage: state.run.last_usage.clone(),
                            updated_at: now_text(),
                        };
                    }
                }

                let windows = states.ensure::<WindowStateManager>();
                if let Some(main_id) = windows.find_window("main") {
                    windows.request_redraw(main_id);
                }
                windows.request_redraw(ctx.app_ctx.window_id);
            })
            .mount(tree, handlers);

        let model_list_label = Text::new("可用模型列表")
            .class("settings_label")
            .mount(tree, handlers);

        let mut model_nodes = Vec::new();
        if model_list.is_empty() {
            model_nodes.push(
                Text::new("暂无模型列表，请先点击“更新模型列表”。")
                    .class("settings_hint")
                    .mount(tree, handlers),
            );
        } else {
            for model in model_list {
                let mut model_btn = Button::new(model.clone())
                    .widget_id(derive_id_from_key(self.model_seed, &model))
                    .class("model_item_btn")
                    .on_click({
                        let model = model.clone();
                        move |states: &mut StateMap, _ctx| {
                            states
                                .ensure::<TiangongState>()
                                .update_settings_api_model_draft(model.clone());
                        }
                    });

                if model == draft_api_model {
                    model_btn = model_btn.class("model_item_btn_active");
                }
                model_nodes.push(model_btn.mount(tree, handlers));
            }
        }

        let model_list_node = Container::new(model_nodes)
            .class("settings_model_list")
            .mount(tree, handlers);

        let save_btn = Button::new("保存并生效")
            .widget_id(self.save_btn)
            .class("settings_save_btn")
            .on_click(|states, ctx| {
                let save_result = states.ensure::<TiangongState>().save_provider_settings();

                match save_result {
                    Ok(()) => {
                        let windows = states.ensure::<WindowStateManager>();
                        if let Some(main_id) = windows.find_window("main") {
                            windows.request_redraw(main_id);
                        }
                        let _ = windows.hide_window(ctx.app_ctx.window_id);
                    }
                    Err(err) => {
                        let state = states.ensure::<TiangongState>();
                        state.run = RunSnapshot {
                            status: RunStatus::Failed,
                            summary: "模型配置保存失败".to_string(),
                            last_session_id: state.run.last_session_id.clone(),
                            last_task_id: state.run.last_task_id.clone(),
                            last_duration_ms: state.run.last_duration_ms,
                            last_result: state.run.last_result.clone(),
                            last_plan: state.run.last_plan.clone(),
                            last_tool_result: state.run.last_tool_result.clone(),
                            last_error: Some(err.to_string()),
                            last_usage: state.run.last_usage.clone(),
                            updated_at: now_text(),
                        };
                        let windows = states.ensure::<WindowStateManager>();
                        if let Some(main_id) = windows.find_window("main") {
                            windows.request_redraw(main_id);
                        }
                        windows.request_redraw(ctx.app_ctx.window_id);
                    }
                }
            })
            .mount(tree, handlers);

        let cancel_btn = Button::new("取消")
            .widget_id(self.cancel_btn)
            .class("settings_cancel_btn")
            .on_click(|states, ctx| {
                states.ensure::<TiangongState>().discard_provider_settings();
                let windows = states.ensure::<WindowStateManager>();
                let _ = windows.hide_window(ctx.app_ctx.window_id);
            })
            .mount(tree, handlers);

        let actions = Container::new(vec![save_btn, cancel_btn])
            .class("settings_actions")
            .mount(tree, handlers);

        let panel = Container::new(vec![
            title,
            current,
            api_auth_token_label,
            api_auth_token_input,
            api_base_url_label,
            api_base_url_input,
            api_timeout_ms_label,
            api_timeout_ms_input,
            api_model_label,
            api_model_input,
            refresh_models_btn,
            model_list_label,
            model_list_node,
            actions,
        ])
        .class("settings_panel")
        .mount(tree, handlers);

        let root = Container::new(vec![panel])
            .size(Dimension::Percent(1.0), Dimension::Percent(1.0))
            .class("settings_root")
            .mount(tree, handlers);

        tree.root_mut().children.push(root);
    }
}

fn build_run_status_node(
    tree: &mut UiTree,
    handlers: &mut UiHandlers<StateMap>,
    run: &RunSnapshot,
) -> NodeId {
    let status_text = match run.status {
        RunStatus::Idle => "状态：空闲",
        RunStatus::Planning => "状态：规划中",
        RunStatus::Executing => "状态：执行中",
        RunStatus::Completed => "状态：已完成",
        RunStatus::Failed => "状态：失败",
    };

    let mut children = vec![
        Text::new(status_text)
            .class("status_title")
            .mount(tree, handlers),
        Text::new(format!("摘要：{}", run.summary))
            .class("status_summary")
            .mount(tree, handlers),
    ];

    if let Some(session_id) = &run.last_session_id {
        children.push(
            Text::new(format!("会话ID：{session_id}"))
                .class("status_summary")
                .mount(tree, handlers),
        );
    }

    if let Some(task_id) = &run.last_task_id {
        children.push(
            Text::new(format!("任务ID：{task_id}"))
                .class("status_summary")
                .mount(tree, handlers),
        );
    }

    if let Some(duration_ms) = run.last_duration_ms {
        children.push(
            Text::new(format!("耗时：{duration_ms}ms"))
                .class("status_summary")
                .mount(tree, handlers),
        );
    }

    if let Some(result) = &run.last_result {
        children.push(
            Text::new(format!("结果：{result}"))
                .class("status_summary")
                .mount(tree, handlers),
        );
    }

    if let Some(plan) = &run.last_plan {
        children.push(
            Text::new(format!("计划：{plan}"))
                .class("status_plan")
                .mount(tree, handlers),
        );
    }

    if let Some(tool_result) = &run.last_tool_result {
        children.push(
            Text::new(format!("工具：{tool_result}"))
                .class("status_tool")
                .mount(tree, handlers),
        );
    }

    if let Some(err) = &run.last_error {
        children.push(
            Text::new(format!("错误：{err}"))
                .class("status_error")
                .mount(tree, handlers),
        );
    }

    if let Some(usage) = &run.last_usage {
        children.push(
            Text::new(format!(
                "token：prompt={} completion={} total={}",
                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
            ))
            .class("status_usage")
            .mount(tree, handlers),
        );
    }

    children.push(
        Text::new(format!("更新时间：{}", run.updated_at))
            .class("status_time")
            .mount(tree, handlers),
    );

    Container::new(children)
        .class("status_panel")
        .mount(tree, handlers)
}

fn derive_id_from_key(seed: WidgetId, key: &str) -> WidgetId {
    let mut hash = 0u128;
    for byte in key.bytes() {
        hash = hash.wrapping_mul(131).wrapping_add(byte as u128 + 1);
    }
    seed.derive(hash)
}

pub fn run() -> anyhow::Result<()> {
    let main_window = Window::new(TiangongApp::default()).with_options(WindowOptions {
        title: "天工".to_string(),
        ..Default::default()
    });

    let settings_window =
        Window::new(ProviderSettingsWindow::default()).with_options(WindowOptions {
            title: "天工 - 模型供应商配置".to_string(),
            width_px: 560,
            height_px: 620,
            visible: false,
            ..Default::default()
        });

    App::new(main_window)
        .with_window("provider-settings", settings_window)
        .with_state(TiangongState::load_or_default())
        .styles_from_css(
            include_str!("style.css"),
            source::RuntimeStyleOverride::Auto {
                file_name: "src/ui/style.css",
            },
        )?
        .run()
}
