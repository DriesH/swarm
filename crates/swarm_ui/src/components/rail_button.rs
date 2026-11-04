use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, Hsla, Pixels, Role, SharedString, Window,
    div, px, rgb,
};

use super::tooltip::Tooltip;
use crate::a11y::accessible;
use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

const SIZE: Pixels = px(36.);

/// A square button of the collapsed sidebar: a project tile or an icon, with an
/// optional status dot. The label is required: VoiceOver reads it.
#[derive(IntoElement)]
#[must_use]
pub struct RailButton {
    id: ElementId,
    label: SharedString,
    child: AnyElement,
    tooltip: Option<SharedString>,
    badge: Option<AgentStatus>,
    toggle_state: bool,
    debug_selector: Option<String>,
    focus_handle: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
}

impl RailButton {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        child: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            child: child.into_any_element(),
            tooltip: None,
            badge: None,
            toggle_state: false,
            debug_selector: None,
            focus_handle: None,
            on_click: None,
        }
    }

    /// The text that shows on hover. VoiceOver reads the label, not this.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// A status dot in the corner. A decoration: the label carries the status.
    pub fn badge(mut self, status: Option<AgentStatus>) -> Self {
        self.badge = status;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// Tracks focus with `handle`, so the owner can move focus onto the button. Create the
    /// handle with `tab_stop(true)` to keep the button in the Tab order.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }
}

impl Clickable for RailButton {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

/// The selected button: the open project or the focused terminal.
impl Toggleable for RailButton {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.toggle_state = selected;
        self
    }
}

impl RenderOnce for RailButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (active, accent) = (colors.active, colors.accent);

        accessible(div().id(self.id), Role::Button, self.label)
            .when_some(self.debug_selector, |button, selector| {
                button.debug_selector(move || selector)
            })
            .aria_selected(self.toggle_state)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(SIZE)
            .rounded(px(8.))
            .when(self.toggle_state, |button| button.bg(active))
            .map(|button| match &self.focus_handle {
                Some(handle) => button.track_focus(handle),
                None => button.focusable().tab_stop(true),
            })
            .cursor_pointer()
            .hover(|style| style.bg(active))
            .focus_visible(|style| focus_ring(style, accent))
            .when_some(self.tooltip, |button, text| {
                button.tooltip(move |_, cx| Tooltip::text(text.clone(), cx))
            })
            .when_some(self.on_click, |button, handler| button.on_click(handler))
            .child(self.child)
            .children(self.badge.map(|status| {
                div()
                    .absolute()
                    .right(px(4.))
                    .bottom(px(4.))
                    .child(StatusDot::new(status))
            }))
    }
}

impl Component for RailButton {
    fn name() -> &'static str {
        "RailButton"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let indigo: Hsla = rgb(0x004f_46e5).into();
        let tile = || ProjectTile::new('S', indigo).size(px(22.));
        let agent = || Icon::new(IconName::ClaudeCode).size(IconSize::Large);

        v_flex()
            .gap_6()
            .child(example_group(
                "Project",
                [
                    example("Default", RailButton::new("project", "swarm", tile())),
                    example(
                        "Selected",
                        RailButton::new("project-on", "swarm", tile()).toggle_state(true),
                    ),
                ],
            ))
            .child(example_group(
                "Agent",
                [
                    example(
                        "Working",
                        RailButton::new("agent-working", "pty-io", agent())
                            .badge(Some(AgentStatus::Working)),
                    ),
                    example(
                        "Blocked",
                        RailButton::new("agent-blocked", "term-render", agent())
                            .badge(Some(AgentStatus::Blocked)),
                    ),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{Context, Modifiers, Render, TestAppContext};

    use super::*;

    struct Harness {
        clicks: Rc<Cell<u32>>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let clicks = Rc::clone(&self.clicks);

            div().size_full().child(
                RailButton::new("button", "swarm", Icon::new(IconName::Plus))
                    .debug_selector(|| "button".to_owned())
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
            )
        }
    }

    #[gpui::test]
    fn a_click_runs_the_handler(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);
        let clicks = Rc::new(Cell::new(0));
        let shared = Rc::clone(&clicks);
        let (_, cx) = cx.add_window_view(move |_, _| Harness { clicks: shared });
        cx.run_until_parked();

        let center = cx.debug_bounds("button").expect("on screen").center();
        cx.simulate_click(center, Modifiers::none());

        assert_eq!(clicks.get(), 1);
    }
}
