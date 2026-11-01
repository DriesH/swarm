//! The Remove project dialog.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable as _,
    HighlightStyle, Role, SharedString, StyledText, Window, div, px, relative,
};
use swarm_protocol::{Client, RemoveCheck, TaskCloseCheck};
use swarm_ui::fonts::{MONO_FONT, UI_FONT};
use swarm_ui::prelude::*;

use crate::close_task::{CheckState, deletes_files, describe, stops_agents, stops_terminals};
use crate::model::ProjectId;
use crate::palette::Overlay;
use crate::review_model::plural;
use crate::workspace::{DataSource, Workspace};

const NOT_RUNNING: &str = "The daemon is not running";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RemoveProjectEvent {
    /// The user backed out, or the project is gone.
    Dismissed,
    Confirmed,
}

/// The open dialog. It reports what the user chose and leaves closing to its owner, which is
/// the main window or the settings window.
pub(crate) struct RemoveProjectDialog {
    pub(crate) project: ProjectId,
    name: SharedString,
    root: Option<PathBuf>,
    check: CheckState<RemoveCheck>,
    busy: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<RemoveProjectEvent> for RemoveProjectDialog {}

impl RemoveProjectDialog {
    pub(crate) fn new(
        project: ProjectId,
        name: SharedString,
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            project,
            name,
            root,
            check: CheckState::Checking,
            busy: false,
            error: None,
            focus_handle,
        }
    }

