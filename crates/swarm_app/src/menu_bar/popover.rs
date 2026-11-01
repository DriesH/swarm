//! The popover under the status item: usage, the tasks that need you, and three actions.
//!
//! It reads the workspace's data and never opens a daemon connection of its own.

use std::time::Duration;

use gpui::{
    App, Context, Entity, FocusHandle, FontWeight, KeyBinding, Render, Role, SharedString,
    Subscription, Task, Window, div, px,
};
use swarm_protocol::Client;
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;
use swarm_ui::{Dismiss, ROOT_CONTEXT, theme};

use super::summary::{MeterRow, NeedsYou, Summary, UsageSection};
use crate::actions::Quit;
use crate::lifecycle;
use crate::model::TaskId;
use crate::workspace::Workspace;

pub(crate) const WIDTH: f32 = 320.;

/// Key context of the popover. Esc closes it.
pub(crate) const CONTEXT: &str = "MenuBarPopover";

/// How often the "resets in" notes are worked out again.
const CLOCK: Duration = Duration::from_secs(30);

const PADDING: f32 = 12.;
const BORDER: f32 = 1.;
const HEADER_HEIGHT: f32 = 28.;
const SECTION_GAP: f32 = 14.;
/// The footer: the line above it, the Stop all row, and the Open Swarm and Quit row.
const FOOTER_HEIGHT: f32 = 75.;

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(CONTEXT))]);
}

/// The window height for what `summary` shows. The popover cannot change its height once it
/// is open (the window may not move), so it is worked out up front. A test keeps it equal to
/// the height of the laid out content.
pub(crate) fn height(summary: &Summary) -> f32 {
    let mut sections: Vec<f32> = summary.usage.iter().map(usage_height).collect();
    if !summary.needs_you.is_empty() {
        sections.push(needs_you_height(&summary.needs_you));
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "a handful of sections fit in f32 exactly"
    )]
    let gaps = sections.len().saturating_sub(1) as f32 * SECTION_GAP;
    let content = sections.iter().sum::<f32>() + gaps;

    // The header, the content and the footer sit in one column with a gap between them.
    let inner = HEADER_HEIGHT + SECTION_GAP + content + SECTION_GAP + FOOTER_HEIGHT;

    (inner + PADDING * 2. + BORDER * 2.).ceil()
}

/// The title, then a gap and a meter for each window, then the problem line.
fn usage_height(section: &UsageSection) -> f32 {
    const TITLE: f32 = 18.;
    const METER: f32 = 26.;
    const METER_NOTE: f32 = 20.5;
    const PROBLEM: f32 = 17.;
    const GAP: f32 = 8.;

    let meters: f32 = section
        .meters
        .iter()
        .map(|meter| GAP + METER + if meter.note.is_some() { METER_NOTE } else { 0. })
        .sum();
    let problem = if section.problem.is_some() {
        GAP + PROBLEM
    } else {
        0.
    };

    TITLE + meters + problem
}

/// The heading, then the rows with a gap before each.
fn needs_you_height(rows: &[NeedsYou]) -> f32 {
    const HEADING: f32 = 17.;
    const ROW: f32 = 30.;
    const ROW_WITH_REASON: f32 = 48.;
    const GAP: f32 = 2.;

    let rows: f32 = rows
        .iter()
        .map(|row| {
            GAP + if row.reason.is_empty() {
                ROW
            } else {
                ROW_WITH_REASON
            }
        })
        .sum();

    HEADING + rows
}

/// What the Stop all control is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StopState {
    Idle,
    /// Waiting for the user to confirm.
    Asking,
    Stopping,
    Failed(SharedString),
}

/// What gets keyboard focus at the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wanted {
    First,
    StopAll,
    Cancel,
    OpenSwarm,
}

struct Handles {
    root: FocusHandle,
    stop_all: FocusHandle,
    cancel: FocusHandle,
    confirm: FocusHandle,
    open_swarm: FocusHandle,
    quit: FocusHandle,
    needs_you: Vec<FocusHandle>,
}

pub(crate) struct Popover {
    workspace: Entity<Workspace>,
    stop: StopState,
    handles: Handles,
    wanted: Option<Wanted>,
    /// False in a snapshot, where the popover sits in a normal window.
    closes_itself: bool,
    _subscriptions: Vec<Subscription>,
    _clock: Option<Task<()>>,
}

impl Popover {
    pub(crate) fn new(
        workspace: Entity<Workspace>,
        closes_itself: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            theme::follow_system_appearance(window, cx),
            swarm_ui::motion::follow_reduce_motion(window, cx),
        ];

        if closes_itself {
            subscriptions.push(cx.observe_window_activation(window, |_, window, _| {
                if !window.is_window_active() {
                    window.remove_window();
                }
            }));
        }

