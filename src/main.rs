use gpui::*;
use gpui_component::{
    Root,
    button::{Button, ButtonVariants},
};

struct Vouch {
    count: i32,
}

impl Vouch {
    fn new() -> Self {
        Self { count: 0 }
    }

    fn increment(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.count += 1;
        cx.notify();
    }

    fn decrement(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.count -= 1;
        cx.notify();
    }
}

impl Render for Vouch {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .items_center()
            .gap_4()
            .child(div().text_xl().child("Welcome to Vouch!"))
            .child(div().text_2xl().child(format!("{}", self.count)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Button::new("decrement")
                            .label("-")
                            .on_click(cx.listener(Self::decrement)),
                    )
                    .child(
                        Button::new("increment")
                            .primary()
                            .label("+")
                            .on_click(cx.listener(Self::increment)),
                    ),
            )
    }
}

fn main() {
    Application::new().run(|cx| {
        gpui_component::init(cx);

        let bounds = Bounds::centered(None, size(px(800.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Vouch".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|_cx| Vouch::new());
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .unwrap();
    });
}
