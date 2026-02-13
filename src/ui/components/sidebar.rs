use components::{Button, Container, Text};
use hetu::prelude::*;

use crate::core::app_state::TiangongState;

#[derive(Debug, Clone)]
pub(crate) struct SessionItemData {
    pub id: String,
    pub title: String,
    pub message_count: usize,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SidebarData {
    pub provider_label: String,
    pub sessions: Vec<SessionItemData>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SidebarIds {
    pub session_seed: WidgetId,
    pub new_session_btn: WidgetId,
    pub sidebar_settings_btn: WidgetId,
}

pub(crate) fn build_sidebar(
    tree: &mut UiTree,
    handlers: &mut UiHandlers<StateMap>,
    data: &SidebarData,
    ids: SidebarIds,
    on_open_settings: fn(&mut StateMap),
) -> NodeId {
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
        .widget_id(ids.new_session_btn)
        .class("session_new_btn")
        .on_click(|states: &mut StateMap, _ctx| {
            states.ensure::<TiangongState>().create_session();
        })
        .mount(tree, handlers);

    let sidebar_settings_btn = Button::new("LLM 设置")
        .widget_id(ids.sidebar_settings_btn)
        .class("settings_open_btn")
        .on_click(move |states: &mut StateMap, _ctx| {
            on_open_settings(states);
        })
        .mount(tree, handlers);

    let mut session_items = Vec::new();
    for session in &data.sessions {
        let btn_label = format!("{} ({})", session.title, session.message_count);
        let mut session_button = Button::new(btn_label)
            .widget_id(derive_id_from_key(ids.session_seed, &session.id))
            .class("session_btn")
            .on_click({
                let session_id = session.id.clone();
                move |states: &mut StateMap, _ctx| {
                    states.ensure::<TiangongState>().switch_session(&session_id);
                }
            });

        if session.is_active {
            session_button = session_button.class("session_btn_active");
        }
        session_items.push(session_button.mount(tree, handlers));
    }

    let session_section = Container::new(session_items)
        .class("session_section")
        .mount(tree, handlers);

    let provider_node = Text::new(data.provider_label.clone())
        .class("sidebar_provider")
        .mount(tree, handlers);
    let sidebar_footer = Container::new(vec![provider_node, sidebar_settings_btn])
        .class("sidebar_footer")
        .mount(tree, handlers);

    Container::new(vec![
        sidebar_brand,
        new_session_btn,
        session_section,
        sidebar_footer,
    ])
    .class("sidebar")
    .mount(tree, handlers)
}

fn derive_id_from_key(seed: WidgetId, key: &str) -> WidgetId {
    let mut hash = 0u128;
    for byte in key.bytes() {
        hash = hash.wrapping_mul(131).wrapping_add(byte as u128 + 1);
    }
    seed.derive(hash)
}