        let clock = workspace.read(cx).fixed_now.is_none().then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(CLOCK).await;

                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            })
        });

        let stop = |cx: &mut Context<Self>| cx.focus_handle().tab_stop(true);
        let handles = Handles {
            root: cx.focus_handle(),
            stop_all: stop(cx),
            cancel: stop(cx),
            confirm: stop(cx),
            open_swarm: stop(cx),
            quit: stop(cx),
            needs_you: Vec::new(),
        };
        window.focus(&handles.root, cx);

        Self {
            workspace,
            stop: StopState::Idle,
            handles,
            wanted: Some(Wanted::First),
            closes_itself,
            _subscriptions: subscriptions,
            _clock: clock,
        }
    }

    fn summary(&self, cx: &App) -> Summary {
        let workspace = self.workspace.read(cx);

        Summary::of(&workspace.data, workspace.now_ms() / 1_000)
    }

    /// Moves focus where `wanted` says. The first control is the first task that needs the
    /// user, else Stop all when it can be used, else Open Swarm. A control that is disabled
    /// or gone cannot keep the focus: it goes to Open Swarm.
    fn apply_focus(&mut self, summary: &Summary, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted.take().or_else(|| {
            self.focus_is_stranded(summary, window)
                .then_some(Wanted::OpenSwarm)
        });
        let Some(wanted) = wanted else {
            return;
        };

        let can_stop = summary.running > 0 && self.stop == StopState::Idle;
        let handle = match wanted {
            Wanted::Cancel => self.handles.cancel.clone(),
            Wanted::First if !summary.needs_you.is_empty() => self.handles.needs_you[0].clone(),
            Wanted::First | Wanted::StopAll if can_stop => self.handles.stop_all.clone(),
            Wanted::First | Wanted::StopAll | Wanted::OpenSwarm => self.handles.open_swarm.clone(),
        };

        window.focus(&handle, cx);
    }

    /// True when the focused control is disabled or not on screen.
    fn focus_is_stranded(&self, summary: &Summary, window: &Window) -> bool {
        let handles = &self.handles;
        let mut stranded: Vec<&FocusHandle> = Vec::new();

        if self.stop != StopState::Idle || summary.running == 0 {
            stranded.push(&handles.stop_all);
        }
        if matches!(self.stop, StopState::Idle | StopState::Stopping) {
            stranded.extend([&handles.cancel, &handles.confirm]);
        }
        stranded.extend(handles.needs_you.iter().skip(summary.needs_you.len()));

        stranded.into_iter().any(|handle| handle.is_focused(window))
    }

    fn ask(&mut self, cx: &mut Context<Self>) {
        self.stop = StopState::Asking;
        self.wanted = Some(Wanted::Cancel);
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.stop = StopState::Idle;
        self.wanted = Some(Wanted::StopAll);
        cx.notify();
    }

    /// Stops every agent through the daemon, off the main thread.
    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.stop == StopState::Stopping {
            return;
        }

        let client = self
            .workspace
            .read(cx)
            .daemon
            .clone()
            .filter(Client::is_connected);
        let Some(client) = client else {
            self.stop = StopState::Failed("The daemon is not connected".into());
            cx.notify();
            return;
        };

        self.stop = StopState::Stopping;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.stop_agents() })
                .await;

            let shown = this.update(cx, |popover, cx| {
                match result {
                    Ok(()) => {
                        popover.stop = StopState::Idle;
                        popover.wanted = Some(Wanted::StopAll);
                    }
                    Err(error) => {
                        popover.stop =
                            StopState::Failed(format!("Could not stop the agents: {error}").into());
                    }
                }

                cx.notify();
            });

            if let Err(error) = shown {
                log::debug!("the popover closed before the agents stopped: {error}");
            }
        })
        .detach();
    }

    fn open_task(&mut self, task: TaskId, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.select_task(task, cx));

        self.open_swarm(window, cx);
    }

    fn open_swarm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closes_itself {
            window.remove_window();
        }

        lifecycle::show_main_window(cx);
    }

    /// Quits the way the Quit key does. The daemon keeps running.
    fn quit(window: &mut Window, cx: &mut App) {
        window.dispatch_action(Box::new(Quit), cx);
    }
}

