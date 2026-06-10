use crate::data::{MockData, Recommendation};
use crate::ui::format::{TimeStyle, format_relative_time, truncate};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::theme::Theme;

pub struct RecordCard;

impl RecordCard {
    fn format_attribution(source: &crate::data::RecordSource, data: &MockData) -> String {
        let author_name = data.get_contact_name(source.original_author);
        if source.original_author == MockData::local_user_id() {
            "by you".to_string()
        } else if let Some(via_id) = source.received_via {
            let via_name = data.get_contact_name(via_id);
            format!("via {}", via_name)
        } else {
            format!("by {}", author_name)
        }
    }

    pub fn render_card(
        recommendation: &Recommendation,
        is_selected: bool,
        data: &MockData,
        theme: &Theme,
    ) -> impl IntoElement {
        let background = if is_selected {
            theme.list_active
        } else {
            theme.colors.list
        };

        let subject_name = recommendation.subject_name.clone();
        let content_preview = truncate(&recommendation.content, 80);
        let attribution = Self::format_attribution(&recommendation.source, data);
        let timestamp = format_relative_time(recommendation.source.timestamp, TimeStyle::Compact);

        div()
            .id(ElementId::Name(
                format!("card-{}", recommendation.id.0).into(),
            ))
            .w_full()
            .p_3()
            .bg(background)
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .cursor_pointer()
            .when(!is_selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(subject_name),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(content_preview),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .mt_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.primary_hover)
                                    .child(attribution),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("•"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(timestamp),
                            ),
                    ),
            )
    }
}
