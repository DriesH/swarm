//! The New task dialog.

use gpui::{
    AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable as _, Role,
    SharedString, Subscription, WeakEntity, Window, div, px,
};
use swarm_protocol::{ClientError, CreateTask};
use swarm_ui::prelude::*;

use crate::model::{AgentKind, FakeLine, ProjectId};
use crate::palette::Overlay;
use crate::workspace::Workspace;

const ORCHESTRATORS: [(&str, AgentKind); 2] = [
    ("Claude Code", AgentKind::ClaudeCode),
    ("Codex", AgentKind::Codex),
];

/// What the user asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewTaskRequest {
    pub project: ProjectId,
    pub name: SharedString,
    pub kind: AgentKind,
    pub first_message: SharedString,
    pub base_branch: SharedString,
    /// The branch to start from; `None` is the project's base branch.
    pub from: Option<SharedString>,
    pub run_setup: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewTaskEvent {
    Dismissed,
    Created(NewTaskRequest),
}

/// The open dialog. It reports the request and leaves closing to its owner.
pub(crate) struct NewTaskDialog {
    project: ProjectId,
    branches: Vec<SharedString>,
    branch: usize,
    orchestrator: usize,
    run_setup: bool,
    busy: bool,
    error: Option<SharedString>,
    name: Entity<TextInput>,
    message: Entity<TextArea>,
    focus_handle: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<NewTaskEvent> for NewTaskDialog {}

impl NewTaskDialog {
    /// `branches` are the branches a task can start from; the first is chosen.
    pub(crate) fn new(
        project: ProjectId,
        branches: Vec<SharedString>,
        default_kind: AgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Name", cx);
            input.set_mono(true, cx);
            input
        });
        let message = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("First message", cx);
            area.set_placeholder("Optional", cx);
            area.set_rows(3, cx);
            area
        });
        window.focus(&name.focus_handle(cx), cx);

        let on_name = cx.subscribe(&name, |this, _, event, cx| match event {
            TextInputEvent::Changed(_) => cx.notify(),
            TextInputEvent::Submitted(_) => this.create(cx),
        });
        let on_message = cx.subscribe(&message, |this, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted(_)) {
                this.create(cx);
            }
        });

        Self {
            project,
            branches,
            branch: 0,
            orchestrator: ORCHESTRATORS
                .iter()
                .position(|(_, kind)| *kind == default_kind)
                .unwrap_or(0),
            run_setup: true,
            busy: false,
            error: None,
            name,
            message,
            focus_handle: cx.focus_handle(),
            _subscriptions: [on_name, on_message],
        }
    }

    /// Takes the full list of branches, and keeps the branch the user picked.
    pub(crate) fn set_branches(&mut self, branches: Vec<SharedString>, cx: &mut Context<Self>) {
        let picked = self.branches.get(self.branch).cloned();

        self.branches = branches;
        self.branch = picked
            .and_then(|picked| self.branches.iter().position(|branch| *branch == picked))
            .unwrap_or(0);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn branch_names(&self) -> Vec<SharedString> {
        self.branches.clone()
    }

    #[cfg(test)]
    pub(crate) fn choose_branch(&mut self, name: &str) {
        if let Some(index) = self.branches.iter().position(|branch| branch == name) {
            self.branch = index;
        }
    }

    #[cfg(test)]
    pub(crate) fn chosen_branch(&self) -> Option<SharedString> {
        self.branches.get(self.branch).cloned()
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.busy && !self.name.read(cx).text().trim().is_empty()
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn orchestrator_kind(&self) -> AgentKind {
        ORCHESTRATORS[self.orchestrator].1
    }

    #[cfg(test)]
    pub(crate) fn choose_orchestrator(&mut self, kind: AgentKind) {
        if let Some(index) = ORCHESTRATORS.iter().position(|(_, other)| *other == kind) {
            self.orchestrator = index;
        }
    }

    #[cfg(test)]
    pub(crate) fn type_message(&self, text: &str, cx: &mut Context<Self>) {
        self.message
            .update(cx, |area, cx| area.set_text(text.to_owned(), cx));
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }

    fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        if busy {
            self.error = None;
        }
        cx.notify();
    }

    /// Stops the busy state and shows why the task was not created.
    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message.into());
        cx.notify();
    }

    pub(crate) fn create(&mut self, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }

        let name = SharedString::from(self.name.read(cx).text().trim().to_owned());
        let first_message = self.message.read(cx).text();
        let base_branch = self.branches.get(self.branch).cloned().unwrap_or_default();
        let kind = ORCHESTRATORS
            .get(self.orchestrator)
            .map_or(AgentKind::ClaudeCode, |(_, kind)| *kind);

        cx.emit(NewTaskEvent::Created(NewTaskRequest {
            project: self.project,
            name,
            kind,
            first_message,
            from: (self.branch > 0).then(|| base_branch.clone()),
            base_branch,
            run_setup: self.run_setup,
        }));
    }
}

