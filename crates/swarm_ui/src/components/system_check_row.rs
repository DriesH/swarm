use gpui::{
    AnyElement, App, ElementId, FontWeight, Pixels, Role, SharedString, Window, div, px, svg,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// How one tool of the system check stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemCheckState {
    Ready,
    /// A check or a fix is running.
    InProgress,
    NotSetUp,
}

impl SystemCheckState {
    pub const ALL: [Self; 3] = [Self::Ready, Self::InProgress, Self::NotSetUp];

    /// The word VoiceOver reads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::InProgress => "In progress",
            Self::NotSetUp => "Not set up",
        }
    }
}

const MARK_SIZE: f32 = 18.;
const DASHED_STROKE: f32 = 1.5;

fn dashed_circle_svg() -> String {
    let center = MARK_SIZE / 2.;
    let radius = center - DASHED_STROKE / 2.;

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {MARK_SIZE} {MARK_SIZE}"><circle cx="{center}" cy="{center}" r="{radius}" fill="none" stroke="#000" stroke-width="{DASHED_STROKE}" stroke-dasharray="4 3.4"/></svg>"##
    )
}

/// One tool of the system check: a state mark, its name, a mono line with the version or
/// the status, and an optional action. A running fix shows its terminal under the row.
#[derive(IntoElement)]
#[must_use]
pub struct SystemCheckRow {
    id: ElementId,
    name: SharedString,
    state: SystemCheckState,
    detail: SharedString,
    action: Option<AnyElement>,
    output: Option<AnyElement>,
    output_height: Pixels,
    debug_selector: Option<String>,
}

impl SystemCheckRow {
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        state: SystemCheckState,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            state,
            detail: SharedString::default(),
            action: None,
            output: None,
            output_height: px(150.),
            debug_selector: None,
        }
    }

    /// The line under the name: a version, or what is going on.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = detail.into();
        self
    }

    /// A control at the end of the row, such as a button.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// Content under the row, for the terminal of a running fix. The row frames it. Give
    /// it its own accessible label.
    pub fn output(mut self, output: impl IntoElement) -> Self {
        self.output = Some(output.into_any_element());
        self
    }

    pub fn output_height(mut self, height: Pixels) -> Self {
        self.output_height = height;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// What VoiceOver reads: "name, state, detail".
    fn spoken(&self) -> String {
        let state = self.state.label();

        if self.detail.is_empty() {
            format!("{}, {state}", self.name)
        } else {
            format!("{}, {state}, {}", self.name, self.detail)
        }
    }
}

fn mark(state: SystemCheckState, cx: &App) -> AnyElement {
    let colors = cx.theme().colors();
    let frame = div()
        .size(px(MARK_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();

    match state {
        SystemCheckState::Ready => frame
            .rounded_full()
            .bg(colors.ok_bg)
            .child(
                Icon::new(IconName::Check)
                    .size(IconSize::XSmall)
                    .color(Color::Ok),
            )
            .into_any_element(),
        SystemCheckState::InProgress => frame
            .child(StatusRing::new(AgentStatus::Working).size(px(12.)))
            .into_any_element(),
        SystemCheckState::NotSetUp => frame
            .child(
                svg()
                    .data(dashed_circle_svg().as_bytes())
                    .size(px(MARK_SIZE))
                    .flex_none()
                    .text_color(colors.faint),
            )
            .into_any_element(),
    }
}

impl RenderOnce for SystemCheckRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let spoken = self.spoken();
        let colors = cx.theme().colors();
        let (line, term) = (colors.line, colors.term);

        let text = v_flex()
            .flex_1()
            .min_w_0()
            .child(Label::new(self.name).weight(FontWeight::MEDIUM).truncate())
            .when(!self.detail.is_empty(), |text| {
                text.child(
                    Label::new(self.detail)
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint)
                        .truncate(),
                )
            });

        v_flex()
            .id(self.id)
            .when_some(self.debug_selector, |row, selector| {
                row.debug_selector(move || selector)
            })
            .role(Role::ListItem)
            .aria_label(spoken)
            .child(
                h_flex()
                    .min_h(px(48.))
                    .px(px(16.))
                    .gap(px(12.))
                    .child(mark(self.state, cx))
                    .child(text)
                    .children(self.action),
            )
            .children(self.output.map(|output| {
                div()
                    .h(self.output_height)
                    .mt(px(0.))
                    .mb(px(12.))
                    .mr(px(16.))
                    .ml(px(46.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(line)
                    .bg(term)
                    .overflow_hidden()
                    .child(output)
            }))
    }
}

impl Component for SystemCheckRow {
    fn name() -> &'static str {
        "SystemCheckRow"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let states = SystemCheckState::ALL
            .into_iter()
            .enumerate()
            .map(|(index, state)| {
                let row = SystemCheckRow::new(("tool", index), "Claude Code", state);
                let row = match state {
                    SystemCheckState::Ready => row.detail("2.1.314 \u{b7} signed in"),
                    SystemCheckState::InProgress => row.detail("installing"),
                    SystemCheckState::NotSetUp => row
                        .detail("not installed")
                        .action(Button::new(("install", index), "Install")),
                };

                example(state.label(), div().w(px(420.)).child(row))
            });

        v_flex()
            .gap_6()
            .child(example_group("States", states))
            .child(example_group(
                "Dimmed action",
                [example(
                    "Waits on another row",
                    div().w(px(420.)).child(
                        SystemCheckRow::new(
                            "waiting",
                            "GitHub account",
                            SystemCheckState::NotSetUp,
                        )
                        .detail("needs GitHub CLI")
                        .action(Button::new("sign-in", "Sign in").disabled(true)),
                    ),
                )],
            ))
            .child(example_group(
                "With output",
                [example(
                    "Running fix",
                    div().w(px(420.)).child(
                        SystemCheckRow::new("running", "GitHub CLI", SystemCheckState::InProgress)
                            .detail("installing with Homebrew")
                            .output_height(px(96.))
                            .output(
                                v_flex()
                                    .p(px(8.))
                                    .child(
                                        Label::new("$ brew install gh")
                                            .mono()
                                            .size(LabelSize::XSmall),
                                    )
                                    .child(
                                        Label::new("==> Fetching gh")
                                            .mono()
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    ),
                            ),
                    ),
                )],
            ))
            .into_any_element()
    }
}
