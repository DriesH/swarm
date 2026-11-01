//! The Remove project dialog.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable as _, Role,
    SharedString, Window, div, px,
};
use swarm_protocol::Client;
use swarm_ui::prelude::*;

use crate::model::ProjectId;
use crate::palette::Overlay;
use crate::workspace::{DataSource, Workspace};

const NOT_RUNNING: &str = "The daemon is not running";

/// What removing does, as short facts.
const FACTS: [&str; 2] = [
    "Files, branches and worktrees stay on disk.",
    "Its Start orchestrator stops.",
];

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
    open_tasks: Vec<SharedString>,
    busy: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EventEmitter<RemoveProjectEvent> for RemoveProjectDialog {}

impl RemoveProjectDialog {
    pub(crate) fn new(
        project: ProjectId,
        name: SharedString,
        open_tasks: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            project,
            name,
            open_tasks,
            busy: false,
            error: None,
            focus_handle,
        }
    }

    pub(crate) fn set_open_tasks(&mut self, open_tasks: Vec<SharedString>, cx: &mut Context<Self>) {
        self.open_tasks = open_tasks;
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

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(RemoveProjectEvent::Dismissed);
        }
    }

    pub(crate) fn title(&self) -> String {
        format!("Remove {}?", self.name)
    }

    pub(crate) fn can_confirm(&self) -> bool {
        !self.busy && self.open_tasks.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn open_tasks(&self) -> &[SharedString] {
        &self.open_tasks
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The note that takes the place of Remove while tasks are open.
    pub(crate) fn note(&self) -> Option<String> {
        note_for(&self.open_tasks)
    }

    fn task_list(&self) -> Option<AnyElement> {
        if self.open_tasks.is_empty() {
            return None;
        }

        Some(
            v_flex()
                .id("remove-project-tasks")
                .role(Role::List)
                .aria_label("Open tasks")
                .gap(px(4.))
                .child(
                    Label::new("Open tasks")
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                )
                .children(self.open_tasks.iter().enumerate().map(|(index, name)| {
                    div()
                        .id(("remove-project-task", index))
                        .role(Role::ListItem)
                        .aria_label(name.clone())
                        .child(Label::new(name.clone()).mono())
                }))
                .into_any_element(),
        )
    }
}

/// "Close login, signup and cart first.", or `None` when no task is open.
fn note_for(open_tasks: &[SharedString]) -> Option<String> {
    let (last, rest) = open_tasks.split_last()?;

    let names = if rest.is_empty() {
        last.to_string()
    } else {
        let rest: Vec<&str> = rest.iter().map(|name| &**name).collect();

        format!("{} and {last}", rest.join(", "))
    };

    Some(format!("Close {names} first."))
}

impl Render for RemoveProjectDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();
        let busy = self.busy;

        let facts = v_flex()
            .id("remove-project-facts")
            .role(Role::List)
            .aria_label("What removing does")
            .gap(px(6.))
            .children(FACTS.iter().enumerate().map(|(index, text)| {
                div()
                    .id(("remove-project-fact", index))
                    .role(Role::ListItem)
                    .aria_label(*text)
                    .child(Label::new(*text))
            }));

        let error = self.error.clone().map(|message| {
            div()
                .id("remove-project-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        let cancel = Button::new("remove-project-cancel", "Cancel")
            .variant(ButtonVariant::Secondary)
            .debug_selector(|| "remove-project-cancel".to_owned())
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)));

        let action = match self.note() {
            Some(note) => div()
                .id("remove-project-note")
                .role(Role::Status)
                .aria_label(note.clone())
                .debug_selector(|| "remove-project-note".to_owned())
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .text_color(Color::Muted.hsla(cx))
                .child(note)
                .into_any_element(),
            None => Button::new("remove-project-confirm", "Remove")
                .variant(ButtonVariant::Destructive)
                .debug_selector(|| "remove-project-confirm".to_owned())
                .disabled(!self.can_confirm())
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.can_confirm() {
                        cx.emit(RemoveProjectEvent::Confirmed);
                    }
                }))
                .into_any_element(),
        };

        Dialog::new("remove-project-dialog", self.title(), &self.focus_handle)
            .width(px(420.))
            .close_disabled(busy)
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, Self::dismiss);
            })
            .child(facts)
            .children(self.task_list())
            .children(error)
            .footer(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.))
                    .child(cancel)
                    .child(action),
            )
    }
}

