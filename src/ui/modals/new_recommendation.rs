use gpui::*;
use gpui_component::WindowExt;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputState};
use gpui_component::theme::ActiveTheme;

use std::time::{SystemTime, UNIX_EPOCH};

use vouch_core::e2ee::{self, ContentKey};
use vouch_core::{Draft, Peer};

pub struct NewRecommendationModal;

impl NewRecommendationModal {
    pub fn open(peer: Peer, key: ContentKey, window: &mut Window, cx: &mut App) {
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            let subject_state = window.use_state(cx, |window, cx| {
                InputState::new(window, cx).placeholder("What are you recommending?")
            });

            let content_state = window.use_state(cx, |window, cx| {
                InputState::new(window, cx)
                    .placeholder("Write your recommendation...")
                    .auto_grow(3, 8)
            });

            let subject_state_clone = subject_state.clone();
            let content_state_clone = content_state.clone();
            let peer = peer.clone();

            dialog
                .title("New Recommendation")
                .width(px(500.))
                .button_props(DialogButtonProps::default().show_cancel(true))
                .on_ok(move |_, _window, cx| {
                    let subject = subject_state_clone.read(cx).text().to_string();
                    let content = content_state_clone.read(cx).text().to_string();

                    if subject.trim().is_empty() || content.trim().is_empty() {
                        return false;
                    }

                    let peer = peer.clone();
                    let at_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);

                    // Fire-and-forget: the write lands asynchronously and
                    // the feed picks it up via the firehose, same as any
                    // other peer's claims. Sealed always — there is no
                    // plaintext authoring path.
                    cx.spawn(async move |_cx| {
                        let draft = Draft::new("rec")
                            .at(at_ms)
                            .text("subject", subject)
                            .text("body", content);
                        let Ok(sealed) = e2ee::seal_draft(&key, &draft) else {
                            return;
                        };
                        let _ = peer.claim(sealed).await;
                    })
                    .detach();

                    true
                })
                .child(
                    NewRecommendationForm::new(subject_state, content_state).into_any_element(),
                )
        });
    }
}

#[derive(IntoElement)]
pub struct NewRecommendationForm {
    subject_state: Entity<InputState>,
    content_state: Entity<InputState>,
}

impl NewRecommendationForm {
    pub fn new(subject_state: Entity<InputState>, content_state: Entity<InputState>) -> Self {
        Self {
            subject_state,
            content_state,
        }
    }
}

impl RenderOnce for NewRecommendationForm {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
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
                            .child("Subject"),
                    )
                    .child(Input::new(&self.subject_state)),
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
                            .child("Your Recommendation"),
                    )
                    .child(Input::new(&self.content_state)),
            )
    }
}
