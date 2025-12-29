use gpui::{App, Window};
use gpui_component::theme::{ActiveTheme, Theme, ThemeMode};

/// Toggle between light and dark theme
pub fn toggle_theme(window: &mut Window, cx: &mut App) {
    let current_mode = cx.theme().mode;
    let new_mode = if current_mode == ThemeMode::Light {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(new_mode, Some(window), cx);
}

/// Check if currently in dark mode
pub fn is_dark_mode(cx: &App) -> bool {
    cx.theme().mode == ThemeMode::Dark
}
