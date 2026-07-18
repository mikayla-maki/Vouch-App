use crate::feed::Feed;
use crate::ui::modals::NewRecommendationModal;
use crate::ui::record_card::RecordCard;
use crate::ui::search_bar::{SearchBar, SearchBarEvent};

use gpui::*;
use gpui_component::theme::{ActiveTheme, Theme};
use gpui_component::{Icon, IconName};
use vouch_core::e2ee::ContentKey;
use vouch_core::{ClaimHash, LogId, Recommendation};

pub struct FeedPanel {
    feed: Entity<Feed>,
    /// Our content key, for sealing what the New Vouch dialog authors.
    key: ContentKey,
    local_log_id: Option<LogId>,
    selected_hash: Option<ClaimHash>,
    search_bar: Entity<SearchBar>,
    search_query: SharedString,
    /// Cached list of matching claim hashes, filtered by search query.
    /// Recomputed when the query changes or the feed reports new claims.
    filtered_hashes: Vec<ClaimHash>,
}

impl FeedPanel {
    pub fn new(
        feed: Entity<Feed>,
        key: ContentKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_bar = cx.new(|cx| SearchBar::new(window, cx));

        cx.subscribe(
            &search_bar,
            |this, _search_bar, event: &SearchBarEvent, cx| {
                let SearchBarEvent::QueryChanged(query) = event;
                this.search_query = query.clone();
                this.recompute_filtered_hashes(cx);
                cx.notify();
            },
        )
        .detach();

        cx.observe(&feed, |this, _feed, cx| {
            this.recompute_filtered_hashes(cx);
            cx.notify();
        })
        .detach();

        let local_log_id = feed.read(cx).peer().id();
        let filtered_hashes = Self::compute_filtered_hashes(feed.read(cx).recs(), "");

        Self {
            feed,
            key,
            local_log_id,
            selected_hash: None,
            search_bar,
            search_query: SharedString::default(),
            filtered_hashes,
        }
    }

    pub fn selected_hash(&self) -> Option<ClaimHash> {
        self.selected_hash
    }

    fn compute_filtered_hashes(recs: &[Recommendation], query: &str) -> Vec<ClaimHash> {
        let query = query.to_lowercase();
        recs.iter()
            .filter(|rec| {
                query.is_empty()
                    || rec.subject.to_lowercase().contains(&query)
                    || rec.body.to_lowercase().contains(&query)
            })
            .map(|rec| rec.id)
            .collect()
    }

    fn recompute_filtered_hashes(&mut self, cx: &mut Context<Self>) {
        self.filtered_hashes =
            Self::compute_filtered_hashes(self.feed.read(cx).recs(), &self.search_query);
    }

    fn render_feed_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut cards: Vec<Stateful<Div>> = Vec::new();
        let feed = self.feed.read(cx);
        let recs = feed.recs();
        let names = feed.names();

        for hash in &self.filtered_hashes {
            let Some(rec) = recs.iter().find(|r| r.id == *hash) else {
                continue;
            };

            let is_selected = self.selected_hash == Some(rec.id);
            let hash = rec.id;

            let card = div()
                .id(ElementId::Name(format!("feed-item-{}", hash).into()))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.selected_hash = Some(hash);
                    cx.notify();
                }))
                .child(RecordCard::render_card(
                    rec,
                    is_selected,
                    self.local_log_id,
                    names,
                    cx.theme(),
                ));

            cards.push(card);
        }

        let theme = cx.theme();

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
                        .text_color(theme.muted_foreground)
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

    // NOTE: gpui-component 0.5.x's `Button` would be the natural widget here,
    // but its released hover style paints the label `red_400()` (upstream bug
    // in button.rs), so this stays hand-rolled until that is fixed.
    fn render_new_vouch_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let peer = self.feed.read(cx).peer().clone();
        let key = self.key;

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
                        NewRecommendationModal::open(peer.clone(), key, window, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_center()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(IconName::Plus)
                                    .size_4()
                                    .text_color(theme.primary_foreground),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.primary_foreground)
                                    .child("New Vouch"),
                            ),
                    ),
            )
    }
}

impl Render for FeedPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

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
