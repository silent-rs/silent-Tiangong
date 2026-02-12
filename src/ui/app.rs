use components::{Container, Text};
use hetu::prelude::*;

#[derive(Debug, Default)]
struct HelloApp;

impl Component for HelloApp {
    fn build(
        &mut self,
        tree: &mut UiTree,
        handlers: &mut UiHandlers<StateMap>,
        _states: &mut StateMap,
        _cx: AppCtx,
    ) {
        let text_id = Text::new("Hello, world!")
            .class("hello_text")
            .mount(tree, handlers);

        let app_id = Container::new(vec![text_id])
            .size(Dimension::Percent(1.0), Dimension::Percent(1.0))
            .class("app")
            .mount(tree, handlers);

        tree.root_mut().children.push(app_id);
    }
}

pub fn run() -> anyhow::Result<()> {
    let window = Window::new(HelloApp);
    App::new(window)
        .styles_from_css(
            include_str!("style.css"),
            source::RuntimeStyleOverride::Auto {
                file_name: "src/ui/style.css",
            },
        )?
        .run()
}
