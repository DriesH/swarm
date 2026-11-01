//! The system check: the tools Swarm needs and does not ship, and a button for each one
//! that is not ready. The welcome screen and Settings › General both show it.
//!
//! The daemon checks the tools, as it runs them for the agents. The app reads the
//! notification authorization itself. A fix runs in a user terminal under its row, and the
//! row asks the check again when that terminal ends.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, Role,
    SharedString, Size, Subscription, WeakEntity, Window, div, px,
};
use swarm_protocol::{
    Client, ClientError, CreateTerminal, SystemCheck as Report, TerminalInfo,
    TerminalKind as DaemonKind, ToolCheck, ToolState,
};
use swarm_term::{TerminalChannels, TerminalInput};
use swarm_term_view::{TerminalEvent, TerminalView, palette_for};
use swarm_ui::ActiveTheme as _;
use swarm_ui::prelude::*;

use crate::daemon::exit_text;
use crate::notifications::{self, Permission};
use crate::workspace::{DataSource, Workspace};

#[cfg(test)]
pub(crate) mod harness;
#[cfg(test)]
mod tests;

const SEPARATOR: &str = " \u{b7} ";
const NOT_CONNECTED: &str = "Not connected to the daemon";
/// The `task` key of every fix terminal. The daemon keeps user terminals when the app goes
/// away, and the next run finds the ones left over by this key.
const FIX_TASK: &str = "system-check";
/// Prints the command it is given as `$1`, then runs the rest. Nothing of the command is
/// part of this script, so no argument can change what runs.
const ANNOUNCE: &str = r#"printf '$ %s\n' "$1"; shift; exec "$@""#;
/// The room a fix terminal gets under its row: the daemon starts it at this size, and the
/// view fits itself to the row once it is drawn.
const FIX_TERMINAL: Size<gpui::Pixels> = Size {
    width: px(440.),
    height: px(120.),
};

/// One line of the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Tool {
    Git,
    ClaudeCode,
    Codex,
    GithubCli,
    GithubAccount,
    Notifications,
}

impl Tool {
    pub(crate) const ALL: [Self; 6] = [
        Self::Git,
        Self::ClaudeCode,
        Self::Codex,
        Self::GithubCli,
        Self::GithubAccount,
        Self::Notifications,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::GithubCli => "GitHub CLI",
            Self::GithubAccount => "GitHub account",
            Self::Notifications => "Notifications",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::GithubCli => "github-cli",
            Self::GithubAccount => "github-account",
            Self::Notifications => "notifications",
        }
    }

    /// Signed in is part of what the check says about these.
    fn is_agent(self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Codex)
    }
}

/// What a row's button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionKind {
    /// Runs the fix in a user terminal.
    Install,
    /// Runs the fix in a user terminal.
    SignIn,
    /// Shows the system's question about notifications.
    Allow,
    OpenSystemSettings,
    /// The check itself failed: asks again.
    CheckAgain,
}

impl ActionKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::SignIn => "Sign in",
            Self::Allow => "Allow",
            Self::OpenSystemSettings => "Open System Settings",
            Self::CheckAgain => "Check again",
        }
    }

    /// The word in the name of the fix terminal.
    fn verb(self) -> &'static str {
        match self {
            Self::SignIn => "sign-in",
            _ => "install",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Action {
    pub kind: ActionKind,
    /// The command a fix runs. Empty for the kinds that run nothing.
    pub fix: Vec<String>,
    /// The button waits on another row: it is dimmed and does nothing.
    pub dimmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub tool: Tool,
    pub state: SystemCheckState,
    pub detail: String,
    pub action: Option<Action>,
}

/// What a row is busy with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Busy {
    /// A fix runs in a terminal.
    Fixing(ActionKind, Vec<String>),
    /// The check runs again after a fix.
    Checking,
}

pub(crate) type Answer = Result<Report, SharedString>;

