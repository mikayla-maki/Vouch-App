use crate::theme::{is_dark_mode, toggle_theme};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::theme::{ActiveTheme, Theme};
use gpui_component::{Icon, IconName};
use vouch_core::LogId;

// Filters (All/Mine/Friends) belong here once the engine exposes them:
// "Mine" needs `Peer::authored()`; petname overrides for the follows list
// are a designed-but-unbuilt concept.

#[derive(IntoElement)]
pub struct Sidebar {
    is_collapsed: bool,
    debug_active: bool,
    /// (advertised name if any, full hex address) for the local writer.
    own_name: Option<String>,
    own_address: Option<String>,
    /// Everyone followed, with their advertised names where known.
    follows: Vec<(LogId, Option<String>)>,
    on_toggle: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_debug: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_add_follow: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            is_collapsed: false,
            debug_active: false,
            own_name: None,
            own_address: None,
            follows: Vec::new(),
            on_toggle: None,
            on_debug: None,
            on_add_follow: None,
        }
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.is_collapsed = collapsed;
        self
    }

    /// The local writer's advertised name and full hex address.
    pub fn identity(mut self, name: Option<String>, address: Option<String>) -> Self {
        self.own_name = name;
        self.own_address = address;
        self
    }

    /// Everyone followed, with advertised names where their profile
    /// claims have synced in.
    pub fn follows(mut self, follows: Vec<(LogId, Option<String>)>) -> Self {
        self.follows = follows;
        self
    }

    /// Open the follow-someone dialog.
    pub fn on_add_follow(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_add_follow = Some(Box::new(handler));
        self
    }

    /// Whether the raw-claims debug view is currently open (drives the toggle
    /// button's highlight).
    pub fn debug_active(mut self, active: bool) -> Self {
        self.debug_active = active;
        self
    }

    pub fn on_toggle(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }

    /// Toggle the raw-claims debug viewer (a dev tool for inspecting the
    /// underlying database, wired up in `VouchApp`).
    pub fn on_debug(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_debug = Some(Box::new(handler));
        self
    }

    /// A "raw claims" toggle button. `compact` renders just the glyph (for the
    /// collapsed rail); otherwise a labeled row.
    fn render_debug_button(
        active: bool,
        compact: bool,
        theme: &Theme,
        on_debug: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    ) -> Stateful<Div> {
        let color = if active {
            theme.primary_hover
        } else {
            theme.muted_foreground
        };

        let mut btn = div()
            .id("debug-toggle")
            .cursor_pointer()
            .rounded_md()
            .hover(|style| style.bg(theme.list_hover))
            .when(active, |this| this.bg(theme.list_active));

        btn = if compact {
            btn.p_2().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(color)
                    .child("{}"),
            )
        } else {
            btn.w_full().px_3().py_2().child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(color)
                            .child("{}"),
                    )
                    .child(div().text_sm().text_color(theme.foreground).child("Raw claims")),
            )
        };

        if let Some(on_debug) = on_debug {
            btn = btn.on_click(move |event, window, cx| {
                on_debug(event, window, cx);
            });
        }
        btn
    }

    /// The "You" block: advertised name, short address, one-click copy of
    /// the full address (the thing you text a friend).
    fn render_identity(
        own_name: Option<String>,
        own_address: Option<String>,
        theme: &Theme,
    ) -> Div {
        let Some(address) = own_address else {
            return div();
        };
        // Show the hex prefix past the `vouch:` scheme; copy the whole
        // capability string.
        let hex = address.strip_prefix("vouch:").unwrap_or(&address);
        let short = format!("{}…", &hex[..8.min(hex.len())]);
        let display_name = own_name.unwrap_or_else(|| "You".to_string());

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("YOU"),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(display_name),
            )
            .child(
                div()
                    .id("copy-own-address")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .rounded_md()
                    .hover(|style| style.bg(theme.list_hover))
                    .on_click(move |_, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(address.clone()));
                    })
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(short),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.primary_hover)
                            .child("copy address"),
                    ),
            )
    }

    /// The people you follow, advertised names first, hash prefixes for
    /// the not-yet-named — plus the button that adds someone new.
    fn render_follows(
        follows: Vec<(LogId, Option<String>)>,
        theme: &Theme,
        on_add_follow: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    ) -> Div {
        let count = follows.len();
        let mut rows: Vec<Div> = Vec::new();
        for (log, name) in follows {
            let (label, sub) = match name {
                Some(name) => (name, log.short()),
                None => (log.short(), String::new()),
            };
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(label),
                    )
                    .when(!sub.is_empty(), |this| {
                        this.child(
                            div()
                                .font_family("monospace")
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(sub),
                        )
                    }),
            );
        }

        let mut add_btn = div()
            .id("add-follow-btn")
            .mx_2()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.primary_hover)
                    .child("+ Follow someone"),
            );
        if let Some(on_add_follow) = on_add_follow {
            add_btn = add_btn.on_click(move |event, window, cx| {
                on_add_follow(event, window, cx);
            });
        }

        div()
            .flex()
            .flex_col()
            .gap_1()
            .mt_2()
            .child(
                div()
                    .px_3()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(format!("FOLLOWING ({count})")),
            )
            .when(count == 0, |this| {
                this.child(
                    div()
                        .px_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Nobody yet — ask a friend for their address."),
                )
            })
            .children(rows)
            .child(add_btn)
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
                .gap_1()
                .items_center()
                .pt_2()
                .child(expand_btn)
                .child(Self::render_debug_button(
                    self.debug_active,
                    true,
                    &theme,
                    self.on_debug,
                ))
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
            .w_56()
            .min_w_56()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(header)
            .child(
                div()
                    .id("sidebar-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .py_2()
                    .overflow_y_scroll()
                    .child(Self::render_identity(
                        self.own_name,
                        self.own_address,
                        &theme,
                    ))
                    .child(Self::render_follows(
                        self.follows,
                        &theme,
                        self.on_add_follow,
                    ))
                    .child(div().flex_1())
                    .child(div().px_2().child(Self::render_debug_button(
                        self.debug_active,
                        false,
                        &theme,
                        self.on_debug,
                    ))),
            )
            .child(
                div()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Self::render_theme_switcher(&theme, is_dark)),
            )
    }
}
