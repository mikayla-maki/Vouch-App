//! Follow someone: paste the `vouch:` address they sent you.
//!
//! Following is private, and the pasted address is the whole deal: it
//! carries the log to sync AND the key to read it, so follow ⇒ read
//! with no round trip. The person you follow learns nothing.

use gpui::*;
use gpui_component::WindowExt;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputState};
use gpui_component::theme::ActiveTheme;

use crate::follows::Follows;

pub struct AddFollowModal;

impl AddFollowModal {
    pub fn open(follows: Entity<Follows>, window: &mut Window, cx: &mut App) {
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            let address_state = window.use_state(cx, |window, cx| {
                InputState::new(window, cx).placeholder("Paste an address (starts with vouch:)")
            });

            let address_state_clone = address_state.clone();
            let follows = follows.clone();

            dialog
                .title("Follow someone")
                .width(px(520.))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Follow")
                        .show_cancel(true),
                )
                .on_ok(move |_, _window, cx| {
                    let text = address_state_clone.read(cx).text().to_string();
                    match vouch_core::e2ee::Address::parse(&text) {
                        // Close only if the follow actually took — your own
                        // address, a duplicate, or garbage all keep the
                        // dialog open rather than silently vanishing.
                        Some(address) => {
                            follows.update(cx, |follows, cx| follows.add(address, cx))
                        }
                        None => false,
                    }
                })
                .child(AddFollowForm::new(address_state).into_any_element())
        });
    }
}

#[derive(IntoElement)]
pub struct AddFollowForm {
    address_state: Entity<InputState>,
}

impl AddFollowForm {
    pub fn new(address_state: Entity<InputState>) -> Self {
        Self { address_state }
    }
}

impl RenderOnce for AddFollowForm {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(
                        "An address is a vouch: code someone sends you — it carries \
                         both where their recommendations live and the key to read \
                         them. Only you know you follow them.",
                    ),
            )
            .child(Input::new(&self.address_state))
    }
}
