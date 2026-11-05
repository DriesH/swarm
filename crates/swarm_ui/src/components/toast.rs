use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BoxShadow, ClickEvent, Context, ElementId,
    FocusHandle, FontWeight, Role, SharedString, Task, Window, div, ease_out_quint, point, px,
};

use crate::a11y::accessible;
use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::traits::ClickHandler;
use crate::{Dismiss, TOAST_CONTEXT};

type DismissHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;
type HoverHandler = Box<dyn Fn(bool, &mut Window, &mut App) + 'static>;

/// A short message that appears over the window: a title, one line of status, an optional
/// action and a dismiss button.
///
/// It is a status region named "title, message". The action and the dismiss button are
/// ordinary buttons in the tab order, and Escape dismisses it while focus is inside. It
/// blocks the mouse, so a click on it never reaches what is under it.
#[derive(IntoElement)]
#[must_use]
pub struct Toast {
    id: ElementId,
    title: SharedString,
    message: SharedString,
    detail: Option<SharedString>,
    leading: Option<AnyElement>,
    action: Option<(SharedString, ClickHandler)>,
    on_dismiss: Option<DismissHandler>,
    on_hover: Option<HoverHandler>,
    focus_handle: Option<FocusHandle>,
    debug_selector: Option<String>,
}

/// The toast is this wide.
pub const TOAST_WIDTH: f32 = 360.;

impl Toast {
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            message: message.into(),
            detail: None,
            leading: None,
            action: None,
            on_dismiss: None,
            on_hover: None,
            focus_handle: None,
            debug_selector: None,
        }
    }

    /// A line of numbers under the message.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Decoration before the text. Keep it out of the accessibility tree.
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    /// The one action, as a button: `Review`, for example.
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some((label.into(), Box::new(handler)));
        self
    }

    /// Called by the dismiss button and by Escape while focus is in the toast.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    /// Called when the pointer enters or leaves the toast.
    pub fn on_hover(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }

    /// Tracks focus for the whole toast: `handle.contains_focused` is true while focus is on
    /// the toast or in it.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }

    /// Names the parts `<selector>`, `<selector>-action` and `<selector>-dismiss` for
    /// `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (raised, line) = (colors.raised, colors.line);

        let label = format!("{}, {}", self.title, self.message);
        let selector = self.debug_selector.clone();

        let text = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .child(
                Label::new(self.title.clone())
                    .weight(FontWeight::SEMIBOLD)
                    .truncate(),
            )
            .child(
                Label::new(self.message)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .when_some(self.detail, |text, detail| {
                text.child(
                    Label::new(detail)
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint),
                )
            });

        let action = self.action.map(|(action_label, handler)| {
            Button::new((self.id.clone(), "action"), action_label.clone())
                .accessible_label(format!("{action_label} {}", self.title))
                .when_some(selector.clone(), |button, selector| {
                    button.debug_selector(move || format!("{selector}-action"))
                })
                .on_click(move |event, window, cx| handler(event, window, cx))
        });

        let dismiss = self.on_dismiss.clone().map(|handler| {
            IconButton::new(
                (self.id.clone(), "dismiss"),
                IconName::Close,
                "Dismiss notification",
            )
            .when_some(selector.clone(), |button, selector| {
                button.debug_selector(move || format!("{selector}-dismiss"))
            })
            .on_click(move |_, window, cx| handler(window, cx))
        });

        accessible(h_flex().id(self.id), Role::Status, label.clone())
            .aria_value(label)
            .when_some(selector, |toast, selector| {
                toast.debug_selector(move || selector)
            })
            .when_some(self.on_dismiss, |toast, handler| {
                toast
                    .key_context(TOAST_CONTEXT)
                    .on_action(move |_: &Dismiss, window, cx| handler(window, cx))
            })
            .when_some(self.focus_handle, |toast, handle| {
                toast.track_focus(&handle)
            })
            .when_some(self.on_hover, |toast, handler| {
                toast.on_hover(move |hovered, window, cx| handler(*hovered, window, cx))
            })
            .occlude()
            .w(px(TOAST_WIDTH))
            .gap(px(12.))
            .p(px(12.))
            .rounded(px(14.))
            .bg(raised)
            .border_1()
            .border_color(line)
            .shadow(vec![BoxShadow {
                color: colors.shadow,
                offset: point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
                inset: false,
            }])
            .children(self.leading)
            .child(text)
            .children(action)
            .children(dismiss)
            .a11y_flags()
            .live(Live::Polite)
    }
}

/// The toast slides in over this long.
pub const TOAST_ENTER: Duration = Duration::from_millis(180);
/// The toast fades out over this long.
pub const TOAST_LEAVE: Duration = Duration::from_millis(140);
/// How far a toast starts to the right of its place: its width and the gap to the window edge.
pub const TOAST_TRAVEL: f32 = TOAST_WIDTH + 48.;

/// Where a toast is in its life. Under Reduce Motion it goes straight to `Shown`, and from
/// there straight to `Removed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum ToastPhase {
    Entering,
    Shown,
    Leaving,
    Removed,
}