/// "installing with Homebrew": what the row says while its fix runs. The daemon names
/// programs by their full path.
pub(crate) fn running_detail(kind: ActionKind, fix: &[String]) -> String {
    let brew = fix
        .first()
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == "brew");

    match kind {
        ActionKind::Install if brew => "installing with Homebrew".to_owned(),
        ActionKind::Install => "installing".to_owned(),
        ActionKind::SignIn => "signing in".to_owned(),
        _ => "working".to_owned(),
    }
}

/// What VoiceOver reads on a row's button: the visible label first, then the tool.
pub(crate) fn action_label(tool: Tool, kind: ActionKind) -> String {
    let name = tool.name();

    match kind {
        ActionKind::Install => format!("Install {name}"),
        ActionKind::SignIn if tool == Tool::GithubAccount => "Sign in to GitHub".to_owned(),
        ActionKind::SignIn => format!("Sign in to {name}"),
        ActionKind::Allow => "Allow notifications".to_owned(),
        ActionKind::OpenSystemSettings => "Open System Settings for notifications".to_owned(),
        ActionKind::CheckAgain => format!("Check again for {name}"),
    }
}

/// A word as a shell reads it: quoted when it has anything but plain characters.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character));

    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The command as the user reads it, for example `/bin/sh -c 'curl -fsSL … | bash'`.
