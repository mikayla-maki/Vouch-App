use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Icon, IconName};

pub struct SearchBar {
    input_state: Entity<InputState>,
    search_query: SharedString,
}

impl SearchBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search recommendations..."));

        cx.subscribe(&input_state, |this, _input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.on_search_changed(cx);
            }
        })
        .detach();

        Self {
            input_state,
            search_query: SharedString::default(),
        }
    }

    pub fn query(&self) -> &SharedString {
        &self.search_query
    }

    fn on_search_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).text().to_string();
        self.search_query = text.into();
        cx.emit(SearchBarEvent::QueryChanged);
        cx.notify();
    }
}

#[derive(Clone)]
pub enum SearchBarEvent {
    QueryChanged,
}

impl EventEmitter<SearchBarEvent> for SearchBar {}

impl Render for SearchBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        div().w_full().p_2().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .bg(theme.list)
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .child(
                    Icon::new(IconName::Search)
                        .size_4()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    Input::new(&self.input_state)
                        .appearance(false)
                        .cleanable(true),
                ),
        )
    }
}
