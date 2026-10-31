//! Pull requests: the workspace side. Drafts, local checks, creating, status, merging and
//! the GitHub setup, through the daemon.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, SharedString, Subscription,
    Window, div,
};
use swarm_protocol::{
    CheckState, ChecksRun, Client, ClientError, GithubCheckState, GithubStatus, MergeMethod,
    PrDraft, PrEvent, PrLink, PrPanel, PrState, PullRequest, TaskId as DaemonTaskId,
    TerminalId as DaemonTerminalId,
};
use swarm_term::ExitStatus;
use swarm_ui::prelude::*;

use crate::model::{
    CheckConfig, ProjectId, TaskId, TerminalId, sample_check_config, sample_checks_run,
    sample_draft, sample_pull_request,
};
use crate::palette::Overlay;
use crate::review_comments::refusal;
use crate::review_model::count_comments;
use crate::workspace::{DataSource, RightPanel, View, Workspace};

/// How often the status is read while a GitHub check queues or runs.
pub(crate) const STATUS_FAST: Duration = Duration::from_secs(10);
/// How often the status is read otherwise, while the panel shows.
pub(crate) const STATUS_SLOW: Duration = Duration::from_secs(30);
/// How often the status is read while the panel is closed, to keep the header current.
pub(crate) const STATUS_HIDDEN: Duration = Duration::from_secs(60);

/// How long the status poll waits between reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusIntervals {
    pub fast: Duration,
    pub slow: Duration,
    pub hidden: Duration,
}

impl Default for StatusIntervals {
    fn default() -> Self {
        Self {
            fast: STATUS_FAST,
            slow: STATUS_SLOW,
            hidden: STATUS_HIDDEN,
        }
    }
}

/// What the Review header offers once the task is approved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrOffer {
    /// The project has no GitHub repository, or its setup is not known yet.
    #[default]
    None,
    Create,
    SetUp,
}

pub(crate) fn offer(github: Option<&GithubStatus>) -> PrOffer {
    match github {
        Some(status) if status.repository.is_some() => {
            if status.gh.is_some() && status.signed_in {
                PrOffer::Create
            } else {
                PrOffer::SetUp
            }
        }
        _ => PrOffer::None,
    }
}

/// `gh` is there, signed in, and `origin` is on GitHub.
pub(crate) fn github_ready(github: Option<&GithubStatus>) -> bool {
    offer(github) == PrOffer::Create
}

/// Why the Create button waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocker {
    Creating,
    NotApproved,
    CommentsPending,
    GithubNotReady,
    NoTitle,
    ChecksNotRun,
    ChecksRunning,
    ChecksFailed,
    ChecksStopped,
    ChecksStale,
    ChecksUnclean,
}

impl Blocker {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Creating => "Creating\u{2026}",
            Self::NotApproved => "Not approved",
            Self::CommentsPending => "Comments pending",
            Self::GithubNotReady => "GitHub not set up",
            Self::NoTitle => "No title",
            Self::ChecksNotRun => "Checks not run",
            Self::ChecksRunning => "Checks running",
            Self::ChecksFailed => "Checks failed",
            Self::ChecksStopped => "Checks stopped",
            Self::ChecksStale => "Checks stale",
            Self::ChecksUnclean => "Uncommitted changes",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreateState {
    Blocked(Blocker),
    Ready,
    /// The checks did not pass but the project does not require them.
    Anyway(Blocker),
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts the rules read, not one state"
)]
pub(crate) struct CreateInputs<'a> {
    pub creating: bool,
    pub approved: bool,
    pub pending_comments: u32,
    pub github_ready: bool,
    pub title_empty: bool,
    /// The project configures no local checks, so nothing waits for them.
    pub no_checks: bool,
    /// `[checks] required`. Without it, a failed or missing run only asks first.
    pub required: bool,
    pub run: Option<&'a ChecksRun>,
    /// HEAD of the loaded Merged diff, once it is loaded.
    pub head: Option<&'a str>,
}

/// A run is over when a check failed, was skipped or stopped, or all passed. Only waiting
/// and running checks without any of those mean it still runs.
pub(crate) fn run_is_over(run: &ChecksRun) -> bool {
    run.checks.iter().any(|check| {
        matches!(
            check.state,
            CheckState::Failed | CheckState::Skipped | CheckState::Stopped
        )
    }) || run
        .checks
        .iter()
        .all(|check| check.state == CheckState::Passed)
}

/// The run was made on the commit the task branch is on now.
pub(crate) fn run_is_current(run: &ChecksRun, head: Option<&str>) -> bool {
    head == Some(run.commit.as_str())
}

/// Why a run does not let a pull request through. `None` when it passed on the current
/// commit with a clean worktree.
pub(crate) fn run_problem(run: &ChecksRun, head: Option<&str>) -> Option<Blocker> {
    let has = |state| run.checks.iter().any(|check| check.state == state);

    if !run_is_over(run) {
        Some(Blocker::ChecksRunning)
    } else if !run_is_current(run, head) {
        Some(Blocker::ChecksStale)
    } else if !run.clean {
        Some(Blocker::ChecksUnclean)
    } else if has(CheckState::Failed) || has(CheckState::Skipped) {
        Some(Blocker::ChecksFailed)
    } else if has(CheckState::Stopped) {
        Some(Blocker::ChecksStopped)
    } else {
        None
    }
}

/// Without a run the app does not know whether the checks pass, so it asks for one first.
pub(crate) fn create_state(input: &CreateInputs<'_>) -> CreateState {
    if input.creating {
        return CreateState::Blocked(Blocker::Creating);
    }
    if !input.approved {
        return CreateState::Blocked(Blocker::NotApproved);
    }
    if input.pending_comments > 0 {
        return CreateState::Blocked(Blocker::CommentsPending);
    }
    if !input.github_ready {
        return CreateState::Blocked(Blocker::GithubNotReady);
    }
    if input.title_empty {
        return CreateState::Blocked(Blocker::NoTitle);
    }
    if input.no_checks {
        return CreateState::Ready;
    }

    let problem = match input.run {
        None => Some(Blocker::ChecksNotRun),
        Some(run) => run_problem(run, input.head),
    };

    match problem {
        None => CreateState::Ready,
        Some(Blocker::ChecksRunning) => CreateState::Blocked(Blocker::ChecksRunning),
        Some(blocker) if input.required => CreateState::Blocked(blocker),
        Some(blocker) => CreateState::Anyway(blocker),
    }
}

/// The checks of a pull request that queue or run.
pub(crate) fn checks_running(pull_request: &PullRequest) -> bool {
    pull_request.checks.iter().any(|check| {
        matches!(
            check.state,
            GithubCheckState::Queued | GithubCheckState::Running
        )
    })
}

/// The wait before the status is read again.
pub(crate) fn status_interval(
    pull_request: Option<&PullRequest>,
    panel: bool,
    intervals: StatusIntervals,
) -> Duration {
    if !panel {
        intervals.hidden
    } else if pull_request.is_some_and(checks_running) {
        intervals.fast
    } else {
        intervals.slow
    }
}

/// The pull request is merged or closed, so its status no longer changes.
pub(crate) fn is_finished(pull_request: Option<&PullRequest>) -> bool {
    pull_request.is_some_and(|status| matches!(status.state, PrState::Merged | PrState::Closed))
}

/// The state shown after the number in the header: "checks running", "ready to merge".
pub(crate) fn state_word(pull_request: &PullRequest) -> &'static str {
    match pull_request.state {
        PrState::Merged => "merged",
        PrState::Closed => "closed",
        PrState::Open => {
            if checks_running(pull_request) {
                "checks running"
            } else if pull_request
                .checks
                .iter()
                .any(|check| check.state == GithubCheckState::Failed)
            {
                "checks failed"
            } else if pull_request.draft {
                "draft"
            } else if pull_request.mergeable {
                "ready to merge"
            } else {
                "open"
            }
        }
    }
}

/// The header toggle's text for a task with a pull request.
pub(crate) fn toggle_label(link: &PrLink, status: Option<&PullRequest>) -> String {
    match status {
        Some(status) => format!("#{} \u{b7} {}", link.number, state_word(status)),
        None => format!("#{}", link.number),
    }
}

/// The logins in a field: separated by commas or spaces, `@` dropped, no repeats.
pub(crate) fn parse_logins(text: &str) -> Vec<String> {
    let mut logins: Vec<String> = Vec::new();

    for part in text.split([',', ' ', '\n', '\t']) {
        let login = part.trim().trim_start_matches('@');

        if !login.is_empty() && !logins.iter().any(|known| known.eq_ignore_ascii_case(login)) {
            logins.push(login.to_owned());
        }
    }

    logins
}

