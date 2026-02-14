use components::{Button, Container, Input, SelectBuilder, SelectOption, SelectValue, Text};
use hetu::prelude::*;

use crate::core::app_state::TiangongState;

#[derive(Debug, Clone)]
pub(crate) struct ComposerData {
    pub draft: String,
    pub has_pending_turn: bool,
    pub current_model: String,
    pub model_list: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ComposerIds {
    pub model_select: WidgetId,
    pub send_btn: WidgetId,
    pub input_widget: WidgetId,
}

pub(crate) fn build_composer(
    tree: &mut UiTree,
    handlers: &mut UiHandlers<StateMap>,
    cx: AppCtx,
    data: &ComposerData,
    ids: ComposerIds,
) -> NodeId {
    let input_node = Input::new()
        .widget_id(ids.input_widget)
        .text(data.draft.clone())
        .placeholder("给天工发送消息")
        .class("composer_input")
        .on_input(|states, ictx| {
            states.ensure::<TiangongState>().update_draft(ictx.value);
        })
        .on_submit(|states, sctx| {
            let state = states.ensure::<TiangongState>();
            state.update_draft(sctx.value);
            if let Err(err) = state.send_current_input() {
                state.report_run_failed("发送失败", err.to_string());
            }
            let windows = states.ensure::<WindowStateManager>();
            windows.request_redraw(sctx.app_ctx.window_id);
        })
        .mount(tree, handlers, cx);

    let mut model_options = data
        .model_list
        .iter()
        .map(|model| SelectOption::new(model.clone(), model.clone()))
        .collect::<Vec<_>>();
    if model_options.is_empty() && !data.current_model.trim().is_empty() {
        model_options.push(SelectOption::new(
            data.current_model.clone(),
            data.current_model.clone(),
        ));
    }

    let selected_model = if data.current_model.trim().is_empty() {
        None
    } else {
        Some(data.current_model.clone())
    };

    let composer_model_label = Text::new("模型")
        .class("composer_model_label")
        .mount(tree, handlers);
    let composer_model_select = SelectBuilder::new()
        .id(ids.model_select)
        .options(model_options)
        .placeholder("选择模型")
        .value(SelectValue::Single(selected_model))
        .on_change(|states, sctx| {
            let model = match sctx.value {
                SelectValue::Single(Some(value)) => value,
                _ => return,
            };

            if let Err(err) = states.ensure::<TiangongState>().select_model(&model) {
                let state = states.ensure::<TiangongState>();
                state.report_run_failed("模型切换失败", err.to_string());
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
        .widget_id(ids.send_btn)
        .class("send_btn")
        .disabled(data.draft.trim().is_empty() || data.has_pending_turn)
        .on_click(|states: &mut StateMap, ctx| {
            let state = states.ensure::<TiangongState>();
            if let Err(err) = state.send_current_input() {
                state.report_run_failed("发送失败", err.to_string());
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

    Container::new(vec![composer_model_row, composer_input_row])
        .class("composer")
        .mount(tree, handlers)
}
