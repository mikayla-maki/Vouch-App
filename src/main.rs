use gpui::*;
use gpui_component::Root;
use gpui_component::theme::{Theme, ThemeMode};

mod app;
mod assets;
mod data;
mod theme;
mod ui;

use app::VouchApp;
use assets::Assets;
use theme::ActiveTheme;

actions!(vouch, [Quit]);

fn main() {
    Application::new().with_assets(Assets).run(|cx| {
        gpui_component::init(cx);

        // Force gpui-component's theme to light mode so Input text is dark
        Theme::change(ThemeMode::Light, None, cx);

        cx.set_global(ActiveTheme::light());

        let mut previous_theme_name: &'static str = "light";
        cx.observe_global::<ActiveTheme>(move |cx| {
            let current_name = cx.global::<ActiveTheme>().name;
            if current_name != previous_theme_name {
                previous_theme_name = current_name;
                cx.refresh_windows();
            }
        })
        .detach();

        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
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
                let view = cx.new(|cx| VouchApp::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .unwrap();

        cx.activate(true);
    });
}
