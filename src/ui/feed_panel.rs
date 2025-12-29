use crate::data::{MockData, RecommendationId};
use crate::theme::{ActiveTheme, Theme};
use crate::ui::record_card::RecordCard;

use gpui::*;

pub struct FeedPanel {
    data: MockData,
    selected_id: Option<RecommendationId>,
}

impl FeedPanel {
    pub fn new(data: MockData) -> Self {
        Self {
            data,
            selected_id: None,
        }
    }

    pub fn select(&mut self, id: Option<RecommendationId>) {
        self.selected_id = id;
    }

    pub fn selected_id(&self) -> Option<RecommendationId> {
        self.selected_id
    }

    pub fn data(&self) -> &MockData {
        &self.data
    }

    fn render_search_bar(&self, theme: &Theme) -> impl IntoElement {
        div().w_full().p_2().child(
            div()
                .w_full()
                .px_3()
                .py_2()
                .bg(theme.card)
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().text_color(theme.text_muted).child("🔍"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.text_muted)
                                .child("Search recommendations..."),
                        ),
                ),
        )
    }

    fn render_feed_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut cards: Vec<Stateful<Div>> = Vec::new();

        for recommendation in &self.data.recommendations {
            let is_selected = self.selected_id == Some(recommendation.id);
            let recommendation_id = recommendation.id;

            let card = div()
                .id(ElementId::Name(
                    format!("feed-item-{}", recommendation.id.0).into(),
                ))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.selected_id = Some(recommendation_id);
                    cx.notify();
                }))
                .child(RecordCard::render_card(
                    recommendation,
                    is_selected,
                    &self.data,
                    cx.global::<ActiveTheme>(),
                ));

            cards.push(card);
        }

        div()
            .id("feed-list")
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .overflow_y_scroll()
            .flex_1()
            .children(cards)
    }

    fn render_new_vouch_button(&self, theme: &Theme) -> impl IntoElement {
        div()
            .w_full()
            .p_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .bg(theme.primary)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.primary_hover))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_center()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().text_color(theme.text).child("+"))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child("New Vouch"),
                            ),
                    ),
            )
    }
}

impl Render for FeedPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<ActiveTheme>().clone();

        div()
            .flex()
            .flex_col()
            .h_full()
            .w_72()
            .min_w_72()
            .bg(theme.background)
            .border_r_1()
            .border_color(theme.border)
            .child(self.render_search_bar(&theme))
            .child(self.render_feed_list(cx))
            .child(self.render_new_vouch_button(&theme))
    }
}