impl Render for Popover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = self.summary(cx);

        if summary.running == 0 && self.stop == StopState::Asking {
            self.stop = StopState::Idle;
        }
        while self.handles.needs_you.len() < summary.needs_you.len() {
            self.handles
                .needs_you
                .push(cx.focus_handle().tab_stop(true));
        }
        self.apply_focus(&summary, window, cx);

        let colors = cx.theme().colors();
        let (panel, line, fg) = (colors.panel, colors.line, colors.fg);

        let mut sections: Vec<gpui::AnyElement> = summary.usage.iter().map(usage_section).collect();
        if !summary.needs_you.is_empty() {
            sections.push(self.needs_you_section(&summary.needs_you, cx));
        }

        swarm_ui::tab_navigation(
            v_flex()
                .id("menu-bar-popover")
                .role(Role::Group)
                .aria_label("Swarm")
                .key_context(format!("{ROOT_CONTEXT} {CONTEXT}").as_str())
                .track_focus(&self.handles.root)
                .on_action(cx.listener(|this, _: &Dismiss, window, _| {
                    if this.closes_itself {
                        window.remove_window();
                    }
                }))
                .size_full()
                .p(px(PADDING))
                .gap(px(SECTION_GAP))
                .border_1()
                .border_color(line)
                .bg(panel)
                .text_color(fg)
                .font_family(UI_FONT)
                .text_size(px(13.)),
        )
        .child(header(summary.running))
        .child(
            v_flex()
                .id("popover-content")
                .debug_selector(|| "popover-content".to_owned())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .debug_selector(|| "popover-sections".to_owned())
                        .gap(px(SECTION_GAP))
                        .children(sections),
                ),
        )
        .child(self.footer(&summary, cx))
    }
}

impl Popover {
    fn needs_you_section(&self, rows: &[NeedsYou], cx: &mut Context<Self>) -> gpui::AnyElement {
        let list = rows.iter().enumerate().map(|(index, row)| {
            let task = row.task;
            let list_row = ListRow::new(("popover-needs-you", index), row.title())
                .debug_selector(move || format!("popover-needs-you-{index}"))
                .accessible_label(row.spoken())
                .start_slot(StatusDot::new(AgentStatus::Blocked))
                .track_focus(&self.handles.needs_you[index])
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_task(task, window, cx);
                }));

            if row.reason.is_empty() {
                list_row
            } else {
                list_row.detail(row.reason.clone())
            }
        });

        v_flex()
            .id("popover-needs-you")
            .debug_selector(|| "popover-needs-you".to_owned())
            .role(Role::List)
            .aria_label("Needs you")
            .gap(px(2.))
            .child(
                Label::new("Needs you")
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM)
                    .color(Color::Faint),
            )
            .children(list)
            .into_any_element()
    }

    fn footer(&self, summary: &Summary, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let line = cx.theme().colors().line;

        let stop_row = match &self.stop {
            StopState::Idle => Button::new("popover-stop-all", "Stop all")
                .full_width()
                .disabled(summary.running == 0)
                .track_focus(&self.handles.stop_all)
                .debug_selector(|| "popover-stop-all".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.ask(cx)))
                .into_any_element(),
            state => {
                let (question, error) = match state {
                    StopState::Failed(message) => (message.clone(), true),
                    StopState::Stopping => ("Stopping agents".into(), false),
                    _ => (stop_question(summary.running).into(), false),
                };

                ConfirmRow::new("popover-confirm-stop", question, "Stop")
                    .destructive(true)
                    .error(error)
                    .busy(*state == StopState::Stopping)
                    .track_focus(&self.handles.cancel, &self.handles.confirm)
                    .debug_selector(|| "popover-stop".to_owned())
                    .on_cancel(cx.listener(|this, _, _, cx| this.cancel(cx)))
                    .on_confirm(cx.listener(|this, _, _, cx| this.confirm(cx)))
                    .into_any_element()
            }
        };

        v_flex()
            .debug_selector(|| "popover-footer".to_owned())
            .pt(px(10.))
            .gap(px(8.))
            .border_t_1()
            .border_color(line)
            .child(div().min_h(px(28.)).child(stop_row))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div().flex_1().child(
                            Button::new("popover-open-swarm", "Open Swarm")
                                .variant(ButtonVariant::Primary)
                                .full_width()
                                .track_focus(&self.handles.open_swarm)
                                .debug_selector(|| "popover-open-swarm".to_owned())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_swarm(window, cx);
                                })),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            Button::new("popover-quit", "Quit")
                                .full_width()
                                .track_focus(&self.handles.quit)
                                .debug_selector(|| "popover-quit".to_owned())
                                .on_click(|_, window, cx| Self::quit(window, cx)),
                        ),
                    ),
            )
    }
}

fn stop_question(running: usize) -> String {
    match running {
        1 => "Stop 1 agent?".to_owned(),
        count => format!("Stop {count} agents?"),
    }
}

/// For example "3 agents running".
pub(crate) fn running_text(running: usize) -> String {
    match running {
        0 => "No agents running".to_owned(),
        1 => "1 agent running".to_owned(),
        count => format!("{count} agents running"),
    }
}

fn header(running: usize) -> impl IntoElement {
    let text = running_text(running);

    h_flex()
        .id("popover-header")
        .role(Role::Group)
        .aria_label(format!("Swarm, {text}"))
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .gap(px(8.))
        .child(BrandMark::new())
        .child(
            Label::new("Swarm")
                .size(LabelSize::Large)
                .weight(FontWeight::SEMIBOLD),
        )
        .child(div().flex_1())
        .child(Label::new(text).size(LabelSize::Small).color(Color::Faint))
}

