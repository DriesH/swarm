//! Settings › Projects: a project's `.swarm/config.toml`, read and saved through the daemon.

use std::path::PathBuf;

use gpui::{App, Context, Entity, Focusable as _, SharedString, Subscription, WeakEntity, Window};
use swarm_protocol::{
    ChecksRun, Client, ClientError, ProjectConfigFields, ProjectId as DaemonProjectId, PromptInfo,
    TaskId as DaemonTaskId,
};
use swarm_ui::prelude::*;

use super::{NOT_CONNECTED, Page, RemoveDialog, SAVE_DELAY, SettingsState, SettingsWindow, field};
use crate::model::{Project, ProjectId, sample_checks_run};
use crate::pull_request::run_is_over;
use crate::remove_project::{RemoveProjectDialog, RemoveProjectEvent};
use crate::review_comments::refusal;

pub(super) const DEFAULT_BRANCH: &str = "Default branch";

/// How a project's settings stand against its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    /// Sample values: nothing is read or saved.
    Sample,
    /// Asked of the daemon, not answered yet.
    Loading,
    Ready,
    /// The file does not parse, or the daemon cannot be reached. Nothing is saved.
    Unreadable(SharedString),
}

impl Load {
    pub(crate) fn editable(&self) -> bool {
        matches!(self, Self::Sample | Self::Ready)
    }
}

/// A prompt the project overrides in `.swarm/prompts/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectPrompt {
    pub name: String,
    pub path: PathBuf,
}

/// Starting the project's local checks, and what came of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChecksStatus {
    Idle,
    Checking {
        task: DaemonTaskId,
        /// The request to start them has not been answered yet.
        requesting: bool,
        /// The latest run the daemon reported since the click.
        run: Option<ChecksRun>,
    },
    Refused(SharedString),
}

impl ChecksStatus {
    /// The tone and the short text to show. `None` before the first run.
    pub(crate) fn pill(&self) -> Option<(AgentStatus, SharedString)> {
        match self {
            Self::Idle => None,
            Self::Refused(message) => Some((AgentStatus::Error, message.clone())),
            Self::Checking {
                requesting: true, ..
            }
            | Self::Checking { run: None, .. } => Some((AgentStatus::Working, "Running".into())),
            Self::Checking { run: Some(run), .. } => {
                let (tone, text) = run_summary(run);

                Some((tone, text.into()))
            }
        }
    }

    /// A start request is on its way, or a run has not ended.
    pub(crate) fn busy(&self) -> bool {
        match self {
            Self::Checking {
                requesting: true, ..
            } => true,
            Self::Checking { run: Some(run), .. } => !run_is_over(run),
            _ => false,
        }
    }
}

/// What a run adds up to, in a few words.
pub(crate) fn run_summary(run: &ChecksRun) -> (AgentStatus, String) {
    use swarm_protocol::CheckState;

    if !run_is_over(run) {
        return (AgentStatus::Working, "Running".to_owned());
    }

    let count = |state| {
        run.checks
            .iter()
            .filter(|check| check.state == state)
            .count()
    };
    let (total, failed) = (run.checks.len(), count(CheckState::Failed));

    if total == 0 {
        (AgentStatus::Idle, "No checks".to_owned())
    } else if failed > 0 {
        (AgentStatus::Error, format!("{failed} of {total} failed"))
    } else if count(CheckState::Stopped) > 0 {
        (AgentStatus::Blocked, "Stopped".to_owned())
    } else if count(CheckState::Skipped) > 0 {
        (AgentStatus::Blocked, "Skipped".to_owned())
    } else {
        (AgentStatus::Done, format!("{total} of {total} passed"))
    }
}

/// The two lists of strings on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum List {
    Copy,
    Checks,
}

impl List {
    fn label(self) -> &'static str {
        match self {
            Self::Copy => "File to copy",
            Self::Checks => "Check",
        }
    }

    /// The name of the row at `index`, which VoiceOver reads: "Check 1", "Check 2".
    fn row_label(self, index: usize) -> String {
        format!("{} {}", self.label(), index + 1)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectSettings {
    pub project: Project,
    pub load: Load,
    /// The local branches of the repository.
    pub branches: Vec<String>,
    /// `None` is the repository's default branch.
    pub base_branch: Option<String>,
    pub worktree_folder: String,
    pub port_range: String,
    pub ports_per_worktree: String,
    pub setup_script: String,
    pub copy_files: Vec<String>,
    pub checks: Vec<String>,
    pub checks_required: bool,
    /// The page has no field for the teardown script. A save sends it back as it was read.
    teardown: String,
    pub prompts: Vec<ProjectPrompt>,
    pub checking: ChecksStatus,
    /// Why the daemon refused the last save.
    pub refusal: Option<SharedString>,
}

impl ProjectSettings {
    /// A project the daemon knows starts empty and loads; any other keeps sample values.
    pub(super) fn new(project: Project) -> Self {
        if project.daemon.is_some() {
            Self::blank(project, Load::Loading)
        } else {
            Self::sample(project)
        }
    }

    fn blank(project: Project, load: Load) -> Self {
        Self {
            project,
            load,
            branches: Vec::new(),
            base_branch: None,
            worktree_folder: String::new(),
            port_range: String::new(),
            ports_per_worktree: String::new(),
            setup_script: String::new(),
            copy_files: Vec::new(),
            checks: Vec::new(),
            checks_required: false,
            teardown: String::new(),
            prompts: Vec::new(),
            checking: ChecksStatus::Idle,
            refusal: None,
        }
    }

    fn sample(project: Project) -> Self {
        let base_branch = project.base_branch.to_string();
        let mut branches = vec![base_branch.clone()];
        if base_branch != "main" {
            branches.push("main".to_owned());
        }

        let prompt = ProjectPrompt {
            name: "worker".to_owned(),
            path: PathBuf::from(".swarm/prompts/worker.md"),
        };

        Self {
            branches,
            base_branch: Some(base_branch),
            worktree_folder: ".worktrees".to_owned(),
            port_range: "5170-5199".to_owned(),
            ports_per_worktree: "3".to_owned(),
            setup_script:
                "cargo fetch\npnpm --dir docs install --frozen-lockfile\ncp .env.example .env"
                    .to_owned(),
            copy_files: vec![".env".to_owned(), "CLAUDE.local.md".to_owned()],
            checks: vec![
                "cargo test --workspace".to_owned(),
                "cargo clippy --workspace".to_owned(),
            ],
            checks_required: true,
            prompts: vec![prompt],
            ..Self::blank(project, Load::Sample)
        }
    }

    /// The dropdown's options: the default branch, then every branch. A base branch the
    /// repository no longer has stays in the list so the dropdown can show it.
    pub(super) fn base_branch_options(&self) -> Vec<String> {
        let mut options = vec![DEFAULT_BRANCH.to_owned()];
        options.extend(self.branches.iter().cloned());

        if let Some(current) = &self.base_branch
            && !self.branches.contains(current)
        {
            options.push(current.clone());
        }

        options
    }

    pub(super) fn base_branch_index(&self) -> usize {
        let Some(current) = &self.base_branch else {
            return 0;
        };

        self.base_branch_options()
            .iter()
            .skip(1)
            .position(|option| option == current)
            .map_or(0, |position| position + 1)
    }

    fn list(&self, list: List) -> &Vec<String> {
        match list {
            List::Copy => &self.copy_files,
            List::Checks => &self.checks,
        }
    }

    fn list_mut(&mut self, list: List) -> &mut Vec<String> {
        match list {
            List::Copy => &mut self.copy_files,
            List::Checks => &mut self.checks,
        }
    }

    /// Takes what the daemon read from the file. A list that holds the same entries as the
    /// file keeps its blank rows, which the user has not filled yet.
    fn apply(&mut self, fields: ProjectConfigFields) {
        self.base_branch = fields.base_branch;
        self.worktree_folder = fields.worktree_folder;
        self.port_range = fields.port_range;
        self.ports_per_worktree = fields.ports_per_worktree.to_string();
        self.setup_script = fields.setup;
        self.teardown = fields.teardown;
        self.checks_required = fields.checks_required;

        if filled(&self.copy_files) != fields.copy {
            self.copy_files = fields.copy;
        }

        if filled(&self.checks) != fields.checks {
            self.checks = fields.checks;
        }
    }

    /// What a save sends, or why the page cannot be saved as it is. Blank entries in the
    /// lists are rows the user has not filled yet.
    pub(super) fn fields(&self) -> Result<ProjectConfigFields, SharedString> {
        let ports_per_worktree = match self.ports_per_worktree.trim() {
            "" => 0,
            digits => digits
                .parse()
                .map_err(|_| SharedString::from("Ports per worktree is too large"))?,
        };

        Ok(ProjectConfigFields {
            base_branch: self.base_branch.clone(),
            worktree_folder: self.worktree_folder.clone(),
            port_range: self.port_range.clone(),
            ports_per_worktree,
            copy: filled(&self.copy_files),
            setup: self.setup_script.clone(),
            teardown: self.teardown.clone(),
            checks: filled(&self.checks),
            checks_required: self.checks_required,
        })
    }
}

/// The entries of a list that hold something, without the spaces around them.
fn filled(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

/// What to do when a pause after an edit ends, or a save ends.
#[derive(Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a Due is made, matched and dropped at once"
)]
enum Due {
    /// Nothing to send: a save is on its way, a newer pause runs, or there are no fields.
    Wait,
    /// The page matches what the daemon holds.
    Same,
    /// `base` is the fields as the app last read or saved them. The daemon writes only the
    /// fields of `fields` that differ from it, so a hand edit to another field stays.
    Send {
        base: ProjectConfigFields,
        fields: ProjectConfigFields,
    },
}

/// How the saves of one project stand. At most one save is on its way, and the fields it
/// sends are always the newest the page held, so the daemon never gets an older value
/// after a newer one.
#[derive(Default)]
pub(super) struct ProjectSave {
    /// What the daemon holds: the last read or saved fields.
    confirmed: Option<ProjectConfigFields>,
    /// The newest fields the page held.
    queued: Option<ProjectConfigFields>,
    in_flight: bool,
    /// The pause after the last edit is running.
    pausing: bool,
    /// Cancels the pause when a newer edit restarts it.
    timer: Option<gpui::Task<()>>,
    /// Goes up with every edit.
    edits: u64,
    /// The count of `edits` when the read that is going started.
    reading: Option<u64>,
}

impl ProjectSave {
    /// An edit is waiting for the pause, or on its way to the daemon.
    fn busy(&self) -> bool {
        self.in_flight || self.pausing
    }

    /// The page holds `fields` now. Returns whether the pause starts over.
    fn edited(&mut self, fields: ProjectConfigFields) -> bool {
        if self.queued.as_ref() == Some(&fields) {
            return false;
        }

        self.queued = Some(fields);
        self.pausing = true;
        self.edits += 1;

        true
    }

    /// The page holds fields that cannot be saved. Nothing goes out until it does again.
    fn invalid(&mut self) {
        self.queued = None;
        self.pausing = false;
        self.timer = None;
        self.edits += 1;
    }

    /// A read may start: none is going and no edit waits to be saved.
    fn can_read(&self) -> bool {
        self.reading.is_none() && !self.busy()
    }