/// "412ms", "38s", "2m 10s", "1h 02m".
pub(crate) fn format_duration(millis: u64) -> String {
    if millis < 1_000 {
        return format!("{millis}ms");
    }

    let seconds = millis / 1_000;

    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, seconds % 3_600 / 60)
    }
}

pub(crate) fn exit_value(status: &ExitStatus) -> String {
    match status {
        ExitStatus::Code(code) => format!("exit {code}"),
        ExitStatus::Signal(signal) => format!("signal {signal}"),
    }
}

/// The daemon's own words for a failure, all lines of them.
pub(crate) fn full_text(error: &ClientError) -> SharedString {
    match error {
        ClientError::Daemon(message) => message.clone().into(),
        other => other.to_string().into(),
    }
}

/// One step of the GitHub setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupStep {
    Install,
    SignIn,
}

impl SetupStep {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Install => "Install GitHub CLI",
            Self::SignIn => "Sign in to GitHub",
        }
    }

    pub(crate) fn running(self) -> &'static str {
        match self {
            Self::Install => "Installing GitHub CLI\u{2026}",
            Self::SignIn => "Signing in\u{2026}",
        }
    }

    pub(crate) fn failed(self) -> &'static str {
        match self {
            Self::Install => "Install failed",
            Self::SignIn => "Sign-in failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GithubSetup {
    Running(SetupStep),
    Failed {
        step: SetupStep,
        /// How the terminal ended.
        reason: SharedString,
        /// The terminal tile that shows the output, until Try again closes it.
        tile: Option<TerminalId>,
    },
}

/// What happens to the status once it is read again after a setup step.
#[derive(Clone, Debug)]
enum Follow {
    Nothing,
    AfterInstall,
    AfterSignIn(Option<(SharedString, Option<TerminalId>)>),
}

/// The last call that creates or updates the pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CreateStatus {
    Idle,
    Creating,
    /// The daemon's text: a push hook can write many lines.
    Failed(SharedString),
}

/// The orchestrator's draft, as far as the panel knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftWait {
    Idle,
    Waiting,
    Refused(SharedString),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionKind {
    Ready,
    Review,
    Merge,
}

/// A call of the status panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PanelAction {
    Idle,
    Busy(ActionKind),
    /// The merge went through but GitHub holds the pull request in its merge queue.
    MergeQueued,
    Failed(SharedString),
}

/// What the user typed and chose for one task's pull request.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent options and flags of one form, not one state"
)]
pub(crate) struct PrForm {
    pub(crate) title: Entity<TextInput>,
    pub(crate) description: Entity<TextArea>,
    pub(crate) reviewers: Entity<TextInput>,
    /// What the last draft put in the fields. A field that differs was changed by the user.
    applied: Option<PrDraft>,
    /// A draft that came after the user changed a field.
    pub(crate) newer: Option<PrDraft>,
    pub(crate) as_draft: bool,
    pub(crate) close_after_merge: bool,
    pub(crate) create: CreateStatus,
    pub(crate) draft_wait: DraftWait,
    pub(crate) checks_busy: bool,
    pub(crate) checks_error: Option<SharedString>,
    pub(crate) method: MergeMethod,
    pub(crate) action: PanelAction,
    /// The closed pull request is being replaced, so the panel shows the new form.
    pub(crate) replacing: bool,
    _subscriptions: [Subscription; 3],
}

impl PrForm {
    fn applied_title(&self) -> &str {
        self.applied.as_ref().map_or("", |draft| &draft.title)
    }

    fn applied_body(&self) -> &str {
        self.applied.as_ref().map_or("", |draft| &draft.body)
    }

    /// The user changed the title or the description since the last draft.
    pub(crate) fn user_changed(&self, cx: &App) -> bool {
        self.title.read(cx).text() != self.applied_title()
            || self.description.read(cx).text() != self.applied_body()
    }

    pub(crate) fn drafted(&self) -> bool {
        self.applied.is_some()
    }

    fn fill(&mut self, draft: &PrDraft, cx: &mut App) {
        self.title.update(cx, |input, cx| {
            input.set_text_at_start(draft.title.clone(), cx);
        });
        self.description.update(cx, |area, cx| {
            area.set_text_at_start(draft.body.clone(), cx);
        });
        self.applied = Some(draft.clone());
        self.newer = None;
    }

    fn lock(&self, locked: bool, cx: &mut App) {
        self.title
            .update(cx, |input, cx| input.set_disabled(locked, cx));
        self.description
            .update(cx, |area, cx| area.set_disabled(locked, cx));
    }

    pub(crate) fn busy(&self) -> bool {
        matches!(self.action, PanelAction::Busy(_))
    }
}

/// A running read of one pull request's status, with or without the panel. Dropping it
/// stops the poll.
pub(crate) struct StatusPoll {
    key: (DaemonTaskId, bool),
    _task: gpui::Task<()>,
}

/// Everything the pull request panels keep besides the daemon's data.
#[derive(Default)]
pub(crate) struct PullRequests {
    forms: HashMap<TaskId, PrForm>,
    /// The project whose GitHub status was asked for while Review or the panel shows.
    github_asked: Option<ProjectId>,
    pub(crate) setups: HashMap<ProjectId, GithubSetup>,
    /// The terminal of each running setup step.
    setup_terminals: HashMap<TerminalId, (ProjectId, SetupStep)>,
    /// Panel reads on their way; `true` once an event came, so the answer is out of date.
    loading: HashMap<DaemonTaskId, bool>,
    /// The task the panel data was asked for while the panel shows, and the Merged head
    /// it was asked at.
    panel_asked: Option<(DaemonTaskId, Option<String>)>,
    poll: Option<StatusPoll>,
    pub(crate) status_intervals: StatusIntervals,
    /// Moves keyboard focus into the panel at the next frame.
    focus_wanted: bool,
    /// The panel itself, so focus can enter it when no field takes it.
    panel_focus: Option<FocusHandle>,
}

impl PullRequests {
    pub(crate) fn form(&self, task: TaskId) -> Option<&PrForm> {
        self.forms.get(&task)
    }

    /// Drops what a closed task left.
    pub(crate) fn forget_task(&mut self, task: TaskId) {
        self.forms.remove(&task);
    }

    /// The panel itself, which holds keyboard focus when no field does.
    pub(crate) fn panel_focus(&self) -> Option<&FocusHandle> {
        self.panel_focus.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn polling(&self) -> bool {
        self.poll.is_some()
    }
}

/// What the Create button of the panel does: opens the pull request, or pushes new
/// commits to the open one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Create,
    Update,
}

impl PrAction {
    fn anyway_title(self) -> &'static str {
        match self {
            Self::Create => "Create PR anyway?",
            Self::Update => "Push changes anyway?",
        }
    }

    fn anyway_confirm(self) -> &'static str {
        match self {
            Self::Create => "Create PR anyway",
            Self::Update => "Push anyway",
        }
    }
}

/// Asks before a pull request opens or changes without passing checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AnywayEvent {
    Dismissed,
    Confirmed,
}

pub(crate) struct AnywayDialog {
    problem: Blocker,
    action: PrAction,
    focus_handle: FocusHandle,
}

impl gpui::EventEmitter<AnywayEvent> for AnywayDialog {}

impl AnywayDialog {
    fn new(
        problem: Blocker,
        action: PrAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        Self {
            problem,
            action,
            focus_handle,
        }
    }
}

impl Render for AnywayDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();

        let footer = h_flex()
            .gap(gpui::px(8.))
            .child(
                Button::new("create-anyway-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .debug_selector(|| "create-anyway-cancel".to_owned())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AnywayEvent::Dismissed))),
            )
            .child(
                Button::new("create-anyway-confirm", self.action.anyway_confirm())
                    .variant(ButtonVariant::Primary)
                    .debug_selector(|| "create-anyway-confirm".to_owned())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AnywayEvent::Confirmed))),
            );

        Dialog::new(
            "create-anyway-dialog",
            self.action.anyway_title(),
            &self.focus_handle,
        )
        .width(gpui::px(380.))
        .on_dismiss(move |_, cx| {
            on_dismiss.update(cx, |_, cx| cx.emit(AnywayEvent::Dismissed));
        })
        .child(
            div()
                .id("create-anyway-problem")
                .role(gpui::Role::Status)
                .aria_label(self.problem.label())
                .child(Label::new(self.problem.label()).color(Color::Warn)),
        )
        .footer(footer)
    }
}