/// The title, a meter for each window and the problem line of one agent kind. Settings ›
/// Usage shows the same sections.
pub(crate) fn usage_section(section: &UsageSection) -> gpui::AnyElement {
    let id = section.id;
    let meters = section
        .meters
        .iter()
        .enumerate()
        .map(|(index, meter)| meter_of(id, index, meter));

    let problem = section.problem.as_ref().map(|(shown, spoken)| {
        div()
            .id(SharedString::from(format!("{id}-problem")))
            .debug_selector(move || format!("{id}-problem"))
            .role(Role::Status)
            .aria_label(spoken.clone())
            .child(
                Label::new(shown.clone())
                    .size(LabelSize::Small)
                    .color(Color::Warn),
            )
    });

    v_flex()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Group)
        .aria_label(format!("{} usage", section.title))
        .gap(px(8.))
        .child(
            Label::new(section.title.clone())
                .weight(FontWeight::MEDIUM)
                .truncate(),
        )
        .children(meters)
        .children(problem)
        .into_any_element()
}

fn meter_of(section: &'static str, index: usize, row: &MeterRow) -> UsageMeter {
    let meter = UsageMeter::new((section, index), row.label.clone())
        .value(format!("{}%", row.percent))
        .fraction(f32::from(row.percent) / 100.)
        .accessible_label(row.spoken.clone());

    match &row.note {
        Some(note) => meter.note(note.clone()),
        None => meter,
    }
}

#[cfg(feature = "snapshot")]
impl Popover {
    pub(super) fn ask_for_snapshot(&mut self, cx: &mut Context<Self>) {
        self.ask(cx);
    }

    pub(super) fn fail_for_snapshot(&mut self, cx: &mut Context<Self>) {
        self.stop =
            StopState::Failed("Could not stop the agents: the daemon did not answer".into());
        cx.notify();
    }
}

