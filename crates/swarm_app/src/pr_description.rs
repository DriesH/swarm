//! The pull request description as a large tile over the app. It grows from the field of the
//! panel and shrinks back into it.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, AppContext as _, Bounds, Context, Entity,
    EventEmitter, FocusHandle, Focusable as _, FontWeight, MouseButton, Pixels, Role, SharedString,
    Size, Subscription, Task, Window, div, ease_out_quint, point, px, size,
};
use swarm_ui::prelude::*;
use swarm_ui::{DIALOG_CONTEXT, Dismiss, trap_focus};

use crate::markdown::{self, Markdown};
use crate::palette::Overlay;
use crate::pull_request::DraftWait;
use crate::review_model::plural;
use crate::workspace::{RightPanel, Workspace};

const OPEN_DURATION: Duration = Duration::from_millis(200);
const CLOSE_DURATION: Duration = Duration::from_millis(150);
/// The room the tile leaves around itself, at most, in a big window.
const MARGIN_SIDE: f32 = 180.;
const MARGIN_TOP: f32 = 84.;
const MARGIN_BOTTOM: f32 = 56.;
const BODY_WIDTH: f32 = 860.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Preview,
    Edit,
}

impl Mode {
    const ALL: [Self; 2] = [Self::Preview, Self::Edit];

    fn label(self) -> &'static str {
        match self {
            Self::Preview => "Preview",
            Self::Edit => "Edit",
        }
    }
}

/// What the Redraft button of the tile needs from the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DraftState {
    /// The description came from the orchestrator.
    pub(crate) drafted: bool,
    /// The approval holds, so the orchestrator can be asked again.
    pub(crate) available: bool,
    /// A new draft is on its way.
    pub(crate) waiting: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrDescriptionEvent {
    Dismissed,
    Redraft,
}

/// Where the tile stands: it grows, it is open, or it shrinks back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Opening,
    Closing,
}

pub(crate) struct PrDescriptionTile {
    title: SharedString,
    area: Entity<TextArea>,
    form_description: Entity<TextArea>,
    parsed: Markdown,
    mode: Mode,
    /// The field the tile grows from, in window coordinates.
    from: Option<Bounds<Pixels>>,
    phase: Phase,
    /// Makes each motion a new animation.
    serial: usize,
    draft: DraftState,
    focus_handle: FocusHandle,
    closing_timer: Option<Task<()>>,
    pub(crate) opener: Option<FocusHandle>,
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<PrDescriptionEvent> for PrDescriptionTile {}

impl gpui::Focusable for PrDescriptionTile {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Where the open tile ends up, in a window of this size.
pub(crate) fn target_bounds(window: Size<Pixels>) -> Bounds<Pixels> {
    let side = px(MARGIN_SIDE).min(window.width * 0.1);
    let top = px(MARGIN_TOP).min(window.height * 0.08);
    let bottom = px(MARGIN_BOTTOM).min(window.height * 0.06);

    Bounds {
        origin: point(side, top),
        size: size(
            window.width - side * 2.,
            (window.height - top - bottom).max(px(0.)),
        ),
    }
}

/// The bounds `progress` of the way from `from` to `to`.
pub(crate) fn between(from: Bounds<Pixels>, to: Bounds<Pixels>, progress: f32) -> Bounds<Pixels> {
    let mix = |start: Pixels, end: Pixels| start + (end - start) * progress;

    Bounds {
        origin: point(
            mix(from.origin.x, to.origin.x),
            mix(from.origin.y, to.origin.y),
        ),
        size: size(
            mix(from.size.width, to.size.width),
            mix(from.size.height, to.size.height),
        ),
    }
}

/// "412 words".
pub(crate) fn word_count(text: &str) -> String {
    plural(
        u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX),
        "word",
        "words",
    )
}

impl PrDescriptionTile {
    pub(crate) fn new(
        form_title: Entity<TextInput>,
        form_description: Entity<TextArea>,
        from: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let opener = window.focused(cx);
        let text = form_description.read(cx).text();

        let area = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Description", cx);
            area.set_mono(true, cx);
            area.set_fill(true, cx);
            area.set_text_at_start(text.clone(), cx);
            area
        });

