use crate::data::{ContactId, MockData, Recommendation, RecommendationId};
use gpui_component::theme::{ActiveTheme, Theme};
use gpui::prelude::FluentBuilder;
use gpui::*;

#[derive(IntoElement)]
pub struct DetailPanel {
    selected_id: Option<RecommendationId>,
    data: MockData,
}

impl DetailPanel {
    pub fn new(data: MockData) -> Self {
        Self {
            selected_id: None,
            data,
        }
    }

    pub fn selected(mut self, id: Option<RecommendationId>) -> Self {
        self.selected_id = id;
        self
    }

    fn render_empty_state(theme: &Theme) -> Stateful<Div> {
        div()
            .id("empty-state")
            .flex()
            .flex_col()
            .h_full()
            .flex_1()
            .bg(theme.background)
            .justify_center()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w_16()
                            .h_16()
                            .rounded_full()
                            .bg(theme.muted)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(div().text_2xl().text_color(theme.muted_foreground).child("💭")),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Select a recommendation"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Click on a vouch in the feed to see details"),
                    ),
            )
    }

    fn render_recommendation_detail(
        recommendation: &Recommendation,
        data: &MockData,
        theme: &Theme,
    ) -> Stateful<Div> {
        let is_own = recommendation.source.original_author == MockData::local_user_id();
        let related_records = Self::find_related_records(recommendation, data);

        div()
            .id("detail-panel")
            .flex()
            .flex_col()
            .h_full()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.background)
            .child(Self::render_subject_header(recommendation, theme))
            .child(Self::render_scrollable_content(
                recommendation,
                data,
                &related_records,
                theme,
            ))
            .child(Self::render_action_bar(is_own, theme))
    }

    fn render_subject_header(recommendation: &Recommendation, theme: &Theme) -> Div {
        let subject_emoji = Self::get_subject_emoji(&recommendation.subject_name);

        div()
            .flex()
            .flex_col()
            .items_center()
            .p_6()
            .pb_4()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(
                div()
                    .w_24()
                    .h_24()
                    .rounded_full()
                    .bg(theme.list)
                    .border_3()
                    .border_color(theme.primary)
                    .flex()
                    .justify_center()
                    .items_center()
                    .shadow_md()
                    .child(div().text_3xl().child(subject_emoji)),
            )
            .child(
                div()
                    .mt_4()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .text_center()
                    .child(recommendation.subject_name.clone()),
            )
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Self::get_subject_category(&recommendation.subject_name)),
            )
    }

    fn render_scrollable_content(
        recommendation: &Recommendation,
        data: &MockData,
        related_records: &[&Recommendation],
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id("detail-scroll-content")
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .min_w_0()
            .overflow_y_scroll()
            .child(Self::render_recommendation_content(recommendation, theme))
            .child(Self::render_vouch_chain(recommendation, data, theme))
            .child(Self::render_related_section(related_records, data, theme))
    }

    fn render_recommendation_content(recommendation: &Recommendation, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w_6()
                            .h_6()
                            .rounded_full()
                            .bg(theme.accent)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(div().text_xs().text_color(theme.foreground).child("📝")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Recommendation"),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .p_4()
                    .bg(theme.list)
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .shadow_sm()
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_base()
                            .text_color(theme.foreground)
                            .child(div().size_full().child(recommendation.content.clone())),
                    ),
            )
    }

    fn render_vouch_chain(recommendation: &Recommendation, data: &MockData, theme: &Theme) -> Div {
        let original_author_id = recommendation.source.original_author;
        let original_author_name = data.get_contact_name(original_author_id);
        let is_own_recommendation = original_author_id == MockData::local_user_id();

        let timestamp = Self::format_timestamp(&recommendation.source.timestamp);

        let mut chain_items: Vec<Div> = Vec::new();

        chain_items.push(Self::render_chain_item(
            "✨",
            "Original Author",
            &original_author_name,
            if is_own_recommendation {
                Some("You wrote this")
            } else {
                None
            },
            &timestamp,
            theme.primary,
            theme,
            true,
        ));

        if let Some(via_id) = recommendation.source.received_via {
            let via_name = data.get_contact_name(via_id);
            chain_items.push(Self::render_chain_item(
                "📬",
                "Received via",
                &via_name,
                None,
                "",
                theme.secondary,
                theme,
                false,
            ));
        }

        if !recommendation.source.revouched_by.is_empty() {
            let revouchers = Self::format_revouchers(&recommendation.source.revouched_by, data);
            chain_items.push(Self::render_chain_item(
                "🔄",
                "Revouched by",
                &revouchers,
                None,
                "",
                theme.accent,
                theme,
                false,
            ));
        }

        div()
            .flex()
            .flex_col()
            .px_4()
            .py_3()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mb_2()
                    .child(
                        div()
                            .w_6()
                            .h_6()
                            .rounded_full()
                            .bg(theme.secondary)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(div().text_xs().text_color(theme.foreground).child("🔗")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Vouch Chain"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pl_2()
                    .border_l_2()
                    .border_color(theme.border)
                    .children(chain_items),
            )
    }

    fn render_chain_item(
        emoji: &'static str,
        label: &'static str,
        value: &str,
        subtitle: Option<&'static str>,
        timestamp: &str,
        accent_color: Hsla,
        theme: &Theme,
        is_primary: bool,
    ) -> Div {
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap_3()
            .p_3()
            .ml_2()
            .bg(if is_primary {
                theme.muted
            } else {
                theme.background
            })
            .rounded_md()
            .when(is_primary, |this| {
                this.border_1().border_color(theme.border)
            })
            .child(
                div()
                    .w_8()
                    .h_8()
                    .rounded_full()
                    .bg(accent_color)
                    .flex()
                    .justify_center()
                    .items_center()
                    .flex_shrink_0()
                    .child(div().text_sm().child(emoji.to_string())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(SharedString::from(value.to_string())),
                    )
                    .when(subtitle.is_some(), |this| {
                        let sub = subtitle.unwrap_or("");
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .mt_1()
                                .child(SharedString::from(sub)),
                        )
                    })
                    .when(!timestamp.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .mt_1()
                                .child(SharedString::from(timestamp.to_string())),
                        )
                    }),
            )
    }

    fn render_related_section(
        related_records: &[&Recommendation],
        data: &MockData,
        theme: &Theme,
    ) -> Div {
        let count = related_records.len();

        div()
            .flex()
            .flex_col()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mb_3()
                    .child(
                        div()
                            .w_6()
                            .h_6()
                            .rounded_full()
                            .bg(theme.primary)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(div().text_xs().text_color(theme.foreground).child("📚")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("Related Vouches ({})", count)),
                    ),
            )
            .when(count == 0, |this| {
                this.child(
                    div().p_4().bg(theme.muted).rounded_md().child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .text_center()
                            .child("No other vouches for this subject yet"),
                    ),
                )
            })
            .when(count > 0, |this| {
                let mut items: Vec<Div> = Vec::new();
                for rec in related_records.iter() {
                    items.push(Self::render_related_item(rec, data, theme));
                }
                this.child(div().flex().flex_col().gap_2().children(items))
            })
    }

    fn render_related_item(recommendation: &Recommendation, data: &MockData, theme: &Theme) -> Div {
        let author_name = data.get_contact_name(recommendation.source.original_author);
        let excerpt = Self::truncate_text(&recommendation.content, 80);

        div()
            .p_3()
            .bg(theme.list)
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w_5()
                                    .h_5()
                                    .rounded_full()
                                    .bg(theme.secondary)
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .child(div().text_xs().child("👤")),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(author_name.to_string()),
                            ),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(excerpt)),
            )
    }

    fn render_action_bar(is_own: bool, theme: &Theme) -> Div {
        let not_implemented_border = gpui::red();

        div()
            .flex()
            .flex_row()
            .justify_center()
            .gap_3()
            .p_4()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .mt_auto()
            .child(Self::render_stub_button(
                "revouch-btn",
                "🔄",
                "Revouch",
                theme,
                not_implemented_border,
            ))
            .child(Self::render_stub_button(
                "disavow-btn",
                "🚫",
                "Disavow",
                theme,
                not_implemented_border,
            ))
            .when(is_own, |this| {
                this.child(Self::render_stub_button(
                    "edit-btn",
                    "✏️",
                    "Edit",
                    theme,
                    not_implemented_border,
                ))
            })
    }

    fn render_stub_button(
        id: &'static str,
        emoji: &str,
        label: &str,
        theme: &Theme,
        border_color: Hsla,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme.list)
            .rounded_lg()
            .cursor_not_allowed()
            .border_2()
            .border_color(border_color)
            .shadow_sm()
            .child(div().text_sm().child(emoji.to_string()))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(label.to_string()),
            )
    }

    fn find_related_records<'a>(
        current: &Recommendation,
        data: &'a MockData,
    ) -> Vec<&'a Recommendation> {
        let current_subject = current.subject_name.to_lowercase();

        data.recommendations
            .iter()
            .filter(|rec| {
                rec.id != current.id && rec.subject_name.to_lowercase() == current_subject
            })
            .collect()
    }

    fn format_revouchers(revouchers: &[ContactId], data: &MockData) -> String {
        let names: Vec<String> = revouchers
            .iter()
            .map(|id| {
                if *id == MockData::local_user_id() {
                    "You".to_string()
                } else {
                    data.get_contact_name(*id).to_string()
                }
            })
            .collect();

        match names.len() {
            0 => String::new(),
            1 => names[0].clone(),
            2 => format!("{} and {}", names[0], names[1]),
            _ => {
                let last = names.last().unwrap();
                let rest = &names[..names.len() - 1];
                format!("{}, and {}", rest.join(", "), last)
            }
        }
    }

    fn format_timestamp(timestamp: &std::time::SystemTime) -> String {
        let now = std::time::SystemTime::now();
        let duration = now
            .duration_since(*timestamp)
            .unwrap_or(std::time::Duration::ZERO);

        let seconds = duration.as_secs();
        let minutes = seconds / 60;
        let hours = minutes / 60;
        let days = hours / 24;

        if days > 0 {
            format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
        } else if hours > 0 {
            format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
        } else if minutes > 0 {
            format!(
                "{} minute{} ago",
                minutes,
                if minutes == 1 { "" } else { "s" }
            )
        } else {
            "Just now".to_string()
        }
    }

    fn truncate_text(text: &str, max_len: usize) -> String {
        if text.len() <= max_len {
            text.to_string()
        } else {
            let truncated: String = text.chars().take(max_len).collect();
            format!("{}...", truncated.trim_end())
        }
    }

    fn get_subject_emoji(subject_name: &str) -> &'static str {
        let lower = subject_name.to_lowercase();

        if lower.contains("restaurant")
            || lower.contains("thai")
            || lower.contains("bakery")
            || lower.contains("food")
            || lower.contains("cafe")
        {
            "🍜"
        } else if lower.contains("auto") || lower.contains("car") || lower.contains("repair") {
            "🚗"
        } else if lower.contains("doctor")
            || lower.contains("dentist")
            || lower.contains("dr.")
            || lower.contains("medical")
        {
            "🏥"
        } else if lower.contains("trail")
            || lower.contains("hike")
            || lower.contains("park")
            || lower.contains("nature")
        {
            "🥾"
        } else if lower.contains("library") || lower.contains("book") || lower.contains("study") {
            "📚"
        } else if lower.contains("plant") || lower.contains("garden") || lower.contains("flower") {
            "🌱"
        } else if lower.contains("shop") || lower.contains("store") {
            "🏪"
        } else {
            "📍"
        }
    }

    fn get_subject_category(subject_name: &str) -> &'static str {
        let lower = subject_name.to_lowercase();

        if lower.contains("restaurant")
            || lower.contains("thai")
            || lower.contains("bakery")
            || lower.contains("food")
            || lower.contains("cafe")
        {
            "Restaurant / Food"
        } else if lower.contains("auto") || lower.contains("car") || lower.contains("repair") {
            "Auto Services"
        } else if lower.contains("doctor")
            || lower.contains("dentist")
            || lower.contains("dr.")
            || lower.contains("medical")
        {
            "Healthcare"
        } else if lower.contains("trail")
            || lower.contains("hike")
            || lower.contains("park")
            || lower.contains("nature")
        {
            "Outdoor / Recreation"
        } else if lower.contains("library") || lower.contains("book") || lower.contains("study") {
            "Library / Education"
        } else if lower.contains("plant") || lower.contains("garden") || lower.contains("flower") {
            "Garden / Plants"
        } else if lower.contains("shop") || lower.contains("store") {
            "Shopping"
        } else {
            "Place"
        }
    }
}

impl RenderOnce for DetailPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();

        match self.selected_id {
            Some(id) => {
                if let Some(recommendation) = self.data.recommendations.iter().find(|r| r.id == id)
                {
                    Self::render_recommendation_detail(recommendation, &self.data, &theme)
                } else {
                    Self::render_empty_state(&theme)
                }
            }
            None => Self::render_empty_state(&theme),
        }
    }
}
