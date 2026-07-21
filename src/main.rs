use gpui::*;
use gpui_component::Root;
use gpui_component::theme::{Theme, ThemeMode, ThemeSet};
use vouch_core::sync::{InstanceId, MemorySyncState, SyncState};
use vouch_core::{Database, Peer, PeerActor, ServePolicy, Writer};

mod app;
mod assets;
mod auto_update;
mod debug_feed;
mod feed;
mod follows;
mod identity;
mod theme;
mod ui;

use app::VouchApp;
use assets::Assets;

actions!(vouch, [Quit]);

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn env_flag(name: &str) -> bool {
    matches!(env_var(name).as_deref(), Some("1") | Some("true"))
}

/// The relay every durable install talks to unless told otherwise: the
/// hosted mailbox server. Ephemeral (demo) instances never default to
/// it — they'd pollute production with throwaway identities.
const DEFAULT_MAILBOX_URL: &str = "wss://vouch-app.online";

/// The normal path opens a durable `Peer` in the OS app-support directory
/// (returned so follows can persist next to identity.key).
/// `VOUCH_EPHEMERAL=1` instead builds one entirely in memory — an
/// identity, a database, and cursors that live only for this process —
/// for running throwaway instances side by side (demos, sync testing)
/// without leaving files behind or colliding with a real install.
///
/// Both paths return the crypto [`Identity`](vouch_core::e2ee::Identity)
/// built from the same seed the writer signs with: all user content is
/// sealed with it, always.
fn build_peer() -> (
    Peer,
    PeerActor,
    Option<std::path::PathBuf>,
    vouch_core::e2ee::Identity,
) {
    if env_flag("VOUCH_EPHEMERAL") {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("OS randomness for an ephemeral identity");
        let writer = Writer::from_seed(seed);
        let identity = vouch_core::e2ee::Identity::from_seed(seed);
        let db = Database::new();
        let state: Box<dyn SyncState> = Box::new(MemorySyncState::new());
        let mut instance_bytes = [0u8; 16];
        getrandom::fill(&mut instance_bytes).expect("OS randomness for an instance id");
        let clock = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        };
        let (peer, actor) = Peer::new(
            db,
            state,
            InstanceId(instance_bytes),
            Some(writer),
            ServePolicy::Owned,
            clock,
        );
        (peer, actor, None, identity)
    } else {
        let dir = identity::app_dir();
        let (writer, identity) = identity::load_or_create(&dir);
        let (peer, actor) = vouch_store::open_peer(&dir, Some(writer), ServePolicy::Owned)
            .expect("open local database");
        (peer, actor, Some(dir), identity)
    }
}

fn load_vouch_theme(cx: &mut App) {
    let theme_json = include_str!("../themes/vouch.json");
    match serde_json::from_str::<ThemeSet>(theme_json) {
        Ok(theme_set) => {
            for theme_config in theme_set.themes {
                let mode = theme_config.mode;
                let rc_config = std::rc::Rc::new(theme_config);

                if mode == ThemeMode::Light {
                    Theme::global_mut(cx).light_theme = rc_config.clone();
                } else {
                    Theme::global_mut(cx).dark_theme = rc_config.clone();
                }
            }
            Theme::change(ThemeMode::Light, None, cx);
        }
        Err(e) => {
            eprintln!("Failed to parse Vouch theme: {}", e);
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let (peer, actor, data_dir, crypto_identity) = build_peer();
    // Self-update only makes sense for real installs; the module also
    // no-ops for dev builds and non-.app binaries on its own.
    if data_dir.is_some() {
        auto_update::spawn();
    }
    let title = match env_var("VOUCH_NAME") {
        Some(name) => format!("Vouch — {name}"),
        None => "Vouch".to_string(),
    };

    gpui_platform::application().with_assets(Assets).run(move |cx| {
        gpui_component::init(cx);

        load_vouch_theme(cx);

        cx.background_executor().spawn(actor.run()).detach();

        // The mailbox relay: durable installs default to the hosted one,
        // ephemeral (demo) instances only connect when told to. Your own
        // mailbox is how you publish; follows connect to friends' — the
        // stored list plus any VOUCH_FOLLOW extras, all handled by the
        // Follows entity inside the app.
        let mailbox_url = env_var("VOUCH_MAILBOX_URL")
            .or_else(|| data_dir.is_some().then(|| DEFAULT_MAILBOX_URL.to_string()));
        if let Some(url) = &mailbox_url {
            let my_log = peer.id().expect("the app peer always holds a writer");
            // The full capability address: what a friend pastes to
            // follow AND read this instance.
            eprintln!("my address: {}", crypto_identity.address());
            vouch_transport::connect_mailbox(&peer, url, my_log, Some(crypto_identity.clone()));
        }
        let env_follows: Vec<vouch_core::e2ee::Address> = env_var("VOUCH_FOLLOW")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|s| {
                let parsed = vouch_core::e2ee::Address::parse(s);
                if parsed.is_none() {
                    eprintln!("VOUCH_FOLLOW entry is not a vouch: address: {s}");
                }
                parsed
            })
            .collect();
        let bootstrap = app::Bootstrap {
            identity: crypto_identity.clone(),
            mailbox_url,
            follows_path: data_dir.as_ref().map(|d| d.join("follows.json")),
            env_follows,
        };

        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let window_size = match (env_var("VOUCH_WINDOW_WIDTH"), env_var("VOUCH_WINDOW_HEIGHT")) {
            (Some(w), Some(h)) => size(
                px(w.parse().unwrap_or(1200.0)),
                px(h.parse().unwrap_or(800.0)),
            ),
            _ => size(px(1200.0), px(800.0)),
        };
        let bounds = match (env_var("VOUCH_WINDOW_X"), env_var("VOUCH_WINDOW_Y")) {
            (Some(x), Some(y)) => Bounds {
                origin: point(
                    px(x.parse().unwrap_or(0.0)),
                    px(y.parse().unwrap_or(0.0)),
                ),
                size: window_size,
            },
            _ => Bounds::centered(None, window_size, cx),
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| VouchApp::new(peer.clone(), bootstrap.clone(), window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .unwrap();

        cx.activate(true);
    });
}