type Call = Box<dyn FnOnce(&Client) -> Result<(), ClientError> + Send>;

impl Workspace {
    fn connected_client(&self) -> Option<Client> {
        self.daemon.clone().filter(Client::is_connected)
    }

    /// The daemon's id of a tab that is a real task.
    fn real_task(&self, task: TaskId) -> Option<DaemonTaskId> {
        self.data
            .task(task)
            .filter(|task| task.daemon.is_some() && !task.is_start)
            .and_then(|_| self.data.diff_id(task))
    }

    /// The task has been approved or has a pull request: its panel and header toggle exist.
    pub(crate) fn pr_available(&self, task: TaskId) -> bool {
        self.data.task(task).is_some_and(|task| {
            !task.is_start && (task.approved.is_some() || task.pull_request.is_some())
        })
    }

    /// Creates the task's form when its panel shows.
    fn ensure_form(&mut self, task: TaskId, cx: &mut Context<Self>) {
        if self.pull_requests.panel_focus.is_none() {
            self.pull_requests.panel_focus = Some(cx.focus_handle());
        }
        if self.pull_requests.forms.contains_key(&task) {
            return;
        }

        let title = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Title", cx);
            input
        });
        let description = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Description", cx);
            area.set_mono(true, cx);
            area.set_rows(9, cx);
            area
        });
        let reviewers = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Reviewers", cx);
            input.set_placeholder("Logins", cx);
            input.set_mono(true, cx);
            input.set_size(swarm_ui::TextFieldSize::Small, cx);
            input
        });

        let on_title = cx.subscribe(&title, |_, _, _, cx| cx.notify());
        let on_description = cx.subscribe(&description, |_, _, _, cx| cx.notify());
        let on_reviewers = cx.subscribe(&reviewers, move |this, _, event, cx| match event {
            TextInputEvent::Changed(_) => cx.notify(),
            TextInputEvent::Submitted(_) => this.request_pr_review(task, cx),
        });

        let mut form = PrForm {
            title,
            description,
            reviewers,
            applied: None,
            newer: None,
            as_draft: true,
            close_after_merge: false,
            create: CreateStatus::Idle,
            draft_wait: DraftWait::Idle,
            checks_busy: false,
            checks_error: None,
            method: MergeMethod::Squash,
            action: PanelAction::Idle,
            replacing: false,
            _subscriptions: [on_title, on_description, on_reviewers],
        };

        if let Some(draft) = self
            .data
            .diff_id(task)
            .and_then(|id| self.data.drafts.get(&id))
        {
            form.fill(draft, cx);
        }

        self.pull_requests.forms.insert(task, form);
    }

    /// The panel shows the form for a new pull request: the task has none yet, or the user
    /// chose to replace the closed one.
    pub(crate) fn shows_new_pull_request(&self, task: TaskId) -> bool {
        let has_link = self
            .data
            .task(task)
            .is_some_and(|entry| entry.pull_request.is_some());
        if !has_link {
            return true;
        }

        let closed = self
            .data
            .pull_request_status(task)
            .is_some_and(|status| status.state == PrState::Closed);
        let replacing = self
            .pull_requests
            .forms
            .get(&task)
            .is_some_and(|form| form.replacing);

        closed && replacing
    }

    /// Keeps the pull request work running only while it is needed: GitHub's status while
    /// Review or the panel shows, the checks and the draft while the panel shows, and the
    /// status poll while the task has an open pull request in an active window.
    pub(crate) fn sync_pull_requests(
        &mut self,
        window_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.active_task;
        let connected = self.connected_client().is_some();
        let id = self.real_task(task).filter(|_| connected);
        let panel = self.visible_panel() == Some(RightPanel::PullRequest);

        let github = id
            .filter(|_| panel || self.view == View::Review)
            .and_then(|_| self.data.task(task))
            .map(|task| task.project);
        self.sync_github(github, window, cx);

        if panel {
            self.ensure_form(task, cx);
        } else {
            self.rest_panel();
        }

        if panel && let Some(diff_id) = self.data.diff_id(task) {
            self.sync_panel_load(task, diff_id, cx);

            if self.shows_new_pull_request(task) {
                self.request_draft_if_needed(task, diff_id, cx);
            }
        }

        let has_link = self
            .data
            .task(task)
            .is_some_and(|task| task.pull_request.is_some());
        let finished = is_finished(self.data.pull_request_status(task));

        match id {
            Some(id) if has_link && window_active && !finished => {
                self.sync_status_poll(id, panel, cx);
            }
            _ => self.pull_requests.poll = None,
        }
    }

    /// The panel closed: the next open reads the panel data again and asks for a draft
    /// again when the last ask was refused.
    fn rest_panel(&mut self) {
        self.pull_requests.panel_asked = None;

        for form in self.pull_requests.forms.values_mut() {
            if matches!(form.draft_wait, DraftWait::Refused(_)) {
                form.draft_wait = DraftWait::Idle;
            }
        }
    }

    fn sync_github(
        &mut self,
        wanted: Option<ProjectId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pull_requests.github_asked == wanted {
            return;
        }

        self.pull_requests.github_asked = wanted;

        if let Some(project) = wanted {
            self.fetch_github(project, Follow::Nothing, window, cx);
        }
    }

    /// Reads the project's GitHub status again, then follows up.
    fn fetch_github(
        &mut self,
        project: ProjectId,
        follow: Follow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let daemon = self
            .data
            .project(project)
            .and_then(|project| project.daemon);
        let (Some(client), Some(daemon)) = (self.connected_client(), daemon) else {
            return;
        };

        let reading = cx.background_spawn(async move { client.github_status(daemon) });

        cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let updated = this.update_in(cx, |workspace, window, cx| {
                workspace.read_github(project, result, follow, window, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before GitHub answered: {error}");
            }
        })
        .detach();
    }

    fn read_github(
        &mut self,
        project: ProjectId,
        result: Result<GithubStatus, ClientError>,
        follow: Follow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &result {
            Ok(status) => {
                self.data.github.insert(project, status.clone());
            }
            Err(error) => log::debug!("cannot read the GitHub status: {error}"),
        }

        match follow {
            Follow::Nothing => {}
            Follow::AfterInstall => self.after_install(project, result, window, cx),
            Follow::AfterSignIn(failure) => {
                let setup = failure.map(|(reason, tile)| GithubSetup::Failed {
                    step: SetupStep::SignIn,
                    reason,
                    tile,
                });

                match setup {
                    Some(setup) => self.pull_requests.setups.insert(project, setup),
                    None => self.pull_requests.setups.remove(&project),
                };
            }
        }

        cx.notify();
    }

    // Drafts

    /// Asks the orchestrator for a draft once, when the panel shows an approved task that
    /// has none. The daemon's stored draft comes first, with the panel data.
    fn request_draft_if_needed(&mut self, task: TaskId, id: DaemonTaskId, cx: &mut Context<Self>) {
        let panel_read = self
            .pull_requests
            .panel_asked
            .as_ref()
            .is_some_and(|(asked, _)| *asked == id)
            && !self.pull_requests.loading.contains_key(&id);
        let wanted = self.pr_available(task)
            && panel_read
            && !self.data.drafts.contains_key(&id)
            && self
                .pull_requests
                .forms
                .get(&task)
                .is_some_and(|form| form.draft_wait == DraftWait::Idle);
        if !wanted {
            return;
        }

        let Some(client) = self.connected_client() else {
            if self.source == DataSource::Sample {
                self.draft_arrived(id, sample_draft(), cx);
            }

            return;
        };

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.draft_wait = DraftWait::Waiting;
        }

        let asking = cx.background_spawn(async move { client.draft_pr(id) });

        cx.spawn(async move |this, cx| {
            let result = asking.await;
            let updated = this.update(cx, |workspace, cx| {
                workspace.draft_asked(task, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the draft was asked for: {error}");
            }
        })
        .detach();
    }

    fn draft_asked(
        &mut self,
        task: TaskId,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Err(error) = result else {
            return;
        };
        log::debug!("the draft was refused: {error}");

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.draft_wait = DraftWait::Refused(refusal(&error));
            cx.notify();
        }
    }

    /// The orchestrator sent a draft. It fills the fields unless the user changed them.
    fn draft_arrived(&mut self, id: DaemonTaskId, draft: PrDraft, cx: &mut Context<Self>) {
        self.data.drafts.insert(id, draft.clone());

        let task = self
            .data
            .tasks
            .iter()
            .find(|task| self.data.diff_id(task.id) == Some(id))
            .map(|task| task.id);

        if let Some(form) = task.and_then(|task| self.pull_requests.forms.get_mut(&task)) {
            form.draft_wait = DraftWait::Idle;

            if form.user_changed(cx) {
                form.newer = Some(draft);
            } else {
                form.fill(&draft, cx);
            }
        }

        cx.notify();
    }

    /// Replaces the fields with the draft that came after the user changed them.
    pub(crate) fn use_new_draft(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task;

        if let Some(form) = self.pull_requests.forms.get_mut(&task)
            && let Some(draft) = form.newer.take()
        {
            form.fill(&draft, cx);
            cx.notify();
        }
    }

    // Events

    /// Applies what the daemon reports about checks and drafts.
    pub(crate) fn pr_event(&mut self, event: PrEvent, cx: &mut Context<Self>) {
        match event {
            PrEvent::Checks(run) => {
                if let Some(superseded) = self.pull_requests.loading.get_mut(&run.task) {
                    *superseded = true;
                }

                self.data.set_checks(*run);
                cx.notify();
            }
            PrEvent::Draft { task, draft } => self.draft_arrived(task, draft, cx),
        }
    }

    // Local checks

    /// Reads the checks, the latest run and the stored draft when the panel opens, and
    /// again when the task branch moves to a new commit.
    fn sync_panel_load(&mut self, task: TaskId, id: DaemonTaskId, cx: &mut Context<Self>) {
        let head = self.data.merged_head(task).map(str::to_owned);

        let previous = self.pull_requests.panel_asked.take();
        let (reload, kept) = match previous {
            // The first head arrives after the first read. Only a move to another commit
            // reads again.
            Some((asked, known)) if asked == id => {
                let moved = known.is_some() && head.is_some() && known != head;

                (moved, head.or(known))
            }
            _ => (true, head),
        };

        self.pull_requests.panel_asked = Some((id, kept));
        if !reload {
            return;
        }

        let Some(client) = self.connected_client() else {
            if self.source == DataSource::Sample {
                self.data.set_check_config(id, sample_check_config());
            }

            return;
        };

        self.pull_requests.loading.insert(id, false);
        let loading = cx.background_spawn(async move { client.pr_panel(id) });

        cx.spawn(async move |this, cx| {
            let result = loading.await;
            let updated = this.update(cx, |workspace, cx| {
                workspace.panel_loaded(id, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the panel was read: {error}");
            }
        })
        .detach();
    }

    fn panel_loaded(
        &mut self,
        id: DaemonTaskId,
        result: Result<PrPanel, ClientError>,
        cx: &mut Context<Self>,
    ) {
        let superseded = self.pull_requests.loading.remove(&id).unwrap_or(false);

        match result {
            Ok(panel) => {
                self.data.set_check_config(
                    id,
                    CheckConfig {
                        commands: panel.commands,
                        required: panel.required,
                        pre_push_hook: panel.pre_push_hook,
                    },
                );
                if !superseded {
                    self.data.replace_run(id, panel.run);
                }
                if let Some(draft) = panel.draft
                    && self.data.drafts.get(&id) != Some(&draft)
                {
                    self.draft_arrived(id, draft, cx);
                }
            }
            Err(error) => log::debug!("cannot read the pull request panel: {error}"),
        }

        cx.notify();
    }

    /// Starts the local checks of the open task.
    pub(crate) fn run_checks(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let busy = self
            .pull_requests
            .forms
            .get(&task)
            .is_none_or(|form| form.checks_busy);

        if busy {
            return;
        }

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.checks_error = None;
        }

        let Some(client) = self.connected_client() else {
            if self.source == DataSource::Sample {
                self.data.set_checks(sample_checks_run(id, "sample"));
            }

            cx.notify();
            return;
        };

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.checks_busy = true;
        }

        let starting = cx.background_spawn(async move { client.run_checks(id) });

        cx.spawn(async move |this, cx| {
            let result = starting.await;
            let updated = this.update(cx, |workspace, cx| {
                if let Some(form) = workspace.pull_requests.forms.get_mut(&task) {
                    form.checks_busy = false;
                    form.checks_error = result.err().map(|error| refusal(&error));
                }

                cx.notify();
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the checks started: {error}");
            }
        })
        .detach();
    }

    /// Shows the process terminal of a check in the Terminals view.
    pub(crate) fn show_check_output(&mut self, terminal: DaemonTerminalId, cx: &mut Context<Self>) {
        let tile = self.tile_of_daemon_terminal(terminal);

        if let Some(tile) = tile {
            self.set_view(View::Terminals, cx);
            self.focus_terminal(tile, cx);
        }
    }

    /// The tile that shows a daemon terminal.
    pub(crate) fn tile_of_daemon_terminal(&self, terminal: DaemonTerminalId) -> Option<TerminalId> {
        self.live_terminals
            .iter()
            .find(|(_, live)| live.daemon_id() == terminal)
            .map(|(tile, _)| *tile)
    }

    // Create

    /// The facts the Create and Push buttons read.
    fn create_inputs(&self, task: TaskId, creating: bool, title_empty: bool) -> CreateInputs<'_> {
        let checks = self.data.checks_of(task);
        let config = checks.and_then(|checks| checks.config.as_ref());
        let run = checks.and_then(|checks| checks.run.as_ref());
        let required = config
            .map(|config| config.required)
            .or_else(|| run.map(|run| run.required))
            .unwrap_or(true);

        CreateInputs {
            creating,
            approved: self.data.approval_holds(task),
            pending_comments: count_comments(&self.data.task_comments(task)).pending,
            github_ready: github_ready(self.data.github_of(task)),
            title_empty,
            no_checks: config.is_some_and(|config| config.commands.is_empty()),
            required,
            run,
            head: self.data.merged_head(task),
        }
    }

    /// What the Create button does now.
    pub(crate) fn create_state(&self, task: TaskId, cx: &App) -> CreateState {
        let form = self.pull_requests.forms.get(&task);
        let creating = form.is_some_and(|form| form.create == CreateStatus::Creating);
        let title_empty = form.is_none_or(|form| form.title.read(cx).text().trim().is_empty());

        create_state(&self.create_inputs(task, creating, title_empty))
    }

    /// What the Push changes button does now.
    pub(crate) fn update_state(&self, task: TaskId) -> CreateState {
        create_state(&self.create_inputs(task, false, false))
    }

    /// The project configures local checks, or the app does not know yet.
    pub(crate) fn has_local_checks(&self, task: TaskId) -> bool {
        self.data
            .checks_of(task)
            .and_then(|checks| checks.config.as_ref())
            .is_none_or(|config| !config.commands.is_empty())
    }

    /// The task has an open pull request and commits that were not pushed to it.
    pub(crate) fn update_offered(&self, task: TaskId) -> bool {
        self.data
            .pull_request_status(task)
            .is_none_or(|status| status.state == PrState::Open)
            && self.data.has_new_commits(task)
    }

    /// Opens the panel for a new pull request, from Review or the palette.
    pub(crate) fn open_pull_request(&mut self, cx: &mut Context<Self>) {
        if !self.data.can_create_pr(self.active_task) {
            return;
        }

        self.right_panel = Some(RightPanel::PullRequest);
        if self.view == View::Swarm {
            self.set_view(View::Terminals, cx);
        }

        self.ensure_form(self.active_task, cx);
        self.pull_requests.focus_wanted = true;

        cx.notify();
    }

    /// The Create button: opens the pull request, or asks first when the checks did not pass.
    pub(crate) fn create_pull_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.create_state(self.active_task, cx) {
            CreateState::Ready => self.send_create(false, cx),
            CreateState::Anyway(problem) => {
                self.ask_anyway(problem, PrAction::Create, window, cx);
            }
            CreateState::Blocked(_) => {}
        }
    }

    /// The Push changes button: sends the new commits, or asks first when the checks did
    /// not pass.
    pub(crate) fn update_pull_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.active_task;
        if !self.update_offered(task) {
            return;
        }

        match self.update_state(task) {
            CreateState::Ready => self.send_update(false, cx),
            CreateState::Anyway(problem) => {
                self.ask_anyway(problem, PrAction::Update, window, cx);
            }
            CreateState::Blocked(_) => {}
        }
    }

    /// The Create PR button of a closed pull request: the panel shows the new form again.
    pub(crate) fn replace_closed_pull_request(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task;

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.replacing = true;
            form.create = CreateStatus::Idle;
            self.pull_requests.focus_wanted = true;
            cx.notify();
        }
    }

    fn ask_anyway(
        &mut self,
        problem: Blocker,
        action: PrAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| AnywayDialog::new(problem, action, window, cx));
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                move |this, _, event, window, cx| match event {
                    AnywayEvent::Dismissed => this.close_overlay(window, cx),
                    AnywayEvent::Confirmed => {
                        this.close_overlay(window, cx);

                        match action {
                            PrAction::Create => this.send_create(true, cx),
                            PrAction::Update => this.send_update(true, cx),
                        }
                    }
                },
            );

        self.overlay = Some(Overlay::CreateAnyway {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn send_create(&mut self, skip_checks: bool, cx: &mut Context<Self>) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let client = self.connected_client();
        let Some(form) = self.pull_requests.forms.get_mut(&task) else {
            return;
        };
        if form.create == CreateStatus::Creating {
            return;
        }

        let draft = PrDraft {
            title: form.title.read(cx).text().trim().to_owned(),
            body: form.description.read(cx).text().to_string(),
        };
        let (as_draft, close_after_merge) = (form.as_draft, form.close_after_merge);

        let Some(client) = client else {
            if self.source == DataSource::Sample {
                self.create_sample_pull_request(task, id, &draft, cx);
            }

            return;
        };

        form.create = CreateStatus::Creating;
        form.lock(true, cx);
        self.pull_requests.focus_wanted = true;
        cx.notify();

        let creating = cx.background_spawn(async move {
            client.create_pr(id, draft, as_draft, close_after_merge, skip_checks)
        });

        cx.spawn(async move |this, cx| {
            let result = creating.await;
            let updated = this.update(cx, |workspace, cx| {
                workspace.pull_request_created(task, id, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the pull request opened: {error}");
            }
        })
        .detach();
    }

    fn pull_request_created(
        &mut self,
        task: TaskId,
        id: DaemonTaskId,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.lock(false, cx);
            form.create = match &result {
                Ok(()) => CreateStatus::Idle,
                Err(error) => {
                    log::warn!("cannot create the pull request: {error}");
                    CreateStatus::Failed(full_text(error))
                }
            };
            form.replacing &= result.is_err();
        }

        self.pull_requests.focus_wanted = true;
        if result.is_ok() {
            self.read_pr_status(id, cx);
        }

        cx.notify();
    }

    fn send_update(&mut self, skip_checks: bool, cx: &mut Context<Self>) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let client = self.connected_client();
        let Some(form) = self.pull_requests.forms.get_mut(&task) else {
            return;
        };
        if form.create == CreateStatus::Creating {
            return;
        }

        let Some(client) = client else {
            if self.source == DataSource::Sample {
                self.update_sample_pull_request(task, cx);
            }

            return;
        };

        form.create = CreateStatus::Creating;
        self.pull_requests.focus_wanted = true;
        cx.notify();

        let pushing = cx.background_spawn(async move { client.update_pr(id, skip_checks) });

        cx.spawn(async move |this, cx| {
            let result = pushing.await;
            let updated = this.update(cx, |workspace, cx| {
                workspace.pull_request_updated(task, id, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the changes were pushed: {error}");
            }
        })
        .detach();
    }

    fn pull_request_updated(
        &mut self,
        task: TaskId,
        id: DaemonTaskId,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.create = match &result {
                Ok(()) => CreateStatus::Idle,
                Err(error) => {
                    log::warn!("cannot push the changes: {error}");
                    CreateStatus::Failed(full_text(error))
                }
            };
        }

        self.pull_requests.focus_wanted = true;
        if result.is_ok() {
            self.read_pr_status(id, cx);
        }

        cx.notify();
    }

    /// Opens a pull request on sample data, so the status panel can be seen without a daemon.
    fn create_sample_pull_request(
        &mut self,
        task: TaskId,
        id: DaemonTaskId,
        draft: &PrDraft,
        cx: &mut Context<Self>,
    ) {
        let mut pull_request = sample_pull_request();
        pull_request.title.clone_from(&draft.title);
        let head = self.data.merged_head(task).unwrap_or("sample").to_owned();

        if let Some(entry) = self.data.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.pull_request = Some(PrLink {
                number: pull_request.number,
                url: pull_request.url.clone(),
                head,
            });
        }
        self.data.pull_requests.insert(id, pull_request);
        cx.notify();
    }

    /// Marks the new commits as pushed on sample data.
    fn update_sample_pull_request(&mut self, task: TaskId, cx: &mut Context<Self>) {
        let head = self.data.merged_head(task).map(str::to_owned);

        if let Some(link) = self
            .data
            .tasks
            .iter_mut()
            .find(|entry| entry.id == task)
            .and_then(|entry| entry.pull_request.as_mut())
            && let Some(head) = head
        {
            link.head = head;
        }

        cx.notify();
    }

    // Focus

    /// Moves keyboard focus into the panel after Create or Push changes: to the title when
    /// the form is open for typing, else to the first control of the panel.
    pub(crate) fn apply_pending_pr_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pull_requests.focus_wanted || self.overlay.is_some() {
            return;
        }

        self.pull_requests.focus_wanted = false;
        if self.visible_panel() != Some(RightPanel::PullRequest) {
            return;
        }

        let task = self.active_task;
        let Some(form) = self.pull_requests.forms.get(&task) else {
            return;
        };

        if self.shows_new_pull_request(task) && form.create != CreateStatus::Creating {
            let title = form.title.focus_handle(cx);

            window.focus(&title, cx);
        } else if let Some(panel) = self.pull_requests.panel_focus.clone() {
            window.focus(&panel, cx);
            window.focus_next(cx);
        }
    }

    // Status

    /// Keeps one poll of the open pull request's status, with or without the panel. A new
    /// poll reads at once.
    fn sync_status_poll(&mut self, id: DaemonTaskId, panel: bool, cx: &mut Context<Self>) {
        let key = (id, panel);

        if self
            .pull_requests
            .poll
            .as_ref()
            .is_some_and(|poll| poll.key == key)
        {
            return;
        }

        self.pull_requests.poll = Some(StatusPoll {
            key,
            _task: Self::poll_status(self.active_task, id, panel, cx),
        });
    }

    fn poll_status(
        task: TaskId,
        id: DaemonTaskId,
        panel: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(client) = this.update(cx, |workspace, _| workspace.connected_client())
                else {
                    return;
                };

                if let Some(client) = client {
                    let reading = cx
                        .background_executor()
                        .spawn(async move { client.pr_status(id) });
                    let result = reading.await;

                    let Ok(wait) = this.update(cx, |workspace, cx| {
                        workspace.read_status(id, result, cx);

                        let status = workspace.data.pull_request_status(task);
                        let intervals = workspace.pull_requests.status_intervals;

                        (!is_finished(status)).then(|| status_interval(status, panel, intervals))
                    }) else {
                        return;
                    };

                    let Some(wait) = wait else {
                        return;
                    };

                    cx.background_executor().timer(wait).await;
                    continue;
                }

                let Ok(intervals) =
                    this.update(cx, |workspace, _| workspace.pull_requests.status_intervals)
                else {
                    return;
                };

                let wait = status_interval(None, panel, intervals);
                cx.background_executor().timer(wait).await;
            }
        })
    }

    /// Reads the status once and keeps it.
    pub(crate) fn read_pr_status(&mut self, id: DaemonTaskId, cx: &mut Context<Self>) {
        let Some(client) = self.connected_client() else {
            return;
        };

        let reading = cx.background_spawn(async move { client.pr_status(id) });

        cx.spawn(async move |this, cx| {
            let result = reading.await;
            let updated = this.update(cx, |workspace, cx| workspace.read_status(id, result, cx));

            if let Err(error) = updated {
                log::debug!("the window closed before the status came: {error}");
            }
        })
        .detach();
    }

    fn read_status(
        &mut self,
        id: DaemonTaskId,
        result: Result<PullRequest, ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(status) => {
                if self.data.pull_requests.get(&id) != Some(&status) {
                    self.data.pull_requests.insert(id, status);
                    cx.notify();
                }
            }
            Err(error) => log::debug!("cannot read the pull request: {error}"),
        }
    }

    // Status panel actions

    /// Runs one call of the status panel on the daemon, then reads the status again.
    fn panel_call(
        &mut self,
        kind: ActionKind,
        call: Call,
        sample: impl FnOnce(&mut Self),
        cx: &mut Context<Self>,
    ) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let client = self.connected_client();
        let Some(form) = self.pull_requests.forms.get_mut(&task) else {
            return;
        };
        if form.busy() {
            return;
        }

        let Some(client) = client else {
            if self.source == DataSource::Sample {
                sample(self);
                cx.notify();
            }

            return;
        };

        form.action = PanelAction::Busy(kind);
        cx.notify();

        let running = cx.background_spawn(async move { call(&client) });

        cx.spawn(async move |this, cx| {
            let result = running.await;
            let updated = this.update(cx, |workspace, cx| {
                workspace.panel_call_done(task, id, kind, &result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the call answered: {error}");
            }
        })
        .detach();
    }

    fn panel_call_done(
        &mut self,
        task: TaskId,
        id: DaemonTaskId,
        kind: ActionKind,
        result: &Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        let merge = kind == ActionKind::Merge;

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.action = match result {
                // The merge settles once the status is read: it may sit in a queue.
                Ok(()) if merge => PanelAction::Busy(kind),
                Ok(()) => PanelAction::Idle,
                Err(error) => PanelAction::Failed(full_text(error)),
            };
        }

        if merge {
            self.read_status_after_merge(task, id, result.is_ok(), cx);
        } else if result.is_ok() {
            self.read_pr_status(id, cx);
        }

        cx.notify();
    }

    /// Reads the status after a merge call, also when the call failed: the pull request
    /// may be merged although the task did not close.
    fn read_status_after_merge(
        &mut self,
        task: TaskId,
        id: DaemonTaskId,
        merged: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.connected_client() else {
            return;
        };

        let reading = cx.background_spawn(async move { client.pr_status(id) });

        cx.spawn(async move |this, cx| {
            let result = reading.await;
            let updated = this.update(cx, |workspace, cx| {
                let read = result.is_ok();

                workspace.read_status(id, result, cx);
                workspace.merge_settled(task, merged && read, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the status came: {error}");
            }
        })
        .detach();
    }

    /// A merge that went through leaves the pull request open only in a merge queue.
    fn merge_settled(&mut self, task: TaskId, merged: bool, cx: &mut Context<Self>) {
        let open = self
            .data
            .pull_request_status(task)
            .is_some_and(|status| status.state == PrState::Open);

        if let Some(form) = self.pull_requests.forms.get_mut(&task)
            && form.action == PanelAction::Busy(ActionKind::Merge)
        {
            form.action = if merged && open {
                PanelAction::MergeQueued
            } else {
                PanelAction::Idle
            };
        }

        cx.notify();
    }

    pub(crate) fn mark_pr_ready(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.data.diff_id(self.active_task) else {
            return;
        };

        self.panel_call(
            ActionKind::Ready,
            Box::new(move |client| client.mark_pr_ready(id)),
            move |workspace| {
                if let Some(status) = workspace.data.pull_requests.get_mut(&id) {
                    status.draft = false;
                }
            },
            cx,
        );
    }

    /// Asks the logins in the reviewers field for a review.
    pub(crate) fn request_pr_review(&mut self, task: TaskId, cx: &mut Context<Self>) {
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let Some(form) = self.pull_requests.forms.get(&task) else {
            return;
        };

        let logins = parse_logins(&form.reviewers.read(cx).text());
        if logins.is_empty() {
            return;
        }

        let reviewers = form.reviewers.clone();
        let asked = logins.clone();

        self.panel_call(
            ActionKind::Review,
            Box::new(move |client| client.request_review(id, logins)),
            move |workspace| {
                if let Some(status) = workspace.data.pull_requests.get_mut(&id) {
                    status.reviewers.extend(asked.into_iter().map(|login| {
                        swarm_protocol::Reviewer {
                            login,
                            state: swarm_protocol::ReviewState::Requested,
                        }
                    }));
                }
            },
            cx,
        );

        reviewers.update(cx, |input, cx| input.set_text("", cx));
    }

    pub(crate) fn merge_pull_request(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let Some(form) = self.pull_requests.forms.get(&task) else {
            return;
        };
        let method = form.method;

        self.panel_call(
            ActionKind::Merge,
            Box::new(move |client| client.merge_pr(id, method)),
            move |workspace| {
                if let Some(status) = workspace.data.pull_requests.get_mut(&id) {
                    status.state = PrState::Merged;
                }
            },
            cx,
        );
    }

    pub(crate) fn choose_merge_method(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        if let Some(form) = self.pull_requests.forms.get_mut(&self.active_task) {
            form.method = method;
            cx.notify();
        }
    }

    pub(crate) fn set_as_draft(&mut self, value: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.pull_requests.forms.get_mut(&self.active_task) {
            form.as_draft = value;
            cx.notify();
        }
    }

    /// Sets "Close the task after merge", before the pull request exists or after.
    pub(crate) fn set_close_after_merge(&mut self, value: bool, cx: &mut Context<Self>) {
        let task = self.active_task;
        let has_link = self
            .data
            .task(task)
            .is_some_and(|entry| entry.pull_request.is_some());

        if let Some(form) = self.pull_requests.forms.get_mut(&task) {
            form.close_after_merge = value;
        }

        if has_link {
            self.send_close_after_merge(task, value, cx);
        }

        cx.notify();
    }

    fn send_close_after_merge(&mut self, task: TaskId, value: bool, cx: &mut Context<Self>) {
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let previous = self
            .data
            .task(task)
            .is_some_and(|entry| entry.close_after_merge);

        self.apply_close_after_merge(task, value);

        let Some(client) = self.connected_client() else {
            return;
        };

        let setting = cx.background_spawn(async move { client.set_close_after_merge(id, value) });

        cx.spawn(async move |this, cx| {
            let result = setting.await;
            let updated = this.update(cx, |workspace, cx| {
                if let Err(error) = result {
                    log::warn!("cannot change close after merge: {error}");
                    workspace.apply_close_after_merge(task, previous);

                    if let Some(form) = workspace.pull_requests.forms.get_mut(&task) {
                        form.action = PanelAction::Failed(full_text(&error));
                    }
                }

                cx.notify();
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the option changed: {error}");
            }
        })
        .detach();
    }

    fn apply_close_after_merge(&mut self, task: TaskId, value: bool) {
        if let Some(entry) = self.data.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.close_after_merge = value;
        }
    }

    /// Opens the pull request in the browser.
    pub(crate) fn open_pull_request_url(&self, cx: &mut App) {
        let url = self
            .data
            .task(self.active_task)
            .and_then(|task| task.pull_request.as_ref())
            .map(|link| link.url.clone());

        match url {
            Some(url) if url.starts_with("https://") => cx.open_url(&url),
            Some(url) => log::warn!("not opening {url}: it is not an https link"),
            None => {}
        }
    }

    // Header

    /// The text of the header's Pull request toggle, once the task is approved.
    pub(crate) fn pull_request_toggle_label(&self, task: TaskId) -> Option<SharedString> {
        let entry = self.data.task(task).filter(|task| !task.is_start)?;

        match &entry.pull_request {
            Some(link) => {
                let status = self.data.pull_request_status(task);

                Some(toggle_label(link, status).into())
            }
            None if entry.approved.is_some() => Some("Pull request".into()),
            None => None,
        }
    }

    /// What Review offers on an approved task.
    pub(crate) fn review_offer(&self, task: TaskId) -> PrOffer {
        let has_link = self
            .data
            .task(task)
            .is_some_and(|entry| entry.pull_request.is_some());

        if has_link {
            return PrOffer::None;
        }

        offer(self.data.github_of(task))
    }

    // GitHub setup

    /// The panel shows, so the Merged diff tells when the checks are stale and when the
    /// branch has commits the pull request lacks.
    pub(crate) fn pr_panel_needs_merged_diff(&self) -> bool {
        self.visible_panel() == Some(RightPanel::PullRequest)
    }

    /// Starts the next step of the GitHub setup in a user terminal: the install when `gh`
    /// is missing, else the sign-in.
    pub(crate) fn start_github_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.active_project;
        let Some(status) = self.data.github.get(&project).cloned() else {
            return;
        };
        if matches!(
            self.pull_requests.setups.get(&project),
            Some(GithubSetup::Running(_))
        ) {
            return;
        }

        let step = if status.gh.is_none() {
            SetupStep::Install
        } else if !status.signed_in {
            SetupStep::SignIn
        } else {
            return;
        };
        let command = match step {
            SetupStep::Install => status.install,
            SetupStep::SignIn => status.sign_in,
        };

        if let Some(GithubSetup::Failed {
            tile: Some(tile), ..
        }) = self.pull_requests.setups.get(&project).cloned()
        {
            self.close_user_terminal(tile, cx);
        }

        self.right_panel = Some(RightPanel::PullRequest);
        if self.view != View::Terminals {
            self.set_view(View::Terminals, cx);
        }

        self.run_setup_step(project, step, command, window, cx);
    }

    fn run_setup_step(
        &mut self,
        project: ProjectId,
        step: SetupStep,
        command: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if command.is_empty() {
            self.pull_requests.setups.insert(
                project,
                GithubSetup::Failed {
                    step,
                    reason: "No command".into(),
                    tile: None,
                },
            );
            cx.notify();
            return;
        }

        self.pull_requests
            .setups
            .insert(project, GithubSetup::Running(step));

        if let Some(client) = self.connected_client() {
            let task = self.active_task;

            self.create_live_terminal(
                client,
                task,
                Some(command),
                Some((project, step)),
                window,
                cx,
            );
        }

        cx.notify();
    }

    /// The setup terminal got its tile.
    pub(crate) fn setup_terminal_added(
        &mut self,
        terminal: TerminalId,
        project: ProjectId,
        step: SetupStep,
    ) {
        self.pull_requests
            .setup_terminals
            .insert(terminal, (project, step));
    }

    /// The daemon could not make the setup terminal.
    pub(crate) fn setup_terminal_failed(
        &mut self,
        project: ProjectId,
        step: SetupStep,
        error: &ClientError,
        cx: &mut Context<Self>,
    ) {
        self.pull_requests.setups.insert(
            project,
            GithubSetup::Failed {
                step,
                reason: refusal(error),
                tile: None,
            },
        );
        cx.notify();
    }

    /// A setup terminal ended. Returns false for any other terminal.
    pub(crate) fn github_terminal_exited(
        &mut self,
        terminal: TerminalId,
        status: &ExitStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((project, step)) = self.pull_requests.setup_terminals.remove(&terminal) else {
            return false;
        };
        let failed = !matches!(status, ExitStatus::Code(0));

        if !failed {
            self.close_user_terminal(terminal, cx);
        }

        match (step, failed) {
            (SetupStep::Install, false) => {
                self.fetch_github(project, Follow::AfterInstall, window, cx);
            }
            (SetupStep::Install, true) => {
                self.pull_requests.setups.insert(
                    project,
                    GithubSetup::Failed {
                        step,
                        reason: exit_value(status).into(),
                        tile: Some(terminal),
                    },
                );
            }
            (SetupStep::SignIn, _) => {
                let failure = failed.then(|| (exit_value(status).into(), Some(terminal)));

                self.fetch_github(project, Follow::AfterSignIn(failure), window, cx);
            }
        }

        cx.notify();

        true
    }

    /// The user closed a setup terminal before it ended.
    pub(crate) fn github_terminal_closed(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let Some((project, step)) = self.pull_requests.setup_terminals.remove(&terminal) else {
            return;
        };

        self.pull_requests.setups.insert(
            project,
            GithubSetup::Failed {
                step,
                reason: "Closed".into(),
                tile: None,
            },
        );
        cx.notify();
    }

    fn after_install(
        &mut self,
        project: ProjectId,
        result: Result<GithubStatus, ClientError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let failed = |reason: SharedString| GithubSetup::Failed {
            step: SetupStep::Install,
            reason,
            tile: None,
        };

        match result {
            Err(error) => {
                self.pull_requests
                    .setups
                    .insert(project, failed(refusal(&error)));
            }
            Ok(status) if status.gh.is_none() => {
                self.pull_requests
                    .setups
                    .insert(project, failed("Not found".into()));
            }
            Ok(status) if status.signed_in => {
                self.pull_requests.setups.remove(&project);
            }
            Ok(status) => {
                self.run_setup_step(project, SetupStep::SignIn, status.sign_in, window, cx);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn setup_of(&self, project: ProjectId) -> Option<&GithubSetup> {
        self.pull_requests.setups.get(&project)
    }
}

#[cfg(test)]
mod live_tests;

#[cfg(test)]
mod tests {
    use swarm_protocol::{GithubCheck, LocalCheck};

    use super::*;

    fn run(checks: &[CheckState], required: bool) -> ChecksRun {
        ChecksRun {
            task: DaemonTaskId(7),
            commit: "abc".into(),
            clean: true,
            required,
            checks: checks
                .iter()
                .map(|state| LocalCheck {
                    command: "cargo test".into(),
                    state: *state,
                    terminal: None,
                    exit: None,
                    duration_ms: None,
                })
                .collect(),
            pre_push_hook: None,
        }
    }

    fn inputs(run: Option<&ChecksRun>) -> CreateInputs<'_> {
        CreateInputs {
            creating: false,
            approved: true,
            pending_comments: 0,
            github_ready: true,
            title_empty: false,
            no_checks: false,
            required: true,
            run,
            head: Some("abc"),
        }
    }

    fn without_checks(run: Option<&ChecksRun>) -> CreateInputs<'_> {
        CreateInputs {
            no_checks: true,
            ..inputs(run)
        }
    }

    fn optional(run: Option<&ChecksRun>) -> CreateInputs<'_> {
        CreateInputs {
            required: false,
            ..inputs(run)
        }
    }

    fn state_of(run: &ChecksRun, change: impl FnOnce(&mut CreateInputs<'_>)) -> CreateState {
        let mut input = inputs(Some(run));
        change(&mut input);

        create_state(&input)
    }

    #[test]
    fn passed_checks_make_the_button_ready() {
        let passed = run(&[CheckState::Passed, CheckState::Passed], true);

        assert_eq!(create_state(&inputs(Some(&passed))), CreateState::Ready);
    }

    #[test]
    fn a_run_without_checks_is_ready() {
        let none = run(&[], true);

        assert_eq!(create_state(&inputs(Some(&none))), CreateState::Ready);
    }

    #[test]
    fn a_project_without_configured_checks_never_waits_for_them() {
        let failed = run(&[CheckState::Failed], true);

        assert_eq!(create_state(&without_checks(None)), CreateState::Ready);
        assert_eq!(
            create_state(&without_checks(Some(&failed))),
            CreateState::Ready
        );
        assert_eq!(
            create_state(&CreateInputs {
                no_checks: true,
                approved: false,
                ..inputs(None)
            }),
            CreateState::Blocked(Blocker::NotApproved),
            "the other rules still hold"
        );
    }

    #[test]
    fn approval_comments_github_and_title_come_first() {
        let passed = run(&[CheckState::Passed], true);

        assert_eq!(
            state_of(&passed, |input| input.approved = false),
            CreateState::Blocked(Blocker::NotApproved)
        );
        assert_eq!(
            state_of(&passed, |input| input.pending_comments = 2),
            CreateState::Blocked(Blocker::CommentsPending)
        );
        assert_eq!(
            state_of(&passed, |input| input.github_ready = false),
            CreateState::Blocked(Blocker::GithubNotReady)
        );
        assert_eq!(
            state_of(&passed, |input| input.title_empty = true),
            CreateState::Blocked(Blocker::NoTitle)
        );
        assert_eq!(
            state_of(&passed, |input| input.creating = true),
            CreateState::Blocked(Blocker::Creating)
        );
    }

    #[test]
    fn a_run_with_a_failed_check_is_over_even_while_others_wait_or_skip() {
        let real_shape = run(
            &[CheckState::Passed, CheckState::Failed, CheckState::Skipped],
            true,
        );
        let skipped_last = run(&[CheckState::Failed, CheckState::Skipped], true);
        let stopped = run(&[CheckState::Passed, CheckState::Stopped], true);
        let passed = run(&[CheckState::Passed, CheckState::Passed], true);
        let running = run(&[CheckState::Passed, CheckState::Running], true);
        let waiting = run(&[CheckState::Running, CheckState::Waiting], true);

        assert!(run_is_over(&real_shape));
        assert!(run_is_over(&skipped_last));
        assert!(run_is_over(&stopped));
        assert!(run_is_over(&passed));
        assert!(!run_is_over(&running));
        assert!(!run_is_over(&waiting));
    }

    #[test]
    fn a_failed_run_with_skipped_checks_reads_as_failed_not_running() {
        let real_shape = run(
            &[CheckState::Passed, CheckState::Failed, CheckState::Skipped],
            true,
        );

        assert_eq!(
            run_problem(&real_shape, Some("abc")),
            Some(Blocker::ChecksFailed)
        );
        assert_eq!(
            create_state(&inputs(Some(&real_shape))),
            CreateState::Blocked(Blocker::ChecksFailed)
        );
    }

    #[test]
    fn missing_failed_stale_unclean_and_running_checks_block_a_required_run() {
        let failed = run(&[CheckState::Passed, CheckState::Failed], true);
        let stopped = run(&[CheckState::Stopped], true);
        let running = run(&[CheckState::Running], false);
        let passed = run(&[CheckState::Passed], true);
        let dirty = ChecksRun {
            clean: false,
            ..run(&[CheckState::Passed], true)
        };

        assert_eq!(
            create_state(&inputs(None)),
            CreateState::Blocked(Blocker::ChecksNotRun)
        );
        assert_eq!(
            create_state(&inputs(Some(&failed))),
            CreateState::Blocked(Blocker::ChecksFailed)
        );
        assert_eq!(
            create_state(&inputs(Some(&stopped))),
            CreateState::Blocked(Blocker::ChecksStopped)
        );
        assert_eq!(
            create_state(&inputs(Some(&running))),
            CreateState::Blocked(Blocker::ChecksRunning)
        );
        assert_eq!(
            state_of(&passed, |input| input.head = Some("def")),
            CreateState::Blocked(Blocker::ChecksStale)
        );
        assert_eq!(
            state_of(&passed, |input| input.head = None),
            CreateState::Blocked(Blocker::ChecksStale),
            "a head that is not loaded yet proves nothing"
        );
        assert_eq!(
            create_state(&inputs(Some(&dirty))),
            CreateState::Blocked(Blocker::ChecksUnclean)
        );
    }

    #[test]
    fn a_run_passes_only_when_current_clean_and_all_passed() {
        let passed = run(&[CheckState::Passed], true);
        let dirty = ChecksRun {
            clean: false,
            ..run(&[CheckState::Passed], true)
        };
        let failed = run(&[CheckState::Failed], true);

        assert_eq!(run_problem(&passed, Some("abc")), None);
        assert_eq!(
            run_problem(&passed, Some("def")),
            Some(Blocker::ChecksStale)
        );
        assert_eq!(
            run_problem(&dirty, Some("abc")),
            Some(Blocker::ChecksUnclean)
        );
        assert_eq!(
            run_problem(&failed, Some("def")),
            Some(Blocker::ChecksStale),
            "a failed run on an old commit only needs a new run"
        );
        assert!(run_is_current(&passed, Some("abc")));
        assert!(!run_is_current(&passed, Some("def")));
        assert!(!run_is_current(&passed, None));
    }

    #[test]
    fn checks_that_are_not_required_turn_the_button_into_create_anyway() {
        let failed = run(&[CheckState::Failed], false);
        let passed = run(&[CheckState::Passed], false);
        let running = run(&[CheckState::Running], false);
        let dirty = ChecksRun {
            clean: false,
            ..run(&[CheckState::Passed], false)
        };

        assert_eq!(
            create_state(&optional(Some(&failed))),
            CreateState::Anyway(Blocker::ChecksFailed)
        );
        assert_eq!(
            create_state(&optional(None)),
            CreateState::Anyway(Blocker::ChecksNotRun)
        );
        assert_eq!(
            create_state(&CreateInputs {
                head: Some("def"),
                ..optional(Some(&passed))
            }),
            CreateState::Anyway(Blocker::ChecksStale)
        );
        assert_eq!(
            create_state(&optional(Some(&dirty))),
            CreateState::Anyway(Blocker::ChecksUnclean)
        );
        assert_eq!(
            create_state(&optional(Some(&running))),
            CreateState::Blocked(Blocker::ChecksRunning),
            "a run that still runs is waited for"
        );
        assert_eq!(create_state(&optional(Some(&passed))), CreateState::Ready);
    }

    #[test]
    fn the_offer_follows_the_repository_and_gh() {
        let ready = crate::model::sample_github();
        let signed_out = GithubStatus {
            signed_in: false,
            ..ready.clone()
        };
        let missing = GithubStatus {
            gh: None,
            ..ready.clone()
        };
        let no_repository = GithubStatus {
            repository: None,
            ..ready.clone()
        };

        assert_eq!(offer(Some(&ready)), PrOffer::Create);
        assert_eq!(offer(Some(&signed_out)), PrOffer::SetUp);
        assert_eq!(offer(Some(&missing)), PrOffer::SetUp);
        assert_eq!(offer(Some(&no_repository)), PrOffer::None);
        assert_eq!(offer(None), PrOffer::None);
    }

    fn pull_request(checks: &[GithubCheckState], draft: bool, mergeable: bool) -> PullRequest {
        PullRequest {
            checks: checks
                .iter()
                .map(|state| GithubCheck {
                    name: "ci".into(),
                    state: *state,
                    duration_secs: None,
                    url: None,
                })
                .collect(),
            draft,
            mergeable,
            ..sample_pull_request()
        }
    }

    #[test]
    fn the_header_shows_the_state_of_the_pull_request() {
        let link = PrLink {
            number: 42,
            url: "https://github.com/a/b/pull/42".into(),
            head: "abc".into(),
        };
        let word = |checks: &[GithubCheckState], draft, mergeable| {
            toggle_label(&link, Some(&pull_request(checks, draft, mergeable)))
        };
        let merged = PullRequest {
            state: PrState::Merged,
            ..pull_request(&[], false, false)
        };
        let closed = PullRequest {
            state: PrState::Closed,
            ..pull_request(&[], false, false)
        };

        assert_eq!(toggle_label(&link, None), "#42");
        assert_eq!(
            word(
                &[GithubCheckState::Running, GithubCheckState::Passed],
                true,
                false
            ),
            "#42 \u{b7} checks running"
        );
        assert_eq!(
            word(&[GithubCheckState::Queued], false, false),
            "#42 \u{b7} checks running"
        );
        assert_eq!(
            word(&[GithubCheckState::Failed], false, false),
            "#42 \u{b7} checks failed"
        );
        assert_eq!(
            word(&[GithubCheckState::Passed], true, false),
            "#42 \u{b7} draft"
        );
        assert_eq!(
            word(&[GithubCheckState::Passed], false, true),
            "#42 \u{b7} ready to merge"
        );
        assert_eq!(
            word(&[GithubCheckState::Skipped], false, false),
            "#42 \u{b7} open"
        );
        assert_eq!(toggle_label(&link, Some(&merged)), "#42 \u{b7} merged");
        assert_eq!(toggle_label(&link, Some(&closed)), "#42 \u{b7} closed");
    }

    #[test]
    fn the_status_is_read_faster_while_checks_run_and_slowest_without_the_panel() {
        let intervals = StatusIntervals::default();
        let running = pull_request(&[GithubCheckState::Running], false, false);
        let queued = pull_request(&[GithubCheckState::Queued], false, false);
        let done = pull_request(&[GithubCheckState::Passed], false, false);

        assert_eq!(
            status_interval(Some(&running), true, intervals),
            STATUS_FAST
        );
        assert_eq!(status_interval(Some(&queued), true, intervals), STATUS_FAST);
        assert_eq!(status_interval(Some(&done), true, intervals), STATUS_SLOW);
        assert_eq!(status_interval(None, true, intervals), STATUS_SLOW);
        assert_eq!(
            status_interval(Some(&running), false, intervals),
            STATUS_HIDDEN
        );
        assert_eq!(status_interval(None, false, intervals), STATUS_HIDDEN);
    }

    #[test]
    fn a_merged_or_closed_pull_request_is_finished() {
        let with_state = |state| PullRequest {
            state,
            ..pull_request(&[], false, false)
        };

        assert!(is_finished(Some(&with_state(PrState::Merged))));
        assert!(is_finished(Some(&with_state(PrState::Closed))));
        assert!(!is_finished(Some(&with_state(PrState::Open))));
        assert!(!is_finished(None));
    }

    #[test]
    fn logins_are_split_cleaned_and_kept_once() {
        assert_eq!(
            parse_logins("@mara, jonas ,,MARA\nlee"),
            ["mara", "jonas", "lee"]
        );
        assert!(parse_logins(" , ").is_empty());
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(format_duration(412), "412ms");
        assert_eq!(format_duration(38_000), "38s");
        assert_eq!(format_duration(130_000), "2m 10s");
        assert_eq!(format_duration(3_720_000), "1h 02m");
    }
}