    pub(crate) fn set_check(&mut self, check: CheckState<RemoveCheck>, cx: &mut Context<Self>) {
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

    /// The project is gone: asks the owner to close the dialog.
    pub(crate) fn finish(&mut self, cx: &mut Context<Self>) {
        self.busy = false;
        cx.emit(RemoveProjectEvent::Dismissed);
    }

    /// The check the user is looking at, once there is one.
    pub(crate) fn confirmed(&self) -> Option<RemoveCheck> {
        match &self.check {
            CheckState::Ready(check) => Some(check.clone()),
            _ => None,
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(RemoveProjectEvent::Dismissed);
        }
    }

    pub(crate) fn title(&self) -> String {
        format!("Remove project \u{201c}{}\u{201d}", self.name)
    }

    pub(crate) fn can_confirm(&self) -> bool {
        !self.busy && matches!(self.check, CheckState::Ready(_))
    }

    /// The label of the Remove button, and what a screen reader says for it.
    pub(crate) fn confirm_labels(&self) -> (String, String) {
        if self.busy {
            (
                "Removing\u{2026}".to_owned(),
                format!("Removing project {}", self.name),
            )
        } else {
            ("Remove".to_owned(), format!("Remove project {}", self.name))
        }
    }

    /// The sentence under the title, and the range of it that shows in mono.
    pub(crate) fn info(&self) -> (String, Option<Range<usize>>) {
        info_text(self.root.as_deref())
    }

    /// The value lines under the info, from what the check found.
    pub(crate) fn lines(&self) -> Vec<(String, Color)> {
        match &self.check {
            CheckState::Ready(check) => lines_for(check),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn check_state(&self) -> &CheckState<RemoveCheck> {
        &self.check
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// What the dialog says about the files, with the home folder shown as `~`.
fn info_text(root: Option<&Path>) -> (String, Option<Range<usize>>) {
    const HEAD: &str = "Removes only Swarm's tasks, agents and worktrees.";

    let Some(root) = root else {
        return (format!("{HEAD} The code stays on disk."), None);
    };

    let shown = crate::settings::tilde(root);
    let before = format!("{HEAD} The code in ");
    let range = before.len()..before.len() + shown.len();

    (format!("{before}{shown} stays on disk."), Some(range))
}

fn lines_for(check: &RemoveCheck) -> Vec<(String, Color)> {
    let mut lines = Vec::new();

    if !check.tasks.is_empty() {
        let names: Vec<&str> = check.tasks.iter().map(|task| task.name.as_str()).collect();
        let count = u32::try_from(names.len()).unwrap_or(u32::MAX);

        lines.push((
            format!(
                "Closes {}: {}",
                plural(count, "task", "tasks"),
                names.join(", ")
            ),
            Color::Default,
        ));
    }

    let total = |count: fn(&TaskCloseCheck) -> u32| {
        check
            .tasks
            .iter()
            .fold(0, |sum: u32, task| sum.saturating_add(count(task)))
    };
    let (agents, terminals) = (
        total(|task| task.check.running_agents),
        total(|task| task.check.user_terminals),
    );

    if agents > 0 {
        lines.push((stops_agents(agents), Color::Default));
    }
    if terminals > 0 {
        lines.push((stops_terminals(terminals), Color::Default));
    }

    for task in &check.tasks {
        if task.check.uncommitted_files > 0 {
            lines.push((
                format!(
                    "{} in {}",
                    deletes_files(task.check.uncommitted_files),
                    task.name
                ),
                Color::Warn,
            ));
        }
    }

    let merged: BTreeSet<&str> = check
        .tasks
        .iter()
        .flat_map(|task| task.check.merged_branches.iter().map(String::as_str))
        .collect();
    if !merged.is_empty() {
        let count = u32::try_from(merged.len()).unwrap_or(u32::MAX);

        lines.push((
            format!(
                "Deletes {}",
                plural(count, "merged branch", "merged branches")
            ),
            Color::Default,
        ));
    }

    lines
}

/// Body text that wraps: the dialog is narrow, and a task name or a message can be long.
fn wrapping(text: impl Into<SharedString>, color: Color, cx: &App) -> Div {
    div()
        .font_family(UI_FONT)
        .text_size(px(13.))
        .line_height(relative(1.4))
        .text_color(color.hsla(cx))
        .child(text.into())
}

impl Render for RemoveProjectDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();
        let busy = self.busy;

        let (text, mono) = self.info();
        let mut styled = StyledText::new(text.clone());
        if let Some(range) = mono {
            // A run of its own, so the font override has a run to apply to.
            styled = styled
                .with_highlights([(range.clone(), HighlightStyle::default())])
                .with_font_family_overrides([(range, SharedString::new_static(MONO_FONT))]);
        }
        let info = div()
            .id("remove-project-info")
            .role(Role::Label)
            .aria_label(text)
            .debug_selector(|| "remove-project-info".to_owned())
            .font_family(UI_FONT)
            .text_size(px(13.))
            .line_height(relative(1.4))
            .text_color(Color::Muted.hsla(cx))
            .child(styled);

        let body = match &self.check {
            CheckState::Checking => Some(
                div()
                    .id("remove-project-checking")
                    .role(Role::Status)
                    .aria_label("Checking")
                    .child(Label::new("Checking\u{2026}").color(Color::Faint))
                    .into_any_element(),
            ),
            CheckState::Failed(message) => Some(
                div()
                    .id("remove-project-check-error")
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .child(wrapping(message.clone(), Color::Del, cx))
                    .into_any_element(),
            ),
            CheckState::Ready(_) => {
                let lines = self.lines();

                (!lines.is_empty()).then(|| {
                    v_flex()
                        .id("remove-project-lines")
                        .role(Role::List)
                        .aria_label("What removing does")
                        .gap(px(6.))
                        .children(lines.into_iter().enumerate().map(|(index, (text, color))| {
                            div()
                                .id(("remove-project-line", index))
                                .role(Role::ListItem)
                                .aria_label(text.clone())
                                .child(wrapping(text, color, cx))
                        }))
                        .into_any_element()
                })
            }
        };

        let error = self.error.clone().map(|message| {
            div()
                .id("remove-project-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        let (label, accessible) = self.confirm_labels();
        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("remove-project-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .debug_selector(|| "remove-project-cancel".to_owned())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
            )
            .child(
                Button::new("remove-project-confirm", label)
                    .variant(ButtonVariant::Destructive)
                    .accessible_label(accessible)
                    .debug_selector(|| "remove-project-confirm".to_owned())
                    .loading(busy)
                    .disabled(!matches!(self.check, CheckState::Ready(_)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.can_confirm() {
                            cx.emit(RemoveProjectEvent::Confirmed);
                        }
                    })),
            );

        Dialog::new("remove-project-dialog", self.title(), &self.focus_handle)
            .width(px(420.))
            .close_disabled(busy)
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, Self::dismiss);
            })
            .child(info)
            .children(body)
            .children(error)
            .footer(footer)
    }
}

impl Workspace {
    /// The name and the folder of a project, from the app's own list.
    pub(crate) fn remove_project_facts(
        &self,
        project: ProjectId,
    ) -> Option<(SharedString, Option<PathBuf>)> {
        let entry = self.data.project(project)?;

        Some((entry.name.clone(), entry.root.clone()))
    }

    /// What the app knows of the project's open tasks without asking the daemon.
    fn local_remove_check(&self, project: ProjectId) -> RemoveCheck {
        let tasks = self
            .data
            .tasks_of(project)
            .filter(|task| !task.is_start)
            .filter_map(|task| {
                Some(TaskCloseCheck {
                    task: self.data.diff_id(task.id)?,
                    name: task.name.to_string(),
                    check: self.local_close_check(task.id),
                })
            })
            .collect();

        RemoveCheck { tasks }
    }

    /// Opens the dialog that asks before a project goes, over the main window.
    pub(crate) fn open_remove_project(
        &mut self,
        project: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((name, root)) = self.remove_project_facts(project) else {
            return;
        };

        let view = cx.new(|cx| RemoveProjectDialog::new(project, name, root, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                RemoveProjectEvent::Dismissed => this.close_remove_project(view, window, cx),
                RemoveProjectEvent::Confirmed => this.confirm_remove_project(view.clone(), cx),
            });

        self.overlay = Some(Overlay::RemoveProject {
            view: view.clone(),
            _subscription: subscription,
        });
        cx.notify();

        self.check_remove_project(&view, cx);
    }

    /// Closes the dialog and gives focus back to the project's row, or to the window when
    /// the row is gone.
    fn close_remove_project(
        &mut self,
        dialog: &Entity<RemoveProjectDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = dialog.read(cx).project;

        self.overlay = None;
        match self.project_focus.get(&project).cloned() {
            Some(row) => window.focus(&row, cx),
            None => window.focus(&self.focus_handle(cx), cx),
        }
        cx.notify();
    }

    /// Fills the dialog with what removing would throw away: the daemon's answer, or the
    /// app's own data when it has no daemon to ask.
    pub(crate) fn check_remove_project(
        &mut self,
        dialog: &Entity<RemoveProjectDialog>,
        cx: &mut Context<Self>,
    ) {
        let project = dialog.read(cx).project;
        let daemon_id = self.data.project(project).and_then(|entry| entry.daemon);
        let client = self.daemon.clone().filter(Client::is_connected);

        let (Some(client), Some(daemon_id)) = (client, daemon_id) else {
            let state = if self.source == DataSource::Sample {
                CheckState::Ready(self.local_remove_check(project))
            } else {
                CheckState::Failed(NOT_RUNNING.into())
            };

            dialog.update(cx, |dialog, cx| dialog.set_check(state, cx));
            return;
        };

        dialog.update(cx, |dialog, cx| dialog.set_check(CheckState::Checking, cx));
        let checking = cx.background_spawn(async move { client.check_remove_project(daemon_id) });
        let dialog = dialog.clone();

        cx.spawn(async move |_, cx| {
            let state = match checking.await {
                Ok(check) => CheckState::Ready(check),
                Err(error) => {
                    log::warn!("cannot check what removing the project does: {error}");
                    CheckState::Failed(describe(&error))
                }
            };

            dialog.update(cx, |dialog, cx| dialog.set_check(state, cx));
        })
        .detach();
    }

    /// Asks the daemon to remove the project, or removes it from the sample data. A refusal
    /// shows in the dialog and the check runs again, as the project changed meanwhile.
    pub(crate) fn confirm_remove_project(
        &mut self,
        dialog: Entity<RemoveProjectDialog>,
        cx: &mut Context<Self>,
    ) {
        let project = dialog.read(cx).project;

        if self.data.project(project).is_none() {
            dialog.update(cx, RemoveProjectDialog::finish);
            return;
        }
        let Some(confirmed) = dialog.read(cx).confirmed() else {
            return;
        };
        if !dialog.read(cx).can_confirm() {
            return;
        }

        let daemon_id = self.data.project(project).and_then(|entry| entry.daemon);
        let client = self.daemon.clone().filter(Client::is_connected);

        let (Some(client), Some(daemon_id)) = (client, daemon_id) else {
            if self.source == DataSource::Sample {
                self.project_removed(project, cx);
                dialog.update(cx, RemoveProjectDialog::finish);
            } else {
                dialog.update(cx, |dialog, cx| dialog.fail(NOT_RUNNING.into(), cx));
            }

            return;
        };

        dialog.update(cx, RemoveProjectDialog::set_busy);

        // The Start orchestrator stops while the project goes. It must not start again.
        let guarded = self.start_requested.insert(project);
        let again = client.clone();
        let removing =
            cx.background_spawn(async move { client.remove_project(daemon_id, confirmed) });

        cx.spawn(async move |this, cx| {
            let result = removing.await;
            let refused = result.is_err();

            let updated = Self::update_with_window(&this, cx, |workspace, _, cx| match result {
                Ok(()) => {
                    workspace.project_removed(project, cx);
                    dialog.update(cx, RemoveProjectDialog::finish);
                }
                Err(error) => {
                    log::warn!("cannot remove the project: {error}");

                    if guarded {
                        workspace.start_requested.remove(&project);
                    }

                    dialog.update(cx, |dialog, cx| {
                        dialog.fail(describe(&error), cx);
                        dialog.set_check(CheckState::Checking, cx);
                    });
                }
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the project was removed: {error}");
                return;
            }

            if refused {
                let state = match cx
                    .background_executor()
                    .spawn(async move { again.check_remove_project(daemon_id) })
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

    /// Drops the project with its tabs, its terminals and what Review kept. When it was
    /// open, the next project opens, else the empty state shows.
    pub(crate) fn project_removed(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        let Some(position) = self
            .data
            .projects
            .iter()
            .position(|entry| entry.id == project)
        else {
            return;
        };

        if self.active_project == project {
            let next = self
                .data
                .projects
                .get(position + 1)
                .or_else(|| {
                    position
                        .checked_sub(1)
                        .and_then(|before| self.data.projects.get(before))
                })
                .map(|entry| entry.id);

            match next {
                Some(next) => self.select_project(next, cx),
                None => self.show_no_project(),
            }
        }

        let tabs: Vec<_> = self.data.tasks_of(project).map(|task| task.id).collect();
        for tab in tabs {
            self.remove_closed_task(tab, cx);
        }

        self.data.forget_project(project);
        self.start_requested.remove(&project);
        self.previous_tabs.remove(&project);
        self.project_focus.remove(&project);
        self.pull_requests.setups.remove(&project);
        crate::settings::project_removed(project, cx);

        cx.notify();
    }

    /// The window has no project to show, so it shows the empty state.
    fn show_no_project(&mut self) {
        self.active_project = ProjectId(u32::MAX);
        self.active_task = crate::model::TaskId(u32::MAX);
        self.focused_terminal = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use swarm_protocol::{
        AgentLifecycle, CloseCheck, Event, ProjectId as DaemonProjectId, ProjectInfo, Request,
        TaskId as DaemonTaskId,
    };

    use super::*;
    use crate::daemon::tests::{
        answer_late, fake_daemon_replying, project_info, redraw, setup, start_agent, task_info,
        use_projects, wait_until,
    };

    type Requests = Arc<Mutex<Vec<Request>>>;

    fn dialog(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<Entity<RemoveProjectDialog>> {
        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::RemoveProject { view, .. }) => Some(view.clone()),
            _ => None,
        })
    }

    fn open_dialog(project: u32, workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_remove_project(ProjectId(project), window, cx);
        });
        redraw(cx);
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            Modifiers::none(),
        );
        redraw(cx);
    }

    fn project_names(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .projects
                .iter()
                .map(|project| project.name.to_string())
                .collect()
        })
    }

    fn read_dialog<T>(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
        read: impl FnOnce(&RemoveProjectDialog) -> T,
    ) -> Option<T> {
        dialog(workspace, cx).map(|dialog| dialog.read_with(cx, |dialog, _| read(dialog)))
    }

    fn is_ready(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
        read_dialog(workspace, cx, |dialog| dialog.confirmed().is_some()).unwrap_or(false)
    }

    fn texts(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
        read_dialog(workspace, cx, RemoveProjectDialog::lines)
            .unwrap_or_default()
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    fn count(requests: &Requests, matching: impl Fn(&Request) -> bool) -> usize {
        requests
            .lock()
            .expect("lock the requests")
            .iter()
            .filter(|request| matching(request))
            .count()
    }

    /// The check of each `RemoveProject` the daemon got.
    fn removals(requests: &Requests) -> Vec<RemoveCheck> {
        requests
            .lock()
            .expect("lock the requests")
            .iter()
            .filter_map(|request| match request {
                Request::RemoveProject { confirmed, .. } => Some(confirmed.clone()),
                _ => None,
            })
            .collect()
    }

    /// Waits until the daemon handled every request sent before: it answers in order.
    fn flush(client: &Client) {
        client.list().expect("the daemon answers");
    }

    fn task_check(task: u64, name: &str, check: CloseCheck) -> TaskCloseCheck {
        TaskCloseCheck {
            task: DaemonTaskId(task),
            name: name.to_owned(),
            check,
        }
    }

    /// A workspace on a fake daemon that records every request and answers with `reply`
    /// before the defaults. It lists `projects`. The client waits `timeout` for plain replies.
    fn live_with<'a>(
        projects: &[ProjectInfo],
        timeout: Option<Duration>,
        reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        &'a mut VisualTestContext,
        Requests,
        Client,
    ) {
        let requests = Requests::default();
        let record = Arc::clone(&requests);
        let socket = fake_daemon_replying(move |request| {
            record
                .lock()
                .expect("lock the requests")
                .push(request.clone());

            reply(&request)
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let mut client = Client::connect(&socket).expect("connect to the fake daemon");
        if let Some(timeout) = timeout {
            client = client.with_reply_timeout(timeout);
        }
        use_projects(&workspace, projects, cx);
        let attached = client.clone();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(attached, &[], window, cx);
        });
        redraw(cx);

        (workspace, cx, requests, client)
    }

    /// The projects `demo` (id 5, with `demo_tasks`) and `other` (id 6).
    fn live(
        demo_tasks: Vec<swarm_protocol::TaskInfo>,
        reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext, Requests, Client) {
        let projects = [
            project_info(5, "demo", demo_tasks),
            project_info(6, "other", Vec::new()),
        ];

        live_with(&projects, None, reply, cx)
    }

    /// Answers a check with `check` and a removal with Done.
    fn answering(check: RemoveCheck) -> impl Fn(&Request) -> Option<Event> + Send + 'static {
        move |request| match request {
            Request::CheckRemoveProject { call, .. } => Some(Event::RemoveCheck {
                call: *call,
                check: check.clone(),
            }),
            _ => None,
        }
    }

