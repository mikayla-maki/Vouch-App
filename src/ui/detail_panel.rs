use crate::ui::format::{TimeStyle, format_relative_time};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::theme::{ActiveTheme, Theme};
use std::time::{Duration, UNIX_EPOCH};
use vouch_core::{LogId, Recommendation};

// Vouch-chain and related-vouches sections belong here once `vouch` claims
// and `rec.about -> entity` linking exist end to end: real relatedness is
// `ClaimStore::backlinks` + `by_type("vouch"/"rec")`, not string-matching a
// subject name.

#[derive(IntoElement)]
pub struct DetailPanel {
    selected: Option<Recommendation>,
    local_log_id: Option<LogId>,
}

impl DetailPanel {
    pub fn new(selected: Option<Recommendation>, local_log_id: Option<LogId>) -> Self {
        Self {
            selected,
            local_log_id,
        }
    }

    fn render_empty_state(theme: &Theme) -> Stateful<Div> {
        div()
            .id("empty-state")
            .flex()
            .flex_col()
            .h_full()
            .flex_1()
            .bg(theme.background)
            .justify_center()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w_16()
                            .h_16()
                            .rounded_full()
                            .bg(theme.muted)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .text_2xl()
                                    .text_color(theme.muted_foreground)
                                    .child("💭"),
                            ),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Select a recommendation"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Click on a vouch in the feed to see details"),
                    ),
            )
    }

    fn render_recommendation_detail(
        rec: &Recommendation,
        is_own: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id("detail-panel")
            .flex()
            .flex_col()
            .h_full()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.background)
            .child(Self::render_subject_header(rec, theme))
            .child(Self::render_scrollable_content(rec, theme))
            .child(Self::render_action_bar(is_own, theme))
    }

    fn render_subject_header(rec: &Recommendation, theme: &Theme) -> Div {
        let timestamp = format_relative_time(
            UNIX_EPOCH + Duration::from_millis(rec.at_ms().max(0) as u64),
            TimeStyle::Verbose,
        );

        div()
            .flex()
            .flex_col()
            .items_center()
            .p_6()
            .pb_4()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(
                div()
                    .w_24()
                    .h_24()
                    .rounded_full()
                    .bg(theme.colors.list)
                    .border_3()
                    .border_color(theme.primary)
                    .flex()
                    .justify_center()
                    .items_center()
                    .shadow_md()
                    .child(div().text_3xl().child("📍")),
            )
            .child(
                div()
                    .mt_4()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .text_center()
                    .child(rec.subject.clone()),
            )
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(timestamp),
            )
    }

    fn render_scrollable_content(rec: &Recommendation, theme: &Theme) -> Stateful<Div> {
        div()
            .id("detail-scroll-content")
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .min_w_0()
            .overflow_y_scroll()
            .child(Self::render_recommendation_content(rec, theme))
    }

    fn render_recommendation_content(rec: &Recommendation, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w_6()
                            .h_6()
                            .rounded_full()
                            .bg(theme.accent)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(div().text_xs().text_color(theme.foreground).child("📝")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Recommendation"),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .p_4()
                    .bg(theme.colors.list)
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .shadow_sm()
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_base()
                            .text_color(theme.foreground)
                            .child(div().size_full().child(rec.body.clone())),
                    ),
            )
    }

    fn render_action_bar(is_own: bool, theme: &Theme) -> Div {
        let not_implemented_border = gpui::red();

        div()
            .flex()
            .flex_row()
            .justify_center()
            .gap_3()
            .p_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .mt_auto()
            .child(Self::render_stub_button(
                "revouch-btn",
                "🔄",
                "Revouch",
                theme,
                not_implemented_border,
            ))
            .child(Self::render_stub_button(
                "disavow-btn",
                "🚫",
                "Disavow",
                theme,
                not_implemented_border,
            ))
            .when(is_own, |this| {
                this.child(Self::render_stub_button(
                    "edit-btn",
                    "✏️",
                    "Edit",
                    theme,
                    not_implemented_border,
                ))
            })
    }

    fn render_stub_button(
        id: &'static str,
        emoji: &str,
        label: &str,
        theme: &Theme,
        border_color: Hsla,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme.colors.list)
            .rounded_lg()
            .cursor_not_allowed()
            .border_2()
            .border_color(border_color)
            .shadow_sm()
            .child(div().text_sm().child(emoji.to_string()))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(label.to_string()),
            )
    }
}

impl RenderOnce for DetailPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        match &self.selected {
            Some(rec) => {
                let is_own = self.local_log_id.is_some() && self.local_log_id == rec.author();
                Self::render_recommendation_detail(rec, is_own, &theme)
            }
            None => Self::render_empty_state(&theme),
        }
    }
}
