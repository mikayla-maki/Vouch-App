use crate::data::{MockData, RecommendationId};
use crate::ui::detail_panel::DetailPanel;
use crate::ui::feed_panel::FeedPanel;
use crate::ui::sidebar::Sidebar;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::Root;
use gpui_component::theme::ActiveTheme;
use std::rc::Rc;

pub struct VouchApp {
    feed_panel: Entity<FeedPanel>,
    data: Rc<MockData>,
    sidebar_collapsed: bool,
}

impl VouchApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let data = Rc::new(MockData::generate());

        let feed_panel = cx.new(|cx| FeedPanel::new(data.clone(), window, cx));

        // FeedPanel owns the selection, but VouchApp reads it in render to
        // drive the detail panel, so re-render whenever the feed notifies.
        cx.observe(&feed_panel, |_, _, cx| cx.notify()).detach();

        Self {
            feed_panel,
            data,
            sidebar_collapsed: true,
        }
    }

    fn selected_recommendation_id(&self, cx: &App) -> Option<RecommendationId> {
        self.feed_panel.read(cx).selected_id()
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }
}

impl Render for VouchApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selected_recommendation_id(cx);
        let theme = cx.theme();

        div()
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(theme.background)
            .child(
                Sidebar::new(self.data.clone())
                    .collapsed(self.sidebar_collapsed)
                    .on_toggle(cx.listener(|this, _, _window, cx| {
                        this.toggle_sidebar(cx);
                    })),
            )
            .child(self.feed_panel.clone())
            .child(DetailPanel::new(self.data.clone()).selected(selected_id))
            .when_some(Root::render_dialog_layer(window, cx), |this, layer| {
                this.child(layer)
            })
    }
}
