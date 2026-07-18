use crate::theme::{is_dark_mode, toggle_theme};
use gpui::*;
use gpui_component::theme::{ActiveTheme, Theme};
use gpui_component::{Icon, IconName};

// Filters (All/Mine/Friends) and a followed-peers list belong here once the
// engine exposes them: "Mine" needs `Peer::authored()`, "Friends" needs a
// public followed-logs query with resolved names (petname via `same-as`
// entity claims), neither of which exist yet.

#[derive(IntoElement)]
pub struct Sidebar {
    is_collapsed: bool,
    on_toggle: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            is_collapsed: false,
            on_toggle: None,
        }
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.is_collapsed = collapsed;
        self
    }

    pub fn on_toggle(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }

    fn render_theme_switcher(theme: &Theme, is_dark: bool) -> Stateful<Div> {
        let icon = if is_dark {
            IconName::Sun
        } else {
            IconName::Moon
        };
        let tooltip = if is_dark {
            "Switch to light mode"
        } else {
            "Switch to dark mode"
        };

        div()
            .id("theme-switcher")
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .bg(theme.sidebar)
            .hover(|style| style.bg(theme.list_hover))
            .on_click(move |_event, window, cx| {
                toggle_theme(window, cx);
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(icon).size_4().text_color(theme.foreground))
                    .child(div().text_sm().text_color(theme.foreground).child(tooltip)),
            )
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let is_dark = is_dark_mode(cx);

        if self.is_collapsed {
            let theme_icon = if is_dark {
                IconName::Sun
            } else {
                IconName::Moon
            };

            let mut expand_btn = div()
                .id("expand-btn")
                .cursor_pointer()
                .p_2()
                .rounded_md()
                .hover(|style| style.bg(theme.list_hover))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.primary_hover)
                        .child("V"),
                );

            if let Some(on_toggle) = self.on_toggle {
                expand_btn = expand_btn.on_click(move |event, window, cx| {
                    on_toggle(event, window, cx);
                });
            }

            let theme_btn = div()
                .id("theme-btn-collapsed")
                .cursor_pointer()
                .p_2()
                .rounded_md()
                .hover(|style| style.bg(theme.list_hover))
                .on_click(move |_event, window, cx| {
                    toggle_theme(window, cx);
                })
                .child(Icon::new(theme_icon).size_4().text_color(theme.foreground));

            return div()
                .flex()
                .flex_col()
                .h_full()
                .w_10()
                .min_w_10()
                .bg(theme.sidebar)
                .border_r_1()
                .border_color(theme.border)
                .items_center()
                .pt_2()
                .child(expand_btn)
                .child(div().flex_1())
                .child(div().pb_2().child(theme_btn));
        }

        let mut collapse_btn = div()
            .id("collapse-btn")
            .cursor_pointer()
            .px_2()
            .py_1()
            .rounded_md()
            .hover(|style| style.bg(theme.list_hover))
            .child(
                Icon::new(IconName::PanelLeftClose)
                    .size_4()
                    .text_color(theme.muted_foreground),
            );

        if let Some(on_toggle) = self.on_toggle {
            collapse_btn = collapse_btn.on_click(move |event, window, cx| {
                on_toggle(event, window, cx);
            });
        }

        let header = div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.primary_hover)
                    .child("Vouch"),
            )
            .child(collapse_btn);

        div()
            .flex()
            .flex_col()
            .h_full()
            .w_40()
            .min_w_40()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(header)
            .child(div().id("sidebar-content").flex_1())
            .child(
                div()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Self::render_theme_switcher(&theme, is_dark)),
            )
    }
}