impl ToastPhase {
    /// The phase of a toast that was just put up.
    pub fn start(reduce_motion: bool) -> Self {
        if reduce_motion {
            Self::Shown
        } else {
            Self::Entering
        }
    }

    /// The phase after the toast is closed or times out.
    pub fn leave(self, reduce_motion: bool) -> Self {
        match self {
            Self::Entering | Self::Shown if reduce_motion => Self::Removed,
            Self::Entering | Self::Shown => Self::Leaving,
            Self::Leaving | Self::Removed => self,
        }
    }

    /// The phase when [`Self::duration`] has passed.
    pub fn next(self) -> Self {
        match self {
            Self::Entering => Self::Shown,
            Self::Leaving => Self::Removed,
            Self::Shown | Self::Removed => self,
        }
    }

    /// How long the phase lasts. `None` for a phase that waits for the next event.
    pub fn duration(self) -> Option<Duration> {
        match self {
            Self::Entering => Some(TOAST_ENTER),
            Self::Leaving => Some(TOAST_LEAVE),
            Self::Shown | Self::Removed => None,
        }
    }

    /// True while the toast is on screen.
    pub fn is_visible(self) -> bool {
        self != Self::Removed
    }

    /// True once the toast is closing.
    pub fn is_leaving(self) -> bool {
        self == Self::Leaving
    }
}

/// Carries a [`Toast`] through its phases: it slides in from the right, and fades out.
/// Under Reduce Motion GPUI draws the end state at once.
#[derive(IntoElement)]
#[must_use]
pub struct ToastSlot {
    id: ElementId,
    phase: ToastPhase,
    toast: AnyElement,
}

impl ToastSlot {
    /// `id` must change for each new toast, so the slide runs again.
    pub fn new(id: impl Into<ElementId>, phase: ToastPhase, toast: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            phase,
            toast: toast.into_any_element(),
        }
    }
}

impl RenderOnce for ToastSlot {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let slot = div().relative().child(self.toast);

        match self.phase {
            ToastPhase::Entering => slot
                .with_animation(
                    (self.id, "enter"),
                    Animation::new(TOAST_ENTER).with_easing(ease_out_quint()),
                    |slot, progress| slot.left(px((1. - progress) * TOAST_TRAVEL)),
                )
                .into_any_element(),
            ToastPhase::Leaving => slot
                .with_animation(
                    (self.id, "leave"),
                    Animation::new(TOAST_LEAVE),
                    |slot, progress| slot.opacity(1. - progress),
                )
                .into_any_element(),
            ToastPhase::Shown | ToastPhase::Removed => slot.into_any_element(),
        }
    }
}

/// The gallery's notice: a button shows it, and it closes the way the app's does.
struct ToastDemo {
    serial: usize,
    phase: ToastPhase,
    timer: Task<()>,
}

impl ToastDemo {
    fn new() -> Self {
        Self {
            serial: 0,
            phase: ToastPhase::Removed,
            timer: Task::ready(()),
        }
    }

    fn show(&mut self, cx: &mut Context<Self>) {
        self.serial += 1;
        self.phase = ToastPhase::start(cx.reduce_motion());
        self.run_phase(cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.phase = self.phase.leave(cx.reduce_motion());
        self.run_phase(cx);
        cx.notify();
    }

    /// Moves on to the next phase when this one is over.
    fn run_phase(&mut self, cx: &mut Context<Self>) {
        let (serial, phase) = (self.serial, self.phase);

        self.timer = match phase.duration() {
            Some(duration) => cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;

                let moved = this.update(cx, |demo, cx| {
                    if demo.serial == serial && demo.phase == phase {
                        demo.phase = phase.next();
                        demo.run_phase(cx);
                        cx.notify();
                    }
                });

                if let Err(error) = moved {
                    log::debug!("the demo closed before the toast moved on: {error}");
                }
            }),
            None => Task::ready(()),
        };
    }
}

impl Render for ToastDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ground = cx.theme().colors().ground;
        let demo = cx.weak_entity();
        let closing = demo.clone();

        let toast = self.phase.is_visible().then(|| {
            let slot = ToastSlot::new(
                ("toast-demo", self.serial),
                self.phase,
                Toast::new("toast-demo", "terminal-renderer", "Ready for review")
                    .action("Review", |_, _, _| {})
                    .on_dismiss(move |_, cx| {
                        if let Err(error) = closing.update(cx, ToastDemo::close) {
                            log::debug!("the demo closed before the toast: {error}");
                        }
                    }),
            );

            div().absolute().right(px(16.)).bottom(px(16.)).child(slot)
        });

        v_flex()
            .gap_3()
            .child(
                div().child(Button::new("toast-demo-show", "Show notice").on_click(
                    move |_, _, cx| {
                        if let Err(error) = demo.update(cx, ToastDemo::show) {
                            log::debug!("the demo closed before the button: {error}");
                        }
                    },
                )),
            )
            .child(
                div()
                    .relative()
                    .w(px(560.))
                    .h(px(150.))
                    .overflow_hidden()
                    .rounded(px(10.))
                    .bg(ground)
                    .children(toast),
            )
    }
}

