//! The Close task dialog.

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Role, SharedString, Window, div,
    px,
};
use swarm_protocol::{ClientError, CloseCheck};
use swarm_ui::prelude::*;

use crate::announce::announce;
use crate::model::TaskId;
use crate::palette::Overlay;
use crate::review_model::plural;
use crate::workspace::Workspace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseTaskEvent {
    Dismissed,
    Confirmed,
}

/// Where a dialog stands with the check of what it will throw away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckState<T = CloseCheck> {
    Checking,
    Ready(T),
    Failed(SharedString),
}

/// "Stops 3 agents". Close task and Remove project use the same words.
pub(crate) fn stops_agents(count: u32) -> String {
    format!("Stops {}", plural(count, "agent", "agents"))
}

/// "Stops 2 terminals".
pub(crate) fn stops_terminals(count: u32) -> String {
    format!("Stops {}", plural(count, "terminal", "terminals"))
}

/// "Deletes 4 uncommitted files".
pub(crate) fn deletes_files(count: u32) -> String {
    format!(
        "Deletes {}",
        plural(count, "uncommitted file", "uncommitted files")
    )
}

/// Moves focus from Confirm to Cancel when Confirm can no longer take it, so the keyboard
/// keeps working inside the dialog.
pub(crate) fn leave_confirm<T: 'static>(
    confirm: &FocusHandle,
    cancel: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    if confirm.is_focused(window) {
        let cancel = cancel.clone();
        window.defer(cx, move |window, cx| window.focus(&cancel, cx));
    }
}

/// The open dialog. It reports what the user chose and leaves closing to its owner.
pub(crate) struct CloseTaskDialog {
    pub(crate) task: TaskId,
    name: SharedString,
    branch: SharedString,
    check: CheckState,
    focus_handle: FocusHandle,
    confirm_focus: FocusHandle,
    cancel_focus: FocusHandle,
    pub(crate) opener: Option<FocusHandle>,
}

impl EventEmitter<CloseTaskEvent> for CloseTaskDialog {}

impl gpui::Focusable for CloseTaskDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CloseTaskDialog {
    pub(crate) fn new(
        task: TaskId,
        name: SharedString,
        branch: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let opener = window.focused(cx);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            task,
            name,
            branch,
            check: CheckState::Checking,
            focus_handle,
            confirm_focus: cx.focus_handle().tab_stop(true),
            cancel_focus: cx.focus_handle().tab_stop(true),
            opener,
        }
    }

    pub(crate) fn set_check(&mut self, check: CheckState, cx: &mut Context<Self>) {
        self.check = check;
        cx.notify();
    }

    /// The check the user is looking at, once there is one.
    pub(crate) fn confirmed(&self) -> Option<CloseCheck> {
        match &self.check {
            CheckState::Ready(check) => Some(check.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn check_state(&self) -> &CheckState {
        &self.check
    }

    /// Speaks "Checking" while the check runs.
    fn status_region(&self) -> LiveRegion {
        let value = if matches!(self.check, CheckState::Checking) {
            "Checking"
        } else {
            ""
        };

        LiveRegion::status("close-task-status", value)
    }

    /// Speaks a failed check.
    fn alert_region(&self) -> LiveRegion {
        let message = match &self.check {
            CheckState::Failed(message) => message.clone(),
            _ => SharedString::default(),
        };

        LiveRegion::alert("close-task-alert", message)
    }

    fn can_confirm(&self) -> bool {
        matches!(self.check, CheckState::Ready(_))
    }

    #[cfg(test)]
    pub(crate) fn focus_handles(&self) -> (FocusHandle, FocusHandle) {
        (self.confirm_focus.clone(), self.cancel_focus.clone())
    }

    /// The value lines under the title, from what the check found.
    pub(crate) fn lines(&self) -> Vec<(String, Color)> {
        let CheckState::Ready(check) = &self.check else {
            return Vec::new();
        };
        let mut lines = Vec::new();

        if check.running_agents > 0 {
            lines.push((stops_agents(check.running_agents), Color::Default));
        }
        if check.user_terminals > 0 {
            lines.push((stops_terminals(check.user_terminals), Color::Default));
        }
        if check.uncommitted_files > 0 {
            lines.push((deletes_files(check.uncommitted_files), Color::Warn));
        }

        for branch in &check.merged_branches {
            lines.push((format!("Deletes merged branch {branch}"), Color::Default));
        }
        if !check
            .merged_branches
            .iter()
            .any(|branch| *branch == *self.branch)
        {
            lines.push((format!("Keeps branch {}", self.branch), Color::Default));
        }

        if !check.unmerged_workers.is_empty() {
            lines.push((
                format!("Not merged: {}", check.unmerged_workers.join(", ")),
                Color::Default,
            ));
        }

        lines
    }
}

impl Render for CloseTaskDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();

        if !self.can_confirm() {
            leave_confirm(&self.confirm_focus, &self.cancel_focus, window, cx);
        }

        let body = match &self.check {
            CheckState::Checking => Label::new("Checking")
                .color(Color::Faint)
                .into_any_element(),
            CheckState::Failed(message) => Label::new(message.clone())
                .color(Color::Del)
                .into_any_element(),
            CheckState::Ready(_) => v_flex()
                .id("close-task-lines")
                .role(Role::List)
                .aria_label("What closing does")
                .gap(px(6.))
                .children(
                    self.lines()
                        .into_iter()
                        .enumerate()
                        .map(|(index, (text, color))| {
                            div()
                                .id(("close-task-line", index))
                                .role(Role::ListItem)
                                .aria_label(text.clone())
                                .child(Label::new(text).color(color))
                        }),
                )
                .into_any_element(),
        };

        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("close-task-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .debug_selector(|| "close-task-cancel".to_owned())
                    .track_focus(&self.cancel_focus)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseTaskEvent::Dismissed))),
            )
            .child(
                Button::new("close-task-confirm", "Close task")
                    .variant(ButtonVariant::Destructive)
                    .accessible_label(format!("Close task {}", self.name))
                    .debug_selector(|| "close-task-confirm".to_owned())
                    .track_focus(&self.confirm_focus)
                    .disabled(!self.can_confirm())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.can_confirm() {
                            cx.emit(CloseTaskEvent::Confirmed);
                        }
                    })),
            );

        Dialog::new(
            "close-task-dialog",
            format!("Close {}?", self.name),
            &self.focus_handle,
        )
        .width(px(420.))
        .on_dismiss(move |_, cx| {
            on_dismiss.update(cx, |_, cx| cx.emit(CloseTaskEvent::Dismissed));
        })
        .child(body)
        .child(announce("close-task-alert", self.alert_region()))
        .child(announce("close-task-status", self.status_region()))
        .footer(footer)
    }
}

