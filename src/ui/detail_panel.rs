use crate::data::{MockData, Recommendation, RecommendationId};
use crate::theme::{ActiveTheme, Theme};
use gpui::prelude::FluentBuilder;
use gpui::*;

#[derive(IntoElement)]
pub struct DetailPanel {
    selected_id: Option<RecommendationId>,
    data: MockData,
}

impl DetailPanel {
    pub fn new(data: MockData) -> Self {
        Self {
            selected_id: None,
            data,
        }
    }

    pub fn selected(mut self, id: Option<RecommendationId>) -> Self {
        self.selected_id = id;
        self
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
                    .gap_2()
                    .child(div().text_3xl().text_color(theme.text_muted).child("💭"))
                    .child(
                        div()
                            .text_lg()
                            .text_color(theme.text_muted)
                            .child("Select a recommendation to view details"),
                    ),
            )
    }

    fn render_recommendation_detail(
        recommendation: &Recommendation,
        data: &MockData,
        theme: &Theme,
    ) -> Stateful<Div> {
        let author_name = data.get_contact_name(recommendation.source.original_author);
        let is_own = recommendation.source.original_author == MockData::local_user_id();

        div()
            .id("detail-panel")
            .flex()
            .flex_col()
            .h_full()
            .flex_1()
            .bg(theme.background)
            .child(Self::render_header(recommendation, theme))
            .child(Self::render_content(recommendation, &author_name, theme))
            .child(Self::render_vouch_chain(recommendation, data, theme))
            .child(Self::render_related_section(theme))
            .child(Self::render_action_bar(is_own, theme))
    }

    fn render_header(recommendation: &Recommendation, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .items_center()
            .p_6()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w_20()
                    .h_20()
                    .rounded_full()
                    .bg(theme.surface)
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(div().text_3xl().child("📍")),
            )
            .child(
                div()
                    .mt_3()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.text)
                    .text_center()
                    .child(recommendation.subject_name.clone()),
            )
    }

    fn render_content(
        recommendation: &Recommendation,
        author_name: &str,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id("detail-content")
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .flex_1()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().child("📝"))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child("Original recommendation:"),
                            ),
                    )
                    .child(
                        div().p_3().bg(theme.surface).rounded_md().child(
                            div()
                                .text_sm()
                                .text_color(theme.text)
                                .child(recommendation.content.clone()),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().child("👤"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.text)
                            .child(format!("Vouched by: {}", author_name)),
                    ),
            )
    }

    fn render_vouch_chain(recommendation: &Recommendation, data: &MockData, theme: &Theme) -> Div {
        let revouchers: Vec<String> = recommendation
            .source
            .revouched_by
            .iter()
            .map(|id| {
                if *id == MockData::local_user_id() {
                    "You".to_string()
                } else {
                    data.get_contact_name(*id).to_string()
                }
            })
            .collect();

        if revouchers.is_empty() {
            div()
        } else {
            div().px_4().pb_3().child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().child("🔄"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.text)
                            .child(format!("Revouched by: {}", revouchers.join(", "))),
                    ),
            )
        }
    }

    fn render_related_section(theme: &Theme) -> Div {
        div()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div().flex().flex_row().items_center().gap_2().child(
                    div()
                        .text_sm()
                        .text_color(theme.text_muted)
                        .child("Related records (0)"),
                ),
            )
    }

    fn render_action_bar(is_own: bool, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .p_4()
            .border_t_1()
            .border_color(theme.border)
            .mt_auto()
            .child(
                div()
                    .id("revouch-btn")
                    .px_4()
                    .py_2()
                    .bg(theme.primary)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.primary_hover))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Revouch"),
                    ),
            )
            .child(
                div()
                    .id("disavow-btn")
                    .px_4()
                    .py_2()
                    .bg(theme.surface)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.border))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Disavow"),
                    ),
            )
            .when(is_own, |this| {
                this.child(
                    div()
                        .id("edit-btn")
                        .px_4()
                        .py_2()
                        .bg(theme.accent)
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.accent_hover))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child("Edit"),
                        ),
                )
            })
    }
}

impl RenderOnce for DetailPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<ActiveTheme>();

        match self.selected_id {
            Some(id) => {
                if let Some(recommendation) = self.data.recommendations.iter().find(|r| r.id == id)
                {
                    Self::render_recommendation_detail(recommendation, &self.data, theme)
                } else {
                    Self::render_empty_state(theme)
                }
            }
            None => Self::render_empty_state(theme),
        }
    }
}
