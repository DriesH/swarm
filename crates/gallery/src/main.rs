//! `swarm-gallery`: every `swarm_ui` component in light and dark.

#[cfg(feature = "snapshot")]
mod snapshot;

use gpui::{
    App, Bounds, Context, FocusHandle, FontWeight, KeyBinding, Role, SharedString, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, px, size,
};
use swarm_ui::component::ComponentEntry;
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;
use swarm_ui::theme::{self, Appearance};

actions!(gallery, [SelectNext, SelectPrevious, Quit]);

const COMPONENT_LIST: &str = "ComponentList";

struct Gallery {
    entries: Vec<ComponentEntry>,
    selected: usize,
    row_focus: Vec<FocusHandle>,
    _appearance: Subscription,
    _motion: Subscription,
}

impl Gallery {
    fn new(selected: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let entries = swarm_ui::all_components();
        let row_focus: Vec<FocusHandle> = entries.iter().map(|_| cx.focus_handle()).collect();
        let selected = selected.min(entries.len().saturating_sub(1));

        if let Some(handle) = row_focus.get(selected) {
            window.focus(handle, cx);
        }

        let appearance = theme::follow_system_appearance(window, cx);
        let motion = swarm_ui::motion::follow_reduce_motion(window, cx);

        Self {
            selected,
            entries,
            row_focus,
            _appearance: appearance,
            _motion: motion,
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = self.row_focus.get(index) else {
            return;
        };

        window.focus(handle, cx);
        self.selected = index;
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        let next = (self.selected + 1).min(self.entries.len().saturating_sub(1));
        self.select(next, window, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), window, cx);
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.entries.len();
        let rows =
            self.entries
                .iter()
                .zip(&self.row_focus)
                .enumerate()
                .map(|(index, (entry, handle))| {
                    let is_selected = index == self.selected;
                    // Roving tab stop: Tab enters the list at the selected row, arrows move inside it.
                    let handle = handle.clone().tab_stop(is_selected);

                    ListRow::new(("component", index), entry.name())
                        .track_focus(&handle)
                        .toggle_state(is_selected)
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.select(index, window, cx)),
                        )
                });

        v_flex()
            .w(px(232.))
            .flex_none()
            .gap(px(2.))
            .px(px(8.))
            .py(px(12.))
            .child(
                h_flex()
                    .h(px(32.))
                    .gap_2()
                    .px(px(6.))
                    .child(
                        div()
                            .size(px(20.))
                            .rounded(px(6.))
                            .bg(cx.theme().colors().fg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::Bee)
                                    .size(IconSize::Custom(px(13.)))
                                    .color(Color::Inverse),
                            ),
                    )
                    .child(
                        Label::new("swarm_ui")
                            .size(LabelSize::Large)
                            .weight(FontWeight::SEMIBOLD),
                    ),
            )
            .child(
                h_flex().h(px(28.)).mt(px(14.)).px(px(8.)).child(
                    Label::new(format!("Components · {count}"))
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                ),
            )
            .child(
                div()
                    .id("components")
                    .role(Role::List)
                    .aria_label("Components")
                    .key_context(COMPONENT_LIST)
                    .on_action(cx.listener(Self::select_next))
                    .on_action(cx.listener(Self::select_previous))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows),
            )
    }

    fn render_toolbar(title: SharedString, cx: &App) -> impl IntoElement + use<> {
        let selected = Appearance::ALL
            .iter()
            .position(|appearance| *appearance == theme::appearance(cx))
            .unwrap_or_default();

        h_flex()
            .flex_none()
            .h(px(46.))
            .gap_2()
            .px(px(16.))
            .border_b_1()
            .border_color(cx.theme().colors().line)
            .child(
                div().flex_1().child(
                    Label::new(title)
                        .size(LabelSize::Large)
                        .weight(FontWeight::SEMIBOLD),
                ),
            )
            .child(
                SegmentedControl::new(
                    "appearance",
                    "Theme",
                    Appearance::ALL.map(Appearance::label),
                )
                .selected(selected)
                .on_select(|index, _, cx| {
                    if let Some(appearance) = Appearance::ALL.get(index) {
                        theme::set_appearance(*appearance, cx);
                    }
                }),
            )
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(entry) = self.entries.get(self.selected).copied() else {
            return div().into_any_element();
        };

        let preview = entry.preview(window, cx);

        let sidebar = self.render_sidebar(cx);
        let toolbar = Self::render_toolbar(entry.name().into(), cx);

        let colors = cx.theme().colors();
        let (ground, panel, line, fg) = (colors.ground, colors.panel, colors.line, colors.fg);

        swarm_ui::tab_navigation(
            div()
                .id("gallery")
                .key_context(swarm_ui::ROOT_CONTEXT)
                .size_full()
                .flex()
                .bg(ground)
                .text_color(fg)
                .font_family(UI_FONT)
                .text_size(px(13.)),
        )
        .child(sidebar)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .m(px(8.))
                .ml_0()
                .bg(panel)
                .border_1()
                .border_color(line)
                .rounded(px(10.))
                .overflow_hidden()
                .child(toolbar)
                .child(
                    div()
                        .id("preview")
                        .role(Role::Main)
                        .aria_label(entry.name())
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p(px(24.))
                        .child(preview),
                ),
        )
        .into_any_element()
    }
}

fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("swarm_ui gallery".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn init(cx: &mut App) {
    swarm_ui::init(cx);

    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(COMPONENT_LIST)),
        KeyBinding::new("up", SelectPrevious, Some(COMPONENT_LIST)),
        KeyBinding::new("cmd-q", Quit, None),
    ]);

    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}

fn open_gallery(selected: usize, cx: &mut App) -> anyhow::Result<gpui::WindowHandle<Gallery>> {
    let options = window_options(cx);

    cx.open_window(options, |window, cx| {
        cx.new(|cx| Gallery::new(selected, window, cx))
    })
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    gpui_platform::application().run(|cx: &mut App| {
        init(cx);

        #[cfg(feature = "snapshot")]
        if let Ok(directory) = std::env::var("SWARM_GALLERY_SNAPSHOT_DIR") {
            snapshot::run(directory.into(), cx);
            return;
        }

        if let Err(error) = open_gallery(0, cx) {
            log::error!("could not open the gallery window: {error:#}");
            cx.quit();
            return;
        }

        cx.activate(true);
    });
}
