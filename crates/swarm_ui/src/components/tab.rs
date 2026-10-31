use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, FontWeight, MouseButton, Pixels, Role,
    SharedString, Window, div, linear_color_stop, linear_gradient, px,
};

use crate::component::{Component, example, example_group};
use crate::components::icon_button::IconButtonSize;
use crate::fonts::UI_FONT;
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

/// The narrowest a tab gets while it shrinks in a crowded row; its label is cut with an
/// ellipsis. A tab whose natural width is smaller never grows to this: it keeps its width.
pub const TAB_MIN_WIDTH: Pixels = px(150.);

const HEIGHT: Pixels = px(32.);
const PADDING_START: Pixels = px(12.);
const PADDING_END: Pixels = px(12.);
/// Padding after the inline close button of the selected tab.
const PADDING_AFTER_BUTTON: Pixels = px(4.);
const GAP: Pixels = px(8.);
const BORDER: Pixels = px(1.);
const RADIUS: Pixels = px(8.);
/// How far the fade under a close button on a hovered tab reaches to the left of the button.
const FADE: Pixels = px(18.);

/// A task tab with a status ring.
#[derive(IntoElement)]
#[must_use]
pub struct Tab {
    id: ElementId,
    label: SharedString,
    status: AgentStatus,
    selected: bool,
    focus: Option<FocusHandle>,
    debug_selector: Option<String>,
    on_click: Option<ClickHandler>,
    on_close: Option<ClickHandler>,
}

impl Tab {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        status: AgentStatus,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status,
            selected: false,
            focus: None,
            debug_selector: None,
            on_click: None,
            on_close: None,
        }
    }

    /// Adds a close button after the label. It shows on hover, on keyboard focus and
    /// while the tab is selected.
    pub fn on_close(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Box::new(handler));
        self
    }

    /// Name for `VisualTestContext::debug_bounds`; the close button is `<name>-close`. Only
    /// recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    pub(crate) fn label(&self) -> &SharedString {
        &self.label
    }

    pub(crate) fn status(&self) -> AgentStatus {
        self.status
    }

    pub(crate) fn id(&self) -> &ElementId {
        &self.id
    }

    /// Lets the owner of a row of tabs follow keyboard focus. The handle must be a tab stop.
    pub(crate) fn focus_handle(mut self, handle: FocusHandle) -> Self {
        self.focus = Some(handle);
        self
    }
}

impl Toggleable for Tab {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Clickable for Tab {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

/// The width the tab takes with its label uncut: what its content needs, with no room
/// held back for a close button that is not shown.
fn natural_width(label: &SharedString, inline_button: bool, window: &Window) -> Pixels {
    let mut style = window.text_style();
    style.font_family = UI_FONT.into();
    style.font_weight = FontWeight::MEDIUM;
    style.font_size = LabelSize::Default.pixels().into();

    let first_line = label.lines().next().unwrap_or_default();
    let run = style.to_run(first_line.len());
    let text = window.text_system().shape_line(
        SharedString::from(first_line.to_owned()),
        LabelSize::Default.pixels(),
        &[run],
        None,
    );

    let ring = px(10.);
    let end = if inline_button {
        GAP + IconButtonSize::Small.square() + PADDING_AFTER_BUTTON
    } else {
        PADDING_END
    };

    // A pixel of slack: layout may measure the text a little wider than the shaped line, and
    // a tab squeezed below its natural width would cut the label.
    BORDER * 2. + PADDING_START + ring + GAP + text.width.ceil() + end + px(1.)
}

impl RenderOnce for Tab {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (raised, line, active, accent) =
            (colors.raised, colors.line, colors.active, colors.accent);
        let accessible_label =
            SharedString::from(format!("{}, {}", self.label, self.status.label()));
        let close_label = SharedString::from(format!("Close task {}", self.label));
        let group = SharedString::from(format!("tab-{:?}", self.id));
        let close_id = (self.id.clone(), "tab-close");
        let close_selector = self
            .debug_selector
            .as_ref()
            .map(|selector| format!("{selector}-close"));
        let selected = self.selected;

        let focus = self.focus.clone().unwrap_or_else(|| {
            window
                .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| {
                    cx.focus_handle().tab_stop(true)
                })
                .read(cx)
                .clone()
        });
        let keyboard_focus = window.last_input_was_keyboard() && focus.contains_focused(window, cx);

