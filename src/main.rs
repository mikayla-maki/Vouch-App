use gpui::*;
use gpui_component::Root;
use gpui_component::theme::{Theme, ThemeMode, ThemeSet};

mod app;
mod assets;
mod data;
mod theme;
mod ui;

use app::VouchApp;
use assets::Assets;

actions!(vouch, [Quit]);

fn load_vouch_theme(cx: &mut App) {
    let theme_json = include_str!("../themes/vouch.json");
    match serde_json::from_str::<ThemeSet>(theme_json) {
        Ok(theme_set) => {
            for theme_config in theme_set.themes {
                let mode = theme_config.mode;
                let rc_config = std::rc::Rc::new(theme_config);

                if mode == ThemeMode::Light {
                    Theme::global_mut(cx).light_theme = rc_config.clone();
                } else {
                    Theme::global_mut(cx).dark_theme = rc_config.clone();
                }
            }
            Theme::change(ThemeMode::Light, None, cx);
        }
        Err(e) => {
            eprintln!("Failed to parse Vouch theme: {}", e);
        }
    }
}

fn main() {
    gpui_platform::application().with_assets(Assets).run(|cx| {
        gpui_component::init(cx);

        load_vouch_theme(cx);

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