    fn begin_read(&mut self) {
        self.reading = Some(self.edits);
    }

    /// The read ended. Returns whether its answer is still the newest word: an answer that
    /// began before an edit is older than the page, so the page keeps its value.
    fn end_read(&mut self) -> bool {
        self.reading.take() == Some(self.edits)
    }

    /// The pause is over.
    fn due(&mut self) -> Due {
        self.pausing = false;
        self.timer = None;

        self.next()
    }

    fn next(&mut self) -> Due {
        if self.in_flight {
            return Due::Wait;
        }

        let (Some(fields), Some(base)) = (self.queued.clone(), self.confirmed.clone()) else {
            return Due::Wait;
        };

        if base == fields {
            return Due::Same;
        }

        self.in_flight = true;

        Due::Send { base, fields }
    }

    /// The save of `sent` ended. A newer value, if one waits, goes out now.
    fn ended(&mut self, sent: &ProjectConfigFields, saved: bool) -> Due {
        self.in_flight = false;

        if saved {
            self.confirmed = Some(sent.clone());
        }

        if self.pausing || self.queued.as_ref() == Some(sent) {
            return Due::Wait;
        }

        self.next()
    }
}

/// One row of a list: its text field, and the subscription that writes the text into the
/// state. The subscription ends with the row.
pub(super) struct ListInput {
    pub(super) input: Entity<TextInput>,
    _subscription: Subscription,
}

/// The text fields of one project's page.
pub(super) struct ProjectInputs {
    pub(super) worktree_folder: Entity<TextInput>,
    pub(super) port_range: Entity<TextInput>,
    pub(super) ports_per_worktree: Entity<TextInput>,
    pub(super) setup_script: Entity<TextArea>,
    pub(super) copy: Vec<ListInput>,
    pub(super) checks: Vec<ListInput>,
}

impl ProjectInputs {
    fn list(&self, list: List) -> &Vec<ListInput> {
        match list {
            List::Copy => &self.copy,
            List::Checks => &self.checks,
        }
    }

    fn list_mut(&mut self, list: List) -> &mut Vec<ListInput> {
        match list {
            List::Copy => &mut self.copy,
            List::Checks => &mut self.checks,
        }
    }
}

/// What the daemon answered when a project page opened.
pub(super) struct Loaded {
    config: Result<(ProjectConfigFields, Option<String>), ClientError>,
    branches: Option<Vec<String>>,
    prompts: Option<Vec<PromptInfo>>,
}

impl Loaded {
    /// Blocks on the daemon: call it on the background executor.
    fn read(client: &Client, project: DaemonProjectId) -> Self {
        let config = client.project_config(project);

        if config.is_err() {
            return Self {
                config,
                branches: None,
                prompts: None,
            };
        }

        Self {
            config,
            branches: logged("branches", client.list_branches(project)),
            prompts: logged("prompts", client.prompts(Some(project))),
        }
    }
}

fn logged<T>(what: &str, result: Result<T, ClientError>) -> Option<T> {
    result
        .inspect_err(|error| log::debug!("cannot list the {what}: {error}"))
        .ok()
}

impl SettingsState {
    /// Lists the projects of the workspace. A project with an edit on its way keeps it.
    pub(super) fn use_projects(&mut self, projects: Vec<Project>) {
        let mut old: Vec<_> = std::mem::take(&mut self.projects)
            .into_iter()
            .zip(std::mem::take(&mut self.saves))
            .collect();

        for project in projects {
            let kept = old
                .iter()
                .position(|(entry, save)| entry.project.id == project.id && save.busy());

            let (entry, save) = match kept {
                Some(position) => {
                    let (mut entry, save) = old.swap_remove(position);
                    entry.project = project;

                    (entry, save)
                }
                None => (ProjectSettings::new(project), ProjectSave::default()),
            };

            self.projects.push(entry);
            self.saves.push(save);
        }
    }

    /// Lists the projects of the workspace as they are now. A project not listed yet gets an
    /// entry at the end; the others keep theirs. Returns where the new entries are.
    pub(super) fn add_projects(&mut self, projects: &[Project]) -> Vec<usize> {
        let mut added = Vec::new();

        for project in projects {
            if let Some(entry) = self.project_mut(project.id) {
                entry.project = project.clone();
                continue;
            }

            added.push(self.projects.len());
            self.projects.push(ProjectSettings::new(project.clone()));
            self.saves.push(ProjectSave::default());
        }

        added
    }

    /// Drops the entry of a removed project, with the save it waited for. Returns where it was.
    pub(super) fn remove_project(
        &mut self,
        id: ProjectId,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_of(id)?;

        self.projects.remove(index);

        if index < self.saves.len() {
            self.saves.remove(index);
        }

        cx.notify();

        Some(index)
    }

    pub(super) fn index_of(&self, id: ProjectId) -> Option<usize> {
        self.projects
            .iter()
            .position(|entry| entry.project.id == id)
    }

    fn project_mut(&mut self, id: ProjectId) -> Option<&mut ProjectSettings> {
        self.projects
            .iter_mut()
            .find(|entry| entry.project.id == id)
    }

    /// The daemon connection, while it holds.
    fn client(&self, cx: &App) -> Option<Client> {
        let workspace = self.workspace.as_ref()?.upgrade()?;

        workspace
            .read(cx)
            .daemon
            .clone()
            .filter(Client::is_connected)
    }

    // Load

    /// Marks the project as loading. Returns what the read needs, or `None` when there is
    /// nothing to read: a sample project, a read already going, or an edit on its way
    /// that a read would undo.
    fn begin_load(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Option<(Client, DaemonProjectId)> {
        let daemon = self.projects.get(index)?.project.daemon?;

        if !self.saves.get(index)?.can_read() {
            return None;
        }

        let client = self.client(cx);
        let entry = self.projects.get_mut(index)?;

        let Some(client) = client else {
            entry.load = Load::Unreadable(NOT_CONNECTED.into());
            cx.notify();

            return None;
        };

        if entry.load != Load::Ready {
            entry.load = Load::Loading;
        }

        self.saves.get_mut(index)?.begin_read();
        cx.notify();

        Some((client, daemon))
    }

    /// Takes the daemon's answer. Returns whether the page values need to reach the inputs.
    fn finish_load(&mut self, index: usize, loaded: Loaded, cx: &mut Context<Self>) -> bool {
        let (Some(entry), Some(save)) = (self.projects.get_mut(index), self.saves.get_mut(index))
        else {
            return false;
        };

        // An edit was made while the read ran. The read is older than the edit.
        if !save.end_read() {
            return false;
        }

        match loaded.config {
            Err(error) => entry.load = Load::Unreadable(refusal(&error)),
            Ok((fields, Some(error))) => {
                entry.apply(fields);
                entry.load = Load::Unreadable(error.into());
            }
            Ok((fields, None)) => {
                entry.apply(fields);
                entry.load = Load::Ready;
                entry.refusal = None;

                if let Some(branches) = loaded.branches {
                    entry.branches = branches;
                }

                if let Some(prompts) = loaded.prompts {
                    entry.prompts = prompts
                        .into_iter()
                        .filter_map(|prompt| {
                            Some(ProjectPrompt {
                                name: prompt.name,
                                path: prompt.project_path?,
                            })
                        })
                        .collect();
                }

                save.confirmed = entry.fields().ok();
                save.queued.clone_from(&save.confirmed);
            }
        }

        cx.notify();

        true
    }

    // Save

    pub(super) fn queue_project_saves(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.projects.len() {
            self.queue_project_save(index, cx);
        }
    }

    /// Saves the page after a pause in editing. A newer edit restarts the pause, so only
    /// the newest value goes out.
    fn queue_project_save(&mut self, index: usize, cx: &mut Context<Self>) {
        let (Some(entry), Some(save)) = (self.projects.get_mut(index), self.saves.get_mut(index))
        else {
            return;
        };

        if entry.load != Load::Ready {
            return;
        }

        let fields = match entry.fields() {
            Ok(fields) => fields,
            Err(message) => {
                save.invalid();

                if entry.refusal.as_ref() != Some(&message) {
                    entry.refusal = Some(message);
                    cx.notify();
                }

                return;
            }
        };

        if !save.edited(fields) {
            return;
        }

        let id = entry.project.id;
        save.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;

            if let Err(error) = this.update(cx, |state, cx| state.pause_over(id, cx)) {
                log::debug!("the settings closed before the project was saved: {error}");
            }
        }));
    }

    fn pause_over(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        let Some(due) = self
            .index_of(id)
            .and_then(|index| self.saves.get_mut(index))
            .map(ProjectSave::due)
        else {
            return;
        };

        self.act(id, due, cx);
    }

    fn act(&mut self, id: ProjectId, due: Due, cx: &mut Context<Self>) {
        match due {
            Due::Wait => {}
            Due::Same => {
                let entry = self
                    .index_of(id)
                    .and_then(|index| self.projects.get_mut(index));

                if entry.is_some_and(|entry| entry.refusal.take().is_some()) {
                    cx.notify();
                }
            }
            Due::Send { base, fields } => self.send(id, base, fields, cx),
        }
    }

    fn send(
        &mut self,
        id: ProjectId,
        base: ProjectConfigFields,
        fields: ProjectConfigFields,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let client = self.client(cx);
        let (Some(entry), Some(save)) = (self.projects.get_mut(index), self.saves.get_mut(index))
        else {
            return;
        };

        let (Some(client), Some(daemon)) = (client, entry.project.daemon) else {
            save.ended(&fields, false);
            entry.refusal = Some(NOT_CONNECTED.into());
            cx.notify();

            return;
        };

        let sent = fields.clone();
        let saving =
            cx.background_spawn(async move { client.save_project_config(daemon, base, fields) });

        cx.spawn(async move |this, cx| {
            let result = saving.await;
            let ended = this.update(cx, |state, cx| state.saved(id, &sent, result, cx));

            if let Err(error) = ended {
                log::debug!("the settings closed before the project was saved: {error}");
            }
        })
        .detach();
    }

    fn saved(
        &mut self,
        id: ProjectId,
        sent: &ProjectConfigFields,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let (Some(entry), Some(save)) = (self.projects.get_mut(index), self.saves.get_mut(index))
        else {
            return;
        };

        let due = save.ended(sent, result.is_ok());

        entry.refusal = result.err().map(|error| refusal(&error));
        self.act(id, due, cx);
        cx.notify();
    }

    // Lists

    pub(super) fn set_item(
        &mut self,
        project: usize,
        list: List,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let item = self
            .projects
            .get_mut(project)
            .and_then(|entry| entry.list_mut(list).get_mut(index));

        if let Some(item) = item {
            *item = text;
            cx.notify();
        }
    }

    pub(super) fn add_item(&mut self, project: usize, list: List, cx: &mut Context<Self>) {
        if let Some(entry) = self.projects.get_mut(project) {
            entry.list_mut(list).push(String::new());
            cx.notify();
        }
    }

    pub(super) fn remove_item(
        &mut self,
        project: usize,
        list: List,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(items) = self
            .projects
            .get_mut(project)
            .map(|entry| entry.list_mut(list))
            && index < items.len()
        {
            items.remove(index);
            cx.notify();
        }
    }

    pub(crate) fn choose_base_branch(&mut self, project: usize, option: usize) {
        let Some(entry) = self.projects.get_mut(project) else {
            return;
        };

        entry.base_branch = match option {
            0 => None,
            option => entry.base_branch_options().get(option).cloned(),
        };
    }

    // Local checks

    /// The task "Run checks now" runs on: the one open in the workspace when it belongs
    /// to the project, else the project's first open task.
    pub(crate) fn checks_task(&self, index: usize, cx: &App) -> Option<DaemonTaskId> {
        let project = self.projects.get(index)?.project.id;
        let workspace = self.workspace.as_ref()?.upgrade()?;
        let workspace = workspace.read(cx);

        let usable = |task: &&crate::model::Task| {
            task.project == project && !task.is_start && task.daemon.is_some()
        };
        let active = workspace.data.task(workspace.active_task).filter(usable);
        let task = active.or_else(|| workspace.data.tasks.iter().find(usable))?;

        task.daemon.as_ref().map(|daemon| daemon.id)
    }

    pub(crate) fn can_run_checks(&self, index: usize, cx: &App) -> bool {
        let (Some(entry), Some(save)) = (self.projects.get(index), self.saves.get(index)) else {
            return false;
        };

        // The daemon runs the checks of the file, so an edit not saved yet would be missed.
        !save.busy()
            && !entry.checking.busy()
            && (entry.load == Load::Sample || self.checks_task(index, cx).is_some())
    }

    pub(crate) fn run_checks(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.can_run_checks(index, cx) {
            return;
        }

        let (client, task) = (self.client(cx), self.checks_task(index, cx));
        let Some(entry) = self.projects.get_mut(index) else {
            return;
        };

        if entry.load == Load::Sample {
            let sample = DaemonTaskId(0);
            entry.checking = ChecksStatus::Checking {
                task: sample,
                requesting: false,
                run: Some(sample_checks_run(sample, "sample")),
            };
            cx.notify();

            return;
        }

        let Some(task) = task else {
            return;
        };

        let Some(client) = client else {
            entry.checking = ChecksStatus::Refused(NOT_CONNECTED.into());
            cx.notify();

            return;
        };

        let id = entry.project.id;
        entry.checking = ChecksStatus::Checking {
            task,
            requesting: true,
            run: None,
        };
        cx.notify();

        let starting = cx.background_spawn(async move { client.run_checks(task) });

        cx.spawn(async move |this, cx| {
            let result = starting.await;
            let ended = this.update(cx, |state, cx| state.checks_started(id, task, result, cx));

            if let Err(error) = ended {
                log::debug!("the settings closed before the checks started: {error}");
            }
        })
        .detach();
    }

    fn checks_started(
        &mut self,
        id: ProjectId,
        started: DaemonTaskId,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .index_of(id)
            .and_then(|index| self.projects.get_mut(index))
        else {
            return;
        };

        match (&mut entry.checking, result) {
            (
                ChecksStatus::Checking {
                    task, requesting, ..
                },
                Ok(()),
            ) if *task == started => *requesting = false,
            (ChecksStatus::Checking { task, .. }, Err(error)) if *task == started => {
                entry.checking = ChecksStatus::Refused(refusal(&error));
            }
            _ => return,
        }

        cx.notify();
    }

    /// Keeps a run the daemon reported, for the page that started it.
    pub(super) fn record_run(&mut self, run: &ChecksRun, cx: &mut Context<Self>) {
        let mut changed = false;

        for entry in &mut self.projects {
            if let ChecksStatus::Checking {
                task, run: shown, ..
            } = &mut entry.checking
                && *task == run.task
                && shown.as_ref() != Some(run)
            {
                *shown = Some(run.clone());
                changed = true;
            }
        }

        if changed {
            cx.notify();
        }
    }
}

