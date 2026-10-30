//! The New task dialog.

use gpui::{
    Context, Div, Entity, EventEmitter, FocusHandle, Focusable as _, SharedString, Subscription,
    Window, div, px,
};
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
            name,
            message,
            focus_handle: cx.focus_handle(),
            _subscriptions: [on_name, on_message],
        }
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.name.read(cx).text().trim().is_empty()
    }

    fn create(&mut self, cx: &mut Context<Self>) {
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

        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("new-task-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed))),
            )
            .child(
                Button::new("new-task-create", "Create task")
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
            .footer(footer)
    }
}

impl Workspace {
    pub(crate) fn open_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut branches: Vec<SharedString> = self
            .data
            .project(self.active_project)
            .map(|project| project.base_branch.clone())
            .into_iter()
            .collect();
        branches.extend(
            self.data
                .task(self.active_task)
                .map(|task| task.branch.clone()),
        );
        branches.dedup();

        let project = self.active_project;
        let view = cx.new(|cx| NewTaskDialog::new(project, branches, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                NewTaskEvent::Dismissed => this.close_overlay(window, cx),
                NewTaskEvent::Created(request) => {
                    this.create_task(request, cx);
                    this.close_overlay(window, cx);
                }
            });

        self.overlay = Some(Overlay::NewTask {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn create_task(&mut self, request: &NewTaskRequest, cx: &mut Context<Self>) {
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