impl Workspace {
    /// The name and the open tasks of a project, from the app's own list.
    pub(crate) fn remove_project_facts(
        &self,
        project: ProjectId,
    ) -> Option<(SharedString, Vec<SharedString>)> {
        let entry = self.data.project(project)?;

        Some((entry.name.clone(), self.data.open_task_names(project)))
    }

    /// Opens the dialog that asks before a project goes, over the main window.
    pub(crate) fn open_remove_project(
        &mut self,
        project: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((name, open_tasks)) = self.remove_project_facts(project) else {
            return;
        };

        let view = cx.new(|cx| RemoveProjectDialog::new(project, name, open_tasks, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                RemoveProjectEvent::Dismissed => this.close_remove_project(view, window, cx),
                RemoveProjectEvent::Confirmed => this.confirm_remove_project(view.clone(), cx),
            });

        self.overlay = Some(Overlay::RemoveProject {
            view,
            _subscription: subscription,
        });
        cx.notify();
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

    /// Asks the daemon to remove the project, or removes it from the sample data. The app's
    /// own task list is checked again first: it may have changed since the dialog opened.
    pub(crate) fn confirm_remove_project(
        &mut self,
        dialog: Entity<RemoveProjectDialog>,
        cx: &mut Context<Self>,
    ) {
        let project = dialog.read(cx).project;
        let Some((_, open_tasks)) = self.remove_project_facts(project) else {
            dialog.update(cx, RemoveProjectDialog::finish);
            return;
        };

        if !open_tasks.is_empty() {
            dialog.update(cx, |dialog, cx| dialog.set_open_tasks(open_tasks, cx));
            return;
        }
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
        let removing = cx.background_spawn(async move { client.remove_project(daemon_id) });

        cx.spawn(async move |this, cx| {
            let result = removing.await;

            let updated = Self::update_with_window(&this, cx, |workspace, _, cx| match result {
                Ok(()) => {
                    workspace.project_removed(project, cx);
                    dialog.update(cx, RemoveProjectDialog::finish);
                }
                Err(error) => {
                    log::warn!("cannot remove the project: {error}");
                    dialog.update(cx, |dialog, cx| dialog.fail(error.to_string().into(), cx));
                }
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the project was removed: {error}");
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
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use swarm_protocol::{Event, ProjectId as DaemonProjectId, Request};

    use super::*;
    use crate::daemon::tests::{
        fake_daemon_replying, project_info, redraw, setup, task_info, use_projects, wait_until,
    };

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

    /// A workspace on a fake daemon with the projects `demo` (id 5) and `other` (id 6). The
    /// daemon sends every `RemoveProject` it gets over the channel and answers with `reply`.
    fn live(
        demo_tasks: Vec<swarm_protocol::TaskInfo>,
        reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        &mut VisualTestContext,
        flume::Receiver<DaemonProjectId>,
    ) {
        let (sent, removals) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| {
            if let Request::RemoveProject { project, .. } = &request {
                sent.send(*project).ok();
            }

            reply(&request)
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[
                project_info(5, "demo", demo_tasks),
                project_info(6, "other", Vec::new()),
            ],
            cx,
        );
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });
        redraw(cx);

        (workspace, cx, removals)
    }

    #[gpui::test]
    fn the_dialog_names_the_project_and_offers_cancel_and_remove(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        open_dialog(3, &workspace, cx);

        let dialog = dialog(&workspace, cx).expect("the dialog is open");
        assert_eq!(
            dialog.read_with(cx, |dialog, _| dialog.title()),
            "Remove extensions?"
        );
        assert!(cx.debug_bounds("remove-project-confirm").is_some());
        assert!(cx.debug_bounds("remove-project-note").is_none());
        assert!(cx.debug_bounds("remove-project-cancel").is_some());
    }

    #[gpui::test]
    fn open_tasks_are_listed_and_remove_gives_way_to_a_note_that_names_them(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);

        open_dialog(0, &workspace, cx);

        let dialog = dialog(&workspace, cx).expect("the dialog is open");
        let (tasks, note) = dialog.read_with(cx, |dialog, _| {
            (dialog.open_tasks().to_vec(), dialog.note())
        });
        assert_eq!(
            tasks,
            ["terminal-renderer", "menu-bar-usage", "worktree-scripts"]
        );
        assert_eq!(
            note.as_deref(),
            Some("Close terminal-renderer, menu-bar-usage and worktree-scripts first.")
        );
        assert!(cx.debug_bounds("remove-project-confirm").is_none());
        assert!(cx.debug_bounds("remove-project-note").is_some());
    }

    #[test]
    fn the_note_names_one_task_alone() {
        let note = |names: &[&'static str]| {
            let names: Vec<SharedString> = names.iter().map(|name| (*name).into()).collect();

            note_for(&names)
        };

        assert_eq!(note(&["login"]).as_deref(), Some("Close login first."));
        assert_eq!(
            note(&["login", "signup"]).as_deref(),
            Some("Close login and signup first.")
        );
        assert_eq!(note(&[]), None);
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
    fn remove_asks_the_daemon_once_and_the_project_leaves_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx, removals) = live(Vec::new(), |_| None, cx);
        assert!(cx.debug_bounds("project-row-0").is_some());
        open_dialog(0, &workspace, cx);

        click("remove-project-confirm", cx);
        let gone = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });

        assert!(gone, "the dialog stayed open");
        assert_eq!(
            removals.try_iter().collect::<Vec<_>>(),
            [DaemonProjectId(5)]
        );
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
        let (workspace, cx, removals) = live(Vec::new(), |_| None, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.select_project(ProjectId(0), cx);
        });
        let before = workspace.read_with(cx, |workspace, _| {
            workspace.data.tasks_of(ProjectId(0)).count()
        });
        open_dialog(0, &workspace, cx);