impl SettingsWindow {
    /// Reads the config of the project the page shows.
    pub(super) fn open_project(&mut self, cx: &mut Context<Self>) {
        let Page::Project(id) = self.page else {
            return;
        };

        if let Some(index) = self.state.read(cx).index_of(id) {
            self.load_project(index, cx);
        }
    }

    /// Lists the projects of the workspace. A new one gets its nav row and its inputs.
    pub(super) fn add_projects(&mut self, projects: &[Project], cx: &mut Context<Self>) {
        let added = self.state.update(cx, |state, cx| {
            let added = state.add_projects(projects);
            cx.notify();

            added
        });

        for index in added {
            self.nav_focus.push(cx.focus_handle().tab_stop(true));

            if let Some(inputs) = self.project_inputs(index, cx) {
                self.inputs.projects.push(inputs);
                self.sync_inputs(index, cx);
            }
        }

        cx.notify();
    }

    pub(super) fn load_project(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self
            .state
            .read(cx)
            .projects
            .get(index)
            .map(|entry| entry.project.id);
        let request = self
            .state
            .update(cx, |state, cx| state.begin_load(index, cx));
        self.sync_inputs(index, cx);

        let Some((client, daemon)) = request else {
            return;
        };

        let reading = cx.background_spawn(async move { Loaded::read(&client, daemon) });

        cx.spawn(async move |this, cx| {
            let loaded = reading.await;
            let shown = this.update(cx, |this, cx| this.finish_load(id, loaded, cx));

            if let Err(error) = shown {
                log::debug!("the settings closed before the project was read: {error}");
            }
        })
        .detach();
    }

    /// Takes the answer for the project, wherever it sits in the list by now.
    fn finish_load(&mut self, id: Option<ProjectId>, loaded: Loaded, cx: &mut Context<Self>) {
        let Some(index) = id.and_then(|id| self.state.read(cx).index_of(id)) else {
            return;
        };
        let applied = self
            .state
            .update(cx, |state, cx| state.finish_load(index, loaded, cx));

        if applied {
            self.sync_inputs(index, cx);
        }
    }

    /// Opens the dialog that asks before the project of page `index` goes.
    pub(super) fn open_remove_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let project = state.projects.get(index).map(|entry| entry.project.id);
        let workspace = state.workspace.as_ref().and_then(WeakEntity::upgrade);

        let (Some(project), Some(workspace)) = (project, workspace) else {
            log::warn!("the settings have no workspace to remove the project from");
            return;
        };
        let Some((name, root)) = workspace.read(cx).remove_project_facts(project) else {
            return;
        };