/// The branches a task can start from: the base branch, the open tab's branch, then
/// every other local branch in the daemon's order.
pub(crate) fn branch_choices(
    base: &SharedString,
    current: Option<&SharedString>,
    all: &[String],
) -> Vec<SharedString> {
    let mut choices: Vec<SharedString> = Vec::new();
    let listed = all.iter().map(|branch| SharedString::from(branch.clone()));

    for branch in std::iter::once(base.clone())
        .chain(current.cloned())
        .chain(listed)
    {
        if !branch.is_empty() && !choices.contains(&branch) {
            choices.push(branch);
        }
    }

    choices
}

fn field(label: &'static str, control: impl IntoElement) -> Div {
    v_flex()
        .gap(px(6.))
        .min_w_0()
        .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
        .child(control)
}

impl Render for NewTaskDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let (on_branch, on_orchestrator, on_toggle) = (this.clone(), this.clone(), this.clone());
        let on_dismiss = this.clone();

        let branch = Dropdown::new(
            "new-task-branch",
            "Branch from",
            self.branches.clone(),
            self.branch,
        )
        .full_width()
        .on_change(move |index, _, cx| {
            on_branch.update(cx, |this, cx| {
                this.branch = index;
                cx.notify();
            });
        });

        let orchestrator = Dropdown::new(
            "new-task-orchestrator",
            "Orchestrator",
            ORCHESTRATORS
                .iter()
                .map(|(label, _)| SharedString::new_static(label))
                .collect(),
            self.orchestrator,
        )
        .full_width()
        .on_change(move |index, _, cx| {
            on_orchestrator.update(cx, |this, cx| {
                this.orchestrator = index;
                cx.notify();
            });
        });

        let setup = Checkbox::new("new-task-setup", "Run setup script")
            .toggle_state(self.run_setup)
            .on_toggle(move |checked, _, cx| {
                on_toggle.update(cx, |this, cx| {
                    this.run_setup = checked;
                    cx.notify();
                });
            });

        let create_label = if self.busy {
            "Creating task"
        } else {
            "Create task"
        };
        let error = self.error.clone().map(|message| {
            div()
                .id("new-task-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("new-task-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed))),
            )
            .child(
                Button::new("new-task-create", create_label)
                    .variant(ButtonVariant::Primary)
                    .disabled(!self.can_create(cx))
                    .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
            );

        Dialog::new("new-task-dialog", "New task", &self.focus_handle)
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, |_, cx| cx.emit(NewTaskEvent::Dismissed));
            })
            .child(field("Name", self.name.clone()))
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(div().flex_1().min_w_0().child(field("Branch from", branch)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(field("Orchestrator", orchestrator)),
                    ),
            )
            .child(field("First message", self.message.clone()))
            .child(setup)
            .children(error)
            .footer(footer)
    }
}

impl Workspace {
    pub(crate) fn open_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_work() {
            log::debug!("no task is open, cannot open the new task sheet");
            return;
        }

        let Some(project) = self.data.project(self.active_project) else {
            return;
        };

        let current = self
            .data
            .task(self.active_task)
            .map(|task| task.branch.clone())
            .filter(|branch| !branch.is_empty());
        let base = project.base_branch.clone();
        let branches = branch_choices(&base, current.as_ref(), &[]);

        let project = self.active_project;
        let default_kind = self.default_orchestrator();
        let view = cx.new(|cx| NewTaskDialog::new(project, branches, default_kind, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                NewTaskEvent::Dismissed => this.close_overlay(window, cx),
                NewTaskEvent::Created(request) => {
                    this.create_task(request, view.clone(), window, cx);
                }
            });

