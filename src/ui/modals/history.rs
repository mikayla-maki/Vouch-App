//! The "see edits/changes" view: a styled read of `Recommendation::timeline()`
//! — every claim that shaped this recommendation, oldest first, with
//! superseded field edits shown alongside the current value rather than
//! hidden. Read-only: this is a window onto history, not an editor.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::WindowExt;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::theme::{ActiveTheme, Theme};

use std::time::{Duration, UNIX_EPOCH};

use vouch_core::{LogId, Recommendation, TimelineEntry, Value};

use crate::ui::format::{TimeStyle, format_relative_time};

pub struct HistoryModal;

impl HistoryModal {
    pub fn open(rec: Recommendation, local_log_id: Option<LogId>, window: &mut Window, cx: &mut App) {
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("History")
                .width(px(520.))
                .button_props(DialogButtonProps::default().ok_text("Close"))
                .child(HistoryList::new(rec.clone(), local_log_id).into_any_element())
        });
    }
}

#[derive(IntoElement)]
pub struct HistoryList {
    rec: Recommendation,
    local_log_id: Option<LogId>,
}

impl HistoryList {
    pub fn new(rec: Recommendation, local_log_id: Option<LogId>) -> Self {
        Self { rec, local_log_id }
    }
}

impl RenderOnce for HistoryList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let timeline = self.rec.timeline();

        if timeline.is_empty() {
            return div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Nothing recorded yet.")
                .into_any_element();
        }

        div()
            .id("history-list")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(420.))
            .overflow_y_scroll()
            .children(
                timeline
                    .iter()
                    .map(|entry| render_entry(entry, self.local_log_id, &theme)),
            )
            .into_any_element()
    }
}

fn render_entry(entry: &TimelineEntry, local_log_id: Option<LogId>, theme: &Theme) -> Div {
    let author = match local_log_id {
        Some(me) if me == entry.author() => "you".to_string(),
        _ => entry.author().short(),
    };
    let when = format_relative_time(
        UNIX_EPOCH + Duration::from_millis(entry.at().max(0) as u64),
        TimeStyle::Compact,
    );

    let (icon, description, current) = match entry {
        TimelineEntry::Field {
            field,
            value,
            current,
            ..
        } => (
            "✏️",
            format!("set {field} to \u{201c}{}\u{201d}", display_value(value)),
            *current,
        ),
        TimelineEntry::Comment { text, .. } => {
            ("💬", format!("commented \u{201c}{text}\u{201d}"), true)
        }
    };

    let text_color = if current {
        theme.foreground
    } else {
        theme.muted_foreground
    };

    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .p_2()
        .rounded_md()
        .when(!current, |this| this.bg(theme.muted.opacity(0.4)))
        .child(div().text_sm().child(icon))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.primary_hover)
                                .child(format!("by {author}")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(when),
                        )
                        .when(!current, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("· superseded"),
                            )
                        }),
                )
                .child(div().text_sm().text_color(text_color).child(description)),
        )
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Text(t) => t.clone(),
        Value::Int(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}
