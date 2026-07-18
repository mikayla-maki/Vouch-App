use crate::feed::Feed;
use crate::ui::detail_panel::DetailPanel;
use crate::ui::feed_panel::FeedPanel;
use crate::ui::sidebar::Sidebar;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::Root;
use gpui_component::theme::ActiveTheme;
use vouch_core::{ClaimHash, LogId, Peer};

pub struct VouchApp {
    feed: Entity<Feed>,
    feed_panel: Entity<FeedPanel>,
    local_log_id: Option<LogId>,
    sidebar_collapsed: bool,
}

impl VouchApp {
    pub fn new(peer: Peer, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_log_id = peer.id();
        let feed = cx.new(|cx| Feed::new(peer, cx));

        let feed_panel = cx.new(|cx| FeedPanel::new(feed.clone(), window, cx));

        // FeedPanel owns the selection, but VouchApp reads it in render to
        // drive the detail panel, so re-render whenever the feed notifies.
        cx.observe(&feed_panel, |_, _, cx| cx.notify()).detach();

        Self {
            feed,
            feed_panel,
            local_log_id,
            sidebar_collapsed: true,
        }
    }

    fn selected_hash(&self, cx: &App) -> Option<ClaimHash> {
        self.feed_panel.read(cx).selected_hash()
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }
}

impl Render for VouchApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_hash(cx).and_then(|hash| {
            self.feed
                .read(cx)
                .recs()
                .iter()
                .find(|r| r.id == hash)
                .cloned()
        });
        let theme = cx.theme();

        div()
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(theme.background)
            .child(
                Sidebar::new()
                    .collapsed(self.sidebar_collapsed)
                    .on_toggle(cx.listener(|this, _, _window, cx| {
                        this.toggle_sidebar(cx);
                    })),
            )
            .child(self.feed_panel.clone())
            .child(DetailPanel::new(selected, self.local_log_id))
            .when_some(Root::render_dialog_layer(window, cx), |this, layer| {
                this.child(layer)
            })
    }
}
