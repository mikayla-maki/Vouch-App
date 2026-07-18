//! First launch: pick the name your log will suggest for itself, and see
//! the address you hand to friends.
//!
//! "Sign up" in Vouch is purely local — the keypair already exists by the
//! time this opens. This dialog only mints the first `profile` claim (the
//! advertised name that syncs to followers like any other speech) and
//! surfaces your address. Closing without a name is allowed; the prompt
//! returns next launch until a profile claim exists.

use gpui::*;
use gpui_component::WindowExt;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputState};
use gpui_component::theme::ActiveTheme;

use std::time::{SystemTime, UNIX_EPOCH};

use vouch_core::e2ee::{self, Identity};
use vouch_core::{Draft, Peer, profile};

pub struct WelcomeModal;

impl WelcomeModal {
    pub fn open(peer: Peer, identity: Identity, window: &mut Window, cx: &mut App) {
        // The full capability address: LogId + content key in one string.
        // Handing it over is what lets a friend follow AND read you.
        let address = identity.address().to_string();
        let key = identity.content_key();

        window.open_alert_dialog(cx, move |dialog, window, cx| {
            let name_state = window.use_state(cx, |window, cx| {
                InputState::new(window, cx).placeholder("What should people call you?")
            });

            let name_state_clone = name_state.clone();
            let peer = peer.clone();

            dialog
                .title("Welcome to Vouch")
                .width(px(520.))
                .button_props(DialogButtonProps::default().ok_text("Let's go"))
                .on_ok(move |_, _window, cx| {
                    let name =
                        profile::sanitize_name(&name_state_clone.read(cx).text().to_string());
                    if name.is_empty() {
                        return false;
                    }
                    let peer = peer.clone();
                    let at_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    cx.spawn(async move |_cx| {
                        // Sealed like all speech: your name resolves for
                        // people you've granted, and nobody else.
                        let draft = Draft::new("profile").at(at_ms).text("name", name);
                        let Ok(sealed) = e2ee::seal_draft(&key, &draft) else {
                            return;
                        };
                        let _ = peer.claim(sealed).await;
                    })
                    .detach();
                    true
                })
                .child(WelcomeForm::new(name_state, address.clone()).into_any_element())
        });
    }
}

#[derive(IntoElement)]
pub struct WelcomeForm {
    name_state: Entity<InputState>,
    address: String,
}

impl WelcomeForm {
    pub fn new(name_state: Entity<InputState>, address: String) -> Self {
        Self {
            name_state,
            address,
        }
    }
}

impl RenderOnce for WelcomeForm {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let address = self.address.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Your account lives on this device — there's nothing to register. \
                         Pick a name to suggest to people who follow you.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child("Your name"),
                    )
                    .child(Input::new(&self.name_state)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child("Your address"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                "Send this to friends so they can follow you — anyone \
                                 holding it can read what you post.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .p_2()
                                    .bg(theme.muted)
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme.border)
                                    .font_family("monospace")
                                    .text_xs()
                                    .text_color(theme.foreground)
                                    // Too long to show whole — the Copy
                                    // button carries the real thing.
                                    .child(format!(
                                        "{}…",
                                        &self.address[..30.min(self.address.len())]
                                    )),
                            )
                            .child(
                                div()
                                    .id("copy-address")
                                    .flex_shrink_0()
                                    .px_3()
                                    .py_2()
                                    .bg(theme.primary)
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme.primary_hover))
                                    .on_click(move |_, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            address.clone(),
                                        ));
                                    })
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.primary_foreground)
                                            .child("Copy"),
                                    ),
                            ),
                    ),
            )
    }
}