impl Workspace {
    /// What the app knows of a task without asking the daemon: its working agents.
    pub(crate) fn local_close_check(&self, task: TaskId) -> CloseCheck {
        let working = self
            .data
            .agents_of(task)
            .filter(|agent| agent.is_working())
            .count();

        CloseCheck {
            running_agents: u32::try_from(working).unwrap_or(u32::MAX),
            ..CloseCheck::default()
        }
    }

    /// True when the open dialog asks about this task.
    pub(crate) fn close_dialog_is_for(&self, task: TaskId, cx: &App) -> bool {
        matches!(
            &self.overlay,
            Some(Overlay::CloseTask { view, .. }) if view.read(cx).task == task
        )
    }

    /// Opens the dialog that asks before a task closes. A Start or Plan tab cannot close.
    pub(crate) fn open_close_task(
        &mut self,
        task: TaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.data.task(task).filter(|entry| !entry.is_start) else {
            return;
        };
        if !self.can_work() {
            return;
        }

        let (name, branch) = (entry.name.clone(), entry.branch.clone());
        let daemon_id = self.data.diff_id(task).filter(|_| entry.daemon.is_some());
        let local = self.local_close_check(task);

        let view = cx.new(|cx| CloseTaskDialog::new(task, name, branch, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                CloseTaskEvent::Dismissed => this.close_overlay(window, cx),
                CloseTaskEvent::Confirmed => this.confirm_close(view.clone(), window, cx),
            });

        self.overlay = Some(Overlay::CloseTask {
            view: view.clone(),
            _subscription: subscription,
        });
        cx.notify();

        let (Some(client), Some(daemon_id)) = (self.daemon.clone(), daemon_id) else {
            view.update(cx, |dialog, cx| {
                dialog.set_check(CheckState::Ready(local), cx);
            });
            return;
        };

        let checking = cx.background_spawn(async move { client.check_close(daemon_id) });

        cx.spawn(async move |_, cx| {
            let state = match checking.await {
                Ok(check) => CheckState::Ready(check),
                Err(error) => {
                    log::warn!("cannot check what closing the task does: {error}");
                    CheckState::Failed(error.to_string().into())
                }
            };

            view.update(cx, |dialog, cx| dialog.set_check(state, cx));
        })
        .detach();
    }

