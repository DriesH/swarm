use gpui::{AnyElement, App, ElementId, Role, SharedString, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// How far a check has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    Waiting,
    Running,
    Passed,
    Failed,
    Skipped,
    Cancelled,
}

impl CheckStatus {
    pub const ALL: [Self; 6] = [
        Self::Waiting,
        Self::Running,
        Self::Passed,
        Self::Failed,
        Self::Skipped,
        Self::Cancelled,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Running => "Running",
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
        }
    }

    fn mark(self) -> AnyElement {
        match self {
            Self::Waiting => StatusRing::new(AgentStatus::Idle)
                .size(px(9.))
                .into_any_element(),
            Self::Running => StatusRing::new(AgentStatus::Working)
                .size(px(9.))
                .into_any_element(),
            Self::Passed => Icon::new(IconName::Check)
                .size(IconSize::Small)
                .color(Color::Ok)
                .into_any_element(),
            Self::Failed => Icon::new(IconName::Close)
                .size(IconSize::Small)
                .color(Color::Del)
                .into_any_element(),
            Self::Skipped | Self::Cancelled => Icon::new(IconName::Minus)
                .size(IconSize::Small)
                .color(Color::Faint)
                .into_any_element(),
        }
    }
}

/// One check of a list: a state mark, the name, a value such as a duration, and an
/// optional action at the end.
#[derive(IntoElement)]
#[must_use]
pub struct CheckRow {
    id: ElementId,
    name: SharedString,
    status: CheckStatus,
    status_label: Option<SharedString>,
    value: Option<SharedString>,
    mono: bool,
    action: Option<AnyElement>,
    debug_selector: Option<String>,
}

impl CheckRow {
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        status: CheckStatus,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status,
            status_label: None,
            value: None,
            mono: false,
            action: None,
            debug_selector: None,
        }
    }

    /// A value after the name, for example a duration. VoiceOver reads it.
    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Replaces the status word VoiceOver reads, for example "Stopped".
    pub fn status_label(mut self, label: impl Into<SharedString>) -> Self {
        self.status_label = Some(label.into());
        self
    }

    /// Shows the name in the mono font, for a command.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// A control at the end of the row, such as a button.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    fn status_word(&self) -> &str {
        self.status_label
            .as_deref()
            .unwrap_or_else(|| self.status.label())
    }

    /// What VoiceOver reads: "name, status, value".
    fn spoken(&self) -> String {
        let status = self.status_word();

        match &self.value {
            Some(value) => format!("{}, {status}, {value}", self.name),
            None => format!("{}, {status}", self.name),
        }
    }

    /// A check that did not run has no value, so the row says so in words.
    fn shown_word(&self) -> Option<SharedString> {
        matches!(self.status, CheckStatus::Skipped | CheckStatus::Cancelled)
            .then(|| SharedString::from(self.status_word().to_owned()))
    }
}

impl RenderOnce for CheckRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let spoken = self.spoken();
        let word = self.shown_word();
        let name = Label::new(self.name).size(LabelSize::Small).truncate();
        let name = if self.mono { name.mono() } else { name };

        h_flex()
            .id(self.id)
            .when_some(self.debug_selector, |row, selector| {
                row.debug_selector(move || selector)
            })
            .role(Role::ListItem)
            .aria_label(spoken)
            .min_h(px(28.))
            .gap(px(8.))
            .child(
                div()
                    .w(px(12.))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(self.status.mark()),
            )
            .child(div().flex_1().min_w_0().flex().child(name))
            .children(word.map(|word| {
                Label::new(word)
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint)
            }))
            .children(self.value.map(|value| {
                Label::new(value)
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint)
            }))
            .children(self.action)
    }
}

impl Component for CheckRow {
    fn name() -> &'static str {
        "CheckRow"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let rows = CheckStatus::ALL
            .into_iter()
            .enumerate()
            .map(|(index, status)| {
                let row = CheckRow::new(("check", index), "cargo test --workspace", status).mono();
                let row = match status {
                    CheckStatus::Waiting => row,
                    CheckStatus::Running => row.value("1m 02s"),
                    CheckStatus::Failed => row.value("exit 1").action(
                        Button::new(("output", index), "Show output").variant(ButtonVariant::Ghost),
                    ),
                    _ => row.value("41s"),
                };

                example(status.label(), div().w(px(300.)).child(row))
            });

        v_flex()
            .gap_6()
            .child(example_group("States", rows))
            .child(example_group(
                "Plain name",
                [example(
                    "Name",
                    div().w(px(300.)).child(
                        CheckRow::new("plain", "build-macos", CheckStatus::Running).value("2m 10s"),
                    ),
                )],
            ))
            .into_any_element()
    }
}