        // The edits are the field's text: they go there as they are typed.
        let on_edit = cx.subscribe(&area, |this, area, event, cx| {
            if let TextInputEvent::Changed(_) = event {
                let text = area.read(cx).text();

                this.form_description
                    .update(cx, |field, cx| field.set_text(text, cx));
                this.refresh(cx);
            }
        });
        // A new draft fills the field while the tile is open.
        let on_description = cx.observe(&form_description, |this, field, cx| {
            let text = field.read(cx).text();

            if this.area.read(cx).text() != text {
                this.area
                    .update(cx, |area, cx| area.set_text_at_start(text, cx));
                this.refresh(cx);
            }
        });
        let on_title = cx.observe(&form_title, |this, input, cx| {
            this.title = input.read(cx).text();
            cx.notify();
        });

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let mut tile = Self {
            title: form_title.read(cx).text(),
            area,
            form_description,
            parsed: Markdown::default(),
            mode: Mode::Preview,
            from,
            phase: Phase::Opening,
            serial: 0,
            draft: DraftState::default(),
            focus_handle,
            closing_timer: None,
            opener,
            _subscriptions: [on_edit, on_description, on_title],
        };
        tile.refresh(cx);

        tile
    }

    fn text(&self, cx: &App) -> SharedString {
        self.area.read(cx).text()
    }

    /// Reads the text again for the preview.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.parsed = markdown::parse(&self.text(cx));
        cx.notify();
    }

    pub(crate) fn set_draft_state(&mut self, draft: DraftState, cx: &mut Context<Self>) {
        if self.draft != draft {
            self.draft = draft;
            cx.notify();
        }
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;

        if mode == Mode::Edit {
            window.focus(&self.area.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Shrinks the tile back into the field, or closes at once without the motion.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Closing || self.closing_timer.is_some() {
            return;
        }
        if self.from.is_none() || swarm_ui::motion::reduce_motion(cx) {
            cx.emit(PrDescriptionEvent::Dismissed);
            return;
        }

        self.phase = Phase::Closing;
        self.serial += 1;
        self.closing_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CLOSE_DURATION).await;

            let done = this.update(cx, |_, cx| cx.emit(PrDescriptionEvent::Dismissed));
            if let Err(error) = done {
                log::debug!("the window closed before the description shrank: {error}");
            }
        }));
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    #[cfg(test)]
    pub(crate) fn is_closing(&self) -> bool {
        self.phase == Phase::Closing
    }

    #[cfg(test)]
    pub(crate) fn draft_state(&self) -> DraftState {
        self.draft
    }

    #[cfg(test)]
    pub(crate) fn shown_text(&self, cx: &App) -> SharedString {
        self.text(cx)
    }

    #[cfg(test)]
    pub(crate) fn shown_title(&self) -> SharedString {
        self.title.clone()
    }

    #[cfg(test)]
    pub(crate) fn root_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PrDescriptionTile {
    fn header(&self, cx: &mut Context<Self>) -> gpui::Div {
        let line = cx.theme().colors().line;
        let this = cx.entity();
        let (on_mode, on_close) = (this.clone(), this);

        h_flex()
            .h(px(52.))
            .flex_none()
            .gap(px(12.))
            .px(px(16.))
            .border_b_1()
            .border_color(line)
            .child(Icon::new(IconName::PullRequest))
            .child(
                Label::new("Description")
                    .heading("pr-description-heading", 2)
                    .size(LabelSize::Large)
                    .weight(FontWeight::SEMIBOLD),
            )
            .children(self.draft.drafted.then(|| {
                Label::new("Drafted by orchestrator")
                    .color(Color::Muted)
                    .truncate()
            }))
            .child(div().flex_1())
            .child(
                SegmentedControl::new("pr-description-view", "View", Mode::ALL.map(Mode::label))
                    .selected(
                        Mode::ALL
                            .iter()
                            .position(|mode| *mode == self.mode)
                            .unwrap_or(0),
                    )
                    .debug_selector(|| "pr-description-view".to_owned())
                    .on_select(move |index, window, cx| {
                        let Some(mode) = Mode::ALL.get(index).copied() else {
                            return;
                        };

                        on_mode.update(cx, |this, cx| this.set_mode(mode, window, cx));
                    }),
            )
            .children((self.draft.available && self.draft.drafted).then(|| {
                Button::new(
                    "pr-description-redraft",
                    if self.draft.waiting {
                        "Drafting\u{2026}"
                    } else {
                        "Redraft"
                    },
                )
                .variant(ButtonVariant::Secondary)
                .loading(self.draft.waiting)
                .debug_selector(|| "pr-description-redraft".to_owned())
                .on_click(cx.listener(|_, _, _, cx| cx.emit(PrDescriptionEvent::Redraft)))
            }))
            .child(
                IconButton::new("pr-description-close", IconName::Close, "Close")
                    .debug_selector(|| "pr-description-close".to_owned())
                    .on_click(move |_, _, cx| on_close.update(cx, Self::close)),
            )
    }

    fn body(&self, colors: &swarm_ui::ThemeColors) -> AnyElement {
        let title = Label::new(self.title.clone())
            .heading("pr-description-title", 3)
            .size(LabelSize::Title)
            .weight(FontWeight::SEMIBOLD);

        match self.mode {
            Mode::Preview => div()
                .id("pr-description-preview")
                .role(Role::Document)
                .aria_label("Description preview")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .max_w(px(BODY_WIDTH))
                        .gap(px(16.))
                        .px(px(40.))
                        .py(px(28.))
                        .child(title)
                        .child(markdown::render(&self.parsed, false, colors)),
                )
                .into_any_element(),
            Mode::Edit => div()
                .flex_1()
                .min_h_0()
                .child(
                    v_flex()
                        .size_full()
                        .max_w(px(BODY_WIDTH))
                        .gap(px(16.))
                        .px(px(40.))
                        .py(px(28.))
                        .child(title)
                        .child(div().flex_1().min_h_0().child(self.area.clone())),
                )
                .into_any_element(),
        }
    }

    fn footer(&self, colors: &swarm_ui::ThemeColors, cx: &App) -> gpui::Div {
        h_flex()
            .h(px(48.))
            .flex_none()
            .gap(px(8.))
            .px(px(16.))
            .border_t_1()
            .border_color(colors.line)
            .child(
                Label::new(word_count(&self.text(cx)))
                    .mono()
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(div().flex_1())
            .child(Kbd::new("pr-description-esc", ["esc"]))
            .child(
                Label::new("Close")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }
}

impl Render for PrDescriptionTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = target_bounds(window.viewport_size());
        let this = cx.entity();
        let (on_scrim, on_key) = (this.clone(), this);

        let header = self.header(cx);
        let colors = cx.theme().colors();
        let body = self.body(colors);
        let footer = self.footer(colors, cx);

        let panel = swarm_ui::a11y::accessible(
            v_flex().id("pr-description"),
            Role::Dialog,
            "Pull request description",
        )
        .key_context(DIALOG_CONTEXT)
        .track_focus(&self.focus_handle)
        .on_action(move |_: &Dismiss, _, cx| on_key.update(cx, Self::close))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .size_full()
        .overflow_hidden()
        .rounded(px(14.))
        .border_2()
        .border_color(colors.accent)
        .bg(colors.panel)
        .child(header)
        .child(body)
        .child(footer);
        let panel = trap_focus(panel, self.focus_handle.clone())
            .a11y_flags()
            .modal(true);

        let place = |bounds: Bounds<Pixels>| {
            move |frame: gpui::Div| {
                frame
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
            }
        };
        let frame = div()
            .debug_selector(|| "pr-description".to_owned())
            .absolute()
            .child(panel);

        let from = self.from.filter(|_| !swarm_ui::motion::reduce_motion(cx));
        let panel = match (from, self.phase) {
            (None, _) => place(target)(frame).into_any_element(),
            (Some(from), Phase::Opening) => frame
                .with_animation(
                    ("pr-description-open", self.serial),
                    Animation::new(OPEN_DURATION).with_easing(ease_out_quint()),
                    move |frame, progress| place(between(from, target, progress))(frame),
                )
                .into_any_element(),
            (Some(from), Phase::Closing) => frame
                .with_animation(
                    ("pr-description-close", self.serial),
                    Animation::new(CLOSE_DURATION).with_easing(ease_out_quint()),
                    move |frame, progress| place(between(target, from, progress))(frame),
                )
                .into_any_element(),
        };

        div()
            .id("pr-description-scrim")
            .absolute()
            .inset_0()
            .bg(colors.scrim)
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                on_scrim.update(cx, Self::close);
            })
            .child(panel)
    }
}

