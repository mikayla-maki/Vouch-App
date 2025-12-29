use crate::data::{Contact, MockData};
use crate::theme::{ActiveTheme, Theme};
use gpui::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterOption {
    All,
    Mine,
    Friends,
}

#[derive(IntoElement)]
pub struct Sidebar {
    data: MockData,
    selected_filter: FilterOption,
    is_collapsed: bool,
    on_toggle: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl Sidebar {
    pub fn new(data: MockData) -> Self {
        Self {
            data,
            selected_filter: FilterOption::All,
            is_collapsed: false,
            on_toggle: None,
        }
    }

    pub fn selected_filter(mut self, filter: FilterOption) -> Self {
        self.selected_filter = filter;
        self
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.is_collapsed = collapsed;
        self
    }

    pub fn on_toggle(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }

    fn render_filter_item(label: &'static str, is_selected: bool, theme: &Theme) -> Div {
        let background = if is_selected {
            theme.selected
        } else {
            theme.sidebar
        };

        div()
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .bg(background)
            .hover(|style| style.bg(theme.card_hover))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(theme.text_muted).child("•"))
                    .child(div().text_sm().text_color(theme.text).child(label)),
            )
    }

    fn render_contact_item(contact: &Contact, theme: &Theme) -> Div {
        let petname = contact.petname.clone();

        div()
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .bg(theme.sidebar)
            .hover(|style| style.bg(theme.card_hover))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(theme.text_muted).child("•"))
                    .child(div().text_sm().text_color(theme.text).child(petname)),
            )
    }

    fn render_section_header(label: &'static str, theme: &Theme) -> Div {
        div().px_3().py_2().child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(label),
        )
    }

    fn render_divider(theme: &Theme) -> Div {
        div().my_2().mx_3().h(px(1.0)).bg(theme.border)
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<ActiveTheme>();

        if self.is_collapsed {
            let mut expand_btn = div()
                .id("expand-btn")
                .cursor_pointer()
                .p_2()
                .rounded_md()
                .hover(|style| style.bg(theme.card_hover))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.primary_hover)
                        .child("V"),
                );

            if let Some(on_toggle) = self.on_toggle {
                expand_btn = expand_btn.on_click(move |event, window, cx| {
                    on_toggle(event, window, cx);
                });
            }

            return div()
                .flex()
                .flex_col()
                .h_full()
                .w_10()
                .min_w_10()
                .bg(theme.sidebar)
                .border_r_1()
                .border_color(theme.border)
                .items_center()
                .pt_2()
                .child(expand_btn);
        }

        let contacts_except_self: Vec<&Contact> = self
            .data
            .contacts
            .iter()
            .filter(|c| c.id != MockData::local_user_id())
            .collect();

        let selected_filter = self.selected_filter;

        let filter_items: Vec<Div> = [
            ("All", FilterOption::All),
            ("Mine", FilterOption::Mine),
            ("Friends", FilterOption::Friends),
        ]
        .into_iter()
        .map(|(label, filter)| Self::render_filter_item(label, selected_filter == filter, theme))
        .collect();

        let contact_items: Vec<Div> = contacts_except_self
            .iter()
            .map(|contact| Self::render_contact_item(contact, theme))
            .collect();

        let mut collapse_btn = div()
            .id("collapse-btn")
            .cursor_pointer()
            .px_2()
            .py_1()
            .rounded_md()
            .hover(|style| style.bg(theme.card_hover))
            .child(div().text_xs().text_color(theme.text_muted).child("◀"));

        if let Some(on_toggle) = self.on_toggle {
            collapse_btn = collapse_btn.on_click(move |event, window, cx| {
                on_toggle(event, window, cx);
            });
        }

        let header = div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.primary_hover)
                    .child("Vouch"),
            )
            .child(collapse_btn);

        div()
            .flex()
            .flex_col()
            .h_full()
            .w_40()
            .min_w_40()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(header)
            .child(
                div()
                    .id("sidebar-content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .p_2()
                    .child(Self::render_section_header("Filters", theme))
                    .children(filter_items)
                    .child(Self::render_divider(theme))
                    .child(Self::render_section_header("Contacts", theme))
                    .children(contact_items),
            )
    }
}
