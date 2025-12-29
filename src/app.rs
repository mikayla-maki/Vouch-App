use crate::data::{MockData, RecommendationId};
use crate::theme::ActiveTheme;
use crate::ui::detail_panel::DetailPanel;
use crate::ui::feed_panel::FeedPanel;
use crate::ui::sidebar::Sidebar;
use gpui::*;

pub struct VouchApp {
    feed_panel: Entity<FeedPanel>,
    data: MockData,
}

impl VouchApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let data = MockData::generate();

        let feed_panel = cx.new(|_cx| FeedPanel::new(data.clone()));

        Self { feed_panel, data }
    }

    fn selected_recommendation_id(&self, cx: &App) -> Option<RecommendationId> {
        self.feed_panel.read(cx).selected_id()
    }
}

impl Render for VouchApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selected_recommendation_id(cx);
        let theme = cx.global::<ActiveTheme>();

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(theme.background)
            .child(Sidebar::new(self.data.clone()))
            .child(self.feed_panel.clone())
            .child(DetailPanel::new(self.data.clone()).selected(selected_id))
    }
}