#[cfg(test)]
impl Popover {
    /// The control that has keyboard focus, by name.
    fn focused_control(&self, window: &Window) -> Option<String> {
        let named = [
            ("stop-all", &self.handles.stop_all),
            ("cancel", &self.handles.cancel),
            ("confirm", &self.handles.confirm),
            ("open-swarm", &self.handles.open_swarm),
            ("quit", &self.handles.quit),
        ];

        let needs_you = self
            .handles
            .needs_you
            .iter()
            .enumerate()
            .map(|(index, handle)| (format!("needs-you-{index}"), handle));

        named
            .into_iter()
            .map(|(name, handle)| (name.to_owned(), handle))
            .chain(needs_you)
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(name, _)| name)
    }

    /// The error shown in place of the Stop all question.
    fn failure(&self) -> Option<SharedString> {
        match &self.stop {
            StopState::Failed(message) => Some(message.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use swarm_protocol::{
        AgentRole, AgentStatus as DaemonAgentStatus, Event, Request, SubscriptionUsage, Usage,
        UsageProblem, UsageWindow,
    };
    use swarm_term::BlockedKind;

    use super::super::harness::{self, activate, popover_is_open, popover_window, setup};
    use super::*;
    use crate::daemon::tests::{
        agent_info, fake_daemon_replying, project_info, task_info, use_projects, wait_until,
    };
    use crate::model::{SAMPLE_NOW, TaskStatus};

    const NOW: u64 = SAMPLE_NOW / 1_000;

    fn window(label: &str, percent: u8, resets: Option<u64>) -> UsageWindow {
        UsageWindow {
            label: label.to_owned(),
            used_percent: percent,
            resets_at: resets,
        }
    }

    fn open(bar: &harness::Bar, cx: &mut VisualTestContext) -> VisualTestContext {
        bar.click(cx);

        let mut popover = popover_window(cx).expect("the popover is open");
        activate(&mut popover);
        draw(&mut popover);

        popover
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    /// Clicks the element. The window can be gone afterwards.
    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            Modifiers::none(),
        );
        cx.run_until_parked();
    }

    fn press(selector: &'static str, cx: &mut VisualTestContext) {
        click(selector, cx);
        draw(cx);
    }

    fn shown(selector: &'static str, cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds(selector).is_some()
    }

    fn read<R>(cx: &mut VisualTestContext, read: impl FnOnce(&Popover, &Window) -> R) -> R {
        let handle = cx
            .read_global(|menu: &super::super::MenuBar, _| menu.popover)
            .expect("the popover is open");

        handle
            .update(cx, |popover, window, _| read(popover, window))
            .expect("read the popover")
    }

    fn focused(cx: &mut VisualTestContext) -> Option<String> {
        read(cx, Popover::focused_control)
    }

    fn quiet(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            for task in &mut workspace.data.tasks {
                task.status = TaskStatus::Idle;
            }
            cx.notify();
        });
    }

    /// A workspace of real tasks with `statuses` as running agents of the first one.
    fn running_agents(
        workspace: &Entity<Workspace>,
        statuses: &[DaemonAgentStatus],
        cx: &mut VisualTestContext,
    ) {
        use_projects(
            workspace,
            &[project_info(
                5,
                "demo",
                vec![task_info(7, 5, "login"), task_info(8, 5, "signup")],
            )],
            cx,
        );

        workspace.update_in(cx, |workspace, window, cx| {
            for (index, status) in statuses.iter().enumerate() {
                let mut info = agent_info(index as u64 + 1, 7, *status);
                info.role = AgentRole::Worker;
                info.name = format!("worker-{index}");

                workspace.agent_changed(&info, Some(window), cx);
            }
        });
    }

    #[test]
    fn the_header_says_how_many_agents_run() {
        assert_eq!(running_text(0), "No agents running");
        assert_eq!(running_text(1), "1 agent running");
        assert_eq!(running_text(3), "3 agents running");
    }

    #[test]
    fn the_question_says_how_many_agents_stop() {
        assert_eq!(stop_question(1), "Stop 1 agent?");
        assert_eq!(stop_question(3), "Stop 3 agents?");
    }

    #[gpui::test]
    fn the_popover_lists_each_agent_kind_and_the_tasks_that_need_you(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let summary = workspace.read_with(cx, |workspace, _| {
            Summary::of(&workspace.data, workspace.now_ms() / 1_000)
        });
        let mut popover = open(&bar, cx);

        assert_eq!(summary.usage.len(), 2);
        assert_eq!(summary.needs_you.len(), 2);
        assert!(shown("popover-usage-claude", &mut popover));
        assert!(shown("popover-usage-codex", &mut popover));
        assert!(shown("popover-needs-you", &mut popover));
        assert!(shown("popover-needs-you-0", &mut popover));
        assert!(shown("popover-needs-you-1", &mut popover));
        assert!(!shown("popover-needs-you-2", &mut popover));
        assert!(shown("popover-stop-all", &mut popover));
        assert!(shown("popover-open-swarm", &mut popover));
        assert!(shown("popover-quit", &mut popover));
    }

    #[gpui::test]
    fn the_needs_you_section_is_hidden_when_no_task_needs_you(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        quiet(&workspace, cx);

        let mut popover = open(&bar, cx);

        assert!(!shown("popover-needs-you", &mut popover));
        assert!(shown("popover-usage-claude", &mut popover));
    }

    #[gpui::test]
    fn an_agent_kind_without_data_has_no_usage_section(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            usage.codex = SubscriptionUsage {
                problem: Some(UsageProblem::NotInstalled),
                ..Default::default()
            };
            workspace.usage_changed(usage, cx);
        });

        let mut popover = open(&bar, cx);

        assert!(shown("popover-usage-claude", &mut popover));
        assert!(!shown("popover-usage-codex", &mut popover));
    }

    #[gpui::test]
    fn the_popover_follows_the_model_while_it_is_open(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let mut popover = open(&bar, cx);
        assert!(shown("popover-needs-you-0", &mut popover));

        quiet(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(Usage::default(), cx);
        });
        draw(&mut popover);

        assert!(!shown("popover-needs-you-0", &mut popover));
        assert!(!shown("popover-usage-claude", &mut popover));
    }

    #[gpui::test]
    fn the_window_is_as_tall_as_its_content(cx: &mut TestAppContext) {
        let now = NOW;
        let both = Usage {
            claude: SubscriptionUsage {
                windows: vec![window("5h", 42, Some(now + 7_200)), window("7d", 18, None)],
                plan: Some("max".to_owned()),
                problem: Some(UsageProblem::NotLoggedIn),
                ..Default::default()
            },
            codex: SubscriptionUsage {
                windows: vec![window("5h", 7, Some(now + 600))],
                ..Default::default()
            },
        };
        let cases = [
            ("nothing", Usage::default(), 0, false),
            ("usage", both.clone(), 0, false),
            ("tasks", Usage::default(), 2, true),
            ("tasks without a reason", Usage::default(), 2, false),
            ("both", both, 1, true),
        ];

        for (name, usage, rows, reason) in cases {
            let (workspace, bar, cx) = setup(cx);
            workspace.update(cx, |workspace, cx| {
                for task in &mut workspace.data.tasks {
                    task.status = TaskStatus::Idle;
                }
                for task in workspace.data.tasks.iter_mut().take(rows) {
                    task.status = TaskStatus::NeedsYou;
                }
                for agent in &mut workspace.data.agents {
                    agent.status = AgentStatus::Idle;
                    agent.note = reason.then(|| "permission".into());
                    if reason {
                        agent.status = AgentStatus::Blocked;
                    }
                }
                workspace.usage_changed(usage, cx);
            });
            let mut popover = open(&bar, cx);

            let content = popover.debug_bounds("popover-content");
            let sections = popover.debug_bounds("popover-sections");
            let slack = content
                .zip(sections)
                .map(|(content, sections)| f32::from(content.size.height - sections.size.height));

            assert!(
                slack.is_some_and(|slack| (0. ..1.).contains(&slack)),
                "{name}: the content has {slack:?} points to spare"
            );
        }
    }

    #[gpui::test]
    fn the_first_control_has_focus_and_tab_moves_through_the_controls(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        let mut popover = open(&bar, cx);

        let mut order = vec![focused(&mut popover)];
        for _ in 0..4 {
            popover.simulate_keystrokes("tab");
            order.push(focused(&mut popover));
        }

        let expected = [
            "needs-you-0",
            "needs-you-1",
            "stop-all",
            "open-swarm",
            "quit",
        ];
        assert_eq!(order, expected.map(|name| Some(name.to_owned())));
    }

    #[gpui::test]
    fn shift_tab_moves_back(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        let mut popover = open(&bar, cx);

        popover.simulate_keystrokes("tab");
        popover.simulate_keystrokes("shift-tab");

        assert_eq!(focused(&mut popover).as_deref(), Some("needs-you-0"));
    }

    #[gpui::test]
    fn without_tasks_that_need_you_the_first_control_is_stop_all(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        quiet(&workspace, cx);

        let mut popover = open(&bar, cx);

        assert_eq!(focused(&mut popover).as_deref(), Some("stop-all"));
    }

    #[gpui::test]
    fn without_running_agents_stop_all_is_skipped_by_focus(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        quiet(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            for agent in &mut workspace.data.agents {
                agent.status = AgentStatus::Done;
            }
            cx.notify();
        });

        let mut popover = open(&bar, cx);
        let first = focused(&mut popover);
        popover.simulate_keystrokes("tab");
        let second = focused(&mut popover);
        popover.simulate_keystrokes("shift-tab");
        popover.simulate_keystrokes("shift-tab");

        assert_eq!(first.as_deref(), Some("open-swarm"));
        assert_eq!(second.as_deref(), Some("quit"));
        assert_ne!(focused(&mut popover).as_deref(), Some("stop-all"));
    }

    #[gpui::test]
    fn stop_all_does_nothing_without_running_agents(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        quiet(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            for agent in &mut workspace.data.agents {
                agent.status = AgentStatus::Done;
            }
            cx.notify();
        });
        let mut popover = open(&bar, cx);

        press("popover-stop-all", &mut popover);

        assert!(!shown("popover-stop-confirm", &mut popover));
        assert!(shown("popover-stop-all", &mut popover));
    }

    /// A fake daemon that counts the stop requests and answers them with `answer`.
    fn stopping_daemon(
        answer: impl Fn(swarm_protocol::CallId) -> Event + Send + 'static,
    ) -> (std::path::PathBuf, flume::Receiver<()>) {
        let (stopped, stops) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::StopAgents { call } => {
                if let Err(error) = stopped.send(()) {
                    log::error!("the test stopped counting the stops: {error}");
                }

                Some(answer(call))
            }
            _ => None,
        });

        (socket, stops)
    }

    fn three_running<'a>(
        socket: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, harness::Bar, &'a mut VisualTestContext) {
        harness::init(cx);
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.executor().allow_parking();
        let client = Client::connect(socket).expect("connect to the fake daemon");
        running_agents(
            &workspace,
            &[
                DaemonAgentStatus::Working,
                DaemonAgentStatus::Working,
                DaemonAgentStatus::Idle,
            ],
            cx,
        );
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });
        let bar = harness::install(&workspace, cx);

        (workspace, bar, cx)
    }

    #[gpui::test]
    fn stop_all_asks_in_place_and_cancel_stops_nothing(cx: &mut TestAppContext) {
        let (socket, stops) = stopping_daemon(|call| Event::Done { call });
        let (_workspace, bar, cx) = three_running(&socket, cx);
        let mut popover = open(&bar, cx);

        press("popover-stop-all", &mut popover);
        let asking = shown("popover-stop-confirm", &mut popover);
        let button_gone = !shown("popover-stop-all", &mut popover);
        let focus = focused(&mut popover);
        press("popover-stop-cancel", &mut popover);

        assert!(asking);
        assert!(button_gone);
        assert_eq!(focus.as_deref(), Some("cancel"));
        assert!(!shown("popover-stop-confirm", &mut popover));
        assert!(shown("popover-stop-all", &mut popover));
        assert_eq!(focused(&mut popover).as_deref(), Some("stop-all"));
        popover.executor().allow_parking();
        popover.run_until_parked();
        assert_eq!(stops.try_iter().count(), 0);
    }

    #[gpui::test]
    fn the_question_names_the_number_of_agents(cx: &mut TestAppContext) {
        let (socket, _stops) = stopping_daemon(|call| Event::Done { call });
        let (workspace, bar, cx) = three_running(&socket, cx);
        let running = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .agents
                .iter()
                .filter(|agent| agent.is_running())
                .count()
        });
        let mut popover = open(&bar, cx);

        press("popover-stop-all", &mut popover);

        assert_eq!(running, 3);
        assert_eq!(stop_question(running), "Stop 3 agents?");
        assert!(shown("popover-stop-confirm", &mut popover));
    }

    #[gpui::test]
    fn stop_calls_the_daemon_once(cx: &mut TestAppContext) {
        let (socket, stops) = stopping_daemon(|call| Event::Done { call });
        let (_workspace, bar, cx) = three_running(&socket, cx);
        let mut popover = open(&bar, cx);

        press("popover-stop-all", &mut popover);
        press("popover-stop-confirm", &mut popover);
        let called = wait_until(&mut popover, |_| !stops.is_empty());
        let finished = wait_until(&mut popover, |cx| {
            draw(cx);
            shown("popover-stop-all", cx)
        });

        assert!(called, "the daemon was never asked");
        assert!(finished, "the confirm row stays");
        assert_eq!(stops.try_iter().count(), 1);
    }

    #[gpui::test]
    fn a_failed_stop_shows_the_error_in_place(cx: &mut TestAppContext) {
        let (socket, stops) = stopping_daemon(|call| Event::Error {
            call: Some(call),
            id: None,
            message: "no permission".to_owned(),
        });
        let (_workspace, bar, cx) = three_running(&socket, cx);
        let mut popover = open(&bar, cx);

        press("popover-stop-all", &mut popover);
        press("popover-stop-confirm", &mut popover);
        let failed = wait_until(&mut popover, |cx| {
            read(cx, |popover, _| popover.failure().is_some())
        });
        draw(&mut popover);

        let message = read(&mut popover, |popover, _| popover.failure());
        assert!(failed, "no error shown");
        assert!(
            message.is_some_and(|message| message.starts_with("Could not stop the agents")
                && message.contains("no permission"))
        );
        assert!(shown("popover-stop-confirm", &mut popover));
        assert!(shown("popover-stop-cancel", &mut popover));
        assert_eq!(stops.try_iter().count(), 1);
    }

    #[gpui::test]
    fn without_a_daemon_stop_shows_an_error_and_asks_nobody(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.data.agents.iter_mut().for_each(|agent| {
                agent.status = AgentStatus::Working;
            });
            cx.notify();
        });
        let mut popover = open(&bar, cx);

        press("popover-stop-all", &mut popover);
        press("popover-stop-confirm", &mut popover);

        let message = read(&mut popover, |popover, _| popover.failure());
        assert_eq!(message.as_deref(), Some("The daemon is not connected"));
    }

    #[gpui::test]
    fn a_needs_you_click_opens_the_main_window_on_that_task(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        running_agents(&workspace, &[DaemonAgentStatus::Working], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            let mut info = agent_info(9, 8, DaemonAgentStatus::Blocked(BlockedKind::Permission));
            info.message = Some("Allow cargo test?".to_owned());

            workspace.agent_changed(&info, Some(window), cx);
        });
        let (open_task, signup) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task,
                workspace
                    .data
                    .task_with_daemon_id(swarm_protocol::TaskId(8)),
            )
        });
        let mut popover = open(&bar, cx);
        assert!(shown("popover-needs-you-0", &mut popover));

        click("popover-needs-you-0", &mut popover);

        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        assert_ne!(Some(open_task), signup);
        assert_eq!(Some(active), signup);
        assert!(!popover_is_open(cx), "the popover stays open");
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn a_needs_you_click_brings_the_main_window_back_when_it_is_closed(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        running_agents(&workspace, &[DaemonAgentStatus::Working], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            let info = agent_info(9, 8, DaemonAgentStatus::Blocked(BlockedKind::Question));

            workspace.agent_changed(&info, Some(window), cx);
        });
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
        assert!(cx.windows().is_empty());
        let mut popover = open(&bar, cx);

        click("popover-needs-you-0", &mut popover);

        let (window, active, signup) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.window,
                workspace.active_task,
                workspace
                    .data
                    .task_with_daemon_id(swarm_protocol::TaskId(8)),
            )
        });
        assert!(window.is_some());
        assert_eq!(Some(active), signup);
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn open_swarm_focuses_the_main_window_and_closes_the_popover(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        let mut popover = open(&bar, cx);
        let behind = cx.update(|window, _| window.is_window_active());
        let activations = cx.update(|_, cx| lifecycle::activations(cx));

        click("popover-open-swarm", &mut popover);

        let in_front = cx.update(|window, _| window.is_window_active());
        assert!(!behind, "the popover has the key window");
        assert!(in_front, "the main window did not become the key window");
        let activated = cx.update(|_, cx| lifecycle::activations(cx)) - activations;
        assert_eq!(activated, 1, "the app was not brought to the front once");
        assert!(!popover_is_open(cx));
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn open_swarm_opens_a_window_when_the_main_window_is_closed(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
        let mut popover = open(&bar, cx);

        click("popover-open-swarm", &mut popover);

        let window = workspace.read_with(cx, |workspace, _| workspace.window);
        assert!(window.is_some());
        assert!(!popover_is_open(cx));
        assert_eq!(cx.windows().len(), 1);
    }

    /// Puts the Stop all control in `state`, as the work it starts would.
    fn set_stop(state: StopState, cx: &mut VisualTestContext) {
        let handle = cx
            .read_global(|menu: &super::super::MenuBar, _| menu.popover)
            .expect("the popover is open");

        handle
            .update(cx, |popover, _, cx| {
                popover.stop = state;
                cx.notify();
            })
            .expect("update the popover");
    }

    /// What the daemon sends once every agent ended.
    fn end_agents(workspace: &Entity<Workspace>, count: u64, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            for id in 1..=count {
                let mut info = agent_info(id, 7, DaemonAgentStatus::Idle);
                info.role = AgentRole::Worker;
                info.name = format!("worker-{id}");
                info.lifecycle = swarm_protocol::AgentLifecycle::Stopped;

                workspace.agent_changed(&info, Some(window), cx);
            }
        });
    }

    #[gpui::test]
    fn while_the_agents_stop_the_disabled_buttons_hand_focus_to_open_swarm(
        cx: &mut TestAppContext,
    ) {
        let (socket, _stops) = stopping_daemon(|call| Event::Done { call });
        let (_workspace, bar, cx) = three_running(&socket, cx);
        let mut popover = open(&bar, cx);
        press("popover-stop-all", &mut popover);
        popover.simulate_keystrokes("tab");
        let on_stop = focused(&mut popover);

        set_stop(StopState::Stopping, &mut popover);
        draw(&mut popover);

        assert_eq!(on_stop.as_deref(), Some("confirm"));
        assert_eq!(focused(&mut popover).as_deref(), Some("open-swarm"));
    }

    #[gpui::test]
    fn after_a_stop_that_ended_every_agent_focus_leaves_the_disabled_stop_all(
        cx: &mut TestAppContext,
    ) {
        let (socket, _stops) = stopping_daemon(|call| Event::Done { call });
        let (workspace, bar, cx) = three_running(&socket, cx);
        let mut popover = open(&bar, cx);
        press("popover-stop-all", &mut popover);
        press("popover-stop-confirm", &mut popover);
        let finished = wait_until(&mut popover, |cx| {
            draw(cx);
            shown("popover-stop-all", cx)
        });
        let after_the_answer = focused(&mut popover);

        end_agents(&workspace, 3, cx);
        draw(&mut popover);

        assert!(finished);
        assert_eq!(after_the_answer.as_deref(), Some("stop-all"));
        assert_eq!(focused(&mut popover).as_deref(), Some("open-swarm"));
    }

    #[gpui::test]
    fn when_every_agent_ends_during_the_question_focus_goes_to_open_swarm(cx: &mut TestAppContext) {
        let (socket, _stops) = stopping_daemon(|call| Event::Done { call });
        let (workspace, bar, cx) = three_running(&socket, cx);
        let mut popover = open(&bar, cx);
        press("popover-stop-all", &mut popover);
        let asking = focused(&mut popover);

        end_agents(&workspace, 3, cx);
        draw(&mut popover);

        assert_eq!(asking.as_deref(), Some("cancel"));
        assert!(shown("popover-stop-all", &mut popover));
        assert!(!shown("popover-stop-confirm", &mut popover));
        assert_eq!(focused(&mut popover).as_deref(), Some("open-swarm"));
    }

    #[gpui::test]
    fn when_the_last_task_that_needs_you_goes_away_focus_goes_to_open_swarm(
        cx: &mut TestAppContext,
    ) {
        let (workspace, bar, cx) = setup(cx);
        let mut popover = open(&bar, cx);
        let first = focused(&mut popover);

        quiet(&workspace, cx);
        draw(&mut popover);

        assert_eq!(first.as_deref(), Some("needs-you-0"));
        assert!(!shown("popover-needs-you-0", &mut popover));
        assert_eq!(focused(&mut popover).as_deref(), Some("open-swarm"));
    }

    #[gpui::test]
    fn a_control_that_stays_keeps_the_focus(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let mut popover = open(&bar, cx);
        popover.simulate_keystrokes("tab");
        popover.simulate_keystrokes("tab");
        let before = focused(&mut popover);

        workspace.update(cx, |_, cx| cx.notify());
        draw(&mut popover);

        assert_eq!(before.as_deref(), Some("stop-all"));
        assert_eq!(focused(&mut popover).as_deref(), Some("stop-all"));
    }

    #[gpui::test]
    fn quit_quits_the_app(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        let (quit, quits) = flume::unbounded();
        cx.update(|_, cx| {
            cx.on_action(move |_: &Quit, _| {
                quit.send(()).expect("the test counts the quits");
            });
        });
        let mut popover = open(&bar, cx);

        click("popover-quit", &mut popover);

        assert_eq!(quits.try_iter().count(), 1);
    }
}