    fn login_check() -> RemoveCheck {
        RemoveCheck {
            tasks: vec![task_check(
                7,
                "login",
                CloseCheck {
                    running_agents: 2,
                    uncommitted_files: 3,
                    merged_branches: vec!["swarm/login".to_owned()],
                    ..CloseCheck::default()
                },
            )],
        }
    }

    #[test]
    fn the_lines_follow_the_check_in_order_and_skip_what_is_zero() {
        let check = RemoveCheck {
            tasks: vec![
                task_check(
                    1,
                    "login",
                    CloseCheck {
                        running_agents: 2,
                        user_terminals: 1,
                        uncommitted_files: 4,
                        unmerged_workers: vec!["a".to_owned()],
                        merged_branches: vec!["swarm/login".to_owned()],
                    },
                ),
                task_check(2, "signup", CloseCheck::default()),
                task_check(
                    3,
                    "cart",
                    CloseCheck {
                        running_agents: 1,
                        user_terminals: 2,
                        uncommitted_files: 1,
                        merged_branches: vec!["swarm/cart".to_owned(), "swarm/cart-b".to_owned()],
                        ..CloseCheck::default()
                    },
                ),
            ],
        };

        let lines = lines_for(&check);

        assert_eq!(
            lines,
            [
                (
                    "Closes 3 tasks: login, signup, cart".to_owned(),
                    Color::Default
                ),
                ("Stops 3 agents".to_owned(), Color::Default),
                ("Stops 3 terminals".to_owned(), Color::Default),
                (
                    "Deletes 4 uncommitted files in login".to_owned(),
                    Color::Warn
                ),
                ("Deletes 1 uncommitted file in cart".to_owned(), Color::Warn),
                ("Deletes 3 merged branches".to_owned(), Color::Default),
            ]
        );
    }