impl Workspace {
    /// Opens the description of the new pull request as a large tile. Only the panel of a
    /// new pull request has a description to open.
    pub(crate) fn open_pr_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.active_task;
        let shows_form = self.visible_panel() == Some(RightPanel::PullRequest)
            && self.shows_new_pull_request(task)
            && !self.approval_outdated(task);
        if !shows_form || self.overlay.is_some() {
            return;
        }

        let Some(form) = self.pull_requests.form(task) else {
            return;
        };
        let (title, description) = (form.title.clone(), form.description.clone());
        let from = self.pull_requests.description_bounds.get();

        let view = cx.new(|cx| PrDescriptionTile::new(title, description, from, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                PrDescriptionEvent::Dismissed => this.close_overlay(window, cx),
                PrDescriptionEvent::Redraft => {
                    this.redraft(cx);
                    this.sync_description_tile(cx);
                }
            });

        self.overlay = Some(Overlay::PrDescription {
            view,
            _subscription: subscription,
        });
        self.sync_description_tile(cx);
        cx.notify();
    }

    /// Tells the open tile whether Redraft is there and whether a draft is on its way.
    pub(crate) fn sync_description_tile(&self, cx: &mut Context<Self>) {
        let Some(Overlay::PrDescription { view, .. }) = &self.overlay else {
            return;
        };

        let task = self.active_task;
        let state = self
            .pull_requests
            .form(task)
            .map(|form| DraftState {
                drafted: form.drafted(),
                available: self.data.approval_holds(task),
                waiting: form.draft_wait == DraftWait::Waiting,
            })
            .unwrap_or_default();

        view.update(cx, |tile, cx| tile.set_draft_state(state, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Size<Pixels> {
        size(px(1600.), px(1000.))
    }

    #[test]
    fn the_tile_leaves_the_margins_of_the_design_in_a_big_window() {
        let bounds = target_bounds(window());

        assert_eq!(bounds.origin, point(px(160.), px(80.)));
        assert_eq!(bounds.size, size(px(1280.), px(864.)));
    }

    #[test]
    fn the_margins_shrink_with_a_small_window() {
        let bounds = target_bounds(size(px(800.), px(500.)));

        assert_eq!(bounds.origin, point(px(80.), px(40.)));
    }

    #[test]
    fn the_tile_is_the_field_at_the_start_and_the_target_at_the_end() {
        let field = Bounds {
            origin: point(px(1200.), px(300.)),
            size: size(px(320.), px(190.)),
        };
        let target = target_bounds(window());

        assert_eq!(between(field, target, 0.), field);
        assert_eq!(between(field, target, 1.), target);
    }

    #[test]
    fn halfway_the_tile_is_half_the_way_in_every_direction() {
        let field = Bounds {
            origin: point(px(100.), px(100.)),
            size: size(px(200.), px(100.)),
        };
        let target = Bounds {
            origin: point(px(200.), px(300.)),
            size: size(px(400.), px(300.)),
        };

        let halfway = between(field, target, 0.5);

        assert_eq!(halfway.origin, point(px(150.), px(200.)));
        assert_eq!(halfway.size, size(px(300.), px(200.)));
    }

    #[test]
    fn the_word_count_counts_words_not_lines() {
        assert_eq!(word_count("## What\n\n- one two  three"), "6 words");
        assert_eq!(word_count("one"), "1 word");
        assert_eq!(word_count(""), "0 words");
    }
}

#[cfg(test)]
mod screen_tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::workspace::follow_test_reduce_motion;

    fn field() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(1000.), px(300.)),
            size: size(px(200.), px(100.)),
        }
    }

    fn open_tile(reduced: bool, cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(|cx| {
            swarm_ui::init(cx);
            follow_test_reduce_motion(reduced, cx);
        });

        let (_tile, cx) = cx.add_window_view(|window, cx| {
            let title = cx.new(TextInput::new);
            let description = cx.new(TextArea::new);

            PrDescriptionTile::new(title, description, Some(field()), window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx
    }

    #[gpui::test]
    fn the_tile_opens_at_once_under_reduce_motion(cx: &mut TestAppContext) {
        let cx = open_tile(true, cx);

        let bounds = cx.debug_bounds("pr-description");

        let target = cx.update(|window, _| target_bounds(window.viewport_size()));
        assert_eq!(bounds, Some(target));
    }

    #[gpui::test]
    fn the_tile_grows_from_the_field_when_motion_is_allowed(cx: &mut TestAppContext) {
        let cx = open_tile(false, cx);

        let bounds = cx.debug_bounds("pr-description");

        assert_eq!(bounds, Some(field()));
    }
}
