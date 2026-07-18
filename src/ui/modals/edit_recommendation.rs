use gpui::*;
use gpui_component::WindowExt;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputState};
use gpui_component::theme::ActiveTheme;

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use vouch_core::e2ee::{self, ContentKey};
use vouch_core::{ClaimHash, ClaimRef, Draft, LogId, Peer, Recommendation, Value};

/// Every claim this edit/comment must reference to causally dominate the
/// recommendation's current state, as an `of` array value.
///
/// Pairs each currently-winning claim (every field-frontier contribution
/// plus every comment) with the log_id that authored it — the authors the
/// materializer already surfaced. Referencing all of them is the safe,
/// non-minimal move the fold engine asks for: a new edit that references
/// every claim on a field's frontier collapses that frontier back to one,
/// so touching `subject`/`body` here resolves any live conflict on them.
pub(crate) fn dominating_refs(rec: &Recommendation) -> Value {
    let mut by_hash: BTreeMap<ClaimHash, LogId> = BTreeMap::new();
    for field in rec.fields.values() {
        for contribution in &field.frontier {
            by_hash.insert(contribution.claim, contribution.author);
        }
    }
    for comment in &rec.comments {
        by_hash.insert(comment.claim, comment.author);
    }
    Value::Array(
        by_hash
            .into_iter()
            .map(|(hash, log_id)| Value::ClaimRef(ClaimRef { log_id, hash }))
            .collect(),
    )
}

pub struct EditRecommendationModal;

impl EditRecommendationModal {
    /// Open the edit dialog for `rec`. The caller is responsible for only
    /// offering this when the local writer is the source author (an `edit`
    /// from anyone else is inert), matching the `is_own` gate in the detail
    /// panel.
    pub fn open(
        peer: Peer,
        key: ContentKey,
        rec: Recommendation,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            let subject_state = window.use_state(cx, {
                let subject = rec.subject.clone();
                move |window, cx| {
                    InputState::new(window, cx)
                        .placeholder("What are you recommending?")
                        .default_value(subject)
                }
            });

            let content_state = window.use_state(cx, {
                let body = rec.body.clone();
                move |window, cx| {
                    InputState::new(window, cx)
                        .placeholder("Write your recommendation...")
                        .auto_grow(3, 8)
                        .default_value(body)
                }
            });

            let subject_state_clone = subject_state.clone();
            let content_state_clone = content_state.clone();
            let peer = peer.clone();
            let of_value = dominating_refs(&rec);

            dialog
                .title("Edit Recommendation")
                .width(px(500.))
                .button_props(DialogButtonProps::default().show_cancel(true))
                .on_ok(move |_, _window, cx| {
                    let subject = subject_state_clone.read(cx).text().to_string();
                    let content = content_state_clone.read(cx).text().to_string();

                    if subject.trim().is_empty() || content.trim().is_empty() {
                        return false;
                    }

                    let peer = peer.clone();
                    let of_value = of_value.clone();
                    let at_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);

                    // Fire-and-forget, same shape as authoring a `rec`: the
                    // edit lands asynchronously and the feed re-folds it in
                    // via the firehose. Re-asserting both fields (even the
                    // unchanged one) is intentional — it collapses any live
                    // frontier on each back to this single contribution.
                    // Sealed always; the reference to the original rides
                    // inside the ciphertext.
                    cx.spawn(async move |_cx| {
                        let draft = Draft::new("edit")
                            .at(at_ms)
                            .field("of", of_value)
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
                    EditRecommendationForm::new(subject_state, content_state).into_any_element(),
                )
        });
    }
}

#[derive(IntoElement)]
pub struct EditRecommendationForm {
    subject_state: Entity<InputState>,
    content_state: Entity<InputState>,
}

impl EditRecommendationForm {
    pub fn new(subject_state: Entity<InputState>, content_state: Entity<InputState>) -> Self {
        Self {
            subject_state,
            content_state,
        }
    }
}

impl RenderOnce for EditRecommendationForm {
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
