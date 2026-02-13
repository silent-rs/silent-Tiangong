use components::{Button, Container, Input, Text};
use hetu::prelude::*;

use crate::core::app_state::TiangongState;
use crate::core::runtime::{RunSnapshot, RunStatus};
use crate::core::session::{Message, MessageRole, now_text};
use crate::ui::components::composer::{ComposerData, ComposerIds, build_composer};

#[derive(Debug, Clone)]
pub(crate) struct ConversationData {
    pub active_title: String,
    pub active_messages: Vec<Message>,
    pub active_message_count: usize,
    pub provider_label: String,
    pub draft: String,
    pub has_pending_turn: bool,
    pub run_snapshot: RunSnapshot,
    pub current_model: String,
    pub model_list: Vec<String>,
    pub session_title_draft: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConversationIds {
    pub header_settings_btn: WidgetId,
    pub session_title_input: WidgetId,
    pub session_title_save_btn: WidgetId,
    pub session_delete_btn: WidgetId,
    pub model_select: WidgetId,
    pub send_btn: WidgetId,
    pub input_widget: WidgetId,
}

pub(crate) fn build_conversation_panel(
    tree: &mut UiTree,
    handlers: &mut UiHandlers<StateMap>,
    cx: AppCtx,
    data: &ConversationData,
    ids: ConversationIds,
    on_open_settings: fn(&mut StateMap),
) -> NodeId {
    let heading_title = Text::new(data.active_title.clone())
        .class("main_title")
        .mount(tree, handlers);
    let heading_subtitle = Text::new(format!("{} 条消息", data.active_message_count))
        .class("main_subtitle")
        .mount(tree, handlers);
    let heading = Container::new(vec![heading_title, heading_subtitle])
        .class("main_heading")
        .mount(tree, handlers);

    let model_pill = Text::new(format!("API：{}", data.provider_label))
        .class("model_pill")
        .mount(tree, handlers);
    let header_settings_btn = Button::new("设置")
        .widget_id(ids.header_settings_btn)
        .class("top_settings_btn")
        .on_click(move |states: &mut StateMap, _ctx| {
            on_open_settings(states);
        })
        .mount(tree, handlers);
    let top_actions = Container::new(vec![model_pill, header_settings_btn])
        .class("top_actions")
        .mount(tree, handlers);
    let top_bar = Container::new(vec![heading, top_actions])
        .class("top_bar")
        .mount(tree, handlers);

    let session_title_input = Input::new()
        .widget_id(ids.session_title_input)
        .text(data.session_title_draft.clone())
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
                set_run_failed(state, "会话重命名失败", err.to_string());
            }
        })
        .mount(tree, handlers, cx);

    let save_session_title_btn = Button::new("保存标题")
        .widget_id(ids.session_title_save_btn)
        .class("session_title_save_btn")
        .disabled(data.session_title_draft.trim().is_empty())
        .on_click(|states: &mut StateMap, _ctx| {
            let state = states.ensure::<TiangongState>();
            if let Err(err) = state.save_active_session_title() {
                set_run_failed(state, "会话重命名失败", err.to_string());
            }
        })
        .mount(tree, handlers);

    let delete_session_btn = Button::new("删除会话")
        .widget_id(ids.session_delete_btn)
        .class("session_delete_btn")
        .on_click(|states: &mut StateMap, _ctx| {
            let state = states.ensure::<TiangongState>();
            if let Err(err) = state.delete_active_session() {
                set_run_failed(state, "删除会话失败", err.to_string());
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
    let visible_messages = data.active_messages.iter().collect::<Vec<_>>();
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

    let run_status_node = build_run_status_node(tree, handlers, &data.run_snapshot);
    let composer_data = ComposerData {
        draft: data.draft.clone(),
        has_pending_turn: data.has_pending_turn,
        current_model: data.current_model.clone(),
        model_list: data.model_list.clone(),
    };
    let composer_ids = ComposerIds {
        model_select: ids.model_select,
        send_btn: ids.send_btn,
        input_widget: ids.input_widget,
    };
    let composer_node = build_composer(tree, handlers, cx, &composer_data, composer_ids);
    let conversation_node = Container::new(vec![message_list_node, composer_node])
        .class("conversation")
        .mount(tree, handlers);

    Container::new(vec![
        top_bar,
        session_actions,
        conversation_node,
        run_status_node,
    ])
    .class("main_panel")
    .mount(tree, handlers)
}

fn set_run_failed(state: &mut TiangongState, summary: &str, error: String) {
    state.run = RunSnapshot {
        status: RunStatus::Failed,
        summary: summary.to_string(),
        last_session_id: state.run.last_session_id.clone(),
        last_task_id: state.run.last_task_id.clone(),
        last_duration_ms: state.run.last_duration_ms,
        last_result: state.run.last_result.clone(),
        last_plan: state.run.last_plan.clone(),
        last_tool_result: state.run.last_tool_result.clone(),
        last_error: Some(error),
        last_usage: state.run.last_usage.clone(),
        updated_at: now_text(),
    };
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