    #[test]
    fn a_task_with_nothing_to_lose_gives_only_the_closing_line() {
        let check = RemoveCheck {
            tasks: vec![task_check(1, "login", CloseCheck::default())],
        };

        let lines = lines_for(&check);

        assert_eq!(lines, [("Closes 1 task: login".to_owned(), Color::Default)]);
        assert!(lines_for(&RemoveCheck::default()).is_empty());
    }

    #[test]
    fn the_info_shows_the_folder_in_mono_with_the_home_as_a_tilde() {
        let home = std::env::var_os("HOME").expect("the test runs with a home folder");
        let root = PathBuf::from(&home).join("personal/swarm");

        let (text, mono) = info_text(Some(&root));

        assert_eq!(
            text,
            "Removes only Swarm's tasks, agents and worktrees. The code in ~/personal/swarm stays on disk."
        );
        let mono = mono.expect("the folder is set in mono");
        assert_eq!(&text[mono], "~/personal/swarm");
    }

    #[test]
    fn a_folder_outside_the_home_shows_whole_and_no_folder_says_less() {
        let (text, mono) = info_text(Some(Path::new("/srv/code/app")));
        let (short, none) = info_text(None);

        assert!(text.ends_with("The code in /srv/code/app stays on disk."));
        assert_eq!(
            mono.map(|range| text[range].to_owned()).as_deref(),
            Some("/srv/code/app")
        );
        assert_eq!(
            short,
            "Removes only Swarm's tasks, agents and worktrees. The code stays on disk."
        );
        assert_eq!(none, None);
    }

