use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName};

pub struct SearchBar {
    input_state: Entity<InputState>,
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

        Self { input_state }
    }

    fn on_search_changed(&mut self, cx: &mut Context<Self>) {
        let query: SharedString = self.input_state.read(cx).text().to_string().into();
        cx.emit(SearchBarEvent::QueryChanged(query));
    }
}

#[derive(Clone)]
pub enum SearchBarEvent {
    QueryChanged(SharedString),
}

impl EventEmitter<SearchBarEvent> for SearchBar {}

impl Render for SearchBar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w_full().p_2().child(
            Input::new(&self.input_state)
                .prefix(Icon::new(IconName::Search).size_4())
                .cleanable(true),
        )
    }
}