impl Component for Toast {
    fn name() -> &'static str {
        "Toast"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let demo = window.use_keyed_state("toast-demo", cx, |_, _| ToastDemo::new());

        v_flex()
            .gap_6()
            .child(example_group("Motion", [example("Show and close", demo)]))
            .child(example_group(
                "States",
                [
                    example(
                        "Action and dismiss",
                        Toast::new("toast-action", "terminal-renderer", "Ready for review")
                            .detail("3 workers \u{b7} +312 \u{2212}48")
                            .action("Review", |_, _, _| {})
                            .on_dismiss(|_, _| {}),
                    ),
                    example(
                        "Message only",
                        Toast::new("toast-plain", "Settings saved", "Nothing else to do")
                            .on_dismiss(|_, _| {}),
                    ),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    #[test]
    fn a_toast_enters_is_shown_leaves_and_is_removed() {
        let entering = ToastPhase::start(false);
        let shown = entering.next();
        let leaving = shown.leave(false);
        let removed = leaving.next();

        assert_eq!(entering, ToastPhase::Entering);
        assert_eq!(shown, ToastPhase::Shown);
        assert_eq!(leaving, ToastPhase::Leaving);
        assert_eq!(removed, ToastPhase::Removed);
    }

    #[test]
    fn closing_a_toast_that_is_still_entering_starts_the_exit() {
        assert_eq!(ToastPhase::Entering.leave(false), ToastPhase::Leaving);
    }

    #[test]
    fn closing_twice_does_not_restart_the_exit() {
        assert_eq!(ToastPhase::Leaving.leave(false), ToastPhase::Leaving);
        assert_eq!(ToastPhase::Removed.leave(false), ToastPhase::Removed);
    }

    #[test]
    fn only_the_moving_phases_have_a_duration() {
        assert_eq!(ToastPhase::Entering.duration(), Some(TOAST_ENTER));
        assert_eq!(ToastPhase::Leaving.duration(), Some(TOAST_LEAVE));
        assert_eq!(ToastPhase::Shown.duration(), None);
        assert_eq!(ToastPhase::Removed.duration(), None);
    }

    #[test]
    fn reduce_motion_skips_the_moving_phases() {
        let shown = ToastPhase::start(true);

        assert_eq!(shown, ToastPhase::Shown);
        assert_eq!(shown.leave(true), ToastPhase::Removed);
    }

    #[test]
    fn the_enter_is_snappy_and_the_exit_is_shorter() {
        assert!(TOAST_ENTER >= Duration::from_millis(150));
        assert!(TOAST_ENTER <= Duration::from_millis(200));
        assert!(TOAST_LEAVE >= Duration::from_millis(120));
        assert!(TOAST_LEAVE <= Duration::from_millis(150));
    }

    fn demo(cx: &mut TestAppContext) -> (gpui::Entity<ToastDemo>, &mut VisualTestContext) {
        cx.update(crate::init);
        cx.add_window_view(|_, _| ToastDemo::new())
    }

    fn phase(demo: &gpui::Entity<ToastDemo>, cx: &VisualTestContext) -> ToastPhase {
        demo.read_with(cx, |demo, _| demo.phase)
    }

    #[gpui::test]
    fn the_demo_follows_the_phases_with_the_clock(cx: &mut TestAppContext) {
        let (demo, cx) = demo(cx);

        demo.update(cx, ToastDemo::show);
        let entering = phase(&demo, cx);

        cx.executor().advance_clock(TOAST_ENTER);
        cx.run_until_parked();
        let shown = phase(&demo, cx);

        demo.update(cx, ToastDemo::close);
        let leaving = phase(&demo, cx);

        cx.executor().advance_clock(TOAST_LEAVE / 2);
        cx.run_until_parked();
        let almost = phase(&demo, cx);

        cx.executor().advance_clock(TOAST_LEAVE / 2);
        cx.run_until_parked();
        let removed = phase(&demo, cx);

        assert_eq!(
            [entering, shown, leaving, almost, removed],
            [
                ToastPhase::Entering,
                ToastPhase::Shown,
                ToastPhase::Leaving,
                ToastPhase::Leaving,
                ToastPhase::Removed,
            ]
        );
    }

    #[gpui::test]
    fn the_demo_shows_and_removes_at_once_under_reduce_motion(cx: &mut TestAppContext) {
        let (demo, cx) = demo(cx);
        cx.update(|_, cx| cx.set_reduce_motion(true));

        demo.update(cx, ToastDemo::show);
        let shown = phase(&demo, cx);

        demo.update(cx, ToastDemo::close);
        let removed = phase(&demo, cx);

        assert_eq!([shown, removed], [ToastPhase::Shown, ToastPhase::Removed]);
    }
}
