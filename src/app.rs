use crate::debug_feed::DebugFeed;
use crate::feed::Feed;
use crate::follows::Follows;
use crate::ui::debug_panel::DebugPanel;
use crate::ui::detail_panel::DetailPanel;
use crate::ui::feed_panel::FeedPanel;
use crate::ui::modals::{AddFollowModal, WelcomeModal};
use crate::ui::sidebar::Sidebar;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::Root;
use gpui_component::theme::ActiveTheme;
use std::path::PathBuf;
use vouch_core::{ClaimHash, LogId, Peer};

/// Everything main() resolves before the window exists: where the relay
/// is (None = offline instance), where follows persist (None =
/// ephemeral), and any follows injected via env for dev workflows.
#[derive(Clone)]
pub struct Bootstrap {
    pub mailbox_url: Option<String>,
    pub follows_path: Option<PathBuf>,
    pub env_follows: Vec<LogId>,
}

pub struct VouchApp {
    feed: Entity<Feed>,
    feed_panel: Entity<FeedPanel>,
    debug_panel: Entity<DebugPanel>,
    follows: Entity<Follows>,
    peer: Peer,
    local_log_id: Option<LogId>,
    sidebar_collapsed: bool,
    show_debug: bool,
}

impl VouchApp {
    pub fn new(peer: Peer, bootstrap: Bootstrap, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_log_id = peer.id();
        let feed = cx.new(|cx| Feed::new(peer.clone(), cx));

        let feed_panel = cx.new(|cx| FeedPanel::new(feed.clone(), window, cx));

        // The raw-claims debug viewer: its own live read model over the same
        // peer, listing every claim of any type (see `DebugFeed`).
        let debug = cx.new(|cx| DebugFeed::new(peer.clone(), cx));
        let debug_panel = cx.new(|cx| DebugPanel::new(debug, cx));

        let follows = cx.new(|_| {
            Follows::new(
                peer.clone(),
                bootstrap.mailbox_url.clone(),
                bootstrap.follows_path.clone(),
                bootstrap.env_follows.clone(),
            )
        });

        // FeedPanel owns the selection, but VouchApp reads it in render to
        // drive the detail panel, so re-render whenever the feed notifies.
        cx.observe(&feed_panel, |_, _, cx| cx.notify()).detach();
        // The sidebar renders follows and advertised names, so re-render
        // when either changes.
        cx.observe(&follows, |_, _, cx| cx.notify()).detach();
        cx.observe(&feed, |_, _, cx| cx.notify()).detach();

        // First launch: no profile claim under our own log yet means
        // nobody's been asked for a name — open the welcome dialog once
        // the check lands.
        let window_handle = window.window_handle();
        cx.spawn({
            let peer = peer.clone();
            async move |_this, cx| {
                let Some(me) = local_log_id else { return };
                let has_profile = peer
                    .query(move |db| {
                        db.claims()
                            .by_type("profile")
                            .iter()
                            .any(|c| c.header.log_id == me)
                    })
                    .await
                    .unwrap_or(true);
                if !has_profile {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        WelcomeModal::open(peer.clone(), window, cx);
                    });
                }
            }
        })
        .detach();

        Self {
            feed,
            feed_panel,
            debug_panel,
            follows,
            peer,
            local_log_id,
            sidebar_collapsed: false,
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
        let names = self.feed.read(cx).names().clone();
        let own_address = self.local_log_id.map(|l| l.to_string());
        let own_name = self.local_log_id.and_then(|l| names.get(&l).cloned());
        let followed: Vec<(LogId, Option<String>)> = self
            .follows
            .read(cx)
            .list()
            .iter()
            .map(|log| (*log, names.get(log).cloned()))
            .collect();
        let follows_entity = self.follows.clone();
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
                    .identity(own_name, own_address)
                    .follows(followed)
                    .on_add_follow(move |_, window, cx| {
                        AddFollowModal::open(follows_entity.clone(), window, cx);
                    })
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
                        self.peer.clone(),
                        names,
                    ))
                }
            })
            .when_some(Root::render_dialog_layer(window, cx), |this, layer| {
                this.child(layer)
            })
    }
}