        self.overlay = Some(Overlay::NewTask {
            view: view.clone(),
            _subscription: subscription,
        });
        self.list_branches(project, base, current, view.downgrade(), cx);
        cx.notify();
    }

    /// Asks the daemon for every local branch and gives the open sheet the list.
    fn list_branches(
        &mut self,
        project: ProjectId,
        base: SharedString,
        current: Option<SharedString>,
        dialog: WeakEntity<NewTaskDialog>,
        cx: &mut Context<Self>,
    ) {
        let daemon = self
            .data
            .project(project)
            .and_then(|project| project.daemon);
        let (Some(client), Some(daemon)) = (self.daemon.clone(), daemon) else {
            return;
        };

        let listing = cx.background_spawn(async move { client.list_branches(daemon) });

        cx.spawn(async move |_, cx| {
            let all = match listing.await {
                Ok(all) => all,
                Err(error) => {
                    log::debug!("cannot list the branches: {error}");
                    return;
                }
            };

            let choices = branch_choices(&base, current.as_ref(), &all);
            let updated = dialog.update(cx, |dialog, cx| dialog.set_branches(choices, cx));

            if let Err(error) = updated {
                log::debug!("the sheet closed before the branches came: {error}");
            }
        })
        .detach();
    }

    /// The orchestrator kind from the settings file, Claude Code when there is none.
    pub(crate) fn default_orchestrator(&self) -> AgentKind {
        let Some(path) = &self.settings_path else {
            return AgentKind::ClaudeCode;
        };

        match swarm_settings::load(path) {
            Ok(settings) => swarm_protocol::AgentKind::from(settings.agents.orchestrator).into(),
            Err(error) => {
                log::warn!("cannot read the settings: {error}");
                AgentKind::ClaudeCode
            }
        }
    }

    /// With a daemon the task is real and the sheet stays open until it answers.
    /// Without one the task is fake.
    fn create_task(
        &mut self,
        request: &NewTaskRequest,
        dialog: Entity<NewTaskDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.daemon.clone() else {
            self.create_fake_task(request, cx);
            self.close_overlay(window, cx);
            return;
        };

        let project = self
            .data
            .project(request.project)
            .and_then(|project| project.daemon);
        let Some(project) = project else {
            dialog.update(cx, |dialog, cx| {
                dialog.fail("This project is not in the daemon".to_owned(), cx);
            });
            return;
        };

        let task = CreateTask {
            project,
            name: request.name.to_string(),
            from: request.from.as_ref().map(ToString::to_string),
            run_setup: request.run_setup,
            orchestrator: Some(request.kind.into()),
            first_message: Some(request.first_message.trim().to_owned())
                .filter(|message| !message.is_empty()),
        };

        dialog.update(cx, |dialog, cx| dialog.set_busy(true, cx));

        let creating = cx.background_spawn(async move {
            let task = client.create_task(task)?;
            let terminals = client.list().unwrap_or_else(|error| {
                log::warn!("cannot list terminals after creating a task: {error}");
                Vec::new()
            });

            Ok::<_, ClientError>((task, terminals))
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = creating.await;

            let updated = this.update_in(cx, |workspace, window, cx| match result {
                Ok((task, terminals)) => {
                    let open = matches!(
                        &workspace.overlay,
                        Some(Overlay::NewTask { view, .. }) if *view == dialog
                    );

                    if open {
                        workspace.close_overlay(window, cx);
                    }

                    workspace.task_created(&task, &terminals, window, cx);
                }
                Err(error) => {
                    log::error!("cannot create the task: {error}");

                    let open = matches!(
                        &workspace.overlay,
                        Some(Overlay::NewTask { view, .. }) if *view == dialog
                    );

                    if open {
                        dialog.update(cx, |dialog, cx| dialog.fail(error.to_string(), cx));
                    } else {
                        workspace.report_sidebar_error(error.to_string(), cx);
                    }
                }
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the task was created: {error}");
            }
        })
        .detach();
    }

    fn create_fake_task(&mut self, request: &NewTaskRequest, cx: &mut Context<Self>) {
        let mut lines = vec![FakeLine {
            text: SharedString::from(format!("\u{203a} {}", request.first_message)),
            color: Color::Default,
        }];
        lines.push(FakeLine {
            text: "\u{273b} Orchestrating\u{2026}".into(),
            color: Color::Muted,
        });

        let task = self
            .data
            .add_task(request.project, &request.name, request.kind, lines);
        self.select_task(task, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(choices: &[SharedString]) -> Vec<&str> {
        choices.iter().map(AsRef::as_ref).collect()
    }

    #[test]
    fn the_base_branch_comes_first_then_the_open_tabs_branch_then_the_rest() {
        let all = ["old", "main", "swarm/login", "feature/x"].map(String::from);
        let choices = branch_choices(&"main".into(), Some(&"swarm/login".into()), &all);

        assert_eq!(names(&choices), ["main", "swarm/login", "old", "feature/x"]);
    }

    #[test]
    fn a_branch_is_listed_once_and_an_empty_name_is_left_out() {
        let all = ["swarm/a", "swarm/a", "", "main"].map(String::from);
        let choices = branch_choices(&"main".into(), None, &all);

        assert_eq!(names(&choices), ["main", "swarm/a"]);
    }

    #[test]
    fn without_a_list_the_two_known_branches_show() {
        let choices = branch_choices(&"main".into(), Some(&"swarm/login".into()), &[]);

        assert_eq!(names(&choices), ["main", "swarm/login"]);
    }
}