pub(crate) fn command_text(fix: &[String]) -> String {
    fix.iter()
        .map(|part| shell_word(part))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the fix terminal runs: a shell that prints the command first, then runs it. A quiet
/// command such as `curl -s` would show nothing otherwise.
pub(crate) fn terminal_command(fix: &[String]) -> Vec<String> {
    let mut command: Vec<String> = ["/bin/sh", "-c", ANNOUNCE, "sh"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    command.push(command_text(fix));
    command.extend_from_slice(fix);

    command
}

fn ready_detail(tool: Tool, check: &ToolCheck) -> String {
    let version = check.version.as_deref();

    if tool.is_agent() {
        return match version {
            Some(version) => format!("{version}{SEPARATOR}signed in"),
            None => "signed in".to_owned(),
        };
    }

    match tool {
        Tool::GithubAccount => "signed in".to_owned(),
        _ => version.map_or_else(|| "installed".to_owned(), str::to_owned),
    }
}

fn not_ready_detail(tool: Tool, check: &ToolCheck) -> String {
    match check.state {
        ToolState::Missing if tool == Tool::GithubAccount => "not signed in".to_owned(),
        ToolState::Missing => "not installed".to_owned(),
        ToolState::SignedOut => match check.version.as_deref() {
            Some(version) => format!("{version}{SEPARATOR}not signed in"),
            None => "not signed in".to_owned(),
        },
        ToolState::Failed => check
            .detail
            .clone()
            .unwrap_or_else(|| "check failed".to_owned()),
        ToolState::Ready => ready_detail(tool, check),
    }
}

/// The row of a tool the daemon checks. `cli` is the GitHub CLI, which the account waits for.
pub(crate) fn tool_row(tool: Tool, check: &ToolCheck, cli: &ToolCheck) -> Row {
    let waits = tool == Tool::GithubAccount
        && cli.state == ToolState::Missing
        && check.state != ToolState::Ready;

    if waits {
        return Row {
            tool,
            state: SystemCheckState::NotSetUp,
            detail: "needs GitHub CLI".to_owned(),
            action: Some(Action {
                kind: ActionKind::SignIn,
                fix: check.fix.clone(),
                dimmed: true,
            }),
        };
    }

    let (state, action) = match check.state {
        ToolState::Ready => (SystemCheckState::Ready, None),
        ToolState::Missing | ToolState::SignedOut => {
            let kind = if tool == Tool::GithubAccount || check.state == ToolState::SignedOut {
                ActionKind::SignIn
            } else {
                ActionKind::Install
            };

            // Nothing can run without a command: no button.
            let action = (!check.fix.is_empty()).then(|| Action {
                kind,
                fix: check.fix.clone(),
                dimmed: false,
            });

            (SystemCheckState::NotSetUp, action)
        }
        ToolState::Failed => (
            SystemCheckState::NotSetUp,
            Some(Action {
                kind: ActionKind::CheckAgain,
                fix: Vec::new(),
                dimmed: false,
            }),
        ),
    };

    Row {
        tool,
        state,
        detail: if check.state == ToolState::Ready {
            ready_detail(tool, check)
        } else {
            not_ready_detail(tool, check)
        },
        action,
    }
}

/// The row of the notifications, from what macOS last said. `None`: it did not answer yet.
pub(crate) fn notifications_row(permission: Option<Permission>) -> Row {
    let (state, detail, kind) = match permission {
        None => (SystemCheckState::InProgress, "checking", None),
        Some(Permission::Allowed) => (SystemCheckState::Ready, "allowed", None),
        Some(Permission::NotAsked) => (
            SystemCheckState::NotSetUp,
            "not allowed yet",
            Some(ActionKind::Allow),
        ),
        Some(Permission::NotAllowed) => (
            SystemCheckState::NotSetUp,
            "turned off in System Settings",
            Some(ActionKind::OpenSystemSettings),
        ),
        Some(Permission::Unavailable) => (SystemCheckState::NotSetUp, "unavailable", None),
    };

    Row {
        tool: Tool::Notifications,
        state,
        detail: detail.to_owned(),
        action: kind.map(|kind| Action {
            kind,
            fix: Vec::new(),
            dimmed: false,
        }),
    }
}

fn busy_row(tool: Tool, state: SystemCheckState, detail: &str) -> Row {
    Row {
        tool,
        state,
        detail: detail.to_owned(),
        action: None,
    }
}

/// All rows, in order. A tool that is busy shows that instead of its state.
pub(crate) fn rows(
    answer: Option<&Answer>,
    permission: Option<Permission>,
    busy: &HashMap<Tool, Busy>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = match answer {
        None => Tool::ALL
            .into_iter()
            .filter(|tool| *tool != Tool::Notifications)
            .map(|tool| busy_row(tool, SystemCheckState::InProgress, "checking"))
            .collect(),
        Some(Err(message)) => Tool::ALL
            .into_iter()
            .filter(|tool| *tool != Tool::Notifications)
            .map(|tool| Row {
                action: Some(Action {
                    kind: ActionKind::CheckAgain,
                    fix: Vec::new(),
                    dimmed: false,
                }),
                ..busy_row(tool, SystemCheckState::NotSetUp, message)
            })
            .collect(),
        Some(Ok(report)) => vec![
            tool_row(Tool::Git, &report.git, &report.gh),
            tool_row(Tool::ClaudeCode, &report.claude, &report.gh),
            tool_row(Tool::Codex, &report.codex, &report.gh),
            tool_row(Tool::GithubCli, &report.gh, &report.gh),
            tool_row(Tool::GithubAccount, &report.github_account, &report.gh),
        ],
    };
    rows.push(notifications_row(permission));

    for row in &mut rows {
        match busy.get(&row.tool) {
            Some(Busy::Fixing(kind, fix)) => {
                row.state = SystemCheckState::InProgress;
                row.detail = running_detail(*kind, fix);
                row.action = None;
            }
            Some(Busy::Checking) => {
                row.state = SystemCheckState::InProgress;
                "checking".clone_into(&mut row.detail);
                row.action = None;
            }
            None => {}
        }
    }

    rows
}

pub(crate) fn ready_count(rows: &[Row]) -> usize {
    rows.iter()
        .filter(|row| row.state == SystemCheckState::Ready)
        .count()
}

/// The first row with a button that works.
pub(crate) fn first_open_action(rows: &[Row]) -> Option<Tool> {
    rows.iter()
        .find(|row| row.action.as_ref().is_some_and(|action| !action.dimmed))
        .map(|row| row.tool)
}

fn check(state: ToolState, version: Option<&str>, fix: &[&str]) -> ToolCheck {
    ToolCheck {
        state,
        version: version.map(str::to_owned),
        detail: None,
        fix: fix.iter().map(|part| (*part).to_owned()).collect(),
    }
}

/// The fix of the sample's GitHub CLI row.
pub(crate) const SAMPLE_INSTALL: [&str; 3] = ["/opt/homebrew/bin/brew", "install", "gh"];

/// What the rows show with no daemon behind them: the design's sample.
fn sample_rows() -> Vec<Row> {
    let report = Report {
        git: check(ToolState::Ready, None, &[]),
        claude: check(ToolState::Ready, Some("2.1.314"), &[]),
        codex: check(ToolState::Ready, Some("0.154.0"), &[]),
        gh: check(ToolState::Missing, None, &SAMPLE_INSTALL),
        github_account: check(
            ToolState::Missing,
            None,
            &["/opt/homebrew/bin/gh", "auth", "login", "--web"],
        ),
    };
    let installing = Busy::Fixing(
        ActionKind::Install,
        SAMPLE_INSTALL.map(str::to_owned).to_vec(),
    );

    rows(
        Some(&Ok(report)),
        Some(Permission::NotAsked),
        &HashMap::from([(Tool::GithubCli, installing)]),
    )
}

/// A fix in its terminal. Dropping it stops the program, as closing a tile would: nobody
/// could see or stop it afterwards.
struct FixTerminal {
    view: Entity<TerminalView>,
    input: flume::Sender<TerminalInput>,
    /// What the row says once the program is over, for example "exit 1".
    ended: Option<String>,
    stopped: bool,
    _subscription: Subscription,
}

impl FixTerminal {
    fn stop(&mut self) {
        if self.ended.is_some() || self.stopped {
            return;
        }

        self.stopped = true;
        kill(&self.input);
    }
}

impl Drop for FixTerminal {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Fix {
    kind: ActionKind,
    /// The program and its arguments, as the daemon found them.
    command: Vec<String>,
    /// `None` while the daemon makes the terminal.
    terminal: Option<FixTerminal>,
}

impl Fix {
    /// How the program ended, once it is over. The terminal stays under the row until a
    /// check says the tool is ready.
    fn ended(&self) -> Option<&str> {
        self.terminal
            .as_ref()
            .and_then(|terminal| terminal.ended.as_deref())
    }
}

fn kill(input: &flume::Sender<TerminalInput>) {
    if input.send(TerminalInput::Kill).is_err() {
        log::debug!("the fix terminal is gone already");
    }
}

/// Stops the fix terminals that an earlier run of the app left in the daemon: nothing shows
/// them, and the daemon keeps user terminals when the app goes away.
pub(crate) fn stop_stale_fixes(client: &Client, terminals: &[TerminalInfo]) {
    let stale = terminals.iter().filter(|info| {
        info.kind == DaemonKind::User
            && info.task.as_deref() == Some(FIX_TASK)
            && info.exit.is_none()
    });

    for info in stale {
        match client.attach(info.id, 0) {
            Ok(channels) => kill(&channels.input),
            Err(error) => log::warn!("cannot stop the fix terminal {}: {error}", info.id.0),
        }
    }
}

/// Where the rows come from.
enum Backend {
    /// Sample data, or no workspace: the design's rows, and the buttons do nothing.
    Sample,
    Daemon(Option<Client>),
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags, not one state"
)]
pub(crate) struct SystemCheckView {
    workspace: Option<WeakEntity<Workspace>>,
    sample: bool,
    answer: Option<Answer>,
    permission: Option<Permission>,
    /// Goes up with every check asked for, so an old answer never replaces a newer one.
    revision: u64,
    checking: bool,
    /// The check found no daemon, and asks again when the workspace connects to one.
    waiting_for_daemon: bool,
    /// Tools whose fix just ended, until the check answers.
    rechecking: Vec<Tool>,
    fixes: HashMap<Tool, Fix>,
    errors: HashMap<Tool, SharedString>,
    action_focus: HashMap<Tool, FocusHandle>,
    /// Focus moves to the first open action once the check has answered.
    pending_focus: bool,
    /// Takes the focus when no action is open.
    fallback_focus: Option<FocusHandle>,
    sample_output: Option<Entity<TerminalView>>,
    check_task: Option<gpui::Task<()>>,
    permission_task: Option<gpui::Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SystemCheckView {
    /// `workspace` holds the daemon connection. With `focus_first`, the first action that
    /// is not ready takes the keyboard focus once the check has answered.
    pub(crate) fn new(
        workspace: Option<WeakEntity<Workspace>>,
        focus_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            // The user may come back from System Settings with a new answer.
            if window.is_window_active() && !this.sample {
                this.read_permission(cx);
            }
        });
        let connection = workspace
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|workspace| cx.observe(&workspace, |this, _, cx| this.daemon_may_be_back(cx)));

        // Dropping a fix on quit does not run: the daemon would keep its program.
        let quit = cx.on_app_quit(|this, _| {
            this.stop_fixes();

            std::future::ready(())
        });

        Self {
            workspace,
            sample: false,
            answer: None,
            permission: None,
            revision: 0,
            checking: false,
            waiting_for_daemon: false,
            rechecking: Vec::new(),
            fixes: HashMap::new(),
            errors: HashMap::new(),
            action_focus: Tool::ALL
                .into_iter()
                .map(|tool| (tool, cx.focus_handle().tab_stop(true)))
                .collect(),
            pending_focus: focus_first,
            fallback_focus: None,
            sample_output: None,
            check_task: None,
            permission_task: None,
            _subscriptions: [Some(activation), connection, Some(quit)]
                .into_iter()
                .flatten()
                .collect(),
        }
    }

    pub(crate) fn set_fallback_focus(&mut self, handle: FocusHandle) {
        self.fallback_focus = Some(handle);
    }

    fn backend(&self, cx: &App) -> Backend {
        let Some(workspace) = self.workspace.as_ref().and_then(WeakEntity::upgrade) else {
            return Backend::Sample;
        };

        let workspace = workspace.read(cx);

        if workspace.source == DataSource::Sample {
            return Backend::Sample;
        }

        Backend::Daemon(workspace.daemon.clone().filter(Client::is_connected))
    }

    /// Reads the workspace, so never call it while the workspace is being updated.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = match self.backend(cx) {
            Backend::Sample => {
                self.sample = true;
                cx.notify();

                return;
            }
            Backend::Daemon(client) => client,
        };
        self.sample = false;
        self.read_permission(cx);

        let Some(client) = client else {
            self.answer = Some(Err(NOT_CONNECTED.into()));
            self.checking = false;
            self.waiting_for_daemon = true;
            self.rechecking.clear();
            cx.notify();

            return;
        };

        self.waiting_for_daemon = false;
        self.revision += 1;
        self.checking = true;
        let revision = self.revision;

        let asking = cx.background_spawn(async move { client.system_check() });
        self.check_task = Some(cx.spawn(async move |this, cx| {
            let answer = asking.await;

            if let Err(error) = this.update(cx, |this, cx| this.checked(revision, answer, cx)) {
                log::debug!("the system check closed before the daemon answered: {error}");
            }
        }));

        cx.notify();
    }

    /// The workspace changed: asks again when it has a daemon now and the check had none.
    fn daemon_may_be_back(&mut self, cx: &mut Context<Self>) {
        if self.waiting_for_daemon && matches!(self.backend(cx), Backend::Daemon(Some(_))) {
            self.refresh(cx);
        }
    }

    fn checked(
        &mut self,
        revision: u64,
        answer: Result<Report, ClientError>,
        cx: &mut Context<Self>,
    ) {
        if revision != self.revision {
            return;
        }

        self.answer = Some(answer.map_err(|error| SharedString::from(error.to_string())));
        self.checking = false;
        self.rechecking.clear();
        self.fold_ready_fixes();
        cx.notify();
    }

    /// A fix that is over folds away once a check says its tool is ready.
    fn fold_ready_fixes(&mut self) {
        let ready: Vec<Tool> = rows(self.answer.as_ref(), self.permission, &HashMap::new())
            .into_iter()
            .filter(|row| row.state == SystemCheckState::Ready)
            .map(|row| row.tool)
            .collect();

        self.fixes
            .retain(|tool, fix| fix.ended().is_none() || !ready.contains(tool));
    }

    fn stop_fixes(&mut self) {
        for fix in self.fixes.values_mut() {
            if let Some(terminal) = fix.terminal.as_mut() {
                terminal.stop();
            }
        }
    }

    fn read_permission(&mut self, cx: &mut Context<Self>) {
        let answers = notifications::check_permission(cx);

        self.wait_for_permission(answers, cx);
    }

    fn ask_for_permission(&mut self, cx: &mut Context<Self>) {
        let answers = notifications::request_permission(cx);

        self.wait_for_permission(answers, cx);
    }

    /// macOS answers on its own thread, so the answer comes through a channel.
    fn wait_for_permission(
        &mut self,
        answers: flume::Receiver<notifications::Authorization>,
        cx: &mut Context<Self>,
    ) {
        self.permission_task = Some(cx.spawn(async move |this, cx| {
            let Ok(authorization) = answers.recv_async().await else {
                return;
            };

            cx.update(|cx| notifications::remember(authorization, cx));

            let shown = this.update(cx, |this, cx| {
                this.permission = Some(authorization.permission);
                cx.notify();
            });

            if let Err(error) = shown {
                log::debug!("the system check closed before the permission came: {error}");
            }
        }));
    }

    fn busy(&self) -> HashMap<Tool, Busy> {
        let mut busy: HashMap<Tool, Busy> = self
            .rechecking
            .iter()
            .map(|tool| (*tool, Busy::Checking))
            .collect();

        busy.extend(
            self.fixes
                .iter()
                .filter(|(_, fix)| fix.ended().is_none())
                .map(|(tool, fix)| (*tool, Busy::Fixing(fix.kind, fix.command.clone()))),
        );

        busy
    }

    pub(crate) fn rows(&self) -> Vec<Row> {
        if self.sample {
            return sample_rows();
        }

        let busy = self.busy();
        let mut rows = rows(self.answer.as_ref(), self.permission, &busy);

        for row in &mut rows {
            if let Some(error) = self.errors.get(&row.tool) {
                row.detail = error.to_string();
            }

            let ended = self.fixes.get(&row.tool).and_then(Fix::ended);
            if let Some(how) = ended
                && !busy.contains_key(&row.tool)
            {
                row.detail = format!("{}{SEPARATOR}{how}", row.detail);
            }
        }

        rows
    }

    /// How many rows are ready, and how many there are.
    #[cfg(test)]
    pub(crate) fn count(&self) -> (usize, usize) {
        let rows = self.rows();

        (ready_count(&rows), rows.len())
    }

    /// True when every row shows a state and not "checking".
    pub(crate) fn is_settled(&self) -> bool {
        self.sample
            || (self.answer.is_some()
                && !self.checking
                && self.permission.is_some()
                && self.fixes.values().all(|fix| fix.terminal.is_some()))
    }

    /// The focus of a tool's button.
    #[cfg(test)]
    pub(crate) fn action_focus(&self, tool: Tool) -> Option<FocusHandle> {
        self.action_focus.get(&tool).cloned()
    }

    /// The focus of the first action that works. The welcome screen starts there.
    pub(crate) fn first_action_focus(&self) -> Option<FocusHandle> {
        first_open_action(&self.rows()).and_then(|tool| self.action_focus.get(&tool).cloned())
    }

    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus || !self.is_settled() {
            return;
        }

        self.pending_focus = false;

        let handle = self
            .first_action_focus()
            .or_else(|| self.fallback_focus.clone());
        if let Some(handle) = handle {
            window.focus(&handle, cx);
        }
    }

    fn act(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if self.sample {
            return;
        }

        let action = self
            .rows()
            .into_iter()
            .find(|row| row.tool == tool)
            .and_then(|row| row.action);
        let Some(action) = action.filter(|action| !action.dimmed) else {
            return;
        };

        match action.kind {
            ActionKind::Allow => self.ask_for_permission(cx),
            ActionKind::OpenSystemSettings => notifications::open_system_settings(cx),
            ActionKind::CheckAgain => self.refresh(cx),
            ActionKind::Install | ActionKind::SignIn => self.start_fix(tool, action, window, cx),
        }
    }

    fn start_fix(
        &mut self,
        tool: Tool,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A fix that is over stays under its row, and a new click replaces it.
        if self
            .fixes
            .get(&tool)
            .is_some_and(|fix| fix.ended().is_none())
        {
            return;
        }

        let Backend::Daemon(Some(client)) = self.backend(cx) else {
            self.errors.insert(tool, NOT_CONNECTED.into());
            cx.notify();

            return;
        };

        self.errors.remove(&tool);
        self.fixes.insert(
            tool,
            Fix {
                kind: action.kind,
                command: action.fix.clone(),
                terminal: None,
            },
        );

        let request = CreateTerminal {
            kind: DaemonKind::User,
            command: Some(terminal_command(&action.fix)),
            cwd: std::env::var_os("HOME").map(PathBuf::from),
            size: TerminalView::size_for(
                FIX_TERMINAL,
                crate::settings::terminal_font_size(cx),
                window,
                cx,
            ),
            palette: palette_for(cx.theme()),
            task: Some(FIX_TASK.to_owned()),
        };
        let creating = cx.background_spawn(async move { client.create(request) });

        cx.spawn_in(window, async move |this, cx| {
            let created = Rc::new(Cell::new(Some(creating.await)));

            let updated = this.update_in(cx, {
                let created = Rc::clone(&created);

                move |this, window, cx| match created.take() {
                    Some(Ok((_, channels))) => this.fix_started(tool, channels, window, cx),
                    Some(Err(error)) => this.fix_failed(tool, &error, cx),
                    None => {}
                }
            });

            if let Err(error) = updated {
                log::debug!("the system check closed before the fix started: {error}");

                // The daemon made the terminal, and nothing will show it.
                if let Some(Ok((_, channels))) = created.take() {
                    kill(&channels.input);
                }
            }
        })
        .detach();

        cx.notify();
    }

    fn fix_failed(&mut self, tool: Tool, error: &ClientError, cx: &mut Context<Self>) {
        log::error!("cannot start the fix of {}: {error}", tool.name());

        self.fixes.remove(&tool);
        self.errors.insert(tool, error.to_string().into());
        cx.notify();
    }

    fn fix_started(
        &mut self,
        tool: Tool,
        channels: TerminalChannels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = channels.input.clone();

        let Some(fix) = self.fixes.get(&tool) else {
            // The row stopped waiting for it.
            kill(&input);

            return;
        };

        let name = format!("{} {} terminal", tool.name(), fix.kind.verb());

        let view = cx.new(|cx| TerminalView::new(channels, name.into(), window, cx));
        let points = crate::settings::terminal_font_size(cx);
        view.update(cx, |view, cx| view.set_font_size(points, window, cx));

        let subscription = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.on_terminal_event(tool, event, window, cx);
        });

        // The user types in it: a password, a sign-in.
        window.focus(&view.focus_handle(cx), cx);

        if let Some(fix) = self.fixes.get_mut(&tool) {
            fix.terminal = Some(FixTerminal {
                view,
                input,
                ended: None,
                stopped: false,
                _subscription: subscription,
            });
        }

        cx.notify();
    }

    fn on_terminal_event(
        &mut self,
        tool: Tool,
        event: &TerminalEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::Exited(status) => self.fix_ended(tool, exit_text(status), cx),
            TerminalEvent::Disconnected => self.fix_ended(tool, "disconnected".to_owned(), cx),
            _ => {}
        }
    }

    /// The row asks the check again. The terminal stays under it, with how it ended, until a
    /// check says the tool is ready.
    fn fix_ended(&mut self, tool: Tool, how: String, cx: &mut Context<Self>) {
        let terminal = self
            .fixes
            .get_mut(&tool)
            .and_then(|fix| fix.terminal.as_mut());

        let Some(terminal) = terminal else {
            return;
        };

        if terminal.ended.is_some() {
            return;
        }

        terminal.ended = Some(how);

        self.rechecking.push(tool);
        self.pending_focus = true;
        self.refresh(cx);
    }

    /// A terminal that shows `channels` under the GitHub CLI row, for the sample.
    #[cfg(feature = "snapshot")]
    pub(crate) fn show_sample_output(
        &mut self,
        channels: TerminalChannels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sample = true;
        self.sample_output = Some(cx.new(|cx| {
            TerminalView::new(channels, "GitHub CLI install terminal".into(), window, cx)
        }));

        cx.notify();
    }

    fn output_of(&self, tool: Tool) -> Option<Entity<TerminalView>> {
        if self.sample {
            return (tool == Tool::GithubCli)
                .then(|| self.sample_output.clone())
                .flatten();
        }

        self.fixes
            .get(&tool)
            .and_then(|fix| fix.terminal.as_ref())
            .map(|terminal| terminal.view.clone())
    }

    fn action_button(
        &self,
        index: usize,
        row: &Row,
        action: &Action,
        cx: &mut Context<Self>,
    ) -> Button {
        let tool = row.tool;
        let spoken = action_label(tool, action.kind);

        let spoken = if action.dimmed {
            format!("{spoken}, {}", row.detail)
        } else {
            spoken
        };

        let button = Button::new(("system-check-action", index), action.kind.label())
            .accessible_label(spoken)
            .debug_selector(move || format!("system-check-action-{}", tool.id()));

        let button = if action.fix.is_empty() {
            button
        } else {
            button.hint(command_text(&action.fix))
        };

        if action.dimmed {
            return button.disabled(true);
        }

        let button = match self.action_focus.get(&tool) {
            Some(handle) => button.track_focus(handle),
            None => button,
        };

        button.on_click(cx.listener(move |this, _, window, cx| this.act(tool, window, cx)))
    }

    fn row_element(&self, index: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let tool = row.tool;
        let element = SystemCheckRow::new(("system-check-row", index), tool.name(), row.state)
            .detail(row.detail.clone())
            .debug_selector(move || format!("system-check-row-{}", tool.id()))
            .output_height(px(120.));

        let element = match &row.action {
            Some(action) => element.action(self.action_button(index, row, action, cx)),
            None => element,
        };

        match self.output_of(tool) {
            Some(view) => element
                .output(div().size_full().child(view))
                .into_any_element(),
            None => element.into_any_element(),
        }
    }
}

/// The card around the rows. Its rows are list items.
fn card() -> Panel {
    Panel::new()
        .accessible("system-check-card", "System check")
        .list()
}

impl Render for SystemCheckView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_pending_focus(window, cx);

        let rows = self.rows();
        let (ready, total) = (ready_count(&rows), rows.len());

        let header = h_flex()
            .id("system-check-header")
            .role(Role::Heading)
            .aria_level(2)
            .aria_label(format!("System check, {ready} of {total} ready"))
            .px(px(16.))
            .py(px(12.))
            .child(
                Label::new("System check")
                    .size(LabelSize::Small)
                    .weight(gpui::FontWeight::MEDIUM)
                    .color(Color::Faint),
            )
            .child(div().flex_1())
            .child(
                Label::new(format!("{ready} / {total}"))
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint),
            );

        let panel = rows
            .iter()
            .enumerate()
            .fold(card().row(header), |panel, (index, row)| {
                panel.row(self.row_element(index, row, cx))
            });

        div()
            .id("system-check")
            .debug_selector(|| "system-check".to_owned())
            .child(panel)
    }
}
