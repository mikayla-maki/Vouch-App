use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::WindowExt;
use gpui_component::input::{Input, InputState};
use gpui_component::theme::ActiveTheme;

use crate::data::MockData;

pub struct NewRecommendationModal;

impl NewRecommendationModal {
    pub fn open(data: MockData, window: &mut Window, cx: &mut App) {
        let subject_suggestions: Vec<SharedString> = data
            .recommendations
            .iter()
            .map(|r| r.subject_name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        window.open_dialog(cx, move |dialog, window, cx| {
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

            dialog
                .title("New Recommendation")
                .w(px(500.))
                .confirm()
                .on_ok(move |_, _window, cx| {
                    let subject = subject_state_clone.read(cx).text().to_string();
                    let content = content_state_clone.read(cx).text().to_string();

                    if subject.trim().is_empty() || content.trim().is_empty() {
                        return false;
                    }

                    println!("SAVE THIS - Subject: {}, Content: {}", subject, content);
                    true
                })
                .child(
                    NewRecommendationForm::new(
                        subject_state,
                        content_state,
                        subject_suggestions.clone(),
                    )
                    .into_any_element(),
                )
        });
    }
}

#[derive(IntoElement)]
pub struct NewRecommendationForm {
    subject_state: Entity<InputState>,
    content_state: Entity<InputState>,
    subject_suggestions: Vec<SharedString>,
}

impl NewRecommendationForm {
    pub fn new(
        subject_state: Entity<InputState>,
        content_state: Entity<InputState>,
        subject_suggestions: Vec<SharedString>,
    ) -> Self {
        Self {
            subject_state,
            content_state,
            subject_suggestions,
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
                    .child(Input::new(&self.subject_state))
                    .when(!self.subject_suggestions.is_empty(), |this: Div| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Existing subjects: ")
                                .child(
                                    self.subject_suggestions
                                        .iter()
                                        .take(3)
                                        .map(|s| s.to_string())
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                ),
                        )
                    }),
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