        // The selected tab keeps its close button in the row. On other tabs it floats over
        // the end of the label while the tab is hovered or has keyboard focus.
        let inline_button = self.on_close.is_some() && selected;
        let floating_button = self.on_close.is_some() && !selected;
        let min_width = TAB_MIN_WIDTH.min(natural_width(&self.label, inline_button, window));

        div()
            .id(self.id)
            .group(group.clone())
            .when_some(self.debug_selector, |element, selector| {
                element.debug_selector(move || selector)
            })
            .role(Role::Tab)
            .aria_label(accessible_label)
            .aria_selected(selected)
            .track_focus(&focus)
            .cursor_pointer()
            .relative()
            .flex()
            .items_center()
            .gap(GAP)
            .h(HEIGHT)
            .min_w(min_width)
            .pl(PADDING_START)
            .pr(if inline_button {
                PADDING_AFTER_BUTTON
            } else {
                PADDING_END
            })
            .rounded(RADIUS)
            .border_1()
            .map(|tab| {
                if selected {
                    tab.bg(raised).border_color(line)
                } else if keyboard_focus {
                    tab.bg(active).border_color(gpui::transparent_black())
                } else {
                    tab.border_color(gpui::transparent_black())
                        .hover(|style| style.bg(active))
                }
            })
            .focus_visible(|style| focus_ring(style, accent))
            .child(StatusRing::new(self.status))
            .child(
                Label::new(self.label)
                    .weight(FontWeight::MEDIUM)
                    .color(if selected {
                        Color::Default
                    } else {
                        Color::Muted
                    })
                    .truncate(),
            )
            .when_some(self.on_close, |tab, on_close| {
                let button = IconButton::new(close_id, IconName::Close, close_label)
                    .on_click(on_close)
                    .when_some(close_selector, |button, selector| {
                        button.debug_selector(move || selector)
                    });

                // Keeps the click from selecting the tab.
                let button = div()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(if keyboard_focus || inline_button {
                        button
                    } else {
                        button.reveal_with_group(group.clone())
                    });

                if !floating_button {
                    return tab.child(button);
                }

                let solid = active;
                let patch = div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .pl(FADE)
                    .pr(PADDING_AFTER_BUTTON)
                    .rounded_tr(RADIUS - BORDER)
                    .rounded_br(RADIUS - BORDER)
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(solid.opacity(0.), 0.),
                        linear_color_stop(solid, 0.4),
                    ))
                    .child(button);

                tab.child(if keyboard_focus {
                    patch
                } else {
                    patch
                        .opacity(0.)
                        .group_hover(group, |style| style.opacity(1.))
                })
            })
            .when_some(self.on_click, |tab, handler| tab.on_click(handler))
    }
}

impl Component for Tab {
    fn name() -> &'static str {
        "Tab"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("tab-demo", cx, |_, _| 0_usize);
        let selected = *state.read(cx);
        let tabs = [
            ("terminal-renderer", AgentStatus::Working),
            ("menu-bar-usage", AgentStatus::Blocked),
            ("worktree-scripts", AgentStatus::Idle),
            ("ui-tokens", AgentStatus::Done),
        ];

        v_flex()
            .gap_6()
            .child(example_group(
                "Tab bar",
                [example(
                    "Interactive",
                    div()
                        .id("tab-list")
                        .role(Role::TabList)
                        .aria_label("Tasks")
                        .flex()
                        .gap_1()
                        .children(
                            tabs.into_iter()
                                .enumerate()
                                .map(|(index, (label, status))| {
                                    let state = state.clone();
                                    Tab::new(("tab", index), label, status)
                                        .toggle_state(index == selected)
                                        .on_click(move |_, _, cx| {
                                            state.update(cx, |state, cx| {
                                                *state = index;
                                                cx.notify();
                                            });
                                        })
                                }),
                        ),
                )],
            ))
            .child(example_group(
                "Closable",
                [
                    example(
                        "Selected",
                        Tab::new("close-selected", "terminal-renderer", AgentStatus::Working)
                            .toggle_state(true)
                            .on_close(|_, _, _| {}),
                    ),
                    example(
                        "Shown on hover",
                        Tab::new("close-idle", "menu-bar-usage", AgentStatus::Blocked)
                            .on_close(|_, _, _| {}),
                    ),
                ],
            ))
            .child(example_group(
                "States",
                tabs.into_iter().enumerate().map(|(index, (_, status))| {
                    example(
                        status.label(),
                        Tab::new(("state", index), status.label(), status).toggle_state(index == 0),
                    )
                }),
            ))
            .into_any_element()
    }
}
