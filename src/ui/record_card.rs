use crate::data::{MockData, Recommendation};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::theme::Theme;
use std::time::SystemTime;

pub struct RecordCard;

impl RecordCard {
    fn format_relative_time(timestamp: SystemTime) -> String {
        let now = SystemTime::now();
        let duration = now
            .duration_since(timestamp)
            .unwrap_or_else(|_| std::time::Duration::from_secs(0));

        let seconds = duration.as_secs();
        if seconds < 60 {
            "just now".to_string()
        } else if seconds < 3600 {
            let minutes = seconds / 60;
            if minutes == 1 {
                "1m ago".to_string()
            } else {
                format!("{}m ago", minutes)
            }
        } else if seconds < 86400 {
            let hours = seconds / 3600;
            if hours == 1 {
                "1h ago".to_string()
            } else {
                format!("{}h ago", hours)
            }
        } else {
            let days = seconds / 86400;
            if days == 1 {
                "1d ago".to_string()
            } else if days < 30 {
                format!("{}d ago", days)
            } else {
                let months = days / 30;
                if months == 1 {
                    "1mo ago".to_string()
                } else {
                    format!("{}mo ago", months)
                }
            }
        }
    }

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

    fn truncate_content(content: &str, max_len: usize) -> String {
        if content.len() <= max_len {
            content.to_string()
        } else {
            let truncated: String = content.chars().take(max_len).collect();
            format!("{}...", truncated.trim_end())
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
            theme.list
        };

        let subject_name = recommendation.subject_name.clone();
        let content_preview = Self::truncate_content(&recommendation.content, 80);
        let attribution = Self::format_attribution(&recommendation.source, data);
        let timestamp = Self::format_relative_time(recommendation.source.timestamp);

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