        click("remove-project-confirm", cx);
        wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });

        assert_eq!(before, 1, "the project had its Start tab");
        assert_eq!(removals.len(), 1);
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks
                .iter()
                .all(|task| task.project != ProjectId(0))
        }));
    }

    #[gpui::test]
    fn a_daemon_refusal_shows_in_the_dialog_and_keeps_the_project(cx: &mut TestAppContext) {
        let (workspace, cx, removals) = live(
            Vec::new(),
            |request| match request {
                Request::RemoveProject { call, .. } => Some(Event::Error {
                    call: Some(*call),
                    id: None,
                    message: "demo has open tasks: login".into(),
                }),
                _ => None,
            },
            cx,
        );
        open_dialog(0, &workspace, cx);

        click("remove-project-confirm", cx);
        let shown = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_some_and(|dialog| {
                dialog.read_with(cx, |dialog, _| dialog.error_text().is_some())
            })
        });

        assert!(shown, "the refusal never showed");
        let error = dialog(&workspace, cx).and_then(|dialog| {
            dialog.read_with(cx, |dialog, _| dialog.error_text().map(str::to_owned))
        });
        assert!(error.is_some_and(|message| message.contains("demo has open tasks: login")));
        assert_eq!(removals.len(), 1);
        assert_eq!(project_names(&workspace, cx), ["demo", "other"]);
        assert!(cx.debug_bounds("remove-project-confirm").is_some());
    }

    #[gpui::test]
    fn a_task_that_opened_meanwhile_stops_the_request_and_shows_the_note(cx: &mut TestAppContext) {
        let (workspace, cx, removals) = live(Vec::new(), |_| None, cx);
        open_dialog(0, &workspace, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&task_info(7, 5, "login"), Some(window), cx);
        });

        click("remove-project-confirm", cx);
        wait_until(cx, |cx| {
            redraw(cx);
            cx.debug_bounds("remove-project-note").is_some()
        });

        assert!(removals.is_empty(), "the daemon was asked");
        assert!(cx.debug_bounds("remove-project-note").is_some());
        assert!(cx.debug_bounds("remove-project-confirm").is_none());
        assert_eq!(project_names(&workspace, cx), ["demo", "other"]);
    }
}