    /// Closes the dialog and swaps the task's surface for the cleanup screen at once. A
    /// refusal shows on that screen.
    fn confirm_close(
        &mut self,
        dialog: Entity<CloseTaskDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = dialog.read(cx).task;
        let Some(confirmed) = dialog.read(cx).confirmed() else {
            return;
        };
        let daemon_id = self
            .data
            .task(task)
            .filter(|entry| entry.daemon.is_some())
            .and_then(|_| self.data.diff_id(task));

        let (Some(_), Some(daemon_id)) = (self.daemon.as_ref(), daemon_id) else {
            self.close_overlay(window, cx);
            self.remove_closed_task(task, cx);
            return;
        };

        let deletes_branch = !confirmed.merged_branches.is_empty();

        self.close_overlay(window, cx);
        self.begin_cleanup(task, deletes_branch, Some(confirmed.clone()), cx);
        self.send_close(task, daemon_id, Some(confirmed), cx);
    }
}

pub(crate) fn describe(error: &ClientError) -> SharedString {
    error.to_string().into()
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::daemon::tests::{redraw, setup};
    use crate::live_probe::{Seen, assert_named, is_quiet, reversed, see_in};
    use swarm_ui::test_support::tab_walk;

    fn open_dialog(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<CloseTaskDialog> {
        let task = workspace.read_with(cx, |workspace, _| workspace.active_task);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_close_task(task, window, cx);
        });
        redraw(cx);

        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::CloseTask { view, .. }) => view.clone(),
            _ => panic!("the dialog is not open"),
        })
    }

    fn spoken(
        view: &Entity<CloseTaskDialog>,
        region: fn(&CloseTaskDialog) -> LiveRegion,
        cx: &mut VisualTestContext,
    ) -> Seen {
        let region = view.read_with(cx, |dialog, _| region(dialog));

        see_in(region, cx)
    }

    #[gpui::test]
    fn the_dialog_draws_the_regions_that_speak_its_state(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let _view = open_dialog(&workspace, cx);

        assert!(cx.debug_bounds("close-task-alert").is_some());
        assert!(cx.debug_bounds("close-task-status").is_some());
    }

    #[gpui::test]
    fn the_check_is_spoken_politely_until_it_is_ready(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        view.update(cx, |dialog, cx| dialog.set_check(CheckState::Checking, cx));
        let checking = spoken(&view, CloseTaskDialog::status_region, cx);
        view.update(cx, |dialog, cx| {
            dialog.set_check(CheckState::Ready(CloseCheck::default()), cx);
        });
        let ready = spoken(&view, CloseTaskDialog::status_region, cx);

        assert_eq!(checking.value.as_deref(), Some("Checking"));
        assert_eq!(checking.live, Some(Live::Polite));
        assert!(is_quiet(&ready));
    }

    #[gpui::test]
    fn a_failed_check_is_an_alert(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        view.update(cx, |dialog, cx| {
            dialog.set_check(CheckState::Failed("No daemon".into()), cx);
        });
        let check = spoken(&view, CloseTaskDialog::alert_region, cx);

        assert_eq!(check.value.as_deref(), Some("No daemon"));
        assert_eq!(check.live, Some(Live::Assertive));
    }

    #[gpui::test]
    fn focus_moves_to_cancel_when_a_new_check_disables_close(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let (confirm, cancel) = view.read_with(cx, |dialog, _| dialog.focus_handles());

        cx.update(|window, cx| window.focus(&confirm, cx));
        view.update(cx, |dialog, cx| dialog.set_check(CheckState::Checking, cx));
        redraw(cx);

        assert!(cx.update(|window, _| cancel.is_focused(window)));
        cx.simulate_keystrokes("escape");
        let closed = workspace.read_with(cx, |workspace, _| workspace.overlay.is_none());
        assert!(closed, "Escape still closes");
    }

    #[gpui::test]
    fn tab_walks_the_dialog_in_order_and_never_leaves_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let root = view.read_with(cx, |dialog, _| dialog.focus_handle.clone());
        let (_, cancel) = view.read_with(cx, |dialog, _| dialog.focus_handles());
        cx.update(|window, cx| window.focus(&cancel, cx));

        let forward = tab_walk(cx, &root, "tab");
        let backward = tab_walk(cx, &root, "shift-tab");

        assert_eq!(
            forward,
            [
                "Button Cancel",
                "Button Close task terminal-renderer",
                "Button Close dialog",
            ]
        );
        assert_eq!(backward, reversed(&forward));
        assert_named(&forward);
    }

    #[gpui::test]
    fn closing_the_dialog_gives_focus_back_to_what_had_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let row = workspace.read_with(cx, |workspace, _| workspace.sidebar_focus.clone());
        cx.update(|window, cx| window.focus(&row, cx));

        open_dialog(&workspace, cx);
        cx.simulate_keystrokes("escape");

        assert!(cx.update(|window, _| row.is_focused(window)));
    }
}
