use crate::ui::format::{TimeStyle, format_relative_time};
use crate::ui::modals::EditRecommendationModal;
use crate::ui::modals::edit_recommendation::dominating_refs;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputState};
use gpui_component::theme::{ActiveTheme, Theme};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vouch_core::{Comment, Draft, LogId, Peer, Recommendation};

// Vouch-chain and related-vouches sections belong here once `vouch` claims
// and `rec.about -> entity` linking exist end to end: real relatedness is
// `ClaimStore::backlinks` + `by_type("vouch"/"rec")`, not string-matching a
// subject name.

#[derive(IntoElement)]
pub struct DetailPanel {
    selected: Option<Recommendation>,
    local_log_id: Option<LogId>,
    peer: Peer,
}

impl DetailPanel {
    pub fn new(selected: Option<Recommendation>, local_log_id: Option<LogId>, peer: Peer) -> Self {
        Self {
            selected,
            local_log_id,
            peer,
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
        local_log_id: Option<LogId>,
        peer: &Peer,
        window: &mut Window,
        cx: &mut App,
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
            .child(Self::render_scrollable_content(
                rec,
                local_log_id,
                peer,
                window,
                cx,
                theme,
            ))
            .child(Self::render_action_bar(rec, is_own, peer, theme))
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
            .when_some(Self::conflict_badge(rec, "subject", theme), |this, badge| {
                this.child(badge)
            })
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(timestamp),
            )
    }

    /// A small, visible marker when a field has an unreconciled frontier
    /// (more than one contribution) — the same author's concurrent offline
    /// edits, most often. Deliberately a plain notice, not a resolution UI:
    /// the detail panel shows only `.current()` above it, so this is what
    /// keeps that from silently hiding a live conflict.
    fn conflict_badge(rec: &Recommendation, field: &str, theme: &Theme) -> Option<Div> {
        let count = rec.fields.get(field).map(|f| f.frontier.len()).unwrap_or(0);
        if count <= 1 {
            return None;
        }
        Some(
            div()
                .mt_2()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.warning.opacity(0.15))
                .border_1()
                .border_color(theme.warning)
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.warning)
                .child(format!("⚠ {} unresolved versions", count)),
        )
    }

    fn render_scrollable_content(
        rec: &Recommendation,
        local_log_id: Option<LogId>,
        peer: &Peer,
        window: &mut Window,
        cx: &mut App,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id("detail-scroll-content")
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .min_w_0()
            .overflow_y_scroll()
            .child(Self::render_recommendation_content(rec, theme))
            .child(Self::render_comments_section(
                rec,
                local_log_id,
                peer,
                window,
                cx,
                theme,
            ))
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
                    )
                    .when_some(Self::conflict_badge(rec, "body", theme), |this, badge| {
                        this.child(div().mt_2().child(badge))
                    }),
            )
    }

    fn render_comments_section(
        rec: &Recommendation,
        local_log_id: Option<LogId>,
        peer: &Peer,
        window: &mut Window,
        cx: &mut App,
        theme: &Theme,
    ) -> Div {
        // Keyed on the recommendation id so switching selections gives a
        // fresh input rather than carrying a half-typed comment across.
        let comment_state = window.use_keyed_state(
            ElementId::Name(format!("detail-comment-input-{}", rec.id).into()),
            cx,
            |window, cx| {
                InputState::new(window, cx)
                    .placeholder("Add a comment…")
                    .auto_grow(1, 4)
            },
        );

        let mut comments = rec.comments.clone();
        comments.sort_by_key(|c| c.at);

        let mut items: Vec<Div> = Vec::new();
        for comment in &comments {
            items.push(Self::render_comment(comment, local_log_id, theme));
        }

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .px_4()
            .py_3()
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
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
                            .bg(theme.secondary)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(div().text_xs().text_color(theme.foreground).child("💬")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("Comments ({})", comments.len())),
                    ),
            )
            .when(items.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No comments yet — be the first."),
                )
            })
            .when(!items.is_empty(), |this| {
                this.child(div().flex().flex_col().gap_2().children(items))
            })
            .child(Self::render_comment_composer(rec, peer, &comment_state, theme))
    }

    fn render_comment(comment: &Comment, local_log_id: Option<LogId>, theme: &Theme) -> Div {
        let author = if local_log_id == Some(comment.author) {
            "you".to_string()
        } else {
            comment.author.short()
        };
        let when = format_relative_time(
            UNIX_EPOCH + Duration::from_millis(comment.at.max(0) as u64),
            TimeStyle::Compact,
        );

        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .bg(theme.colors.list)
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(author),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(when),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(comment.text.clone()),
            )
    }

    fn render_comment_composer(
        rec: &Recommendation,
        peer: &Peer,
        comment_state: &Entity<InputState>,
        theme: &Theme,
    ) -> Div {
        let peer = peer.clone();
        let of_value = dominating_refs(rec);
        let comment_state = comment_state.clone();

        div()
            .flex()
            .flex_row()
            .items_end()
            .gap_2()
            .w_full()
            .child(div().flex_1().min_w_0().child(Input::new(&comment_state)))
            .child(
                div()
                    .id("submit-comment-btn")
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .bg(theme.primary)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.primary_hover))
                    .on_click(move |_event, window, cx| {
                        let text = comment_state.read(cx).value().to_string();
                        if text.trim().is_empty() {
                            return;
                        }

                        let peer = peer.clone();
                        let of_value = of_value.clone();
                        let at_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);

                        // Comments are open to anyone (no source-author gate)
                        // and never touch `fields`; fire-and-forget, same as
                        // authoring any other claim.
                        cx.spawn(async move |_cx| {
                            let draft = Draft::new("comment")
                                .at(at_ms)
                                .field("of", of_value)
                                .text("text", text);
                            let _ = peer.claim(draft).await;
                        })
                        .detach();

                        comment_state.update(cx, |state, cx| {
                            state.set_value("", window, cx);
                        });
                    })
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.primary_foreground)
                            .child("Comment"),
                    ),
            )
    }

    fn render_action_bar(rec: &Recommendation, is_own: bool, peer: &Peer, theme: &Theme) -> Div {
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
                this.child(Self::render_edit_button(rec, peer, theme))
            })
    }

    fn render_edit_button(rec: &Recommendation, peer: &Peer, theme: &Theme) -> Stateful<Div> {
        let peer = peer.clone();
        let rec = rec.clone();

        div()
            .id("edit-btn")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme.colors.list)
            .rounded_lg()
            .cursor_pointer()
            .border_1()
            .border_color(theme.border)
            .shadow_sm()
            .hover(|style| style.bg(theme.list_hover))
            .on_click(move |_event, window, cx| {
                EditRecommendationModal::open(peer.clone(), rec.clone(), window, cx);
            })
            .child(div().text_sm().child("✏️"))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Edit"),
            )
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        match &self.selected {
            Some(rec) => {
                let is_own = self.local_log_id.is_some() && self.local_log_id == rec.author();
                Self::render_recommendation_detail(
                    rec,
                    is_own,
                    self.local_log_id,
                    &self.peer,
                    window,
                    cx,
                    &theme,
                )
            }
            None => Self::render_empty_state(&theme),
        }
    }
}
