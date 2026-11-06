//! The New task dialog.

use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable as _,
    SharedString, Subscription, WeakEntity, Window, div, px,
};
use swarm_protocol::{
    ClientError, CreateTask, ExistingBranch, TaskBranch, TaskBranchList, creatures, task_slug,
};
use swarm_ui::prelude::*;

use crate::announce::announce;
use crate::model::{AgentKind, FakeLine, ProjectId};
use crate::palette::Overlay;
use crate::workspace::Workspace;

pub(crate) const ORCHESTRATORS: [(&str, AgentKind); 2] = [
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
    pub branch: TaskBranch,
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
    prefix: String,
    branches: Vec<SharedString>,
    branch: usize,
    /// 0 makes a new branch, 1 continues an existing one.
    mode: usize,
    task_branches: BranchListState,
    /// Index into the branches that have no task.
    existing: usize,
    switch_folder: bool,
    orchestrator: usize,
    run_setup: bool,
    busy: bool,
    error: Option<SharedString>,
    /// The creature name the task gets when the name field stays empty.
    suggested: SharedString,
    name: Entity<TextInput>,
    branch_name: Entity<TextInput>,
    message: Entity<TextArea>,
    focus_handle: FocusHandle,
    pub(crate) opener: Option<FocusHandle>,
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<NewTaskEvent> for NewTaskDialog {}

impl gpui::Focusable for NewTaskDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl NewTaskDialog {
    /// `branches` are the branches a task can start from; the first is chosen.
    pub(crate) fn new(
        project: ProjectId,
        prefix: String,
        branches: Vec<SharedString>,
        default_kind: AgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let opener = window.focused(cx);
        let suggested = suggest_name(&branches, &prefix, creatures::random_seed());
        let name = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Name", cx);
            input.set_placeholder(suggested.clone(), cx);
            input.set_mono(true, cx);
            input
        });
        let branch_name = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Branch name", cx);
            input.set_placeholder(placeholder_for(&prefix, &suggested), cx);
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
            TextInputEvent::Changed(_) => {
                this.refresh_placeholders(cx);
                cx.notify();
            }
            TextInputEvent::Submitted(_) => this.create(cx),
        });
        let on_branch_name = cx.subscribe(&branch_name, |this, _, event, cx| match event {
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
            prefix,
            branches,
            branch: 0,
            mode: 0,
            task_branches: BranchListState::Loading,
            existing: 0,
            switch_folder: true,
            orchestrator: ORCHESTRATORS
                .iter()
                .position(|(_, kind)| *kind == default_kind)
                .unwrap_or(0),
            run_setup: true,
            busy: false,
            error: None,
            suggested,
            name,
            branch_name,
            message,
            focus_handle: cx.focus_handle(),
            opener,
            _subscriptions: [on_name, on_branch_name, on_message],
        }
    }

    /// Takes the full list of branches, and keeps the branch the user picked.
    pub(crate) fn set_branches(&mut self, branches: Vec<SharedString>, cx: &mut Context<Self>) {
        let picked = self.branches.get(self.branch).cloned();

        self.branches = branches;
        self.branch = picked
            .and_then(|picked| self.branches.iter().position(|branch| *branch == picked))
            .unwrap_or(0);

        if branch_is_taken(&self.branches, &self.prefix, &self.suggested) {
            self.suggested = suggest_name(&self.branches, &self.prefix, creatures::random_seed());
        }
        self.refresh_placeholders(cx);
        cx.notify();
    }

    /// Takes the branches a task can continue, and keeps the branch the user picked.
    pub(crate) fn set_task_branches(&mut self, list: TaskBranchList, cx: &mut Context<Self>) {
        let picked = self.chosen().map(|branch| branch.name.clone());

        self.task_branches = BranchListState::Loaded(list);

        let kept =
            picked.and_then(|picked| self.available().position(|branch| branch.name == picked));
        self.existing = kept.unwrap_or(0);
        if kept.is_none() {
            self.switch_folder = true;
        }

        self.refresh_placeholders(cx);
        cx.notify();
    }

    /// Shows why the branches to continue are missing.
    pub(crate) fn set_task_branches_failed(&mut self, error: SharedString, cx: &mut Context<Self>) {
        self.task_branches = BranchListState::Failed(error);
        self.existing = 0;

        cx.notify();
    }

    fn task_branch_list(&self) -> Option<&TaskBranchList> {
        match &self.task_branches {
            BranchListState::Loaded(list) => Some(list),
            BranchListState::Loading | BranchListState::Failed(_) => None,
        }
    }

    /// The branches without a task, in the daemon's order.
    fn available(&self) -> impl Iterator<Item = &ExistingBranch> {
        self.task_branch_list()
            .into_iter()
            .flat_map(|list| &list.branches)
            .filter(|branch| branch.task.is_none())
    }

    fn chosen(&self) -> Option<&ExistingBranch> {
        self.available().nth(self.existing)
    }

    fn existing_labels(&self) -> Vec<SharedString> {
        self.available()
            .map(|branch| {
                if branch.remote_only {
                    SharedString::from(format!("origin/{}", branch.name))
                } else {
                    SharedString::from(branch.name.clone())
                }
            })
            .collect()
    }

    /// Where the chosen existing branch is checked out.
    fn occupancy(&self) -> Option<Occupancy> {
        let list = self.task_branch_list()?;
        let branch = self.chosen()?;

        let Some(path) = &branch.checked_out else {
            return Some(Occupancy::Free);
        };

        if branch.in_project_folder {
            Some(match list.project_folder_changes {
                0 => Occupancy::ProjectFolderClean,
                changes => Occupancy::ProjectFolderChanged(changes),
            })
        } else {
            Some(Occupancy::Elsewhere(path.clone()))
        }
    }

    /// The Branch dropdown with the line about where the branch is checked out.
    fn existing_branch_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let (on_existing, on_switch) = (this.clone(), this);

        let labels = self.existing_labels();

        let control = match &self.task_branches {
            BranchListState::Loading => Label::new(self.list_message())
                .color(Color::Muted)
                .into_any_element(),
            BranchListState::Failed(_) => Label::new(self.list_message())
                .size(LabelSize::Small)
                .color(Color::Del)
                .into_any_element(),
            BranchListState::Loaded(_) if labels.is_empty() => Label::new(self.list_message())
                .color(Color::Muted)
                .into_any_element(),
            BranchListState::Loaded(_) => Dropdown::new(
                "new-task-existing-branch",
                "Existing branch",
                labels,
                self.existing,
            )
            .full_width()
            .on_change(move |index, _, cx| {
                on_existing.update(cx, |this, cx| this.choose_existing(index, cx));
            })
            .into_any_element(),
        };

        let switch = matches!(self.occupancy(), Some(Occupancy::ProjectFolderClean)).then(|| {
            let base = self
                .task_branch_list()
                .map(|list| list.base_branch.clone())
                .unwrap_or_default();

            Checkbox::new(
                "new-task-switch-folder",
                format!("Switch project folder to {base}"),
            )
            .toggle_state(self.switch_folder)
            .on_toggle(move |checked, _, cx| {
                on_switch.update(cx, |this, cx| {
                    this.switch_folder = checked;
                    cx.notify();
                });
            })
        });
        let line = self
            .occupancy_line()
            .map(|line| Label::new(line).size(LabelSize::Small).color(Color::Del));

        v_flex()
            .gap(px(8.))
            .child(field("Existing branch", control))
            .children(switch)
            .children(line)
            .into_any_element()
    }

    /// What stands in place of the dropdown while it has no branch to offer.
    fn list_message(&self) -> String {
        match &self.task_branches {
            BranchListState::Loading => "Loading branches\u{2026}".to_owned(),
            BranchListState::Failed(error) => format!("Cannot list branches: {error}"),
            BranchListState::Loaded(_) => "No branches".to_owned(),
        }
    }

    /// The line under the existing branch dropdown.
    fn occupancy_line(&self) -> Option<String> {
        match self.occupancy()? {
            Occupancy::ProjectFolderChanged(1) => {
                Some("Checked out in the project folder \u{b7} 1 file changed".to_owned())
            }
            Occupancy::ProjectFolderChanged(changes) => Some(format!(
                "Checked out in the project folder \u{b7} {changes} files changed"
            )),
            Occupancy::Elsewhere(path) => Some(format!(
                "Checked out in {}",
                crate::settings::pages::tilde(&path)
            )),
            Occupancy::Free | Occupancy::ProjectFolderClean => None,
        }
    }

    /// The placeholder of the branch name field.
    fn branch_placeholder(&self, cx: &App) -> SharedString {
        let typed = self.name.read(cx).text();

        placeholder_for(&self.prefix, &name_or(&typed, &self.suggested))
    }

    /// The placeholder of the name field.
    fn name_placeholder(&self) -> SharedString {
        match (self.mode, self.chosen()) {
            (1, Some(branch)) => SharedString::from(branch.name.clone()),
            _ => self.suggested.clone(),
        }
    }

    fn refresh_placeholders(&mut self, cx: &mut Context<Self>) {
        let branch_name = self.branch_placeholder(cx);
        let name = self.name_placeholder();

        self.branch_name
            .update(cx, |input, cx| input.set_placeholder(branch_name, cx));
        self.name
            .update(cx, |input, cx| input.set_placeholder(name, cx));
    }

    fn set_mode(&mut self, mode: usize, cx: &mut Context<Self>) {
        self.mode = mode;
        self.refresh_placeholders(cx);
        cx.notify();
    }

    fn choose_existing(&mut self, index: usize, cx: &mut Context<Self>) {
        self.existing = index;
        self.switch_folder = true;
        self.refresh_placeholders(cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn suggested_name(&self) -> SharedString {
        self.suggested.clone()
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

    fn can_create(&self) -> bool {
        if self.busy {
            return false;
        }

        if self.mode == 0 {
            return true;
        }

        match self.occupancy() {
            Some(Occupancy::Free) => true,
            Some(Occupancy::ProjectFolderClean) => self.switch_folder,
            _ => false,
        }
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
    pub(crate) fn type_branch_name(&self, text: &str, cx: &mut Context<Self>) {
        self.branch_name
            .update(cx, |input, cx| input.set_text(text.to_owned(), cx));
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy
    }

    pub(crate) fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
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

    /// Speaks "Creating task" while the daemon works.
    fn status_region(&self) -> LiveRegion {
        LiveRegion::status(
            "new-task-status",
            if self.busy { "Creating task" } else { "" },
        )
    }

    /// Speaks why the task was not created.
    fn alert_region(&self) -> LiveRegion {
        LiveRegion::alert("new-task-error", self.error.clone().unwrap_or_default())
    }

    pub(crate) fn create(&mut self, cx: &mut Context<Self>) {
        if !self.can_create() {
            return;
        }

        let typed = self.name.read(cx).text();
        let first_message = self.message.read(cx).text();
        let kind = ORCHESTRATORS
            .get(self.orchestrator)
            .map_or(AgentKind::ClaudeCode, |(_, kind)| *kind);

        let (fallback, branch) = if self.mode == 0 {
            let typed_branch = self.branch_name.read(cx).text();
            let from = self.branches.get(self.branch).filter(|_| self.branch > 0);

            (
                self.suggested.clone(),
                TaskBranch::New {
                    name: Some(typed_branch.trim().to_owned()).filter(|name| !name.is_empty()),
                    from: from.map(ToString::to_string),
                },
            )
        } else {
            let Some(chosen) = self.chosen() else {
                return;
            };
            let in_folder = matches!(
                self.occupancy(),
                Some(Occupancy::ProjectFolderClean | Occupancy::ProjectFolderChanged(_))
            );

            (
                SharedString::from(chosen.name.clone()),
                TaskBranch::Existing {
                    name: chosen.name.clone(),
                    switch_project_folder: self.switch_folder && in_folder,
                },
            )
        };
        let name = SharedString::from(name_or(&typed, &fallback));

        cx.emit(NewTaskEvent::Created(NewTaskRequest {
            project: self.project,
            name,
            kind,
            first_message,
            branch,
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

/// Where the chosen existing branch is checked out.
enum Occupancy {
    Free,
    ProjectFolderClean,
    ProjectFolderChanged(u32),
    Elsewhere(std::path::PathBuf),
}

/// The branches a task can continue, as the sheet knows them.
enum BranchListState {
    Loading,
    Failed(SharedString),
    Loaded(TaskBranchList),
}

/// The typed text, or `fallback` when it is blank.
fn name_or(typed: &str, fallback: &str) -> String {
    match typed.trim() {
        "" => fallback.to_owned(),
        typed => typed.to_owned(),
    }
}

fn placeholder_for(prefix: &str, name: &str) -> SharedString {
    let slug = task_slug(name).unwrap_or_default();

    SharedString::from(format!("{prefix}{slug}"))
}

fn branch_is_taken(branches: &[SharedString], prefix: &str, slug: &str) -> bool {
    let branch = format!("{prefix}{slug}");

    branches.iter().any(|existing| *existing == branch)
}

/// A creature name that no listed branch `<prefix><name>` uses.
fn suggest_name(branches: &[SharedString], prefix: &str, seed: u64) -> SharedString {
    let picked = creatures::pick_slug(seed, |slug| {
        Ok::<_, std::convert::Infallible>(branch_is_taken(branches, prefix, slug))
    });

    SharedString::from(picked.unwrap_or_default())
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
        let on_mode = this.clone();
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

        let mode = SegmentedControl::new(
            "new-task-branch-mode",
            "Branch",
            ["New branch", "Existing branch"],
        )
        .selected(self.mode)
        .on_select(move |index, _, cx| {
            on_mode.update(cx, |this, cx| this.set_mode(index, cx));
        });

        let branch_row = if self.mode == 0 {
            h_flex()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(field("Branch name", self.branch_name.clone())),
                )
                .child(div().flex_1().min_w_0().child(field("Branch from", branch)))
                .into_any_element()
        } else {
            self.existing_branch_view(cx)
        };

        let create_label = if self.busy {
            "Creating task"
        } else {
            "Create task"
        };
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
                    .loading(self.busy)
                    .disabled(!self.busy && !self.can_create())
                    .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
            );

        Dialog::new("new-task-dialog", "New task", &self.focus_handle)
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, |_, cx| cx.emit(NewTaskEvent::Dismissed));
            })
            .child(field("Name", self.name.clone()))
            .child(field("Branch", mode))
            .child(branch_row)
            .child(div().w_1_2().child(field("Orchestrator", orchestrator)))
            .child(field("First message", self.message.clone()))
            .child(setup)
            .children(
                self.error
                    .clone()
                    .map(|message| Label::new(message).size(LabelSize::Small).color(Color::Del)),
            )
            .child(announce("new-task-alert", self.alert_region()))
            .child(announce("new-task-status", self.status_region()))
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
        let prefix = self.branch_prefix();
        let view =
            cx.new(|cx| NewTaskDialog::new(project, prefix, branches, default_kind, window, cx));
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
        self.list_task_branches(project, view.downgrade(), cx);
        cx.notify();
    }

    /// Asks the daemon for the branches a task can continue and gives the open sheet the list.
    fn list_task_branches(
        &mut self,
        project: ProjectId,
        dialog: WeakEntity<NewTaskDialog>,
        cx: &mut Context<Self>,
    ) {
        let daemon = self
            .data
            .project(project)
            .and_then(|project| project.daemon);
        let (Some(client), Some(daemon)) = (self.daemon.clone(), daemon) else {
            let updated = dialog.update(cx, |dialog, cx| {
                dialog.set_task_branches_failed("No daemon".into(), cx);
            });
            if let Err(error) = updated {
                log::debug!("the sheet closed before the task branches failed: {error}");
            }

            return;
        };

        let listing = cx.background_spawn(async move { client.task_branches(daemon) });

        cx.spawn(async move |_, cx| {
            let list = match listing.await {
                Ok(list) => list,
                Err(error) => {
                    log::debug!("cannot list the branches to continue: {error}");
                    let updated = dialog.update(cx, |dialog, cx| {
                        dialog.set_task_branches_failed(error.to_string().into(), cx);
                    });
                    if let Err(error) = updated {
                        log::debug!("the sheet closed before the task branches failed: {error}");
                    }

                    return;
                }
            };

            let updated = dialog.update(cx, |dialog, cx| dialog.set_task_branches(list, cx));

            if let Err(error) = updated {
                log::debug!("the sheet closed before the task branches came: {error}");
            }
        })
        .detach();
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

    /// The branch prefix from the settings file, the default when there is none.
    pub(crate) fn branch_prefix(&self) -> String {
        let default = || swarm_settings::TaskSettings::default().branch_prefix;
        let Some(path) = &self.settings_path else {
            return default();
        };

        match swarm_settings::load(path) {
            Ok(settings) => settings.tasks.branch_prefix,
            Err(error) => {
                log::warn!("cannot read the settings: {error}");
                default()
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
            branch: request.branch.clone(),
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
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::daemon::tests::{redraw, setup};
    use crate::live_probe::{assert_named, is_quiet, reversed, see_in};
    use swarm_ui::test_support::tab_walk;

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

    fn open_dialog(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<NewTaskDialog> {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_new_task(window, cx);
        });
        redraw(cx);

        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::NewTask { view, .. }) => view.clone(),
            _ => panic!("the dialog is not open"),
        })
    }

    #[gpui::test]
    fn the_dialog_draws_the_regions_that_speak_its_state(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let _view = open_dialog(&workspace, cx);

        assert!(cx.debug_bounds("new-task-alert").is_some());
        assert!(cx.debug_bounds("new-task-status").is_some());
    }

    #[gpui::test]
    fn creating_a_task_is_spoken_politely(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        let idle = see_in(view.read_with(cx, |dialog, _| dialog.status_region()), cx);
        view.update(cx, |dialog, cx| dialog.set_busy(true, cx));
        let busy = see_in(view.read_with(cx, |dialog, _| dialog.status_region()), cx);

        assert!(is_quiet(&idle));
        assert_eq!(busy.value.as_deref(), Some("Creating task"));
        assert_eq!(busy.live, Some(Live::Polite));
    }

    fn dialog_root(view: &Entity<NewTaskDialog>, cx: &VisualTestContext) -> FocusHandle {
        view.read_with(cx, |dialog, _| dialog.focus_handle.clone())
    }

    fn listed(
        name: &str,
        remote_only: bool,
        task: Option<u64>,
        checked_out: Option<&str>,
    ) -> ExistingBranch {
        ExistingBranch {
            name: name.to_owned(),
            remote_only,
            task: task.map(swarm_protocol::TaskId),
            checked_out: checked_out.map(std::path::PathBuf::from),
            in_project_folder: checked_out == Some("/tmp/demo"),
        }
    }

    fn task_branch_list(branches: Vec<ExistingBranch>, changes: u32) -> TaskBranchList {
        TaskBranchList {
            branches,
            project_folder: "/tmp/demo".into(),
            project_folder_changes: changes,
            base_branch: "main".to_owned(),
        }
    }

    fn continue_branches(
        view: &Entity<NewTaskDialog>,
        branches: Vec<ExistingBranch>,
        changes: u32,
        cx: &mut VisualTestContext,
    ) {
        view.update(cx, |dialog, cx| {
            dialog.set_task_branches(task_branch_list(branches, changes), cx);
            dialog.set_mode(1, cx);
        });
        redraw(cx);
    }

    fn collect_requests(
        view: &Entity<NewTaskDialog>,
        cx: &mut VisualTestContext,
    ) -> (
        std::rc::Rc<std::cell::RefCell<Vec<NewTaskRequest>>>,
        Subscription,
    ) {
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&requests);
        let subscription = cx.update(|_, app| {
            app.subscribe(view, move |_, event: &NewTaskEvent, _| {
                if let NewTaskEvent::Created(request) = event {
                    seen.borrow_mut().push(request.clone());
                }
            })
        });
        (requests, subscription)
    }

    #[gpui::test]
    fn tab_walks_the_dialog_in_order_and_never_leaves_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let root = dialog_root(&view, cx);

        let forward = tab_walk(cx, &root, "tab");
        let backward = tab_walk(cx, &root, "shift-tab");

        assert_eq!(
            forward,
            [
                "TextInput Name",
                "RadioButton New branch",
                "TextInput Branch name",
                "ComboBox Branch from",
                "ComboBox Orchestrator",
                "MultilineTextInput First message",
                "CheckBox Run setup script",
                "Button Cancel",
                "Button Create task",
                "Button Close dialog",
            ]
        );
        assert_eq!(backward, reversed(&forward));
        assert_named(&forward);
    }

    #[gpui::test]
    fn tab_walks_the_existing_branch_mode_in_order(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        continue_branches(
            &view,
            vec![listed("feature/x", false, None, Some("/tmp/demo"))],
            0,
            cx,
        );
        let root = dialog_root(&view, cx);

        let forward = tab_walk(cx, &root, "tab");
        let backward = tab_walk(cx, &root, "shift-tab");

        assert_eq!(
            forward,
            [
                "TextInput Name",
                "RadioButton Existing branch",
                "ComboBox Existing branch",
                "CheckBox Switch project folder to main",
                "ComboBox Orchestrator",
                "MultilineTextInput First message",
                "CheckBox Run setup script",
                "Button Cancel",
                "Button Create task",
                "Button Close dialog",
            ]
        );
        assert_eq!(backward, reversed(&forward));
        assert_named(&forward);
    }

    #[gpui::test]
    fn the_branch_name_placeholder_is_the_prefix_and_the_slug_of_the_suggestion(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        let placeholder = view.read_with(cx, NewTaskDialog::branch_placeholder);
        let suggested = view.read_with(cx, |dialog, _| dialog.suggested_name());

        assert_eq!(placeholder, format!("swarm/{suggested}"));
    }

    #[gpui::test]
    fn an_empty_prefix_leaves_the_slug_alone(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        view.update(cx, |dialog, _| dialog.prefix = String::new());
        let placeholder = view.read_with(cx, NewTaskDialog::branch_placeholder);
        let suggested = view.read_with(cx, |dialog, _| dialog.suggested_name());

        assert_eq!(placeholder, suggested);
    }

    #[gpui::test]
    fn the_branch_name_placeholder_follows_the_typed_name(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        cx.simulate_input("Fix the Login");

        let placeholder = view.read_with(cx, NewTaskDialog::branch_placeholder);
        assert_eq!(placeholder, "swarm/fix-the-login");
    }

    #[gpui::test]
    fn a_typed_branch_name_is_sent_and_a_blank_one_is_not(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let (requests, _subscription) = collect_requests(&view, cx);

        view.update(cx, NewTaskDialog::create);
        view.update(cx, |dialog, cx| {
            dialog
                .branch_name
                .update(cx, |input, cx| input.set_text("  dries/login ", cx));
            dialog.create(cx);
        });

        let branches: Vec<TaskBranch> = requests
            .borrow()
            .iter()
            .map(|request| request.branch.clone())
            .collect();
        assert_eq!(
            branches,
            [
                TaskBranch::New {
                    name: None,
                    from: None
                },
                TaskBranch::New {
                    name: Some("dries/login".to_owned()),
                    from: None
                },
            ]
        );
    }

    #[gpui::test]
    fn the_existing_mode_lists_only_branches_without_a_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let branches = vec![
            listed("busy", false, Some(3), None),
            listed("local", false, None, None),
            listed("remote", true, None, None),
        ];

        continue_branches(&view, branches, 0, cx);

        let labels = view.read_with(cx, |dialog, _| dialog.existing_labels());
        assert_eq!(names(&labels), ["local", "origin/remote"]);
    }

    #[gpui::test]
    fn a_branch_in_the_clean_project_folder_is_sent_with_the_switch(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let (requests, _subscription) = collect_requests(&view, cx);
        continue_branches(
            &view,
            vec![listed("feature/x", false, None, Some("/tmp/demo"))],
            0,
            cx,
        );

        let line = view.read_with(cx, |dialog, _| dialog.occupancy_line());
        view.update(cx, NewTaskDialog::create);

        let branches: Vec<TaskBranch> = requests
            .borrow()
            .iter()
            .map(|request| request.branch.clone())
            .collect();
        assert_eq!(line, None);
        assert_eq!(
            branches,
            [TaskBranch::Existing {
                name: "feature/x".to_owned(),
                switch_project_folder: true
            }]
        );
    }

    #[gpui::test]
    fn turning_the_switch_off_disables_create(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        continue_branches(
            &view,
            vec![listed("feature/x", false, None, Some("/tmp/demo"))],
            0,
            cx,
        );

        let before = view.read_with(cx, |dialog, _| dialog.can_create());
        view.update(cx, |dialog, _| dialog.switch_folder = false);
        let after = view.read_with(cx, |dialog, _| dialog.can_create());

        assert!(before);
        assert!(!after);
    }

    #[gpui::test]
    fn changes_in_the_project_folder_block_the_branch(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        continue_branches(
            &view,
            vec![listed("feature/x", false, None, Some("/tmp/demo"))],
            2,
            cx,
        );

        let line = view.read_with(cx, |dialog, _| dialog.occupancy_line());
        let can_create = view.read_with(cx, |dialog, _| dialog.can_create());

        assert_eq!(
            line.as_deref(),
            Some("Checked out in the project folder \u{b7} 2 files changed")
        );
        assert!(!can_create);
    }

    #[gpui::test]
    fn a_branch_in_another_worktree_blocks_the_branch(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        continue_branches(
            &view,
            vec![listed("feature/x", false, None, Some("/tmp/elsewhere"))],
            0,
            cx,
        );

        let line = view.read_with(cx, |dialog, _| dialog.occupancy_line());
        let can_create = view.read_with(cx, |dialog, _| dialog.can_create());

        assert_eq!(line.as_deref(), Some("Checked out in /tmp/elsewhere"));
        assert!(!can_create);
    }

    #[gpui::test]
    fn an_empty_name_in_the_existing_mode_uses_the_branch_name(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let (requests, _subscription) = collect_requests(&view, cx);
        continue_branches(&view, vec![listed("remote", true, None, None)], 0, cx);

        view.update(cx, NewTaskDialog::create);

        let sent: Vec<(SharedString, TaskBranch)> = requests
            .borrow()
            .iter()
            .map(|request| (request.name.clone(), request.branch.clone()))
            .collect();
        assert_eq!(
            sent,
            [(
                "remote".into(),
                TaskBranch::Existing {
                    name: "remote".to_owned(),
                    switch_project_folder: false
                }
            )]
        );
    }

    #[gpui::test]
    fn the_existing_mode_shows_the_chosen_branch_as_the_name_placeholder(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        continue_branches(&view, vec![listed("remote", true, None, None)], 0, cx);

        let existing = view.read_with(cx, |dialog, _| dialog.name_placeholder());
        view.update(cx, |dialog, cx| dialog.set_mode(0, cx));
        let new = view.read_with(cx, |dialog, _| dialog.name_placeholder());
        let suggested = view.read_with(cx, |dialog, _| dialog.suggested_name());

        assert_eq!(existing, "remote");
        assert_eq!(new, suggested);
    }

    #[gpui::test]
    fn without_branches_the_existing_mode_says_so(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        continue_branches(&view, vec![listed("busy", false, Some(3), None)], 0, cx);

        let can_create = view.read_with(cx, |dialog, _| dialog.can_create());

        assert!(cx.debug_bounds("new-task-existing-branch").is_none());
        assert!(!can_create);
    }

    #[gpui::test]
    fn while_the_branches_load_the_existing_mode_says_so_and_cannot_create(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        view.update(cx, |dialog, cx| {
            dialog.task_branches = BranchListState::Loading;
            dialog.set_mode(1, cx);
        });
        redraw(cx);

        let message = view.read_with(cx, |dialog, _| dialog.list_message());
        let can_create = view.read_with(cx, |dialog, _| dialog.can_create());
        assert_eq!(message, "Loading branches\u{2026}");
        assert!(cx.debug_bounds("new-task-existing-branch").is_none());
        assert!(!can_create);
    }

    #[gpui::test]
    fn a_failed_branch_list_shows_the_error_and_cannot_create(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        view.update(cx, |dialog, cx| {
            dialog.set_task_branches_failed("connection lost".into(), cx);
            dialog.set_mode(1, cx);
        });
        redraw(cx);

        let message = view.read_with(cx, |dialog, _| dialog.list_message());
        let can_create = view.read_with(cx, |dialog, _| dialog.can_create());
        assert_eq!(message, "Cannot list branches: connection lost");
        assert!(cx.debug_bounds("new-task-existing-branch").is_none());
        assert!(!can_create);
    }

    #[gpui::test]
    fn a_sheet_without_a_daemon_fails_the_branch_list(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        let message = view.read_with(cx, |dialog, _| dialog.list_message());

        assert_eq!(message, "Cannot list branches: No daemon");
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

    #[test]
    fn the_suggested_name_is_a_creature_that_no_branch_uses() {
        let taken: Vec<SharedString> = creatures::CREATURES
            .iter()
            .skip(1)
            .map(|creature| SharedString::from(format!("swarm/{}", creature.slug)))
            .collect();

        for seed in 0..30 {
            assert_eq!(
                suggest_name(&taken, "swarm/", seed),
                creatures::CREATURES[0].slug
            );
        }
    }

    #[gpui::test]
    fn the_name_field_suggests_a_creature_name(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        let suggested = view.read_with(cx, |dialog, _| dialog.suggested_name());

        assert!(creatures::is_creature_slug(&suggested));
    }

    #[gpui::test]
    fn a_suggestion_that_a_listed_branch_uses_is_replaced(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let suggested = view.read_with(cx, |dialog, _| dialog.suggested_name());

        view.update(cx, |dialog, cx| {
            dialog.set_branches(vec!["main".into(), format!("swarm/{suggested}").into()], cx);
        });

        let replaced = view.read_with(cx, |dialog, _| dialog.suggested_name());
        assert_ne!(replaced, suggested);
    }

    #[gpui::test]
    fn an_empty_name_creates_the_task_with_the_suggested_name(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&requests);
        let _subscription = cx.update(|_, app| {
            app.subscribe(&view, move |_, event: &NewTaskEvent, _| {
                if let NewTaskEvent::Created(request) = event {
                    seen.borrow_mut().push(request.clone());
                }
            })
        });

        view.update(cx, NewTaskDialog::create);

        let suggested = view.read_with(cx, |dialog, _| dialog.suggested_name());
        let names: Vec<SharedString> = requests
            .borrow()
            .iter()
            .map(|request| request.name.clone())
            .collect();
        assert_eq!(names, [suggested]);
    }

    #[gpui::test]
    fn a_typed_name_wins_over_the_suggestion(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&requests);
        let _subscription = cx.update(|_, app| {
            app.subscribe(&view, move |_, event: &NewTaskEvent, _| {
                if let NewTaskEvent::Created(request) = event {
                    seen.borrow_mut().push(request.name.clone());
                }
            })
        });

        cx.simulate_input("login");
        view.update(cx, NewTaskDialog::create);

        assert_eq!(*requests.borrow(), ["login"]);
    }

    #[gpui::test]
    fn a_failed_task_is_an_alert(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open_dialog(&workspace, cx);

        view.update(cx, |dialog, cx| dialog.fail("Cannot create".to_owned(), cx));
        let alert = see_in(view.read_with(cx, |dialog, _| dialog.alert_region()), cx);

        assert_eq!(alert.value.as_deref(), Some("Cannot create"));
        assert_eq!(alert.live, Some(Live::Assertive));
    }
}
