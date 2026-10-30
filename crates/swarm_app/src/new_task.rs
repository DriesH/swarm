//! The New task dialog.

use gpui::{
    AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable as _, Role,
    SharedString, Subscription, Window, div, px,
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
            orchestrator: 0,
            run_setup: true,
            busy: false,
            error: None,
            name,
            message,
            focus_handle: cx.focus_handle(),
            _subscriptions: [on_name, on_message],
        }
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.busy && !self.name.read(cx).text().trim().is_empty()
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
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
        let Some(project) = self.data.project(self.active_project) else {
            return;
        };

        let mut branches = vec![project.base_branch.clone()];
        branches.extend(
            self.data
                .task(self.active_task)
                .map(|task| task.branch.clone())
                .filter(|branch| !branch.is_empty()),
        );
        branches.dedup();

        let project = self.active_project;
        let view = cx.new(|cx| NewTaskDialog::new(project, branches, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                NewTaskEvent::Dismissed => this.close_overlay(window, cx),
                NewTaskEvent::Created(request) => {
                    this.create_task(request, view.clone(), window, cx);
                }
            });

        self.overlay = Some(Overlay::NewTask {
            view,
            _subscription: subscription,
        });
        cx.notify();
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
