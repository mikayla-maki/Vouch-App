use crate::debug_feed::DebugFeed;
use crate::feed::Feed;
use crate::ui::debug_panel::DebugPanel;
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
    debug_panel: Entity<DebugPanel>,
    local_log_id: Option<LogId>,
    sidebar_collapsed: bool,
    show_debug: bool,
}

impl VouchApp {
    pub fn new(peer: Peer, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_log_id = peer.id();
        let feed = cx.new(|cx| Feed::new(peer.clone(), cx));

        let feed_panel = cx.new(|cx| FeedPanel::new(feed.clone(), window, cx));

        // The raw-claims debug viewer: its own live read model over the same
        // peer, listing every claim of any type (see `DebugFeed`).
        let debug = cx.new(|cx| DebugFeed::new(peer, cx));
        let debug_panel = cx.new(|cx| DebugPanel::new(debug, cx));

        // FeedPanel owns the selection, but VouchApp reads it in render to
        // drive the detail panel, so re-render whenever the feed notifies.
        cx.observe(&feed_panel, |_, _, cx| cx.notify()).detach();

        Self {
            feed,
            feed_panel,
            debug_panel,
            local_log_id,
            sidebar_collapsed: true,
            show_debug: false,
        }
    }

    fn toggle_debug(&mut self, cx: &mut Context<Self>) {
        self.show_debug = !self.show_debug;
        cx.notify();
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
                    .debug_active(self.show_debug)
                    .on_toggle(cx.listener(|this, _, _window, cx| {
                        this.toggle_sidebar(cx);
                    }))
                    .on_debug(cx.listener(|this, _, _window, cx| {
                        this.toggle_debug(cx);
                    })),
            )
            .map(|this| {
                if self.show_debug {
                    // The debug viewer replaces the feed + detail area.
                    this.child(self.debug_panel.clone())
                } else {
                    this.child(self.feed_panel.clone()).child(DetailPanel::new(
                        selected,
                        self.local_log_id,
                        self.feed.read(cx).peer().clone(),
                    ))
                }
            })
            .when_some(Root::render_dialog_layer(window, cx), |this, layer| {
                this.child(layer)
            })
    }
}
