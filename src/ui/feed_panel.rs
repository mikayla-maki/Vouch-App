use crate::data::{MockData, RecommendationId};
use crate::theme::{ActiveTheme, Theme};
use crate::ui::modals::NewRecommendationModal;
use crate::ui::record_card::RecordCard;
use crate::ui::search_bar::{SearchBar, SearchBarEvent};

use gpui::*;

// TODO: When recommendations can change (add/edit/delete), we need to invalidate
// the cached filtered IDs. Options to consider:
// - Add a version/generation number to MockData that increments on changes
// - Use an event subscription pattern to listen for data changes
// - Store a hash of recommendation IDs to detect changes cheaply
// Comparing the full Vec<Recommendation> is expensive and should be avoided.

pub struct FeedPanel {
    data: MockData,
    selected_id: Option<RecommendationId>,
    search_bar: Entity<SearchBar>,
    search_query: SharedString,
    /// Cached list of recommendation IDs, filtered and sorted.
    /// Recomputed when search_query changes.
    filtered_ids: Vec<RecommendationId>,
}

impl FeedPanel {
    pub fn new(data: MockData, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_bar = cx.new(|cx| SearchBar::new(window, cx));

        cx.subscribe(
            &search_bar,
            |this, search_bar, _event: &SearchBarEvent, cx| {
                this.search_query = search_bar.read(cx).query().clone();
                this.recompute_filtered_ids();
                cx.notify();
            },
        )
        .detach();

        let filtered_ids = Self::compute_filtered_ids(&data, "");

        Self {
            data,
            selected_id: None,
            search_bar,
            search_query: SharedString::default(),
            filtered_ids,
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

    fn compute_filtered_ids(data: &MockData, query: &str) -> Vec<RecommendationId> {
        let query = query.to_lowercase();

        let mut results: Vec<&crate::data::Recommendation> = if query.is_empty() {
            data.recommendations.iter().collect()
        } else {
            data.recommendations
                .iter()
                .filter(|rec| {
                    let subject_match = rec.subject_name.to_lowercase().contains(&query);
                    let content_match = rec.content.to_lowercase().contains(&query);
                    let author_name = data.get_contact_name(rec.source.original_author);
                    let author_match = author_name.to_lowercase().contains(&query);

                    subject_match || content_match || author_match
                })
                .collect()
        };

        // Sort by timestamp, newest first
        results.sort_by(|a, b| b.source.timestamp.cmp(&a.source.timestamp));

        results.into_iter().map(|rec| rec.id).collect()
    }

    fn recompute_filtered_ids(&mut self) {
        self.filtered_ids = Self::compute_filtered_ids(&self.data, &self.search_query);
    }

    fn render_feed_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut cards: Vec<Stateful<Div>> = Vec::new();

        for recommendation_id in &self.filtered_ids {
            let recommendation = self
                .data
                .recommendations
                .iter()
                .find(|r| r.id == *recommendation_id);

            let Some(recommendation) = recommendation else {
                continue;
            };

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

        let theme = cx.global::<ActiveTheme>().clone();

        if cards.is_empty() {
            div()
                .id("feed-list")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .flex_1()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.text_muted)
                        .child("No recommendations found"),
                )
        } else {
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
    }

    fn render_new_vouch_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let data = self.data.clone();

        div()
            .w_full()
            .p_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("new-vouch-button")
                    .w_full()
                    .px_3()
                    .py_2()
                    .bg(theme.primary)
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.primary_hover))
                    .on_click(cx.listener(move |_this, _event, window, cx| {
                        NewRecommendationModal::open(data.clone(), window, cx);
                    }))
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
            .child(self.search_bar.clone())
            .child(self.render_feed_list(cx))
            .child(self.render_new_vouch_button(&theme, cx))
    }
}
