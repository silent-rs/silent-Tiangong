use components::{Button, Container, Input, Text};
use hetu::prelude::*;

use crate::core::app_state::TiangongState;
use crate::core::runtime::{RunSnapshot, RunStatus};
use crate::core::session::now_text;
use crate::ui::components::conversation::{
    ConversationData, ConversationIds, build_conversation_panel,
};
use crate::ui::components::sidebar::{SessionItemData, SidebarData, SidebarIds, build_sidebar};

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
        let provider_label = state.provider_label().to_string();

        let sessions = state
            .sessions()
            .iter()
            .map(|session| SessionItemData {
                id: session.id.clone(),
                title: session.title.clone(),
                message_count: session.messages.len(),
                is_active: session.id == active_session_id,
            })
            .collect::<Vec<_>>();

        let (active_title, active_messages) = if let Some(session) = state.active_session() {
            (session.title.clone(), session.messages.clone())
        } else {
            ("未找到会话".to_string(), Vec::new())
        };

        let sidebar_data = SidebarData {
            provider_label: provider_label.clone(),
            sessions,
        };
        let sidebar_ids = SidebarIds {
            session_seed: self.session_seed,
            new_session_btn: self.new_session_btn,
            sidebar_settings_btn: self.sidebar_settings_btn,
        };
        let sidebar_node = build_sidebar(
            tree,
            handlers,
            &sidebar_data,
            sidebar_ids,
            open_provider_settings_window,
        );

        let conversation_data = ConversationData {
            active_title,
            active_message_count: active_messages.len(),
            active_messages,
            provider_label,
            draft: state.input_draft.clone(),
            has_pending_turn: state.has_pending_turn(),
            run_snapshot: state.run.clone(),
            current_model: state.current_model().to_string(),
            model_list: state.model_list().to_vec(),
            session_title_draft: state.session_title_draft().to_string(),
        };
        let conversation_ids = ConversationIds {
            header_settings_btn: self.header_settings_btn,
            session_title_input: self.session_title_input,
            session_title_save_btn: self.session_title_save_btn,
            session_delete_btn: self.session_delete_btn,
            model_select: self.model_select,
            send_btn: self.send_btn,
            input_widget: self.input_widget,
        };
        let main_panel = build_conversation_panel(
            tree,
            handlers,
            cx,
            &conversation_data,
            conversation_ids,
            open_provider_settings_window,
        );

        let root = Container::new(vec![sidebar_node, main_panel])
            .size(Dimension::Percent(1.0), Dimension::Percent(1.0))
            .class("app_root")
            .mount(tree, handlers);

        tree.root_mut().children.push(root);

        if conversation_data.has_pending_turn {
            let windows = states.ensure::<WindowStateManager>();
            windows.request_redraw(cx.window_id);
        }
    }
}

fn derive_id_from_key(seed: WidgetId, key: &str) -> WidgetId {
    let mut hash = 0u128;
    for byte in key.bytes() {
        hash = hash.wrapping_mul(131).wrapping_add(byte as u128 + 1);
    }
    seed.derive(hash)
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
