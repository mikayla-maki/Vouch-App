use crate::ui::format::{TimeStyle, attribution, format_relative_time, truncate};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::theme::Theme;
use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};
use vouch_core::{LogId, Recommendation};

pub struct RecordCard;

impl RecordCard {
    fn format_attribution(
        rec: &Recommendation,
        local_log_id: Option<LogId>,
        names: &BTreeMap<LogId, String>,
    ) -> String {
        match rec.author() {
            Some(author) => format!("by {}", attribution(author, local_log_id, names)),
            None => "by someone".to_string(),
        }
    }

    pub fn render_card(
        rec: &Recommendation,
        is_selected: bool,
        local_log_id: Option<LogId>,
        names: &BTreeMap<LogId, String>,
        theme: &Theme,
    ) -> impl IntoElement {
        let background = if is_selected {
            theme.list_active
        } else {
            theme.colors.list
        };

        let subject_name = rec.subject.clone();
        let content_preview = truncate(&rec.body, 80);
        let attribution = Self::format_attribution(rec, local_log_id, names);
        let timestamp = format_relative_time(
            UNIX_EPOCH + Duration::from_millis(rec.at_ms().max(0) as u64),
            TimeStyle::Compact,
        );

        div()
            .id(ElementId::Name(format!("card-{}", rec.id).into()))
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
                            )
                            // The tier, at a glance: deniable is unmarked;
                            // the scroll means the shown words are attested.
                            .when(rec.on_the_record(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.primary)
                                        .child("📜 on the record"),
                                )
                            }),
                    ),
            )
    }
}
