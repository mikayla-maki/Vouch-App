use gpui::*;
use gpui_component::Root;
use gpui_component::theme::{Theme, ThemeMode, ThemeSet};
use vouch_core::sync::{InstanceId, MemorySyncState, SyncState};
use vouch_core::{Database, Peer, PeerActor, ServePolicy, Writer};

mod app;
mod assets;
mod feed;
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

/// The normal path opens a durable `Peer` in the OS app-support directory.
/// `VOUCH_EPHEMERAL=1` instead builds one entirely in memory — an
/// identity, a database, and cursors that live only for this process —
/// for running throwaway instances side by side (demos, sync testing)
/// without leaving files behind or colliding with a real install.
fn build_peer() -> (Peer, PeerActor) {
    if env_flag("VOUCH_EPHEMERAL") {
        let writer = Writer::generate().expect("generate an ephemeral identity");
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
        Peer::new(
            db,
            state,
            InstanceId(instance_bytes),
            Some(writer),
            ServePolicy::Owned,
            clock,
        )
    } else {
        let dir = identity::app_dir();
        let writer = identity::load_or_create_writer(&dir);
        vouch_store::open_peer(&dir, Some(writer), ServePolicy::Owned).expect("open local database")
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

    let (peer, actor) = build_peer();
    let title = match env_var("VOUCH_NAME") {
        Some(name) => format!("Vouch — {name}"),
        None => "Vouch".to_string(),
    };

    gpui_platform::application().with_assets(Assets).run(move |cx| {
        gpui_component::init(cx);

        load_vouch_theme(cx);

        cx.background_executor().spawn(actor.run()).detach();

        // A relay/direct connection is opt-in and dev-only for now: no
        // discovery or UI to drive it yet, just env vars so two instances
        // (or a headless vouch-node) can be pointed at each other.
        if let Some(relay_addr) = env_var("VOUCH_RELAY_ADDR") {
            let auto_follow = env_flag("VOUCH_AUTO_FOLLOW");
            let peer_for_relay = peer.clone();
            std::thread::spawn(move || {
                match vouch_transport::connect_relay(&peer_for_relay, &relay_addr, auto_follow) {
                    Ok((remote, _)) => eprintln!("connected to relay; remote log id: {remote}"),
                    Err(e) => eprintln!("failed to connect to relay: {e}"),
                }
            });
        }

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
                let view = cx.new(|cx| VouchApp::new(peer.clone(), window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .unwrap();

        cx.activate(true);
    });
}
