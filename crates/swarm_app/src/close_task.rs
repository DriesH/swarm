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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckState {
    Checking,
    Ready(CloseCheck),
    Failed(SharedString),
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
}

impl EventEmitter<CloseTaskEvent> for CloseTaskDialog {}

impl CloseTaskDialog {
    pub(crate) fn new(
        task: TaskId,
        name: SharedString,
        branch: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

    /// The value lines under the title, from what the check found.
    pub(crate) fn lines(&self) -> Vec<(String, Color)> {
        let CheckState::Ready(check) = &self.check else {
            return Vec::new();
        };
        let mut lines = Vec::new();

        if check.running_agents > 0 {
            lines.push((
                format!("Stops {}", plural(check.running_agents, "agent", "agents")),
                Color::Default,
            ));
        }
        if check.user_terminals > 0 {
            lines.push((
                format!(
                    "Stops {}",
                    plural(check.user_terminals, "terminal", "terminals")
                ),
                Color::Default,
            ));
        }
        if check.uncommitted_files > 0 {
            lines.push((
                format!(
                    "Deletes {} uncommitted {}",
                    check.uncommitted_files,
                    if check.uncommitted_files == 1 {
                        "file"
                    } else {
                        "files"
                    }
                ),
                Color::Warn,
            ));
        }

        lines.push((format!("Keeps branch {}", self.branch), Color::Default));

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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();
        let busy = self.busy;

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

        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("close-task-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .debug_selector(|| "close-task-cancel".to_owned())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
            )
            .child(
                Button::new("close-task-confirm", "Close task")
                    .variant(ButtonVariant::Destructive)
                    .debug_selector(|| "close-task-confirm".to_owned())
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
        let local = CloseCheck {
            running_agents: u32::try_from(
                self.data
                    .agents_of(task)
                    .filter(|agent| agent.is_working())
                    .count(),
            )
            .unwrap_or(u32::MAX),
            ..CloseCheck::default()
        };

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

fn describe(error: &ClientError) -> SharedString {
    error.to_string().into()
}