    #[gpui::test]
    fn the_title_names_the_project_in_curly_quotes_and_has_no_question_mark(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);

        open_dialog(3, &workspace, cx);

        let title = read_dialog(&workspace, cx, RemoveProjectDialog::title);
        assert_eq!(
            title.as_deref(),
            Some("Remove project \u{201c}extensions\u{201d}")
        );
        assert!(cx.debug_bounds("remove-project-confirm").is_some());
        assert!(cx.debug_bounds("remove-project-cancel").is_some());
        assert!(cx.debug_bounds("remove-project-info").is_some());
    }

    #[gpui::test]
    fn without_a_daemon_the_check_comes_from_the_open_tasks_of_the_app(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        open_dialog(0, &workspace, cx);

        let lines = texts(&workspace, cx);
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Closes 3 tasks: terminal-renderer, menu-bar-usage, worktree-scripts")
        );
        assert!(
            cx.debug_bounds("remove-project-confirm").is_some(),
            "tasks no longer replace Remove"
        );
    }

    #[gpui::test]
    fn cancel_closes_the_dialog_and_focus_goes_back_to_the_row(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_dialog(3, &workspace, cx);

        click("remove-project-cancel", cx);

        assert!(dialog(&workspace, cx).is_none());
        let row = workspace.read_with(cx, |workspace, _| {
            workspace.project_focus.get(&ProjectId(3)).cloned()
        });
        assert!(row.is_some_and(|row| cx.update(|window, _| row.is_focused(window))));
    }

    #[gpui::test]
    fn escape_closes_the_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_dialog(3, &workspace, cx);

        cx.simulate_keystrokes("escape");

        assert!(dialog(&workspace, cx).is_none());
        assert_eq!(project_names(&workspace, cx).len(), 4);
    }

    #[gpui::test]
    fn the_buttons_sit_on_the_right_of_the_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_dialog(3, &workspace, cx);

        let (cancel, remove) = (
            cx.debug_bounds("remove-project-cancel"),
            cx.debug_bounds("remove-project-confirm"),
        );
        let panel = cx.debug_bounds("remove-project-info");

        let (Some(cancel), Some(remove), Some(info)) = (cancel, remove, panel) else {
            panic!("the dialog is not on screen");
        };
        // The info line starts at the left edge of the body, 20 px in. The buttons end 20 px
        // before the right edge, so they sit right of the middle.
        assert!(cancel.left() > info.left() + info.size.width / 2.);
        assert!(remove.right() >= info.right() - px(1.));
    }

    #[gpui::test]
    fn without_a_daemon_remove_takes_the_project_out_of_the_sample_data(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.select_project(ProjectId(3), cx);
        });
        open_dialog(3, &workspace, cx);

        click("remove-project-confirm", cx);

        assert_eq!(
            project_names(&workspace, cx),
            ["swarm", "website", "billing-api"]
        );
        assert!(dialog(&workspace, cx).is_none());
        let (active, has_start_tab) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_project,
                workspace
                    .data
                    .tasks
                    .iter()
                    .any(|task| task.project == ProjectId(3)),
            )
        });
        assert_eq!(active, ProjectId(2));
        assert!(!has_start_tab, "the project's tabs are still there");
        assert!(cx.debug_bounds("project-row-3").is_none());
    }

    #[gpui::test]
    fn removing_the_last_project_shows_the_empty_state(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace
                .data
                .projects
                .retain(|project| project.id == ProjectId(3));
            workspace
                .data
                .tasks
                .retain(|task| task.project == ProjectId(3));
            workspace.select_project(ProjectId(3), cx);
        });
        open_dialog(3, &workspace, cx);

        click("remove-project-confirm", cx);

        assert!(workspace.read_with(cx, |workspace, _| workspace.has_no_projects()));
        assert!(cx.debug_bounds("project-row-3").is_none());
        assert!(dialog(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn the_dialog_says_checking_until_the_daemon_answers_and_remove_waits(cx: &mut TestAppContext) {
        let (workspace, cx, requests, client) =
            live(vec![task_info(7, 5, "login")], answering(login_check()), cx);

        // Nothing runs until the test waits, so the dialog still checks.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_remove_project(ProjectId(0), window, cx);
        });
        let checking = read_dialog(&workspace, cx, |dialog| {
            *dialog.check_state() == CheckState::Checking
        });
        let dialog_now = dialog(&workspace, cx).expect("the dialog is open");
        workspace.update(cx, |workspace, cx| {
            workspace.confirm_remove_project(dialog_now, cx);
        });
        let busy = read_dialog(&workspace, cx, RemoveProjectDialog::is_busy);

        let ready = wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });
        flush(&client);

        assert_eq!(checking, Some(true));
        assert_eq!(busy, Some(false), "Remove worked before the check came");
        assert!(ready);
        assert_eq!(removals(&requests), []);
        assert_eq!(
            texts(&workspace, cx),
            [
                "Closes 1 task: login",
                "Stops 2 agents",
                "Deletes 3 uncommitted files in login",
                "Deletes 1 merged branch"
            ]
        );
    }

    #[gpui::test]
    fn a_failed_check_shows_its_message_and_remove_stays_off(cx: &mut TestAppContext) {
        let (workspace, cx, requests, client) = live(
            Vec::new(),
            |request| match request {
                Request::CheckRemoveProject { call, .. } => Some(Event::Error {
                    call: Some(*call),
                    id: None,
                    message: "cannot read the worktrees".into(),
                }),
                _ => None,
            },
            cx,
        );
        open_dialog(0, &workspace, cx);

        let failed = wait_until(cx, |cx| {
            redraw(cx);
            read_dialog(&workspace, cx, |dialog| {
                matches!(dialog.check_state(), CheckState::Failed(_))
            })
            .unwrap_or(false)
        });
        click("remove-project-confirm", cx);
        flush(&client);

        let message = read_dialog(&workspace, cx, |dialog| match dialog.check_state() {
            CheckState::Failed(message) => message.to_string(),
            _ => String::new(),
        });
        assert!(failed);
        assert!(message.is_some_and(|message| message.contains("cannot read the worktrees")));
        assert_eq!(
            count(&requests, |request| matches!(
                request,
                Request::RemoveProject { .. }
            )),
            0
        );
        assert!(dialog(&workspace, cx).is_some());
    }

    #[gpui::test]
    fn remove_sends_the_check_the_user_saw_and_the_project_leaves_the_sidebar(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx, requests, _client) =
            live(vec![task_info(7, 5, "login")], answering(login_check()), cx);
        assert!(cx.debug_bounds("project-row-0").is_some());
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });

        click("remove-project-confirm", cx);
        let gone = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });

        assert!(gone, "the dialog stayed open");
        assert_eq!(removals(&requests), [login_check()]);
        assert_eq!(project_names(&workspace, cx), ["other"]);
        assert!(cx.debug_bounds("project-row-0").is_none());
        assert!(cx.debug_bounds("project-row-1").is_some());
        let (active, tabs) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_project,
                workspace.data.tasks_of(ProjectId(0)).count(),
            )
        });
        assert_eq!(active, ProjectId(1), "the other project opens");
        assert_eq!(tabs, 0);
    }

    #[gpui::test]
    fn the_tabs_of_a_removed_project_close(cx: &mut TestAppContext) {
        let (workspace, cx, requests, _client) = live(Vec::new(), |_| None, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.select_project(ProjectId(0), cx);
        });
        let before = workspace.read_with(cx, |workspace, _| {
            workspace.data.tasks_of(ProjectId(0)).count()
        });
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });

        click("remove-project-confirm", cx);
        wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });

        assert_eq!(before, 1, "the project had its Start tab");
        assert_eq!(removals(&requests).len(), 1);
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks
                .iter()
                .all(|task| task.project != ProjectId(0))
        }));
    }

    #[gpui::test]
    fn confirming_makes_the_dialog_busy_and_a_busy_dialog_ignores_cancel_and_escape(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx, _requests, _client) = live(Vec::new(), |_| None, cx);
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });
        let view = dialog(&workspace, cx).expect("the dialog is open");

        // The removal only starts when the test waits: the dialog is busy until then.
        workspace.update(cx, |workspace, cx| {
            workspace.confirm_remove_project(view.clone(), cx);
        });
        let (busy, labels) = read_dialog(&workspace, cx, |dialog| {
            (dialog.is_busy(), dialog.confirm_labels())
        })
        .expect("the dialog is open");
        let confirmed_again = view.read_with(cx, |dialog, _| dialog.can_confirm());

        assert!(busy);
        assert!(
            !confirmed_again,
            "a second click would send a second removal"
        );
        assert_eq!(
            labels,
            (
                "Removing\u{2026}".to_owned(),
                "Removing project demo".to_owned()
            )
        );
    }

    #[gpui::test]
    fn cancel_and_escape_do_nothing_while_the_removal_runs(cx: &mut TestAppContext) {
        let (workspace, cx, _requests, _client) = live(Vec::new(), |_| None, cx);
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });
        let view = dialog(&workspace, cx).expect("the dialog is open");

        view.update(cx, RemoveProjectDialog::set_busy);
        redraw(cx);
        click("remove-project-cancel", cx);
        cx.simulate_keystrokes("escape");
        let open_while_busy = dialog(&workspace, cx).is_some();
        let remove_is_there = cx.debug_bounds("remove-project-confirm").is_some();

        view.update(cx, |dialog, cx| dialog.fail("refused".into(), cx));
        redraw(cx);
        click("remove-project-cancel", cx);

        assert!(open_while_busy, "Cancel or Escape closed a running removal");
        assert!(remove_is_there, "Remove goes while it runs");
        assert!(dialog(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn a_refusal_shows_in_the_dialog_and_the_check_runs_again(cx: &mut TestAppContext) {
        let round = Arc::new(AtomicU32::new(0));
        let (workspace, cx, requests, _client) = live(
            vec![task_info(7, 5, "login")],
            {
                let round = Arc::clone(&round);

                move |request| match request {
                    Request::CheckRemoveProject { call, .. } => {
                        let files = round.fetch_add(1, Ordering::Relaxed) + 1;

                        Some(Event::RemoveCheck {
                            call: *call,
                            check: RemoveCheck {
                                tasks: vec![task_check(
                                    7,
                                    "login",
                                    CloseCheck {
                                        uncommitted_files: files,
                                        ..CloseCheck::default()
                                    },
                                )],
                            },
                        })
                    }
                    Request::RemoveProject {
                        call, confirmed, ..
                    } => {
                        let files = confirmed
                            .tasks
                            .first()
                            .map_or(0, |task| task.check.uncommitted_files);

                        if files < 2 {
                            Some(Event::Error {
                                call: Some(*call),
                                id: None,
                                message: "login has more changes".into(),
                            })
                        } else {
                            Some(Event::Done { call: *call })
                        }
                    }
                    _ => None,
                }
            },
            cx,
        );
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });

        click("remove-project-confirm", cx);
        let refreshed = wait_until(cx, |cx| {
            redraw(cx);
            read_dialog(&workspace, cx, |dialog| {
                dialog.error_text().is_some()
                    && dialog.confirmed().is_some_and(|check| {
                        check
                            .tasks
                            .iter()
                            .any(|task| task.check.uncommitted_files == 2)
                    })
            })
            .unwrap_or(false)
        });
        let lines = texts(&workspace, cx);
        let error = read_dialog(&workspace, cx, |dialog| {
            dialog.error_text().map(str::to_owned)
        });
        let guarded = workspace.read_with(cx, |workspace, _| {
            workspace.start_requested.contains(&ProjectId(0))
        });
        assert_eq!(project_names(&workspace, cx), ["demo", "other"]);

        click("remove-project-confirm", cx);
        let gone = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });

        assert!(refreshed, "the dialog did not show the new check");
        assert_eq!(
            lines,
            [
                "Closes 1 task: login",
                "Deletes 2 uncommitted files in login"
            ]
        );
        assert!(
            error
                .flatten()
                .is_some_and(|message| message.contains("login has more changes"))
        );
        assert!(!guarded, "the Start orchestrator stays off after a refusal");
        assert!(gone);
        let sent: Vec<u32> = removals(&requests)
            .iter()
            .map(|check| {
                check
                    .tasks
                    .first()
                    .map_or(0, |task| task.check.uncommitted_files)
            })
            .collect();
        assert_eq!(sent, [1, 2]);
        assert_eq!(project_names(&workspace, cx), ["other"]);
    }

    #[gpui::test]
    fn the_start_orchestrator_does_not_start_again_while_the_project_goes(cx: &mut TestAppContext) {
        let mut listed = project_info(5, "demo", Vec::new());
        listed.start = Some(start_agent(100, 5, swarm_protocol::AgentStatus::Idle));
        let projects = [listed, project_info(6, "other", Vec::new())];
        let (workspace, cx, requests, client) = live_with(&projects, None, |_| None, cx);
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });
        let view = dialog(&workspace, cx).expect("the dialog is open");

        // The removal only starts when the test waits. Meanwhile the daemon stops the Start
        // orchestrator, and the window draws.
        workspace.update(cx, |workspace, cx| {
            workspace.confirm_remove_project(view, cx);
        });
        let mut ended = start_agent(100, 5, swarm_protocol::AgentStatus::Done);
        ended.lifecycle = AgentLifecycle::Resumable;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.agent_changed(&ended, Some(window), cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let guarded = workspace.read_with(cx, |workspace, _| {
            workspace.start_requested.contains(&ProjectId(0))
        });

        let gone = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });
        flush(&client);

        assert!(guarded);
        assert!(gone);
        // The next project opens on its own Start tab, which may ask for its orchestrator.
        assert_eq!(
            count(&requests, |request| matches!(
                request,
                Request::StartOrchestrator { project, .. } if *project == DaemonProjectId(5)
            )),
            0,
            "the app asked for a Start orchestrator while the project went"
        );
    }

    #[gpui::test]
    fn a_removal_that_answers_after_the_default_timeout_still_succeeds(cx: &mut TestAppContext) {
        let projects = [
            project_info(5, "demo", Vec::new()),
            project_info(6, "other", Vec::new()),
        ];
        let (workspace, cx, requests, client) = live_with(
            &projects,
            Some(Duration::from_millis(200)),
            |request| match request {
                Request::StopAgents { call } | Request::RemoveProject { call, .. } => {
                    answer_late();

                    Some(Event::Done { call: *call })
                }
                _ => None,
            },
            cx,
        );
        open_dialog(0, &workspace, cx);
        wait_until(cx, |cx| {
            redraw(cx);
            is_ready(&workspace, cx)
        });
        let plain = client.stop_agents();

        click("remove-project-confirm", cx);
        let gone = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });

        assert!(
            matches!(plain, Err(swarm_protocol::ClientError::Timeout)),
            "the daemon is not slower than the default timeout: {plain:?}"
        );
        assert!(gone, "the removal gave up before the daemon answered");
        assert_eq!(removals(&requests).len(), 1);
        assert_eq!(project_names(&workspace, cx), ["other"]);
    }

    #[gpui::test]
    fn a_project_list_without_a_project_drops_it_with_its_tabs(cx: &mut TestAppContext) {
        let (workspace, cx, _requests, _client) =
            live(vec![task_info(7, 5, "login")], |_| None, cx);
        assert_eq!(project_names(&workspace, cx), ["demo", "other"]);

        use_projects(&workspace, &[project_info(6, "other", Vec::new())], cx);
        redraw(cx);

        assert_eq!(project_names(&workspace, cx), ["other"]);
        let (active, tabs, task) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_project,
                workspace.data.tasks_of(ProjectId(0)).count(),
                workspace.data.has_daemon_task(DaemonTaskId(7)),
            )
        });
        assert_eq!(active, ProjectId(1));
        assert_eq!(tabs, 0);
        assert!(!task, "the tab of the dropped project is still there");
        assert!(cx.debug_bounds("project-row-0").is_none());
    }

    #[gpui::test]
    fn removing_a_project_forgets_its_github_setup(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            workspace.pull_requests.setups.insert(
                ProjectId(3),
                crate::pull_request::GithubSetup::Running(crate::pull_request::SetupStep::SignIn),
            );
        });

        workspace.update(cx, |workspace, cx| {
            workspace.project_removed(ProjectId(3), cx);
        });

        assert!(workspace.read_with(cx, |workspace, _| {
            workspace.pull_requests.setups.is_empty()
        }));
    }
}