        let view = cx.new(|cx| RemoveProjectDialog::new(project, name, root, window, cx));
        let checked = view.clone();
        let asked = workspace.clone();
        let workspace = workspace.downgrade();
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                move |this, view, event, window, cx| match event {
                    RemoveProjectEvent::Dismissed => this.close_remove_project(window, cx),
                    RemoveProjectEvent::Confirmed => {
                        let dialog = view.clone();

                        crate::sidebar::update_workspace(&workspace, cx, move |workspace, cx| {
                            workspace.confirm_remove_project(dialog, cx);
                        });
                    }
                },
            );

        self.remove_dialog = Some(RemoveDialog {
            view,
            _subscription: subscription,
        });
        cx.notify();

        asked.update(cx, |workspace, cx| {
            workspace.check_remove_project(&checked, cx);
        });
    }

    /// Closes the dialog. Focus goes back to the Remove project button that opened it.
    fn close_remove_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_dialog = None;

        if matches!(self.page, Page::Project(_)) {
            window.focus(&self.remove_focus, cx);
        } else {
            self.focus_nav(window, cx);
        }

        cx.notify();
    }

    /// The project is gone: drops its inputs and its nav row, and leaves its page.
    pub(super) fn drop_project(
        &mut self,
        project: ProjectId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index < self.inputs.projects.len() {
            self.inputs.projects.remove(index);
        }

        let row = Page::SECTIONS.len() + index;
        if row < self.nav_focus.len() {
            self.nav_focus.remove(row);
        }

        if self.page == Page::Project(project) {
            self.select_page(Page::Agents, cx);
        }

        if window.is_window_active() {
            self.focus_nav(window, cx);
        }
        cx.notify();
    }

    pub(super) fn project_inputs(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Option<ProjectInputs> {
        let project = self.state.read(cx).projects.get(index)?.clone();
        let id = project.project.id;
        let state = self.state.clone();

        let worktree_folder = field("Worktree folder", &project.worktree_folder, true, cx);
        super::bind_text(
            &mut self.subscriptions,
            &worktree_folder,
            &state,
            move |state, text| {
                if let Some(project) = state.project_mut(id) {
                    project.worktree_folder = text;
                }
            },
            cx,
        );

        let port_range = field("Port range", &project.port_range, true, cx);
        super::bind_text(
            &mut self.subscriptions,
            &port_range,
            &state,
            move |state, text| {
                if let Some(project) = state.project_mut(id) {
                    project.port_range = text;
                }
            },
            cx,
        );

        let ports_per_worktree =
            super::numeric_field("Ports per worktree", &project.ports_per_worktree, cx);
        super::bind_text(
            &mut self.subscriptions,
            &ports_per_worktree,
            &state,
            move |state, text| {
                if let Some(project) = state.project_mut(id) {
                    project.ports_per_worktree = text;
                }
            },
            cx,
        );

        let setup_script = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Setup script", cx);
            area.set_mono(true, cx);
            area.set_rows(3, cx);
            area.set_text(project.setup_script.clone(), cx);
            area
        });
        self.subscriptions.push(cx.subscribe(
            &setup_script,
            move |this, _, event: &TextInputEvent, cx| {
                if let TextInputEvent::Changed(text) = event {
                    let text = text.to_string();
                    this.state.update(cx, |state, cx| {
                        if let Some(project) = state.project_mut(id) {
                            project.setup_script = text;
                            cx.notify();
                        }
                    });
                }
            },
        ));

        let copy = Self::list_inputs(id, List::Copy, &project.copy_files, cx);
        let checks = Self::list_inputs(id, List::Checks, &project.checks, cx);

        Some(ProjectInputs {
            worktree_folder,
            port_range,
            ports_per_worktree,
            setup_script,
            copy,
            checks,
        })
    }

    fn list_inputs(
        project: ProjectId,
        list: List,
        items: &[String],
        cx: &mut Context<Self>,
    ) -> Vec<ListInput> {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| Self::list_input(project, list, index, item, cx))
            .collect()
    }

    /// The row's subscription finds its project by id when the text changes, as the
    /// project may have moved in the list since the row was made.
    fn list_input(
        project: ProjectId,
        list: List,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> ListInput {
        let input = field(list.row_label(index), text, true, cx);

        let subscription = cx.subscribe(&input, move |this, input, event: &TextInputEvent, cx| {
            let TextInputEvent::Changed(text) = event else {
                return;
            };
            let Some(position) = this.state.read(cx).index_of(project) else {
                return;
            };

            let index = this.inputs.projects.get(position).and_then(|inputs| {
                inputs
                    .list(list)
                    .iter()
                    .position(|candidate| candidate.input == input)
            });

            if let Some(index) = index {
                let text = text.to_string();
                this.state.update(cx, |state, cx| {
                    state.set_item(position, list, index, text, cx);
                });
            }
        });

        ListInput {
            input,
            _subscription: subscription,
        }
    }

    /// Names the rows of a list by their place, so a removed row does not leave a gap.
    fn name_rows(&self, project: usize, list: List, cx: &mut Context<Self>) {
        let Some(inputs) = self.inputs.projects.get(project) else {
            return;
        };

        for (index, row) in inputs.list(list).iter().enumerate() {
            row.input
                .update(cx, |input, cx| input.set_label(list.row_label(index), cx));
        }
    }

    /// Adds an empty row and moves focus to it.
    pub(super) fn add_item(
        &mut self,
        project: usize,
        list: List,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self
            .state
            .read(cx)
            .projects
            .get(project)
            .map(|entry| entry.project.id)
        else {
            return;
        };
        let next = self
            .inputs
            .projects
            .get(project)
            .map_or(0, |inputs| inputs.list(list).len());
        let row = Self::list_input(id, list, next, "", cx);
        let focus = row.input.read(cx).focus_handle(cx);

        if let Some(inputs) = self.inputs.projects.get_mut(project) {
            inputs.list_mut(list).push(row);
        }

        self.state
            .update(cx, |state, cx| state.add_item(project, list, cx));
        window.focus(&focus, cx);
    }

    pub(super) fn remove_item(
        &mut self,
        project: usize,
        list: List,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(inputs) = self.inputs.projects.get_mut(project)
            && index < inputs.list(list).len()
        {
            inputs.list_mut(list).remove(index);
        }

        self.name_rows(project, list, cx);
        self.state
            .update(cx, |state, cx| state.remove_item(project, list, index, cx));
    }

    /// Writes the state of a project into its inputs, and makes them read-only while the
    /// page cannot be edited.
    pub(super) fn sync_inputs(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(project) = self.state.read(cx).projects.get(index).cloned() else {
            return;
        };
        let editable = project.load.editable();
        let placeholder = if project.load == Load::Loading {
            "Loading\u{2026}"
        } else {
            ""
        };

        if let Some(inputs) = self.inputs.projects.get(index) {
            for (input, text) in [
                (&inputs.worktree_folder, &project.worktree_folder),
                (&inputs.port_range, &project.port_range),
                (&inputs.ports_per_worktree, &project.ports_per_worktree),
            ] {
                input.update(cx, |input, cx| {
                    if input.text().as_ref() != text {
                        input.set_text(text.clone(), cx);
                    }

                    input.set_disabled(!editable, cx);
                    input.set_placeholder(placeholder, cx);
                });
            }

            inputs.setup_script.update(cx, |input, cx| {
                if input.text().as_ref() != project.setup_script {
                    input.set_text(project.setup_script.clone(), cx);
                }

                input.set_disabled(!editable, cx);
                input.set_placeholder(placeholder, cx);
            });
        }

        for list in [List::Copy, List::Checks] {
            self.sync_list(index, list, project.list(list), editable, cx);
        }
    }

    fn sync_list(
        &mut self,
        project: usize,
        list: List,
        items: &[String],
        editable: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(inputs) = self.inputs.projects.get(project) else {
            return;
        };
        let Some(id) = self
            .state
            .read(cx)
            .projects
            .get(project)
            .map(|entry| entry.project.id)
        else {
            return;
        };

        let shown: Vec<String> = inputs
            .list(list)
            .iter()
            .map(|row| row.input.read(cx).text().to_string())
            .collect();

        if shown != items {
            let rebuilt = Self::list_inputs(id, list, items, cx);

            if let Some(inputs) = self.inputs.projects.get_mut(project) {
                *inputs.list_mut(list) = rebuilt;
            }
        }

        if let Some(inputs) = self.inputs.projects.get(project) {
            for row in inputs.list(list) {
                row.input
                    .update(cx, |input, cx| input.set_disabled(!editable, cx));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use gpui::{Focusable, Modifiers, TestAppContext, VisualTestContext, WindowHandle, px, size};
    use swarm_protocol::{CheckState, Event, LocalCheck, PrEvent, PromptLayer, Request};
    use swarm_term::ExitStatus;

    use super::*;
    use crate::daemon::tests::{
        fake_daemon_replying, project_info, redraw, task_info, wait_a_little, wait_until,
    };
    use crate::settings::{link_workspace, open_window, use_opener};
    use crate::workspace::Workspace;

    type Requests = Arc<Mutex<Vec<Request>>>;

    /// A daemon that records every request, then answers with `reply` or the defaults.
    fn daemon(reply: impl Fn(&Request) -> Option<Event> + Send + 'static) -> (PathBuf, Requests) {
        let seen = Requests::default();
        let record = Arc::clone(&seen);

        let socket = fake_daemon_replying(move |request| {
            record
                .lock()
                .expect("lock the requests")
                .push(request.clone());

            reply(&request)
        });

        (socket, seen)
    }

    fn count(seen: &Requests, matching: impl Fn(&Request) -> bool) -> usize {
        seen.lock()
            .expect("lock the requests")
            .iter()
            .filter(|request| matching(request))
            .count()
    }

    fn save_count(seen: &Requests) -> usize {
        count(seen, |request| {
            matches!(request, Request::SaveProjectConfig { .. })
        })
    }

    fn saves(seen: &Requests) -> Vec<ProjectConfigFields> {
        seen.lock()
            .expect("lock the requests")
            .iter()
            .filter_map(|request| match request {
                Request::SaveProjectConfig { fields, .. } => Some(fields.clone()),
                _ => None,
            })
            .collect()
    }

    fn bases(seen: &Requests) -> Vec<ProjectConfigFields> {
        seen.lock()
            .expect("lock the requests")
            .iter()
            .filter_map(|request| match request {
                Request::SaveProjectConfig { base, .. } => Some(base.clone()),
                _ => None,
            })
            .collect()
    }

    /// The project and the fields of each save.
    fn saved_to(seen: &Requests) -> Vec<(DaemonProjectId, ProjectConfigFields)> {
        seen.lock()
            .expect("lock the requests")
            .iter()
            .filter_map(|request| match request {
                Request::SaveProjectConfig {
                    project, fields, ..
                } => Some((*project, fields.clone())),
                _ => None,
            })
            .collect()
    }

    /// Where in the requests the first one that `matching` accepts stands.
    fn place(seen: &Requests, matching: impl Fn(&Request) -> bool) -> Option<usize> {
        seen.lock()
            .expect("lock the requests")
            .iter()
            .position(matching)
    }

    fn run_requests(seen: &Requests) -> Vec<DaemonTaskId> {
        seen.lock()
            .expect("lock the requests")
            .iter()
            .filter_map(|request| match request {
                Request::RunChecks { task, .. } => Some(*task),
                _ => None,
            })
            .collect()
    }

    fn fields() -> ProjectConfigFields {
        ProjectConfigFields {
            base_branch: Some("develop".to_owned()),
            worktree_folder: ".trees".to_owned(),
            port_range: "6000-6009".to_owned(),
            ports_per_worktree: 2,
            copy: vec![".env".to_owned(), "CLAUDE.local.md".to_owned()],
            setup: "make setup".to_owned(),
            teardown: "make clean".to_owned(),
            checks: vec!["make test".to_owned(), "make lint".to_owned()],
            checks_required: false,
        }
    }

    /// Answers the config and the branches of the project.
    fn project_reply(request: &Request) -> Option<Event> {
        config_reply(request, fields())
    }

    fn config_reply(request: &Request, config: ProjectConfigFields) -> Option<Event> {
        match request {
            Request::ProjectConfig { call, .. } => Some(Event::ProjectConfig {
                call: *call,
                fields: config,
                error: None,
            }),
            Request::ListBranches { call, .. } => Some(Event::Branches {
                call: *call,
                branches: vec!["main".to_owned(), "develop".to_owned()],
            }),
            _ => None,
        }
    }

    fn refusal_event(call: swarm_protocol::CallId, message: &str) -> Event {
        Event::Error {
            call: Some(call),
            id: None,
            message: message.to_owned(),
        }
    }

    struct Rig {
        workspace: Entity<Workspace>,
        window: WindowHandle<SettingsWindow>,
        state: Entity<SettingsState>,
        cx: VisualTestContext,
    }

    fn connected_workspace(socket: &Path, cx: &mut TestAppContext) -> Entity<Workspace> {
        cx.update(|cx| {
            swarm_ui::init(cx);
            crate::actions::bind_keys(cx);
            crate::settings::bind_keys(cx);
        });
        cx.executor().allow_parking();

        let client = Client::connect(socket).expect("connect to the fake daemon");
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    cx.new(|cx| Workspace::with_sample(window, cx))
                })
            })
            .expect("the workspace window opens");
        window
            .update(cx, |workspace, window, cx| {
                workspace.use_daemon(client, &[], window, cx);
            })
            .expect("the workspace window is open");

        window.root(cx).expect("the workspace")
    }

    /// A workspace on a fake daemon with the project `demo` holding `demo` tasks, and a
    /// second project, `other`, when it has tasks.
    fn rig(
        socket: &Path,
        demo: Vec<swarm_protocol::TaskInfo>,
        other: Vec<swarm_protocol::TaskInfo>,
        cx: &mut TestAppContext,
    ) -> Rig {
        let workspace = connected_workspace(socket, cx);

        let mut projects = vec![project_info(5, "demo", demo)];
        if !other.is_empty() {
            projects.push(project_info(6, "other", other));
        }
        workspace.update(cx, |workspace, cx| workspace.use_projects(&projects, cx));

        settings_window(workspace, cx)
    }

    fn settings_window(workspace: Entity<Workspace>, cx: &mut TestAppContext) -> Rig {
        let projects = workspace.read_with(cx, |workspace, _| workspace.data.projects.clone());
        let window = cx
            .update(|cx| {
                link_workspace(workspace.downgrade(), cx);

                open_window(None, Some(projects), cx)
            })
            .expect("the settings window opens");
        let state = window
            .read_with(cx, |settings, _| settings.state.clone())
            .expect("the settings window is open");

        let visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1280.), px(2400.)));

        Rig {
            workspace,
            window,
            state,
            cx: visual,
        }
    }

    /// One task in `demo`: the usual start.
    fn loaded(cx: &mut TestAppContext) -> (Rig, Requests) {
        let (socket, seen) = daemon(project_reply);
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);

        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        (rig, seen)
    }

    impl Rig {
        /// The page of the project at place `index` in the list.
        fn page_of(&self, index: usize) -> Page {
            let id = self
                .state
                .read_with(&self.cx, |state, _| state.projects[index].project.id);

            Page::Project(id)
        }

        /// Opens the page of project number `index`.
        fn show(&mut self, index: usize) {
            let page = self.page_of(index);

            self.window
                .update(&mut self.cx, |settings, _, cx| {
                    settings.select_page(page, cx);
                })
                .expect("the settings window is open");
            redraw(&mut self.cx);
        }

        /// Opens the page and draws it, before the daemon can answer.
        fn show_without_waiting(&mut self, index: usize) {
            let page = self.page_of(index);

            self.window
                .update(&mut self.cx, |settings, _, cx| {
                    settings.select_page(page, cx);
                })
                .expect("the settings window is open");
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn project(&self) -> ProjectSettings {
            self.state
                .read_with(&self.cx, |state, _| state.projects[0].clone())
        }

        fn wait_until_saved(&mut self) -> bool {
            let state = self.state.clone();

            wait_until(&mut self.cx, |cx| {
                redraw(cx);

                !state.read_with(cx, |state, _| state.saves[0].busy())
            })
        }

        fn wait_for_project(&mut self, done: impl Fn(&ProjectSettings) -> bool) -> bool {
            let state = self.state.clone();

            wait_until(&mut self.cx, |cx| {
                redraw(cx);

                state.read_with(cx, |state, _| done(&state.projects[0]))
            })
        }

        fn wait_for_requests(&mut self, seen: &Requests, done: impl Fn(&Requests) -> bool) -> bool {
            wait_until(&mut self.cx, |cx| {
                redraw(cx);

                done(seen)
            })
        }

        fn input(&self, pick: impl Fn(&ProjectInputs) -> Entity<TextInput>) -> Entity<TextInput> {
            self.window
                .read_with(&self.cx, |settings, _| pick(&settings.inputs.projects[0]))
                .expect("the settings window is open")
        }

        fn text_of(&self, input: &Entity<TextInput>) -> String {
            input.read_with(&self.cx, |input, _| input.text().to_string())
        }

        /// Types over the whole text of the input, as a user would.
        fn type_into(&mut self, input: &Entity<TextInput>, text: &str) {
            let focus = input.read_with(&self.cx, Focusable::focus_handle);

            self.cx.update(|window, cx| window.focus(&focus, cx));
            self.cx.simulate_keystrokes("cmd-a");
            self.cx.simulate_input(text);
        }

        fn edit_folder(&mut self, text: &str) {
            let folder = self.input(|inputs| inputs.worktree_folder.clone());

            self.type_into(&folder, text);
        }

        fn pause(&mut self, by: Duration) {
            self.cx.executor().advance_clock(by);
            self.cx.run_until_parked();
        }

        fn click(&mut self, selector: &'static str) {
            let bounds = self
                .cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not on screen"));

            self.cx.simulate_click(bounds.center(), Modifiers::none());
            redraw(&mut self.cx);
        }

        fn is_on_screen(&mut self, selector: &'static str) -> bool {
            self.cx.debug_bounds(selector).is_some()
        }

        fn app_task(&self, daemon: u64) -> crate::model::TaskId {
            self.workspace.read_with(&self.cx, |workspace, _| {
                workspace
                    .data
                    .tasks
                    .iter()
                    .find(|task| {
                        task.daemon.as_ref().map(|task| task.id) == Some(DaemonTaskId(daemon))
                    })
                    .map(|task| task.id)
                    .expect("the task is in the workspace")
            })
        }

        fn open_task(&mut self, daemon: u64) {
            let task = self.app_task(daemon);

            self.workspace.update(&mut self.cx, |workspace, cx| {
                workspace.select_task(task, cx);
            });
        }
    }

    #[gpui::test]
    fn the_page_shows_loading_and_then_the_values_of_the_daemon(cx: &mut TestAppContext) {
        let (socket, _seen) = daemon(project_reply);
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);

        // The daemon answers when the executor runs, so nothing has come yet.
        rig.show_without_waiting(0);
        let waiting = rig.project();
        let folder = rig.input(|inputs| inputs.worktree_folder.clone());
        let before = rig.text_of(&folder);

        assert!(rig.wait_for_project(|project| project.load == Load::Ready));
        let shown = rig.project();
        let texts = (
            rig.text_of(&folder),
            rig.text_of(&rig.input(|inputs| inputs.port_range.clone())),
            rig.text_of(&rig.input(|inputs| inputs.ports_per_worktree.clone())),
            rig.window
                .read_with(&rig.cx, |settings, cx| {
                    settings.inputs.projects[0]
                        .setup_script
                        .read(cx)
                        .text()
                        .to_string()
                })
                .expect("the settings window is open"),
        );
        let lists = rig
            .window
            .read_with(&rig.cx, |settings, cx| {
                let inputs = &settings.inputs.projects[0];
                let read = |list: &Vec<ListInput>| -> Vec<String> {
                    list.iter()
                        .map(|row| row.input.read(cx).text().to_string())
                        .collect()
                };

                (read(&inputs.copy), read(&inputs.checks))
            })
            .expect("the settings window is open");

        assert_eq!(waiting.load, Load::Loading);
        assert_eq!(before, "");
        assert!(waiting.copy_files.is_empty() && waiting.checks.is_empty());
        assert_eq!(shown.fields(), Ok(fields()));
        assert_eq!(shown.branches, ["main", "develop"]);
        assert_eq!(
            texts,
            (
                ".trees".to_owned(),
                "6000-6009".to_owned(),
                "2".to_owned(),
                "make setup".to_owned()
            )
        );
        assert_eq!(lists.0, [".env", "CLAUDE.local.md"]);
        assert_eq!(lists.1, ["make test", "make lint"]);
    }

    #[gpui::test]
    fn an_edit_sends_one_save_with_the_new_value_after_the_pause(cx: &mut TestAppContext) {
        let (mut rig, seen) = loaded(cx);

        rig.edit_folder(".new");
        rig.pause(Duration::from_millis(200));
        let early = save_count(&seen);
        rig.pause(Duration::from_millis(100));
        let sent = rig.wait_for_requests(&seen, |seen| save_count(seen) == 1);
        let idle = rig.wait_until_saved();
        rig.pause(Duration::from_secs(2));
        wait_a_little();
        redraw(&mut rig.cx);

        assert_eq!(early, 0);
        assert!(sent && idle);
        assert_eq!(
            saves(&seen),
            [ProjectConfigFields {
                worktree_folder: ".new".to_owned(),
                ..fields()
            }]
        );
    }

    #[gpui::test]
    fn quick_edits_send_only_the_newest_value(cx: &mut TestAppContext) {
        let (mut rig, seen) = loaded(cx);

        rig.edit_folder(".a");
        rig.pause(Duration::from_millis(200));
        rig.edit_folder(".b");
        rig.pause(Duration::from_millis(200));
        let early = save_count(&seen);
        rig.pause(Duration::from_millis(100));
        let sent = rig.wait_for_requests(&seen, |seen| save_count(seen) == 1);
        rig.pause(Duration::from_secs(2));
        wait_a_little();
        redraw(&mut rig.cx);

        assert_eq!(early, 0);
        assert!(sent);
        let folders: Vec<_> = saves(&seen)
            .into_iter()
            .map(|fields| fields.worktree_folder)
            .collect();
        assert_eq!(folders, [".b"]);
    }

    fn with_folder(folder: &str) -> ProjectConfigFields {
        ProjectConfigFields {
            worktree_folder: folder.to_owned(),
            ..fields()
        }
    }

    fn send(base: ProjectConfigFields, fields: ProjectConfigFields) -> Due {
        Due::Send { base, fields }
    }

    fn confirmed(fields: ProjectConfigFields) -> ProjectSave {
        ProjectSave {
            confirmed: Some(fields.clone()),
            queued: Some(fields),
            ..ProjectSave::default()
        }
    }

    #[test]
    fn a_newer_edit_waits_for_the_save_on_its_way_and_then_goes_out() {
        let mut save = confirmed(fields());

        assert!(save.edited(with_folder(".a")));
        let first = save.due();
        assert!(save.edited(with_folder(".b")));
        let held_back = save.due();
        let after = save.ended(&with_folder(".a"), true);

        assert_eq!(first, send(fields(), with_folder(".a")));
        assert_eq!(held_back, Due::Wait);
        assert_eq!(after, send(with_folder(".a"), with_folder(".b")));
        assert_eq!(save.ended(&with_folder(".b"), true), Due::Wait);
        assert_eq!(save.confirmed, Some(with_folder(".b")));
    }

    #[test]
    fn an_edit_during_the_pause_of_a_save_on_its_way_waits_for_its_own_pause() {
        let mut save = confirmed(fields());

        save.edited(with_folder(".a"));
        save.due();
        save.edited(with_folder(".b"));

        // The save of ".a" ends while the pause of ".b" still runs.
        assert_eq!(save.ended(&with_folder(".a"), true), Due::Wait);
        assert_eq!(save.due(), send(with_folder(".a"), with_folder(".b")));
    }

    #[test]
    fn a_refused_value_is_not_sent_again_and_the_next_edit_is() {
        let mut save = confirmed(fields());

        save.edited(with_folder("/bad"));
        let sent = save.due();
        let after_refusal = save.ended(&with_folder("/bad"), false);
        let same_again = save.edited(with_folder("/bad"));
        save.edited(with_folder(".fine"));

        assert_eq!(sent, send(fields(), with_folder("/bad")));
        assert_eq!(after_refusal, Due::Wait);
        assert!(!same_again);
        assert_eq!(save.due(), send(fields(), with_folder(".fine")));
        assert_eq!(save.confirmed, Some(fields()));
    }

    #[test]
    fn going_back_to_what_the_daemon_holds_sends_nothing() {
        let mut save = confirmed(fields());

        save.edited(with_folder(".a"));
        save.edited(fields());

        assert_eq!(save.due(), Due::Same);
    }

    #[test]
    fn going_back_while_a_save_is_on_its_way_sends_the_old_value_after_it() {
        let mut save = confirmed(fields());

        save.edited(with_folder(".a"));
        save.due();
        save.edited(fields());
        save.due();

        assert_eq!(
            save.ended(&with_folder(".a"), true),
            send(with_folder(".a"), fields())
        );
    }

    #[test]
    fn a_save_names_the_last_saved_fields_as_its_base_and_a_refusal_changes_nothing() {
        let mut save = confirmed(fields());

        save.edited(with_folder(".a"));
        let first = save.due();
        save.ended(&with_folder(".a"), true);
        save.edited(with_folder("/bad"));
        let refused = save.due();
        save.ended(&with_folder("/bad"), false);
        save.edited(with_folder(".b"));

        assert_eq!(first, send(fields(), with_folder(".a")));
        assert_eq!(refused, send(with_folder(".a"), with_folder("/bad")));
        assert_eq!(save.due(), send(with_folder(".a"), with_folder(".b")));
    }

    #[test]
    fn nothing_is_sent_before_the_page_was_read() {
        let mut save = ProjectSave::default();

        save.edited(with_folder(".a"));

        assert_eq!(save.due(), Due::Wait);
    }

    #[test]
    fn a_read_that_began_before_an_edit_is_out_of_date_even_after_the_edit_was_saved() {
        let mut save = confirmed(fields());

        save.begin_read();
        save.edited(with_folder(".a"));
        save.due();
        save.ended(&with_folder(".a"), true);

        assert!(!save.busy());
        assert!(!save.end_read());
    }

    #[test]
    fn a_read_with_no_edit_meanwhile_is_current() {
        let mut save = confirmed(fields());

        save.begin_read();

        assert!(save.end_read());
        assert!(save.can_read());
    }

    #[test]
    fn a_read_waits_for_a_pause_a_save_and_another_read() {
        let mut save = confirmed(fields());

        save.edited(with_folder(".a"));
        let during_pause = save.can_read();
        save.due();
        let during_save = save.can_read();
        save.ended(&with_folder(".a"), true);
        let after = save.can_read();
        save.begin_read();

        assert!(!during_pause && !during_save);
        assert!(after);
        assert!(!save.can_read());
    }

    #[test]
    fn fields_that_cannot_be_saved_count_as_an_edit_for_a_read() {
        let mut save = confirmed(fields());

        save.begin_read();
        save.invalid();

        assert!(!save.end_read());
    }

    #[gpui::test]
    fn a_save_sends_the_loaded_fields_as_its_base_and_the_next_one_the_saved_fields(
        cx: &mut TestAppContext,
    ) {
        let (mut rig, seen) = loaded(cx);

        rig.edit_folder(".a");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));
        assert!(rig.wait_until_saved());

        rig.edit_folder(".b");
        rig.pause(Duration::from_millis(300));
        let second = rig.wait_for_requests(&seen, |seen| save_count(seen) == 2);

        assert!(second);
        assert_eq!(bases(&seen), [fields(), with_folder(".a")]);
        assert_eq!(saves(&seen), [with_folder(".a"), with_folder(".b")]);
    }

    #[gpui::test]
    fn a_base_stays_the_loaded_fields_after_a_refusal(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(|request| match request {
            Request::SaveProjectConfig { call, fields, .. } if fields.worktree_folder == "/bad" => {
                Some(refusal_event(*call, "the worktree folder must be inside"))
            }
            other => project_reply(other),
        });
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.edit_folder("/bad");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_project(|project| project.refusal.is_some()));
        rig.edit_folder(".fine");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 2));

        assert_eq!(bases(&seen), [fields(), fields()]);
    }

    /// A daemon whose config the test can change, as a hand edit would.
    fn file_on_disk() -> (PathBuf, Requests, Arc<Mutex<ProjectConfigFields>>) {
        let on_disk = Arc::new(Mutex::new(fields()));
        let answer = Arc::clone(&on_disk);
        let (socket, seen) = daemon(move |request| {
            let config = answer.lock().expect("lock the file").clone();

            config_reply(request, config)
        });

        (socket, seen, on_disk)
    }

    fn config_requests(seen: &Requests) -> usize {
        count(seen, |request| {
            matches!(request, Request::ProjectConfig { .. })
        })
    }

    impl Rig {
        /// The window loses focus and gets it back, as when the user edits the file in an
        /// editor.
        fn come_back(&mut self) {
            self.cx.deactivate_window();
            self.cx.update(|window, _| window.activate_window());
            self.cx.run_until_parked();
        }

        fn list_labels(&self, list: List) -> Vec<String> {
            self.window
                .read_with(&self.cx, |settings, cx| {
                    settings.inputs.projects[0]
                        .list(list)
                        .iter()
                        .map(|row| row.input.read(cx).label().to_string())
                        .collect()
                })
                .expect("the settings window is open")
        }

        fn subscription_count(&self) -> usize {
            self.window
                .read_with(&self.cx, |settings, _| settings.subscriptions.len())
                .expect("the settings window is open")
        }
    }

    #[gpui::test]
    fn a_hand_edit_shows_when_the_window_comes_back(cx: &mut TestAppContext) {
        let (socket, seen, on_disk) = file_on_disk();
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        on_disk.lock().expect("lock the file").worktree_folder = ".by-hand".to_owned();
        rig.come_back();
        let shown = rig.wait_for_project(|project| project.worktree_folder == ".by-hand");
        let folder = rig.input(|inputs| inputs.worktree_folder.clone());

        assert!(shown);
        assert_eq!(rig.text_of(&folder), ".by-hand");
        assert_eq!(config_requests(&seen), 2);
    }

    #[gpui::test]
    fn the_page_is_not_read_again_while_an_edit_waits_or_is_on_its_way(cx: &mut TestAppContext) {
        let (socket, seen, _on_disk) = file_on_disk();
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.edit_folder(".typed");
        rig.come_back();
        let waiting = config_requests(&seen);
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));
        assert!(rig.wait_until_saved());

        assert_eq!(waiting, 1);
        assert_eq!(rig.project().worktree_folder, ".typed");
        assert_eq!(config_requests(&seen), 1);
    }

    #[gpui::test]
    fn a_hand_edit_to_one_field_is_not_sent_back_by_a_save_of_another(cx: &mut TestAppContext) {
        let (socket, seen, on_disk) = file_on_disk();
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        on_disk.lock().expect("lock the file").port_range = "7000-7009".to_owned();
        rig.come_back();
        assert!(rig.wait_for_project(|project| project.port_range == "7000-7009"));
        rig.edit_folder(".new");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));

        let base = bases(&seen).remove(0);
        let sent = saves(&seen).remove(0);
        assert_eq!(base.port_range, "7000-7009");
        assert_eq!(sent.port_range, "7000-7009");
        assert_eq!(sent.worktree_folder, ".new");
    }

    #[gpui::test]
    fn a_slow_reload_does_not_undo_an_edit_that_was_saved_meanwhile(cx: &mut TestAppContext) {
        let (mut rig, seen) = loaded(cx);
        let started = rig
            .state
            .update(&mut rig.cx, |state, cx| state.begin_load(0, cx));

        rig.edit_folder(".saved-meanwhile");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));
        assert!(rig.wait_until_saved());
        let stale = Loaded {
            config: Ok((fields(), None)),
            branches: None,
            prompts: None,
        };
        let applied = rig
            .state
            .update(&mut rig.cx, |state, cx| state.finish_load(0, stale, cx));
        redraw(&mut rig.cx);

        assert!(started.is_some());
        assert!(!applied);
        assert_eq!(rig.project().worktree_folder, ".saved-meanwhile");
        let folder = rig.input(|inputs| inputs.worktree_folder.clone());
        assert_eq!(rig.text_of(&folder), ".saved-meanwhile");
    }

    #[gpui::test]
    fn a_blank_row_the_user_added_survives_a_reload(cx: &mut TestAppContext) {
        let (socket, seen, _on_disk) = file_on_disk();
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.click("add-check");
        rig.come_back();
        assert!(rig.wait_for_requests(&seen, |seen| config_requests(seen) == 2));
        rig.pause(Duration::from_millis(300));
        redraw(&mut rig.cx);

        assert_eq!(rig.project().checks, ["make test", "make lint", ""]);
        assert_eq!(rig.list_labels(List::Checks).len(), 3);
        assert_eq!(save_count(&seen), 0);
    }

    #[gpui::test]
    fn rows_are_named_by_their_place_in_the_list(cx: &mut TestAppContext) {
        let (mut rig, _seen) = loaded(cx);
        let start = (rig.list_labels(List::Copy), rig.list_labels(List::Checks));

        rig.click("add-check");
        let added = rig.list_labels(List::Checks);
        rig.click("check-remove-0");
        let removed = rig.list_labels(List::Checks);

        assert_eq!(start.0, ["File to copy 1", "File to copy 2"]);
        assert_eq!(start.1, ["Check 1", "Check 2"]);
        assert_eq!(added, ["Check 1", "Check 2", "Check 3"]);
        assert_eq!(removed, ["Check 1", "Check 2"]);
        assert_eq!(rig.project().checks, ["make lint", ""]);
    }

    #[gpui::test]
    fn rebuilding_the_rows_of_a_list_does_not_pile_up_subscriptions(cx: &mut TestAppContext) {
        let (socket, seen, on_disk) = file_on_disk();
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));
        let before = rig.subscription_count();

        for round in 1..=3 {
            on_disk.lock().expect("lock the file").checks = vec![format!("check {round}")];
            rig.come_back();
            let wanted = round + 1;
            assert!(rig.wait_for_requests(&seen, |seen| config_requests(seen) == wanted));
            assert!(rig.wait_for_project(|project| project.checks == [format!("check {round}")]));
        }

        assert_eq!(rig.subscription_count(), before);
        assert_eq!(rig.list_labels(List::Checks), ["Check 1"]);
    }

    #[gpui::test]
    fn typing_in_a_rebuilt_row_edits_that_row_once(cx: &mut TestAppContext) {
        let (socket, seen, on_disk) = file_on_disk();
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));
        on_disk.lock().expect("lock the file").checks = vec!["one".to_owned(), "two".to_owned()];
        rig.come_back();
        assert!(rig.wait_for_project(|project| project.checks == ["one", "two"]));

        let second = rig.input(|inputs| inputs.checks[1].input.clone());
        rig.type_into(&second, "three");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));

        assert_eq!(rig.project().checks, ["one", "three"]);
        assert_eq!(saves(&seen)[0].checks, ["one", "three"]);
    }

    #[gpui::test]
    fn run_checks_now_waits_for_an_edit_that_is_not_saved_yet(cx: &mut TestAppContext) {
        let (mut rig, seen) = loaded(cx);

        rig.edit_folder(".before-the-run");
        let during_pause = rig
            .state
            .read_with(&rig.cx, |state, cx| state.can_run_checks(0, cx));
        rig.click("run-checks");
        let early = run_requests(&seen).len();
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));
        assert!(rig.wait_until_saved());
        redraw(&mut rig.cx);
        let after = rig
            .state
            .read_with(&rig.cx, |state, cx| state.can_run_checks(0, cx));

        rig.click("run-checks");
        assert!(rig.wait_for_requests(&seen, |seen| !run_requests(seen).is_empty()));
        let saved_at = place(&seen, |request| {
            matches!(request, Request::SaveProjectConfig { .. })
        });
        let ran_at = place(&seen, |request| {
            matches!(request, Request::RunChecks { .. })
        });

        assert!(!during_pause);
        assert_eq!(early, 0);
        assert!(after);
        assert!(saved_at < ran_at, "{saved_at:?} {ran_at:?}");
    }

    #[gpui::test]
    fn a_refusal_shows_its_message_and_keeps_the_value_until_a_save_works(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(|request| match request {
            Request::SaveProjectConfig { call, fields, .. } if fields.worktree_folder == "/bad" => {
                Some(refusal_event(
                    *call,
                    "the worktree folder must be inside the repository",
                ))
            }
            other => project_reply(other),
        });
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.edit_folder("/bad");
        rig.pause(Duration::from_millis(300));
        let refused = rig.wait_for_project(|project| project.refusal.is_some());
        let message = rig.project().refusal;
        let shown = rig.is_on_screen("project-refusal");
        let folder = rig.input(|inputs| inputs.worktree_folder.clone());
        let kept = (rig.text_of(&folder), rig.project().worktree_folder);

        rig.edit_folder(".fine");
        rig.pause(Duration::from_millis(300));
        let cleared = rig.wait_for_project(|project| project.refusal.is_none());
        redraw(&mut rig.cx);
        let gone = !rig.is_on_screen("project-refusal");

        assert!(refused);
        assert_eq!(
            message.as_deref(),
            Some("the worktree folder must be inside the repository")
        );
        assert!(shown);
        assert_eq!(kept, ("/bad".to_owned(), "/bad".to_owned()));
        assert!(cleared && gone);
        assert_eq!(save_count(&seen), 2);
    }

    #[gpui::test]
    fn a_parse_error_makes_the_fields_read_only_and_reload_asks_again(cx: &mut TestAppContext) {
        let asked = Arc::new(AtomicUsize::new(0));
        let (socket, seen) = daemon({
            let asked = Arc::clone(&asked);

            move |request| match request {
                Request::ProjectConfig { call, .. } => {
                    let broken = asked.fetch_add(1, Ordering::SeqCst) == 0;
                    let (config, error) = if broken {
                        (
                            ProjectConfigFields::default(),
                            Some("TOML parse error at line 3, column 1".to_owned()),
                        )
                    } else {
                        (fields(), None)
                    };

                    Some(Event::ProjectConfig {
                        call: *call,
                        fields: config,
                        error,
                    })
                }
                other => project_reply(other),
            }
        });
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);

        rig.show(0);
        assert!(rig.wait_for_project(|project| matches!(project.load, Load::Unreadable(_))));
        let error = rig.project().load;
        let banner = (
            rig.is_on_screen("config-error"),
            rig.is_on_screen("reload-config"),
        );

        rig.edit_folder("typed");
        rig.pause(Duration::from_secs(1));
        wait_a_little();
        redraw(&mut rig.cx);
        let folder = rig.project().worktree_folder;

        rig.click("reload-config");
        let ready = rig.wait_for_project(|project| project.load == Load::Ready);
        redraw(&mut rig.cx);
        let after = (
            rig.project().worktree_folder,
            rig.is_on_screen("config-error"),
        );

        assert_eq!(
            error,
            Load::Unreadable("TOML parse error at line 3, column 1".into())
        );
        assert_eq!(banner, (true, true));
        assert_eq!(folder, "");
        assert_eq!(save_count(&seen), 0);
        assert!(ready);
        assert_eq!(after, (".trees".to_owned(), false));
        assert_eq!(
            count(&seen, |request| matches!(
                request,
                Request::ProjectConfig { .. }
            )),
            2
        );
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    fn the_base_branch_dropdown_lists_the_branches_and_the_default(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(|request| {
            config_reply(
                request,
                ProjectConfigFields {
                    base_branch: None,
                    ..fields()
                },
            )
        });
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));
        let project = rig.project();
        let listed = count(
            &seen,
            |request| matches!(request, Request::ListBranches { project, .. } if *project == DaemonProjectId(5)),
        );

        rig.click("base-branch");
        rig.cx.simulate_keystrokes("down down enter");
        rig.pause(Duration::from_millis(300));
        assert!(rig.wait_for_requests(&seen, |seen| save_count(seen) == 1));
        assert!(rig.wait_until_saved());

        rig.click("base-branch");
        rig.cx.simulate_keystrokes("up up enter");
        rig.pause(Duration::from_millis(300));
        let second = rig.wait_for_requests(&seen, |seen| save_count(seen) == 2);

        assert_eq!(
            project.base_branch_options(),
            ["Default branch", "main", "develop"]
        );
        assert_eq!(project.base_branch_index(), 0);
        assert_eq!(listed, 1);
        assert!(second);
        let chosen: Vec<_> = saves(&seen)
            .into_iter()
            .map(|fields| fields.base_branch)
            .collect();
        assert_eq!(chosen, [Some("develop".to_owned()), None]);
    }

    fn passed_run(task: u64) -> ChecksRun {
        let check = |command: &str, state| LocalCheck {
            command: command.to_owned(),
            state,
            terminal: None,
            exit: (state == CheckState::Passed).then_some(ExitStatus::Code(0)),
            duration_ms: Some(1_000),
        };

        ChecksRun {
            task: DaemonTaskId(task),
            commit: "abc".to_owned(),
            clean: true,
            required: true,
            checks: vec![
                check("make test", CheckState::Passed),
                check("make lint", CheckState::Passed),
            ],
            pre_push_hook: None,
        }
    }

    fn send_run(rig: &mut Rig, run: ChecksRun) {
        rig.workspace.update(&mut rig.cx, |workspace, cx| {
            workspace.pr_event(PrEvent::Checks(Box::new(run)), cx);
        });
        redraw(&mut rig.cx);
    }

    fn tasks_of(rig: &Rig) -> (bool, bool) {
        rig.state.read_with(&rig.cx, |state, cx| {
            (state.can_run_checks(0, cx), state.can_run_checks(1, cx))
        })
    }

    #[gpui::test]
    fn run_checks_now_runs_the_task_open_in_the_workspace_once(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(project_reply);
        let mut rig = rig(
            &socket,
            vec![task_info(7, 5, "login"), task_info(8, 5, "logout")],
            vec![task_info(9, 6, "billing")],
            cx,
        );
        rig.open_task(8);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        // Two clicks before the daemon can answer start one run.
        rig.state.update(&mut rig.cx, |state, cx| {
            state.run_checks(0, cx);
            state.run_checks(0, cx);
        });
        let running = rig.project().checking.pill();
        redraw(&mut rig.cx);
        let asked = rig.wait_for_requests(&seen, |seen| !run_requests(seen).is_empty());
        let shown = rig.is_on_screen("run-checks-result");
        let started = rig.project().checking.pill();
        send_run(&mut rig, passed_run(8));
        let done = rig.project().checking.pill();

        assert!(asked && shown);
        assert_eq!(run_requests(&seen), [DaemonTaskId(8)]);
        assert_eq!(running, Some((AgentStatus::Working, "Running".into())));
        assert_eq!(started, Some((AgentStatus::Working, "Running".into())));
        assert_eq!(done, Some((AgentStatus::Done, "2 of 2 passed".into())));
    }

    #[gpui::test]
    fn the_run_checks_button_starts_a_run(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(project_reply);
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.click("run-checks");
        let asked = rig.wait_for_requests(&seen, |seen| !run_requests(seen).is_empty());
        wait_a_little();

        assert!(asked);
        assert_eq!(run_requests(&seen), [DaemonTaskId(7)]);
    }

    #[gpui::test]
    fn run_checks_now_falls_back_to_the_first_open_task_of_the_project(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(project_reply);
        let mut rig = rig(
            &socket,
            vec![task_info(7, 5, "login"), task_info(8, 5, "logout")],
            vec![task_info(9, 6, "billing")],
            cx,
        );
        rig.open_task(9);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.click("run-checks");
        let first = rig.wait_for_requests(&seen, |seen| run_requests(seen).len() == 1);
        rig.show(1);
        assert!(rig.wait_for_requests(&seen, |seen| {
            count(seen, |request| matches!(request, Request::ProjectConfig { project, .. } if *project == DaemonProjectId(6))) == 1
        }));
        rig.click("run-checks");
        let second = rig.wait_for_requests(&seen, |seen| run_requests(seen).len() == 2);

        assert!(first && second);
        assert_eq!(run_requests(&seen), [DaemonTaskId(7), DaemonTaskId(9)]);
    }

    #[gpui::test]
    fn run_checks_now_is_disabled_without_an_open_task(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(project_reply);
        let mut rig = rig(&socket, Vec::new(), vec![task_info(9, 6, "billing")], cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));
        let (without, with) = tasks_of(&rig);

        rig.click("run-checks");
        rig.pause(Duration::from_secs(1));
        wait_a_little();
        redraw(&mut rig.cx);

        assert!(!without);
        assert!(with);
        assert_eq!(run_requests(&seen), []);
        assert_eq!(rig.project().checking, ChecksStatus::Idle);
    }

    #[gpui::test]
    fn run_checks_now_shows_the_refusal_of_the_daemon(cx: &mut TestAppContext) {
        let (socket, _seen) = daemon(|request| match request {
            Request::RunChecks { call, .. } => {
                Some(refusal_event(*call, "no checks are configured"))
            }
            other => project_reply(other),
        });
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));

        rig.click("run-checks");
        let refused =
            rig.wait_for_project(|project| matches!(project.checking, ChecksStatus::Refused(_)));

        assert!(refused);
        assert_eq!(
            rig.project().checking.pill(),
            Some((AgentStatus::Error, "no checks are configured".into()))
        );
    }

    #[test]
    fn a_run_adds_up_to_a_short_result() {
        let run = |states: &[CheckState]| ChecksRun {
            checks: states
                .iter()
                .map(|state| LocalCheck {
                    command: "make".to_owned(),
                    state: *state,
                    terminal: None,
                    exit: None,
                    duration_ms: None,
                })
                .collect(),
            ..passed_run(1)
        };

        assert_eq!(
            run_summary(&run(&[CheckState::Passed, CheckState::Running])),
            (AgentStatus::Working, "Running".to_owned())
        );
        assert_eq!(
            run_summary(&run(&[
                CheckState::Passed,
                CheckState::Failed,
                CheckState::Waiting
            ])),
            (AgentStatus::Error, "1 of 3 failed".to_owned())
        );
        assert_eq!(
            run_summary(&run(&[CheckState::Passed, CheckState::Stopped])),
            (AgentStatus::Blocked, "Stopped".to_owned())
        );
        assert_eq!(
            run_summary(&run(&[])),
            (AgentStatus::Idle, "No checks".to_owned())
        );
        assert_eq!(
            run_summary(&run(&[CheckState::Passed])),
            (AgentStatus::Done, "1 of 1 passed".to_owned())
        );
    }

    fn prompt(name: &str, layer: PromptLayer, project: Option<&str>) -> PromptInfo {
        PromptInfo {
            name: name.to_owned(),
            layer,
            user_path: PathBuf::from(format!("/home/sam/.config/swarm/prompts/{name}.md")),
            project_path: project.map(PathBuf::from),
        }
    }

    #[gpui::test]
    fn the_prompts_section_lists_the_project_overrides_and_open_opens_the_file(
        cx: &mut TestAppContext,
    ) {
        let (socket, _seen) = daemon(|request| match request {
            Request::Prompts { call, project } => {
                assert_eq!(*project, Some(DaemonProjectId(5)));

                Some(Event::Prompts {
                    call: *call,
                    prompts: vec![
                        prompt(
                            "orchestrator",
                            PromptLayer::Project,
                            Some("/tmp/demo/.swarm/prompts/orchestrator.md"),
                        ),
                        prompt("worker", PromptLayer::BuiltIn, None),
                        prompt(
                            "reviewer",
                            PromptLayer::Project,
                            Some("/tmp/demo/.swarm/prompts/reviewer.md"),
                        ),
                    ],
                })
            }
            other => project_reply(other),
        });
        let mut rig = rig(&socket, vec![task_info(7, 5, "login")], Vec::new(), cx);
        let opened = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
        rig.cx.update(|_, cx| {
            let opened = Rc::clone(&opened);

            use_opener(
                Rc::new(move |path, _| opened.borrow_mut().push(path.to_path_buf())),
                cx,
            );
        });

        rig.show(0);
        assert!(rig.wait_for_project(|project| project.load == Load::Ready));
        redraw(&mut rig.cx);
        let names: Vec<_> = rig
            .project()
            .prompts
            .into_iter()
            .map(|prompt| prompt.name)
            .collect();
        let rows = (
            rig.is_on_screen("project-prompts"),
            rig.is_on_screen("open-prompt-0"),
            rig.is_on_screen("open-prompt-1"),
            rig.is_on_screen("open-prompt-2"),
        );

        rig.click("open-prompt-1");

        assert_eq!(names, ["orchestrator", "reviewer"]);
        assert_eq!(rows, (true, true, true, false));
        assert_eq!(
            *opened.borrow(),
            [PathBuf::from("/tmp/demo/.swarm/prompts/reviewer.md")]
        );
    }

    #[gpui::test]
    fn the_prompts_section_is_hidden_without_project_overrides(cx: &mut TestAppContext) {
        let (mut rig, _seen) = loaded(cx);
        redraw(&mut rig.cx);

        assert!(rig.project().prompts.is_empty());
        assert!(!rig.is_on_screen("project-prompts"));
    }

    #[gpui::test]
    fn sample_projects_keep_their_values_and_save_nothing(cx: &mut TestAppContext) {
        let (socket, seen) = daemon(project_reply);
        let workspace = connected_workspace(&socket, cx);
        let mut rig = settings_window(workspace, cx);

        rig.show(0);
        rig.edit_folder(".edited");
        rig.pause(Duration::from_secs(1));
        wait_a_little();
        redraw(&mut rig.cx);

        let project = rig.project();
        assert_eq!(project.load, Load::Sample);
        assert_eq!(project.worktree_folder, ".edited");
        assert_eq!(project.port_range, "5170-5199");
        let asked = count(&seen, |request| {
            matches!(
                request,
                Request::ProjectConfig { .. }
                    | Request::SaveProjectConfig { .. }
                    | Request::ListBranches { .. }
                    | Request::Prompts { .. }
            )
        });
        assert_eq!(asked, 0);
    }

    #[gpui::test]
    fn added_and_removed_rows_are_saved_and_blank_rows_are_not(cx: &mut TestAppContext) {
        let (mut rig, seen) = loaded(cx);

        rig.click("add-check");
        rig.pause(Duration::from_secs(1));
        wait_a_little();
        redraw(&mut rig.cx);
        let blank = save_count(&seen);
        rig.cx.simulate_input("make fmt");
        rig.pause(Duration::from_millis(300));
        let added = rig.wait_for_requests(&seen, |seen| save_count(seen) == 1);
        assert!(rig.wait_until_saved());

        rig.click("copy-remove-0");
        rig.pause(Duration::from_millis(300));
        let removed = rig.wait_for_requests(&seen, |seen| save_count(seen) == 2);

        assert_eq!(blank, 0);
        assert!(added && removed);
        let sent = saves(&seen);
        assert_eq!(sent[0].checks, ["make test", "make lint", "make fmt"]);
        assert_eq!(sent[1].copy, ["CLAUDE.local.md"]);
        assert_eq!(sent[1].teardown, "make clean");
    }

    fn two_projects(cx: &mut TestAppContext) -> (Rig, Requests) {
        let (socket, seen) = daemon(project_reply);
        let rig = rig(&socket, Vec::new(), vec![task_info(8, 6, "signup")], cx);

        (rig, seen)
    }

    /// Opens the dialog from the button of the page and waits for its check.
    fn open_remove_dialog(rig: &mut Rig) {
        rig.click("remove-project");
        let window = rig.window;

        let ready = wait_until(&mut rig.cx, |cx| {
            redraw(cx);

            window
                .read_with(cx, |settings, cx| {
                    settings
                        .remove_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.view.read(cx).confirmed().is_some())
                })
                .unwrap_or(false)
        });
        assert!(ready, "the check never came");
    }

    #[gpui::test]
    fn the_remove_button_at_the_bottom_of_the_page_opens_the_dialog(cx: &mut TestAppContext) {
        let (mut rig, _) = two_projects(cx);
        rig.show(0);
        assert!(!rig.is_on_screen("remove-project-confirm"));

        rig.click("remove-project");

        assert!(rig.is_on_screen("remove-project-confirm"));
        assert!(rig.is_on_screen("remove-project-cancel"));
        let asked = rig
            .window
            .read_with(&rig.cx, |settings, cx| {
                settings
                    .remove_dialog
                    .as_ref()
                    .map(|dialog| dialog.view.read(cx).project)
            })
            .expect("the settings window is open");
        assert_eq!(asked, Some(ProjectId(0)));
    }

    #[gpui::test]
    fn the_dialog_of_a_project_with_tasks_lists_them_and_keeps_remove(cx: &mut TestAppContext) {
        let (socket, _seen) = daemon(|request| match request {
            Request::CheckRemoveProject { call, .. } => Some(Event::RemoveCheck {
                call: *call,
                check: swarm_protocol::RemoveCheck {
                    tasks: vec![swarm_protocol::TaskCloseCheck {
                        task: DaemonTaskId(8),
                        name: "signup".to_owned(),
                        check: swarm_protocol::CloseCheck::default(),
                    }],
                },
            }),
            other => project_reply(other),
        });
        let mut rig = rig(&socket, Vec::new(), vec![task_info(8, 6, "signup")], cx);
        rig.show(1);

        open_remove_dialog(&mut rig);

        let lines = rig
            .window
            .read_with(&rig.cx, |settings, cx| {
                settings
                    .remove_dialog
                    .as_ref()
                    .map(|dialog| dialog.view.read(cx).lines())
            })
            .expect("the settings window is open")
            .unwrap_or_default();
        let texts: Vec<_> = lines.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["Closes 1 task: signup"]);
        assert!(rig.is_on_screen("remove-project-confirm"));
    }

    #[gpui::test]
    fn removing_the_project_of_the_page_switches_to_another_page(cx: &mut TestAppContext) {
        let (mut rig, seen) = two_projects(cx);
        rig.show(0);
        open_remove_dialog(&mut rig);

        rig.click("remove-project-confirm");
        let gone = wait_until(&mut rig.cx, |cx| {
            redraw(cx);
            cx.debug_bounds("remove-project-confirm").is_none()
        });

        assert!(gone, "the dialog stayed open");
        assert_eq!(
            count(&seen, |request| matches!(
                request,
                Request::RemoveProject { .. }
            )),
            1
        );
        let (page, inputs, nav, names) = rig
            .window
            .read_with(&rig.cx, |settings, cx| {
                let names: Vec<String> = settings
                    .state
                    .read(cx)
                    .projects
                    .iter()
                    .map(|entry| entry.project.name.to_string())
                    .collect();

                (
                    settings.page,
                    settings.inputs.projects.len(),
                    settings.nav_focus.len(),
                    names,
                )
            })
            .expect("the settings window is open");
        assert_eq!(page, Page::Agents);
        assert_eq!(inputs, 1);
        assert_eq!(nav, Page::SECTIONS.len() + 1);
        assert_eq!(names, ["other"]);
    }

    #[gpui::test]
    fn cancel_gives_focus_back_to_the_remove_project_button(cx: &mut TestAppContext) {
        let (mut rig, _) = two_projects(cx);
        rig.show(0);
        open_remove_dialog(&mut rig);

        rig.click("remove-project-cancel");

        let focus = rig
            .window
            .read_with(&rig.cx, |settings, _| settings.remove_focus.clone())
            .expect("the settings window is open");
        let on_button = rig.cx.update(|window, _| focus.is_focused(window));
        assert!(!rig.is_on_screen("remove-project-confirm"));
        assert!(on_button, "focus did not return to Remove project");
    }

    #[gpui::test]
    fn removing_a_project_above_the_page_keeps_the_page_on_its_project(cx: &mut TestAppContext) {
        let (mut rig, seen) = two_projects(cx);
        rig.show(1);
        let state = rig.state.clone();
        let loaded = wait_until(&mut rig.cx, |cx| {
            redraw(cx);

            state.read_with(cx, |state, _| state.projects[1].load == Load::Ready)
        });
        assert!(loaded);

        rig.workspace.update(&mut rig.cx, |workspace, cx| {
            workspace.project_removed(ProjectId(0), cx);
        });
        redraw(&mut rig.cx);
        rig.edit_folder(".moved");
        rig.pause(Duration::from_millis(300));
        let saved = rig.wait_for_requests(&seen, |seen| save_count(seen) == 1);

        let (page, shown) = rig
            .window
            .read_with(&rig.cx, |settings, cx| {
                let shown = match settings.page {
                    Page::Project(id) => settings
                        .state
                        .read(cx)
                        .projects
                        .iter()
                        .find(|entry| entry.project.id == id)
                        .map(|entry| entry.project.name.to_string()),
                    _ => None,
                };

                (settings.page, shown)
            })
            .expect("the settings window is open");
        assert_eq!(page, Page::Project(ProjectId(1)));
        assert_eq!(shown.as_deref(), Some("other"));
        assert!(saved, "the edit was not saved");
        let sent: Vec<_> = saved_to(&seen)
            .into_iter()
            .map(|(project, fields)| (project, fields.worktree_folder))
            .collect();
        assert_eq!(sent, [(DaemonProjectId(6), ".moved".to_owned())]);
    }

    /// Three projects with the page of the second one loaded, and the first one removed.
    fn after_the_first_of_three_went(cx: &mut TestAppContext) -> (Rig, Requests) {
        let (socket, seen) = daemon(project_reply);
        let workspace = connected_workspace(&socket, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.use_projects(
                &[
                    project_info(5, "first", Vec::new()),
                    project_info(6, "second", Vec::new()),
                    project_info(7, "third", Vec::new()),
                ],
                cx,
            );
        });
        let mut rig = settings_window(workspace, cx);
        rig.show(1);
        let state = rig.state.clone();
        let loaded = wait_until(&mut rig.cx, |cx| {
            redraw(cx);

            state.read_with(cx, |state, _| state.projects[1].load == Load::Ready)
        });
        assert!(loaded);

        rig.workspace.update(&mut rig.cx, |workspace, cx| {
            workspace.project_removed(ProjectId(0), cx);
        });
        redraw(&mut rig.cx);

        (rig, seen)
    }

    #[gpui::test]
    fn an_edit_after_a_removal_goes_to_its_own_project_and_not_the_next_one(
        cx: &mut TestAppContext,
    ) {
        let (mut rig, seen) = after_the_first_of_three_went(cx);

        rig.edit_folder(".second");
        rig.pause(Duration::from_millis(300));
        let saved = rig.wait_for_requests(&seen, |seen| save_count(seen) == 1);

        let folders = rig.state.read_with(&rig.cx, |state, _| {
            state
                .projects
                .iter()
                .map(|entry| entry.worktree_folder.clone())
                .collect::<Vec<_>>()
        });
        let sent: Vec<_> = saved_to(&seen)
            .into_iter()
            .map(|(project, fields)| (project, fields.worktree_folder))
            .collect();
        assert!(saved, "the edit was not saved");
        assert_eq!(sent, [(DaemonProjectId(6), ".second".to_owned())]);
        assert_eq!(
            folders,
            [".second", ""],
            "the edit reached the third project"
        );
    }

    #[gpui::test]
    fn a_check_typed_after_a_removal_goes_to_its_own_project_and_not_the_next_one(
        cx: &mut TestAppContext,
    ) {
        let (mut rig, seen) = after_the_first_of_three_went(cx);
        let check = rig.input(|inputs| inputs.checks[0].input.clone());

        rig.type_into(&check, "make fmt");
        rig.pause(Duration::from_millis(300));
        let saved = rig.wait_for_requests(&seen, |seen| save_count(seen) == 1);

        let checks = rig.state.read_with(&rig.cx, |state, _| {
            state
                .projects
                .iter()
                .map(|entry| entry.checks.clone())
                .collect::<Vec<_>>()
        });
        let sent: Vec<_> = saved_to(&seen)
            .into_iter()
            .map(|(project, fields)| (project, fields.checks))
            .collect();
        assert!(saved, "the edit was not saved");
        assert_eq!(
            sent,
            [(
                DaemonProjectId(6),
                vec!["make fmt".to_owned(), "make lint".to_owned()]
            )]
        );
        assert!(checks[1].is_empty(), "the edit reached the third project");
    }
}
