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

/// Everything main() resolves before the window exists: the crypto
/// identity every claim seals with, where the relay is (None = offline
/// instance), where follows persist (None = ephemeral), and any follows
/// injected via env for dev workflows.
#[derive(Clone)]
pub struct Bootstrap {
    pub identity: vouch_core::e2ee::Identity,
    pub mailbox_url: Option<String>,
    pub follows_path: Option<PathBuf>,
    pub env_follows: Vec<vouch_core::e2ee::Address>,
}

pub struct VouchApp {
    feed: Entity<Feed>,
    feed_panel: Entity<FeedPanel>,
    debug_panel: Entity<DebugPanel>,
    follows: Entity<Follows>,
    peer: Peer,
    identity: vouch_core::e2ee::Identity,
    local_log_id: Option<LogId>,
    sidebar_collapsed: bool,
    show_debug: bool,
    /// A nightly is downloaded, validated, and waiting; the sidebar shows
    /// "restart to update" and nothing happens until it's clicked.
    update_ready: bool,
    /// Where the last update check stands — drives the "check for
    /// updates" row's label.
    update_check: crate::auto_update::CheckState,
}

impl VouchApp {
    pub fn new(peer: Peer, bootstrap: Bootstrap, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_log_id = peer.id();
        let identity = bootstrap.identity.clone();

        // Follows before feed: the followed addresses are the feed's
        // keyring, so the feed observes them.
        let follows = cx.new(|_| {
            Follows::new(
                peer.clone(),
                bootstrap.mailbox_url.clone(),
                bootstrap.follows_path.clone(),
                bootstrap.env_follows.clone(),
            )
        });
        let feed = cx.new(|cx| Feed::new(peer.clone(), identity.clone(), follows.clone(), cx));

        let feed_panel =
            cx.new(|cx| FeedPanel::new(feed.clone(), identity.content_key(), window, cx));

        // The raw-claims debug viewer: its own live read model over the same
        // peer, listing every claim of any type (see `DebugFeed`).
        let debug = cx.new(|cx| DebugFeed::new(peer.clone(), cx));
        let debug_panel = cx.new(|cx| DebugPanel::new(debug, cx));

        // FeedPanel owns the selection, but VouchApp reads it in render to
        // drive the detail panel, so re-render whenever the feed notifies.
        cx.observe(&feed_panel, |_, _, cx| cx.notify()).detach();
        // The sidebar renders follows and advertised names, so re-render
        // when either changes.
        cx.observe(&follows, |_, _, cx| cx.notify()).detach();
        cx.observe(&feed, |_, _, cx| cx.notify()).detach();

        // First launch: no (decryptable) profile claim under our own log
        // yet means nobody's been asked for a name — open the welcome
        // dialog once the check lands. Profiles are sealed like all
        // speech, so the check runs through the decrypted view.
        let window_handle = window.window_handle();
        cx.spawn({
            let peer = peer.clone();
            let identity = identity.clone();
            async move |_this, cx| {
                let Some(me) = local_log_id else { return };
                let check_identity = identity.clone();
                let has_profile = peer
                    .query(move |db| {
                        // Only our own key matters here — the question is
                        // whether WE have named ourselves.
                        let keys = vouch_core::e2ee::keys_for(&check_identity, &[]);
                        let view = vouch_core::e2ee::decrypted_view(db.claims(), &keys);
                        vouch_core::profile::names(&view).contains_key(&me)
                    })
                    .await
                    .unwrap_or(true);
                if !has_profile {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        WelcomeModal::open(peer.clone(), identity.clone(), window, cx);
                    });
                }
            }
        })
        .detach();

        // The updater thread stages downloads in the background; this
        // just watches for "ready" flipping so the sidebar button can
        // appear. Polling a static is the whole protocol — the updater
        // has no handle to the UI, on purpose.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                let ready = crate::auto_update::ready().is_some();
                let check = crate::auto_update::check_state();
                let Ok(()) = this.update(cx, |app: &mut VouchApp, cx| {
                    if app.update_ready != ready || app.update_check != check {
                        app.update_ready = ready;
                        app.update_check = check;
                        cx.notify();
                    }
                }) else {
                    return;
                };
            }
        })
        .detach();

        Self {
            feed,
            feed_panel,
            debug_panel,
            follows,
            peer,
            identity,
            local_log_id,
            sidebar_collapsed: false,
            show_debug: false,
            update_ready: false,
            update_check: crate::auto_update::CheckState::Idle,
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

    /// Dev-only escape hatch for eyeballing the update UI, which real
    /// logic hides outside bundled installs: `VOUCH_UPDATE_UI_PREVIEW=check`
    /// forces the check row, `=ready` forces the restart button. The
    /// buttons are inert in preview (there's genuinely nothing to check
    /// or restart into) — this exists to see them, not to use them.
    fn update_ui_preview() -> Option<String> {
        std::env::var("VOUCH_UPDATE_UI_PREVIEW").ok()
    }

    fn show_update_ready(&self) -> bool {
        self.update_ready || Self::update_ui_preview().as_deref() == Some("ready")
    }

    /// The "check for updates" row label, or `None` to hide the row: dev
    /// builds have no updater, and a staged update replaces the row with
    /// the restart button.
    fn check_update_label(&self) -> Option<SharedString> {
        use crate::auto_update::CheckState;
        if self.show_update_ready() {
            return None;
        }
        if !crate::auto_update::active() && Self::update_ui_preview().is_none() {
            return None;
        }
        Some(match self.update_check {
            CheckState::Idle => "Check for updates".into(),
            CheckState::Checking => "Checking…".into(),
            CheckState::UpToDate => "Up to date ✓".into(),
            CheckState::Failed => "Check failed — retry".into(),
        })
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
        // The full capability address (log + read key) — the string a
        // friend pastes to follow AND read you.
        let own_address = self
            .local_log_id
            .map(|_| self.identity.address().to_string());
        let own_name = self.local_log_id.and_then(|l| names.get(&l).cloned());
        let followed: Vec<(LogId, Option<String>)> = self
            .follows
            .read(cx)
            .list()
            .iter()
            .map(|address| (address.log, names.get(&address.log).cloned()))
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
                    .update_ready(self.show_update_ready())
                    .check_update(self.check_update_label())
                    .on_check_update(cx.listener(|this, _, _window, cx| {
                        // Optimistic: show "Checking…" now; the poller
                        // syncs the real outcome as it lands.
                        this.update_check = crate::auto_update::CheckState::Checking;
                        crate::auto_update::check_now();
                        cx.notify();
                    }))
                    .on_restart_update(|_, _window, cx| {
                        // Swap the staged bundle, then quit; a detached
                        // helper relaunches the new build once we're gone.
                        match crate::auto_update::restart_into_update() {
                            Ok(()) => cx.quit(),
                            Err(e) => eprintln!("auto-update: restart failed: {e}"),
                        }
                    })
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
                        self.identity.clone(),
                        names,
                    ))
                }
            })
            .when_some(Root::render_dialog_layer(window, cx), |this, layer| {
                this.child(layer)
            })
    }
}
