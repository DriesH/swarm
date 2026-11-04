//! The Close task dialog.

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Role, SharedString, Window, div,
    px,
};
use swarm_protocol::{ClientError, CloseCheck};
use swarm_ui::prelude::*;

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
    busy: bool,
    error: Option<SharedString>,
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
            busy: false,
            error: None,
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

    pub(crate) fn set_busy(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
    }

    pub(crate) fn fail(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message);
        cx.notify();
    }

    /// The check the user is looking at, once there is one.
    pub(crate) fn confirmed(&self) -> Option<CloseCheck> {
        match &self.check {
            CheckState::Ready(check) => Some(check.clone()),
            _ => None,
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(CloseTaskEvent::Dismissed);
        }
    }

    #[cfg(test)]
    pub(crate) fn check_state(&self) -> &CheckState {
        &self.check
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn can_confirm(&self) -> bool {
        !self.busy && matches!(self.check, CheckState::Ready(_))
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
        let busy = self.busy;

        if !busy && !self.can_confirm() {
            leave_confirm(&self.confirm_focus, &self.cancel_focus, window, cx);
        }

        let body = match &self.check {
            CheckState::Checking => div()
                .id("close-task-checking")
                .role(Role::Status)
                .aria_label("Checking")
                .child(Label::new("Checking").color(Color::Faint))
                .into_any_element(),
            CheckState::Failed(message) => div()
                .id("close-task-check-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .child(Label::new(message.clone()).color(Color::Del))
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

        let error = self.error.clone().map(|message| {
            div()
                .id("close-task-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        let (confirm_label, accessible_label) = if busy {
            ("Closing\u{2026}", format!("Closing task {}", self.name))
        } else {
            ("Close task", format!("Close task {}", self.name))
        };
        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("close-task-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .debug_selector(|| "close-task-cancel".to_owned())
                    .track_focus(&self.cancel_focus)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
            )
            .child(
                Button::new("close-task-confirm", confirm_label)
                    .variant(ButtonVariant::Destructive)
                    .accessible_label(accessible_label)
                    .debug_selector(|| "close-task-confirm".to_owned())
                    .track_focus(&self.confirm_focus)
                    .loading(busy)
                    .disabled(!busy && !self.can_confirm())
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
        .close_disabled(busy)
        .on_dismiss(move |_, cx| {
            on_dismiss.update(cx, Self::dismiss);
        })
        .child(body)
        .children(error)
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

        let (Some(client), Some(daemon_id)) = (self.daemon.clone(), daemon_id) else {
            self.close_overlay(window, cx);
            self.remove_closed_task(task, cx);
            return;
        };

        dialog.update(cx, CloseTaskDialog::set_busy);
        let again = client.clone();
        let closing = cx.background_spawn(async move { client.close_task(daemon_id, confirmed) });

        cx.spawn_in(window, async move |this, cx| {
            let result = closing.await;

            let refused = result.is_err();

            let updated = this.update_in(cx, |workspace, window, cx| match result {
                Ok(()) => {
                    if workspace.close_dialog_is_for(task, cx) {
                        workspace.close_overlay(window, cx);
                    }
                }
                Err(error) => {
                    log::warn!("cannot close the task: {error}");
                    dialog.update(cx, |dialog, cx| {
                        dialog.fail(describe(&error), cx);
                        dialog.set_check(CheckState::Checking, cx);
                    });
                }
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the task closed: {error}");
                return;
            }

            if refused {
                let state = match cx
                    .background_executor()
                    .spawn(async move { again.check_close(daemon_id) })
                    .await
                {
                    Ok(check) => CheckState::Ready(check),
                    Err(error) => CheckState::Failed(describe(&error)),
                };

                dialog.update(cx, |dialog, cx| dialog.set_check(state, cx));
            }
        })
        .detach();
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
    fn closing_the_dialog_gives_focus_back_to_what_had_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let row = workspace.read_with(cx, |workspace, _| workspace.sidebar_focus.clone());
        cx.update(|window, cx| window.focus(&row, cx));

        open_dialog(&workspace, cx);
        cx.simulate_keystrokes("escape");

        assert!(cx.update(|window, _| row.is_focused(window)));
    }
}
