//! The settings window: a nav of sections and projects, and one page at a time.

mod general_page;
mod keyboard_page;
mod pages;
mod project;
mod prompts_page;
mod terminals_page;
mod usage_page;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Axis, Bounds, Context, ElementId, Entity, FocusHandle, Global, KeyBinding, KeyContext,
    KeyDownEvent, Pixels, Role, ScrollHandle, SharedString, Size, Subscription, TitlebarOptions,
    WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, actions, div, point, px, size,
};
use swarm_settings::{
    AccentSetting, AppearanceSettings, CliSettings, NotificationSettings, ProjectMode,
    PullRequestSettings, Settings, ThemeSetting, UpdateSettings,
};
use swarm_ui::a11y::ignore_press;
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;
use swarm_ui::reveal::{RevealFocus, reveal_focused};
use swarm_ui::roving::RovingFocus;
use swarm_ui::styles::focus_ring;
use swarm_ui::theme;
use swarm_ui::{ROOT_CONTEXT, SelectNext, SelectPrevious, TextFieldSize, scroll_for_key};

use self::prompts_page::PromptsPage;
use self::usage_page::UsagePage;
use crate::model::{FakeData, Project, ProjectId};
use crate::notifications::{self, Permission};
use crate::remove_project::RemoveProjectDialog;
use crate::system_check::SystemCheckView;
use crate::window_drag::drag_region;
use crate::workspace::Workspace;
pub(crate) use pages::tilde;
use project::{ProjectInputs, ProjectSave, ProjectSettings};

actions!(swarm_app, [CloseSettings]);

const SETTINGS_CONTEXT: &str = "Settings";
const NAV_CONTEXT: &str = "SettingsNav";

/// Edits are saved once typing pauses this long.
const SAVE_DELAY: Duration = Duration::from_millis(300);

const NOT_CONNECTED: &str = "Not connected to the daemon";
const MAX_AGENTS_PER_TASK_ERROR: &str = "Max agents per task: 1 or more";
const MAX_AGENTS_TOTAL_ERROR: &str = "Max agents in total: 1 or more";

/// How far an arrow key scrolls a page without controls.
const PAGE_LINE: Pixels = px(40.);

const WINDOW_SIZE: Size<Pixels> = size(px(1280.), px(900.));
const MIN_WINDOW_SIZE: Size<Pixels> = size(px(900.), px(600.));

pub(crate) struct AgentInfo {
    pub name: &'static str,
    pub icon: IconName,
}

pub(crate) const AGENTS: [AgentInfo; 2] = [
    AgentInfo {
        name: "Claude Code",
        icon: IconName::ClaudeCode,
    },
    AgentInfo {
        name: "Codex",
        icon: IconName::Codex,
    },
];

pub(crate) const IDLE_OPTIONS: [&str; 4] = ["10 minutes", "30 minutes", "1 hour", "Never"];

/// The minutes of each option in [`IDLE_OPTIONS`]. `0` is Never.
const IDLE_MINUTES: [u32; 4] = [10, 30, 60, 0];

/// The position in [`IDLE_OPTIONS`] of the option nearest to `minutes`.
pub(crate) fn idle_index(minutes: u32) -> usize {
    if minutes == 0 {
        return IDLE_MINUTES.len() - 1;
    }

    IDLE_MINUTES[..IDLE_MINUTES.len() - 1]
        .iter()
        .enumerate()
        .min_by_key(|(_, option)| option.abs_diff(minutes))
        .map_or(0, |(index, _)| index)
}

/// The minutes of the option at `index` of [`IDLE_OPTIONS`].
fn idle_minutes(index: usize) -> Option<u32> {
    IDLE_MINUTES.get(index).copied()
}

/// What the Theme control offers, in the order of [`theme::Appearance::ALL`].
const THEME_CHOICES: [ThemeSetting; 3] = [
    ThemeSetting::Light,
    ThemeSetting::Dark,
    ThemeSetting::System,
];

/// The theme the app uses for a setting.
pub(crate) fn appearance_of(setting: ThemeSetting) -> theme::Appearance {
    match setting {
        ThemeSetting::Light => theme::Appearance::Light,
        ThemeSetting::Dark => theme::Appearance::Dark,
        ThemeSetting::System => theme::Appearance::System,
    }
}

/// Reads a limit typed in a field. A number below 1 or no number leaves `limit` as it is
/// and returns false.
fn apply_limit(limit: &mut u32, text: &str) -> bool {
    match text.trim().parse::<u32>() {
        Ok(number) if number >= 1 => {
            *limit = number;

            true
        }
        _ => false,
    }
}
pub(crate) const NOTIFY_WHEN: [&str; 4] = [
    "Orchestrator needs a decision",
    "Permission or question prompt",
    "Task ready for review",
    "Agent stopped or failed",
];
pub(crate) const DELIVERY: [&str; 4] = [
    "Play sound",
    "Bounce dock for urgent alerts",
    "Mute while viewing the task",
    "Dot on menu bar icon",
];
pub(crate) const PROJECT_NOTIFICATIONS: [&str; 3] = ["All", "Errors only", "Off"];
const PROJECT_MODES: [ProjectMode; 3] =
    [ProjectMode::All, ProjectMode::ErrorsOnly, ProjectMode::Off];

/// The position of `mode` in [`PROJECT_NOTIFICATIONS`].
pub(crate) fn mode_index(mode: ProjectMode) -> usize {
    PROJECT_MODES
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(0)
}

/// The trigger switch at `index` of [`NOTIFY_WHEN`].
pub(crate) fn notify_when(notifications: &NotificationSettings, index: usize) -> bool {
    match index {
        0 => notifications.needs_decision,
        1 => notifications.prompt,
        2 => notifications.ready_for_review,
        _ => notifications.broke,
    }
}

fn notify_when_mut(notifications: &mut NotificationSettings, index: usize) -> &mut bool {
    match index {
        0 => &mut notifications.needs_decision,
        1 => &mut notifications.prompt,
        2 => &mut notifications.ready_for_review,
        _ => &mut notifications.broke,
    }
}

/// The delivery switch at `index` of [`DELIVERY`].
pub(crate) fn delivery(notifications: &NotificationSettings, index: usize) -> bool {
    match index {
        0 => notifications.sound,
        1 => notifications.dock_bounce,
        2 => notifications.mute_while_viewing,
        _ => notifications.menu_bar_dot,
    }
}

fn delivery_mut(notifications: &mut NotificationSettings, index: usize) -> &mut bool {
    match index {
        0 => &mut notifications.sound,
        1 => &mut notifications.dock_bounce,
        2 => &mut notifications.mute_while_viewing,
        _ => &mut notifications.menu_bar_dot,
    }
}

/// The agent settings of CLI number `index`, in the order of [`AGENTS`].
pub(crate) fn cli(agents: &swarm_settings::AgentSettings, index: usize) -> &CliSettings {
    if index == 0 {
        &agents.claude_code
    } else {
        &agents.codex
    }
}

fn cli_mut(agents: &mut swarm_settings::AgentSettings, index: usize) -> &mut CliSettings {
    if index == 0 {
        &mut agents.claude_code
    } else {
        &mut agents.codex
    }
}

/// Settings shared by every page. The agent, terminal and notification settings live in
/// `settings.toml`. A project's settings live in its `.swarm/config.toml`, which the daemon
/// reads and writes.
pub(crate) struct SettingsState {
    pub settings: Settings,
    path: Option<PathBuf>,
    /// Why the settings file could not be read or written.
    pub save_error: Option<SharedString>,
    /// The last load failed. Nothing is saved until a load works, so the file stays as it is.
    load_failed: bool,
    /// The settings last loaded or queued for saving.
    queued: Settings,
    pending_save: Option<gpui::Task<()>>,
    /// The keys of the settings file that fell back to their default when it was loaded.
    /// A save writes them corrected and clears this.
    pub warnings: Vec<String>,
    /// The text in the Max agents per task field is not a number of at least 1.
    pub max_agents_per_task_invalid: bool,
    /// The text in the Max agents in total field is not a number of at least 1.
    pub max_agents_total_invalid: bool,
    pub projects: Vec<ProjectSettings>,
    /// How each project's saves stand, in the order of `projects`.
    saves: Vec<ProjectSave>,
    /// The workspace that holds the daemon connection and the open tasks.
    workspace: Option<WeakEntity<Workspace>>,
    /// What macOS lets the app do, once it answered.
    pub permission: Option<Permission>,
}

impl SettingsState {
    /// Sample data for the pages that are not real yet. `path` is the settings file; with
    /// `None` the agent settings stay the defaults and nothing is read or written.
    pub(crate) fn sample(path: Option<PathBuf>) -> Self {
        let projects: Vec<ProjectSettings> = FakeData::sample()
            .projects
            .into_iter()
            .map(|mut project| {
                project.root = Some(Path::new("/Users/sam/personal").join(&*project.name));

                ProjectSettings::new(project)
            })
            .collect();
        let saves = projects.iter().map(|_| ProjectSave::default()).collect();

        Self {
            settings: Settings::default(),
            path,
            save_error: None,
            load_failed: false,
            queued: Settings::default(),
            pending_save: None,
            warnings: Vec::new(),
            max_agents_per_task_invalid: false,
            max_agents_total_invalid: false,
            projects,
            saves,
            workspace: None,
            permission: None,
        }
    }

    /// A mode other than All on the second and third project, for snapshots.
    #[cfg(feature = "snapshot")]
    fn sample_modes(&mut self, _cx: &mut Context<Self>) {
        for (index, mode) in [(1, ProjectMode::ErrorsOnly), (2, ProjectMode::Off)] {
            let root = self
                .projects
                .get(index)
                .and_then(|entry| entry.project.root.clone());

            if let Some(root) = root {
                self.settings.notifications.set_mode(&root, mode);
            }
        }
    }

    /// Reads the settings file again. A file that cannot be read keeps the current values
    /// and shows why.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.path else {
            return;
        };

        match swarm_settings::load_with_warnings(path) {
            Ok((settings, warnings)) => {
                for warning in &warnings {
                    log::warn!("{}: {warning}", path.display());
                }

                self.settings = settings;
                self.warnings = warnings;
                self.save_error = None;
                self.load_failed = false;
            }
            Err(error) => {
                self.save_error = Some(error.to_string().into());
                self.load_failed = true;
            }
        }

        self.queued = self.settings.clone();
        self.pending_save = None;
        cx.notify();
    }

    /// Saves the settings after a pause in editing.
    fn queue_save(&mut self, cx: &mut Context<Self>) {
        if self.path.is_none() || self.load_failed || self.settings == self.queued {
            return;
        }

        self.queued = self.settings.clone();
        self.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;

            if let Err(error) = this.update(cx, SettingsState::save) {
                log::debug!("the settings closed before they were saved: {error}");
            }
        }));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.path else {
            return;
        };

        if self.load_failed {
            return;
        }

        let saved = swarm_settings::save(path, &self.settings);

        if saved.is_ok() {
            self.warnings.clear();
        }

        self.save_error = saved.err().map(|error| error.to_string().into());
        cx.notify();
    }

    /// The warnings of the settings file that belong to the page of `section`, such as
    /// `agents`.
    pub(crate) fn warnings_of(&self, section: &str) -> Vec<SharedString> {
        self.warnings
            .iter()
            .filter(|warning| {
                warning
                    .strip_prefix(section)
                    .is_some_and(|rest| rest.starts_with(['.', ':']))
            })
            .map(|warning| SharedString::from(warning.clone()))
            .collect()
    }

    /// The welcome screen is dealt with. The flag is saved at once, as nothing else would
    /// save it: no settings window needs to be open.
    fn finish_welcome(&mut self, cx: &mut Context<Self>) {
        self.settings.welcome_done = true;
        self.save(cx);
        self.queued = self.settings.clone();
        cx.notify();
    }

    /// "Do not show this again" on the Close terminal dialog. Saved at once: the dialog is
    /// gone before a pause in editing ends, and no settings window needs to be open.
    fn stop_confirming_close(&mut self, cx: &mut Context<Self>) {
        self.settings.terminals.confirm_close = false;
        self.save(cx);
        if let Some(error) = &self.save_error {
            log::warn!("cannot save the choice not to ask before closing a terminal: {error}");
        }
        self.queued = self.settings.clone();
        cx.notify();
    }

    pub(crate) fn orchestrator_index(&self) -> usize {
        match self.settings.agents.orchestrator {
            swarm_settings::AgentKindSetting::ClaudeCode => 0,
            swarm_settings::AgentKindSetting::Codex => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    General,
    Agents,
    Terminals,
    Prompts,
    Notifications,
    Appearance,
    Usage,
    Keyboard,
    Project(ProjectId),
}

impl Page {
    pub(crate) const SECTIONS: [Self; 8] = [
        Self::General,
        Self::Agents,
        Self::Terminals,
        Self::Prompts,
        Self::Notifications,
        Self::Appearance,
        Self::Usage,
        Self::Keyboard,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Agents => "Agents",
            Self::Terminals => "Terminals",
            Self::Prompts => "Prompts",
            Self::Notifications => "Notifications",
            Self::Appearance => "Appearance",
            Self::Usage => "Usage",
            Self::Keyboard => "Keyboard",
            Self::Project(_) => "Project",
        }
    }
}

struct CliInputs {
    command: Entity<TextInput>,
    orchestrator_model: Entity<TextInput>,
    worker_model: Entity<TextInput>,
}

struct Inputs {
    agents: [CliInputs; 2],
    max_agents_per_task: Entity<TextInput>,
    max_agents_total: Entity<TextInput>,
    projects: Vec<ProjectInputs>,
}

pub(crate) struct SettingsWindow {
    state: Entity<SettingsState>,
    page: Page,
    /// One Tab stop for the page list and the project list below it; the arrows move inside.
    nav: RovingFocus,
    inputs: Inputs,
    prompts: Entity<PromptsPage>,
    usage: Entity<UsagePage>,
    system_check: Entity<SystemCheckView>,
    check_again_focus: FocusHandle,
    /// Where a page without controls takes keyboard focus, to scroll with the keys.
    page_focus: FocusHandle,
    page_scroll: ScrollHandle,
    /// Scrolls the page to the block that takes focus.
    page_reveal: Entity<RevealFocus>,
    /// One handle per block of the open page, to see which block has focus.
    block_focus: Vec<FocusHandle>,
    subscriptions: Vec<Subscription>,
    /// The dialog that asks before a project goes, while it shows.
    remove_dialog: Option<RemoveDialog>,
    /// The Remove project button of the open page, where focus returns after the dialog.
    remove_focus: FocusHandle,
    /// The window's root, to tell whether focus is inside the window.
    root_focus: FocusHandle,
}

struct RemoveDialog {
    view: Entity<RemoveProjectDialog>,
    _subscription: Subscription,
}

fn field(
    label: impl Into<SharedString>,
    text: &str,
    mono: bool,
    cx: &mut App,
) -> Entity<TextInput> {
    let (label, text) = (label.into(), text.to_owned());

    cx.new(|cx| {
        let mut input = TextInput::new(cx);
        input.set_label(label, cx);
        input.set_mono(mono, cx);
        input.set_size(TextFieldSize::Small, cx);
        input.set_text(text, cx);
        input
    })
}

fn is_digit(character: char) -> bool {
    character.is_ascii_digit()
}

/// A field that only takes digits, typed or pasted.
fn numeric_field(label: impl Into<SharedString>, text: &str, cx: &mut App) -> Entity<TextInput> {
    let input = field(label, text, true, cx);
    input.update(cx, |input, cx| input.set_char_filter(Some(is_digit), cx));

    input
}

/// A number field for a limit of at least 1, with its text bound to the state. `apply`
/// writes the text into the state and says whether the text is invalid; an invalid text
/// puts the field in an error state with `message`.
fn limit_field(
    subscriptions: &mut Vec<Subscription>,
    state: &Entity<SettingsState>,
    (label, value): (&'static str, u32),
    message: &'static str,
    apply: impl Fn(&mut SettingsState, String) -> bool + 'static,
    cx: &mut Context<SettingsWindow>,
) -> Entity<TextInput> {
    let input = numeric_field(label, &value.to_string(), cx);
    let state = state.clone();

    subscriptions.push(
        cx.subscribe(&input, move |_, input, event: &TextInputEvent, cx| {
            let TextInputEvent::Changed(text) = event else {
                return;
            };

            let invalid = state.update(cx, |state, cx| {
                let invalid = apply(state, text.to_string());
                cx.notify();

                invalid
            });
            let error = invalid.then(|| SharedString::from(message));
            input.update(cx, |input, cx| input.set_error(error, cx));
        }),
    );

    input
}

/// Writes the input's text into the state.
fn bind_text(
    subscriptions: &mut Vec<Subscription>,
    input: &Entity<TextInput>,
    state: &Entity<SettingsState>,
    apply: impl Fn(&mut SettingsState, String) + 'static,
    cx: &mut Context<SettingsWindow>,
) {
    let state = state.clone();

    subscriptions.push(
        cx.subscribe(input, move |_, _, event: &TextInputEvent, cx| {
            let TextInputEvent::Changed(text) = event else {
                return;
            };

            state.update(cx, |state, cx| {
                apply(state, text.to_string());
                cx.notify();
            });
        }),
    );
}

/// Writes the input's text into the agent settings.
fn bind_agent_text(
    subscriptions: &mut Vec<Subscription>,
    input: &Entity<TextInput>,
    state: &Entity<SettingsState>,
    apply: impl Fn(&mut swarm_settings::AgentSettings, String) + 'static,
    cx: &mut Context<SettingsWindow>,
) {
    let state = state.clone();

    subscriptions.push(
        cx.subscribe(input, move |_, _, event: &TextInputEvent, cx| {
            let TextInputEvent::Changed(text) = event else {
                return;
            };

            state.update(cx, |state, cx| {
                apply(&mut state.settings.agents, text.to_string());
                cx.notify();
            });
        }),
    );
}

impl SettingsWindow {
    fn new(
        state: Entity<SettingsState>,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.observe(&state, |_, state, cx| {
                state.update(cx, |state, cx| {
                    state.queue_save(cx);
                    state.queue_project_saves(cx);
                });
                cx.notify();
            }),
            theme::follow_system_appearance(window, cx),
            swarm_ui::motion::follow_reduce_motion(window, cx),
            Self::restore_focus_when_lost(window, cx),
            cx.observe_window_activation(window, |this, window, cx| {
                // The user may come back from System Settings with a new answer.
                if window.is_window_active() {
                    this.refresh_permission(cx);
                    // The user may have fixed a tool while away.
                    this.refresh_system_check(cx);
                    // The user edits the prompt files and the project config outside the app.
                    this.refresh_prompts(cx);
                    this.open_project(cx);
                }
            }),
        ];

        let workspace = state
            .read(cx)
            .workspace
            .as_ref()
            .and_then(WeakEntity::upgrade);
        if let Some(workspace) = workspace {
            subscriptions.push(cx.observe(&workspace, |this, _, cx| {
                this.reconnect_prompts(cx);
                cx.notify();
            }));
        }

        let agents = state.read(cx).settings.agents.clone();
        let projects = state.read(cx).projects.clone();

        let agent_inputs = Self::agent_inputs(&state, &agents, &mut subscriptions, cx);

        let max_agents_per_task = limit_field(
            &mut subscriptions,
            &state,
            ("Max agents per task", agents.max_agents_per_task),
            MAX_AGENTS_PER_TASK_ERROR,
            |state, text| {
                state.max_agents_per_task_invalid =
                    !apply_limit(&mut state.settings.agents.max_agents_per_task, &text);

                state.max_agents_per_task_invalid
            },
            cx,
        );
        let max_agents_total = limit_field(
            &mut subscriptions,
            &state,
            ("Max agents in total", agents.max_agents_total),
            MAX_AGENTS_TOTAL_ERROR,
            |state, text| {
                state.max_agents_total_invalid =
                    !apply_limit(&mut state.settings.agents.max_agents_total, &text);

                state.max_agents_total_invalid
            },
            cx,
        );

        // The Updates section shows the status of the updater.
        if let Some(updater) = crate::updates::updater(cx) {
            subscriptions.push(cx.observe(&updater, |_, _, cx| cx.notify()));
        }

        let (system_check, check_again_focus) = Self::system_check_for(&state, window, cx);
        // The Agents page shows the versions this check finds.
        subscriptions.push(cx.observe(&system_check, |_, _, cx| cx.notify()));

        let page_reveal = cx.new(|_| RevealFocus::new(Axis::Vertical));
        let page_scroll = page_reveal.read(cx).scroll().clone();

        let mut this = Self {
            state: state.clone(),
            page,
            nav: RovingFocus::new(Axis::Vertical),
            inputs: Inputs {
                agents: agent_inputs,
                max_agents_per_task,
                max_agents_total,
                projects: Vec::new(),
            },
            prompts: cx.new(|cx| PromptsPage::new(state.clone(), cx)),
            usage: cx.new(|cx| UsagePage::new(state.clone(), cx)),
            system_check,
            check_again_focus,
            page_focus: cx.focus_handle().tab_stop(true),
            page_scroll,
            page_reveal,
            block_focus: Vec::new(),
            subscriptions,
            remove_dialog: None,
            remove_focus: cx.focus_handle().tab_stop(true),
            root_focus: cx.focus_handle(),
        };

        for index in 0..projects.len() {
            if let Some(inputs) = this.project_inputs(index, cx) {
                this.inputs.projects.push(inputs);
                this.sync_inputs(index, cx);
            }
        }

        // The page draws the rows of the prompts itself, so a change must redraw this window.
        let prompts_changed = cx.observe(&this.prompts, |_, _, cx| cx.notify());
        this.subscriptions.push(prompts_changed);

        this.sync_nav(cx);
        this.focus_nav(window, cx);
        this.refresh_permission(cx);
        this.refresh_prompts(cx);
        this.open_project(cx);
        // The check reads the workspace, which may be the one opening this window.
        cx.defer_in(window, |this, _, cx| this.refresh_system_check(cx));

        this
    }

    /// The fields of both agent cards, each bound to the settings state.
    fn agent_inputs(
        state: &Entity<SettingsState>,
        agents: &swarm_settings::AgentSettings,
        subscriptions: &mut Vec<Subscription>,
        cx: &mut Context<Self>,
    ) -> [CliInputs; 2] {
        [0, 1].map(|index| {
            let current = cli(agents, index);
            let agent = AGENTS[index].name;
            let mut input = |label: &str,
                             text: &str,
                             apply: fn(&mut CliSettings) -> &mut String,
                             cx: &mut Context<Self>| {
                let input = field(label.to_owned(), text, true, cx);
                bind_agent_text(
                    subscriptions,
                    &input,
                    state,
                    move |agents, text| *apply(cli_mut(agents, index)) = text,
                    cx,
                );
                input
            };

            CliInputs {
                command: input(
                    &format!("{agent} command"),
                    &current.command,
                    |cli| &mut cli.command,
                    cx,
                ),
                orchestrator_model: input(
                    &format!("{agent} orchestrator model"),
                    &current.orchestrator_model,
                    |cli| &mut cli.orchestrator_model,
                    cx,
                ),
                worker_model: input(
                    &format!("{agent} worker model"),
                    &current.worker_model,
                    |cli| &mut cli.worker_model,
                    cx,
                ),
            }
        })
    }

    /// Without it, a control that goes away takes the keyboard with it. A focus that waits for
    /// the remove dialog is left alone.
    fn restore_focus_when_lost(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.on_focus_lost(window, |this, window, cx| {
            if this.remove_dialog.is_some() {
                return;
            }

            match window.focus_lost_restore_target(cx) {
                Some(target)
                    if target != this.root_focus && !this.block_focus.contains(&target) =>
                {
                    window.focus(&target, cx);
                }
                _ => this.focus_nav(window, cx),
            }
        })
    }

    /// The check of the General page, and the focus of its Check again button, where focus
    /// goes when no action is left.
    fn system_check_for(
        state: &Entity<SettingsState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<SystemCheckView>, FocusHandle) {
        let workspace = state.read(cx).workspace.clone();
        let check_again = cx.focus_handle().tab_stop(true);
        let check = cx.new({
            let fallback = check_again.clone();

            |cx| {
                let mut check = SystemCheckView::new(workspace, false, window, cx);
                check.set_fallback_focus(fallback);

                check
            }
        });

        (check, check_again)
    }

    #[cfg(test)]
    pub(crate) fn system_check(&self) -> &Entity<SystemCheckView> {
        &self.system_check
    }

    /// Asks the daemon about the tools again, while the General or the Agents page shows.
    fn refresh_system_check(&self, cx: &mut Context<Self>) {
        if matches!(self.page, Page::General | Page::Agents) {
            self.system_check.update(cx, SystemCheckView::refresh);
        }
    }

    #[cfg(test)]
    pub(crate) fn page(&self) -> Page {
        self.page
    }

    pub(crate) fn select_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.page_scroll.set_offset(point(px(0.), px(0.)));
        self.refresh_permission(cx);
        self.refresh_system_check(cx);
        self.refresh_prompts(cx);
        self.open_project(cx);
        cx.notify();
    }

    /// Asks macOS what the app may do, while the Notifications page shows. The answer
    /// comes back through a channel, as macOS answers on its own thread.
    fn refresh_permission(&self, cx: &mut Context<Self>) {
        if self.page != Page::Notifications {
            return;
        }

        let answers = notifications::check_permission(cx);
        let state = self.state.clone();

        cx.spawn(async move |this, cx| {
            let Ok(authorization) = answers.recv_async().await else {
                return;
            };

            cx.update(|cx| notifications::remember(authorization, cx));

            let shown = this.update(cx, |_, cx| {
                state.update(cx, |state, cx| {
                    state.permission = Some(authorization.permission);
                    cx.notify();
                });
            });

            if let Err(error) = shown {
                log::debug!("the settings closed before the permission came: {error}");
            }
        })
        .detach();
    }

    /// Position of the page in the nav: sections first, then projects.
    fn nav_index(&self, page: Page, cx: &App) -> usize {
        match page {
            Page::Project(project) => {
                Page::SECTIONS.len() + self.state.read(cx).index_of(project).unwrap_or(0)
            }
            section => Page::SECTIONS
                .iter()
                .position(|candidate| *candidate == section)
                .unwrap_or(0),
        }
    }

    fn page_at(&self, index: usize, cx: &App) -> Page {
        if let Some(section) = Page::SECTIONS.get(index) {
            return *section;
        }

        self.state
            .read(cx)
            .projects
            .get(index - Page::SECTIONS.len())
            .map_or(Page::Agents, |entry| Page::Project(entry.project.id))
    }

    /// Gives every nav row a focus handle and makes the open page's row the Tab stop.
    fn sync_nav(&mut self, cx: &mut App) {
        let keys: Vec<ElementId> =
            (0..Page::SECTIONS.len())
                .map(|index| ElementId::from(("settings-section", index)))
                .chain(self.state.read(cx).projects.iter().map(|entry| {
                    ElementId::from(format!("settings-project-{}", entry.project.id.0))
                }))
                .collect();

        self.nav.sync_keyed(&keys, cx);

        let active = self.nav_index(self.page, cx);
        self.nav.set_active(active);
    }

    fn nav_handle(&self, index: usize) -> Option<FocusHandle> {
        (index < self.nav.len()).then(|| self.nav.handle(index))
    }

    #[cfg(test)]
    fn nav_handles(&self) -> Vec<FocusHandle> {
        (0..self.nav.len())
            .map(|index| self.nav.handle(index))
            .collect()
    }

    fn focus_nav(&self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.nav_handle(self.nav_index(self.page, cx)) {
            window.focus(&handle, cx);
        }
    }

    fn move_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.nav_index(self.page, cx).saturating_add_signed(step);

        self.select_row(next, window, cx);
    }

    /// Opens the page of nav row `index`, or of the nearest row, and moves focus to it.
    fn select_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.nav.len().saturating_sub(1);
        let next = index.min(last);

        self.page = self.page_at(next, cx);
        self.sync_nav(cx);
        self.page_scroll.set_offset(point(px(0.), px(0.)));
        self.refresh_permission(cx);
        self.refresh_system_check(cx);
        self.refresh_prompts(cx);
        self.open_project(cx);
        self.focus_nav(window, cx);
        cx.notify();
    }

    fn nav_row(&self, index: usize, cx: &Context<Self>) -> impl IntoElement + use<> {
        let page = self.page_at(index, cx);
        let projects = self.state.read(cx).projects.clone();

        let (label, tile) = match page {
            Page::Project(project) => {
                match projects.iter().find(|entry| entry.project.id == project) {
                    Some(settings) => (
                        settings.project.name.clone(),
                        Some(
                            ProjectTile::new(settings.project.letter, settings.project.tile_color)
                                .size(px(16.)),
                        ),
                    ),
                    None => (SharedString::default(), None),
                }
            }
            section => (SharedString::from(section.title()), None),
        };

        let mut row = ListRow::new(("settings-nav", index), label)
            .toggle_state(self.page == page)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_page(page, cx);
                this.focus_nav(window, cx);
            }));

        if let Some(handle) = self.nav_handle(index) {
            row = row.track_focus(&handle);
        }

        if let Some(tile) = tile {
            row = row.start_slot(tile);
        }

        row
    }

    fn nav(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let faint = cx.theme().colors().faint;
        let project_count = self.state.read(cx).projects.len();

        let sections: Vec<_> = (0..Page::SECTIONS.len())
            .map(|index| self.nav_row(index, cx))
            .collect();
        let projects: Vec<_> = (0..project_count)
            .map(|index| self.nav_row(Page::SECTIONS.len() + index, cx))
            .collect();
        let top_drag = drag_region("settings-top-drag", window, cx).h(px(24.));
        let title_drag = drag_region("settings-title-drag", window, cx);

        v_flex()
            .id("settings-nav")
            .role(Role::Navigation)
            .aria_label("Settings sections")
            .key_context(NAV_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.move_selection(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                this.move_selection(1, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }

                match event.keystroke.key.as_str() {
                    "home" => this.select_row(0, window, cx),
                    "end" => this.select_row(usize::MAX, window, cx),
                    _ => return,
                }

                cx.stop_propagation();
            }))
            .w(px(232.))
            .flex_none()
            .h_full()
            .gap(px(2.))
            .px(px(8.))
            .py(px(12.))
            .child(top_drag)
            .child(
                title_drag
                    .flex()
                    .items_center()
                    .h(px(32.))
                    .px(px(8.))
                    .mt(px(12.))
                    .mb(px(8.))
                    .child(
                        Label::new("Settings")
                            .size(LabelSize::Large)
                            .weight(gpui::FontWeight::SEMIBOLD),
                    ),
            )
            .children(sections)
            .child(
                h_flex()
                    .h(px(28.))
                    .px(px(8.))
                    .mt(px(16.))
                    .text_color(faint)
                    .child(
                        Label::new("Projects")
                            .size(LabelSize::Small)
                            .color(Color::Faint),
                    ),
            )
            .children(projects)
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (ground, panel, line, fg, accent) = (
            colors.ground,
            colors.panel,
            colors.line,
            colors.fg,
            colors.accent,
        );

        self.sync_nav(cx);

        let nav = self.nav(window, cx);
        let page = self.render_page(window, cx);
        while self.block_focus.len() < page.blocks.len() {
            self.block_focus.push(cx.focus_handle());
        }
        let handles: Vec<FocusHandle> = self.block_focus[..page.blocks.len()].to_vec();
        let blocks: Vec<_> = page
            .blocks
            .into_iter()
            .zip(&handles)
            .enumerate()
            .map(|(index, (block, handle))| {
                ignore_press(
                    div()
                        .id(("settings-block", index))
                        .debug_selector(|| format!("settings-block-{index}"))
                        .track_focus(handle)
                        .flex_none()
                        .max_w(px(660.))
                        .child(block),
                )
            })
            .collect();
        let scrolls_with_keys = matches!(self.page, Page::Keyboard | Page::Usage);
        let scroll = self.page_scroll.clone();
        let gap_drag = drag_region("settings-gap-drag", window, cx);
        let page_drag = drag_region("settings-page-drag", window, cx);

        swarm_ui::tab_navigation(ignore_press(
            div()
                .id("settings")
                .track_focus(&self.root_focus)
                .key_context(root_context())
                .on_action(cx.listener(|_, _: &CloseSettings, window, _| window.remove_window()))
                .relative()
                .size_full()
                .bg(ground)
                .text_color(fg)
                .font_family(UI_FONT)
                .text_size(px(13.)),
        ))
        .child(
            h_flex().size_full().child(nav).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pb(px(8.))
                    .pr(px(8.))
                    .child(
                        gap_drag
                            .debug_selector(|| "settings-gap-drag".to_owned())
                            .h(px(8.))
                            .flex_none(),
                    )
                    .child(
                        v_flex()
                                .flex_1()
                                .min_h_0()
                                .bg(panel)
                                .border_1()
                                .border_color(line)
                                .rounded(px(10.))
                                .overflow_hidden()
                                // The top of the page moves the window, as the titlebar would.
                                .child(
                                    page_drag
                                        .debug_selector(|| "settings-page-drag".to_owned())
                                        .h(px(36.))
                                        .flex_none(),
                                )
                                .child(
                                    reveal_focused(
                                        v_flex()
                                            .debug_selector(|| "settings-page".to_owned())
                                            .flex_1()
                                            .min_h_0()
                                            .px(px(48.))
                                            .pb(px(36.))
                                            .gap(px(page.gap)),
                                        "settings-page",
                                        &self.page_reveal,
                                        handles,
                                        cx,
                                    )
                                    .children(blocks)
                                        .when(scrolls_with_keys, |page_area| {
                                            let scroll = scroll.clone();

                                            page_area
                                                .role(Role::ScrollView)
                                                .aria_label(self.page.title())
                                                .track_focus(&self.page_focus)
                                                .focus_visible(move |style| {
                                                    focus_ring(style, accent)
                                                })
                                                .on_key_down(move |event, window, cx| {
                                                    let key = event.keystroke.key.as_str();

                                                    if scroll_for_key(&scroll, key, PAGE_LINE) {
                                                        window.refresh();
                                                        cx.stop_propagation();
                                                    }
                                                })
                                        }),
                                ),
                    ),
            ),
        )
        .children(
            self.remove_dialog
                .as_ref()
                .map(|dialog| dialog.view.clone()),
        )
    }
}

/// The root has both contexts, so ⌘W works wherever focus is in the window.
fn root_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add(ROOT_CONTEXT);
    context.add(SETTINGS_CONTEXT);

    context
}

/// Remembers the settings state across window opens, and the open window.
struct Registry {
    state: Entity<SettingsState>,
    window: Option<WindowHandle<SettingsWindow>>,
}

impl Global for Registry {}

struct KeysBound;

impl Global for KeysBound {}

type Opener = Rc<dyn Fn(&Path, &mut App)>;

struct OpenWith(Opener);

impl Global for OpenWith {}

/// Opens a file with the system's default app. This is the one place the settings pages
/// open files, so a test can see which file would open.
pub(crate) fn open_file(path: &Path, cx: &mut App) {
    let opener = cx
        .try_global::<OpenWith>()
        .map(|opener| Rc::clone(&opener.0));

    match opener {
        Some(open) => open(path, cx),
        None => cx.open_with_system(path),
    }
}

/// Replaces the system call, so a test can see which file would open.
#[cfg(test)]
pub(crate) fn use_opener(opener: Opener, cx: &mut App) {
    cx.set_global(OpenWith(opener));
}

fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }

    cx.set_global(KeysBound);
    cx.bind_keys([
        KeyBinding::new("cmd-w", CloseSettings, Some(SETTINGS_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(NAV_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(NAV_CONTEXT)),
    ]);
}

/// Loads the settings file at startup, so the settings apply before the window opens. The
/// saved theme applies here, before the first frame.
pub(crate) fn load(path: Option<PathBuf>, cx: &mut App) {
    let state = shared_state(path, cx);

    state.update(cx, SettingsState::reload);
    theme::set_appearance(appearance_of(appearance(cx).theme), cx);
}

/// The notification settings. They stay the defaults until the settings load.
pub(crate) fn notifications(cx: &App) -> NotificationSettings {
    cx.try_global::<Registry>()
        .map_or_else(NotificationSettings::default, |registry| {
            registry.state.read(cx).settings.notifications.clone()
        })
}

/// The appearance settings. They stay the defaults until the settings load.
pub(crate) fn appearance(cx: &App) -> AppearanceSettings {
    cx.try_global::<Registry>()
        .map_or_else(AppearanceSettings::default, |registry| {
            registry.state.read(cx).settings.appearance
        })
}

/// True unless the settings turn the easter eggs off. The switch turns every egg off.
pub(crate) fn easter_eggs(cx: &App) -> bool {
    appearance(cx).easter_eggs
}

/// True when the Swarm view shows a honeycomb behind the graph: the setting is on, and so
/// are the eggs.
pub(crate) fn swarm_view_honeycomb(cx: &App) -> bool {
    let appearance = appearance(cx);

    appearance.easter_eggs && appearance.swarm_view_honeycomb
}

/// The accent the theme uses for a setting.
pub(crate) fn accent_of(accent: AccentSetting) -> theme::Accent {
    match accent {
        AccentSetting::Blue => theme::Accent::Blue,
        AccentSetting::Honey => theme::Accent::Honey,
    }
}

/// Applies the accent of the settings now and each time it changes, and draws the workspace
/// again when any appearance setting changes: the eggs and the Swarm view background read them.
pub(crate) fn follow_appearance(workspace: Entity<Workspace>, cx: &mut App) {
    let settings = shared_state(None, cx);
    let mut applied = appearance(cx);

    theme::set_accent(accent_of(applied.accent), cx);

    cx.observe(&settings, move |_, cx| {
        let current = appearance(cx);

        if current == applied {
            return;
        }

        let theme_changed = current.theme != applied.theme;

        applied = current;
        theme::set_accent(accent_of(current.accent), cx);

        if theme_changed {
            theme::set_appearance(appearance_of(current.theme), cx);
        }

        workspace.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// Whether agents start without permission prompts. It stays off until the settings load.
pub(crate) fn skip_permissions(cx: &App) -> bool {
    cx.try_global::<Registry>()
        .is_some_and(|registry| registry.state.read(cx).settings.agents.skip_permissions)
}

/// Draws the workspace again when the skip permissions setting changes: the status bar
/// shows a note while it is on.
pub(crate) fn follow_skip_permissions(workspace: Entity<Workspace>, cx: &mut App) {
    let settings = shared_state(None, cx);
    let mut applied = skip_permissions(cx);

    cx.observe(&settings, move |_, cx| {
        let current = skip_permissions(cx);

        if current != applied {
            applied = current;
            workspace.update(cx, |_, cx| cx.notify());
        }
    })
    .detach();
}

/// Whether the app checks for updates by itself. On until the settings say otherwise.
pub(crate) fn auto_check_updates(cx: &App) -> bool {
    cx.try_global::<Registry>().map_or_else(
        || UpdateSettings::default().auto_check,
        |registry| registry.state.read(cx).settings.updates.auto_check,
    )
}

/// Whether new pull requests start as drafts. On until the settings say otherwise.
pub(crate) fn open_pull_requests_as_draft(cx: &App) -> bool {
    cx.try_global::<Registry>().map_or_else(
        || PullRequestSettings::default().open_as_draft,
        |registry| registry.state.read(cx).settings.pull_requests.open_as_draft,
    )
}

/// Changes the update settings the way the page does.
#[cfg(test)]
pub(crate) fn change_updates(edit: impl FnOnce(&mut UpdateSettings), cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, cx| {
        edit(&mut state.settings.updates);
        cx.notify();
    });
}

/// Changes the agent settings the way the page does.
#[cfg(test)]
pub(crate) fn change_agents(edit: impl FnOnce(&mut swarm_settings::AgentSettings), cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, cx| {
        edit(&mut state.settings.agents);
        cx.notify();
    });
}

/// Changes the appearance settings the way the page does.
#[cfg(any(test, feature = "snapshot"))]
pub(crate) fn change_appearance(edit: impl FnOnce(&mut AppearanceSettings), cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, cx| {
        edit(&mut state.settings.appearance);
        cx.notify();
    });
}

/// The shared settings state, for what must follow a change in the settings.
pub(crate) fn state(cx: &mut App) -> Entity<SettingsState> {
    shared_state(None, cx)
}

/// True when the welcome screen has not been dealt with. It stays false until the settings
/// load, and when the file cannot be read: the flag could not be saved, so the welcome
/// would come back at every launch.
pub(crate) fn welcome_pending(cx: &App) -> bool {
    cx.try_global::<Registry>().is_some_and(|registry| {
        let state = registry.state.read(cx);

        !state.load_failed && !state.settings.welcome_done
    })
}

/// Marks the welcome screen as dealt with, and saves the flag.
pub(crate) fn finish_welcome(cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, SettingsState::finish_welcome);
}

/// Whether the user may type into worker terminals. It stays on until the settings turn it off.
pub(crate) fn allow_worker_typing(cx: &App) -> bool {
    cx.try_global::<Registry>().is_none_or(|registry| {
        registry
            .state
            .read(cx)
            .settings
            .terminals
            .allow_worker_typing
    })
}

/// Whether closing a user terminal with a running program asks first. It stays on until the
/// settings turn it off. Read each time the question would open, so a change counts at once.
pub(crate) fn confirm_terminal_close(cx: &App) -> bool {
    cx.try_global::<Registry>()
        .is_none_or(|registry| registry.state.read(cx).settings.terminals.confirm_close)
}

/// Turns the question off and saves that.
pub(crate) fn stop_confirming_terminal_close(cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, SettingsState::stop_confirming_close);
}

/// Draws the workspace again when the terminal font size in the settings changes. That is
/// how a new size reaches every terminal view: the workspace measures the cell again.
pub(crate) fn follow_font_size(workspace: Entity<Workspace>, cx: &mut App) {
    let settings = shared_state(None, cx);
    let mut applied = terminal_font_size(cx);

    cx.observe(&settings, move |_, cx| {
        let points = terminal_font_size(cx);

        if (points - applied).abs() > f32::EPSILON {
            applied = points;
            workspace.update(cx, |_, cx| cx.notify());
        }
    })
    .detach();
}

/// The font size of every terminal, in points. It stays the default until the settings load.
pub(crate) fn terminal_font_size(cx: &App) -> f32 {
    let points = cx
        .try_global::<Registry>()
        .map_or(swarm_settings::FONT_SIZE_DEFAULT, |registry| {
            registry.state.read(cx).settings.terminals.font_size
        });

    f32::from(swarm_settings::clamp_font_size(points))
}

/// Sets the terminal font size the way the Terminals page does.
#[cfg(any(test, feature = "snapshot"))]
pub(crate) fn set_terminal_font_size(points: u8, cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, cx| {
        state.settings.terminals.font_size = points;
        cx.notify();
    });
}

#[cfg(test)]
pub(crate) fn set_allow_worker_typing(allowed: bool, cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, _| {
        state.settings.terminals.allow_worker_typing = allowed;
    });
}

/// Changes the notification settings the way the page does.
#[cfg(test)]
pub(crate) fn change_notifications(edit: impl FnOnce(&mut NotificationSettings), cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, cx| {
        edit(&mut state.settings.notifications);
        cx.notify();
    });
}

fn shared_state(path: Option<PathBuf>, cx: &mut App) -> Entity<SettingsState> {
    if let Some(registry) = cx.try_global::<Registry>() {
        return registry.state.clone();
    }

    let state = cx.new(|_| SettingsState::sample(path));
    cx.set_global(Registry {
        state: state.clone(),
        window: None,
    });

    state
}

fn create_window(
    state: Entity<SettingsState>,
    page: Page,
    window_size: Size<Pixels>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<SettingsWindow>> {
    let bounds = Bounds::centered(None, window_size, cx);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(MIN_WINDOW_SIZE),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    };

    cx.open_window(options, move |window, cx| {
        cx.new(|cx| SettingsWindow::new(state, page, window, cx))
    })
}

fn open_window(
    path: Option<PathBuf>,
    projects: Option<Vec<Project>>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<SettingsWindow>> {
    bind_keys(cx);
    let state = shared_state(path, cx);

    let open = cx
        .try_global::<Registry>()
        .and_then(|registry| registry.window);
    if let Some(handle) = open
        && handle
            .update(cx, |settings, window, cx| {
                window.activate_window();

                if let Some(projects) = &projects {
                    settings.add_projects(projects, cx);
                }
            })
            .is_ok()
    {
        return Ok(handle);
    }

    state.update(cx, SettingsState::reload);
    if let Some(projects) = projects {
        state.update(cx, |state, _| state.use_projects(projects));
    }

    let handle = create_window(state, Page::Agents, WINDOW_SIZE, cx)?;
    cx.global_mut::<Registry>().window = Some(handle);

    Ok(handle)
}

/// Opens the settings window, or focuses it when it is already open. `projects` are the
/// workspace's: the window lists them. The workspace holds the daemon connection and the
/// open tasks the Projects pages use. `page` is the page to show, also when the window was
/// open already.
pub(crate) fn open(
    path: Option<PathBuf>,
    projects: Vec<Project>,
    workspace: WeakEntity<Workspace>,
    page: Option<Page>,
    cx: &mut App,
) {
    link_workspace(workspace, cx);

    let handle = match open_window(path, Some(projects), cx) {
        Ok(handle) => handle,
        Err(error) => {
            log::error!("could not open the settings window: {error:#}");
            return;
        }
    };

    let Some(page) = page else {
        return;
    };

    // The page asks the workspace, which is updating when it is the one that opens settings.
    cx.defer(move |cx| {
        let shown = handle.update(cx, |settings, window, cx| {
            settings.select_page(page, cx);
            settings.focus_nav(window, cx);
        });

        if let Err(error) = shown {
            log::debug!("the settings window closed before it showed {page:?}: {error}");
        }
    });
}

fn link_workspace(workspace: WeakEntity<Workspace>, cx: &mut App) {
    let state = shared_state(None, cx);

    state.update(cx, |state, _| state.workspace = Some(workspace));
}

/// The daemon reported a run of local checks. A Projects page that started one shows it.
pub(crate) fn checks_changed(run: &swarm_protocol::ChecksRun, cx: &mut App) {
    let Some(state) = cx
        .try_global::<Registry>()
        .map(|registry| registry.state.clone())
    else {
        return;
    };

    state.update(cx, |state, cx| state.record_run(run, cx));
}

/// The page the open settings window shows.
#[cfg(test)]
pub(crate) fn shown_page(cx: &App) -> Option<Page> {
    let window = cx.try_global::<Registry>()?.window?;

    window.read(cx).ok().map(|settings| settings.page)
}

/// The workspace removed a project: its page, its inputs and its nav row go, and the window
/// leaves the page if it showed it. It waits for the current update to end, as the removal
/// may come from a dialog of the settings window itself.
pub(crate) fn project_removed(project: ProjectId, cx: &mut App) {
    cx.defer(move |cx| {
        let Some((state, window)) = cx
            .try_global::<Registry>()
            .map(|registry| (registry.state.clone(), registry.window))
        else {
            return;
        };

        let Some(index) = state.update(cx, |state, cx| state.remove_project(project, cx)) else {
            return;
        };

        if let Some(window) = window
            && let Err(error) = window.update(cx, |settings, window, cx| {
                settings.drop_project(project, index, window, cx);
            })
        {
            log::debug!("the settings closed before the project went: {error}");
        }
    });
}

/// The pages to save, with the window size and the answer the sample delivery gives to
/// every page for the notification permission.
#[cfg(feature = "snapshot")]
const SNAPSHOT_PAGES: [(Page, &str, Size<Pixels>, Permission); 12] = [
    (
        Page::General,
        "general",
        size(px(1280.), px(900.)),
        Permission::Allowed,
    ),
    (
        Page::Agents,
        "agents",
        size(px(1280.), px(900.)),
        Permission::Allowed,
    ),
    (
        Page::Notifications,
        "notifications",
        size(px(1280.), px(1000.)),
        Permission::NotAllowed,
    ),
    (
        Page::Notifications,
        "notifications-allowed",
        size(px(1280.), px(1000.)),
        Permission::Allowed,
    ),
    (
        Page::Project(ProjectId(0)),
        "project",
        size(px(1280.), px(1180.)),
        Permission::Allowed,
    ),
    (
        Page::Terminals,
        "terminals",
        size(px(1280.), px(600.)),
        Permission::Allowed,
    ),
    (
        Page::Prompts,
        "prompts",
        size(px(1280.), px(700.)),
        Permission::Allowed,
    ),
    (
        Page::Prompts,
        "prompts-reset",
        size(px(1280.), px(700.)),
        Permission::Allowed,
    ),
    (
        Page::Usage,
        "usage",
        size(px(1280.), px(700.)),
        Permission::Allowed,
    ),
    (
        Page::Keyboard,
        "keyboard",
        size(px(1280.), px(1900.)),
        Permission::Allowed,
    ),
    (
        Page::Appearance,
        "appearance",
        size(px(1280.), px(700.)),
        Permission::Allowed,
    ),
    (
        Page::Appearance,
        "appearance-honey",
        size(px(1280.), px(700.)),
        Permission::Allowed,
    ),
];

/// Gives the check of the General page a terminal under its GitHub CLI row.
#[cfg(feature = "snapshot")]
fn show_install_output(handle: WindowHandle<SettingsWindow>, cx: &mut gpui::AsyncApp) {
    let shown = cx.update(|cx| {
        handle.update(cx, |settings, window, cx| {
            let output = crate::demo::install_output(cx);

            settings.system_check.update(cx, |check, cx| {
                check.show_sample_output(output, window, cx);
            });
        })
    });

    if let Err(error) = shown {
        log::error!("cannot show the install output: {error:#}");
    }
}

#[cfg(feature = "snapshot")]
pub(crate) fn snapshot(directory: PathBuf, cx: &mut App) {
    use std::time::Duration;

    use gpui::{AnyWindowHandle, AsyncApp};
    use swarm_ui::theme::Appearance;

    use crate::notifications::SampleDelivery;

    const SETTLE: Duration = Duration::from_millis(700);

    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        for (appearance, suffix) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
            cx.update(|cx| theme::set_appearance(appearance, cx));

            for (page, name, window_size, permission) in SNAPSHOT_PAGES {
                let path = directory.join(format!("settings-{name}-{suffix}.png"));

                let opened = cx.update(|cx| {
                    notifications::use_delivery(Rc::new(SampleDelivery(permission)), cx);

                    let state = cx.new(|_| SettingsState::sample(None));
                    state.update(cx, SettingsState::sample_modes);

                    // The Honey accent with the Swarm view honeycomb on, to see both.
                    let honey = name == "appearance-honey";
                    if honey {
                        state.update(cx, |state, _| {
                            state.settings.appearance = AppearanceSettings {
                                accent: AccentSetting::Honey,
                                swarm_view_honeycomb: true,
                                ..AppearanceSettings::default()
                            };
                        });
                    }
                    theme::set_accent(
                        if honey {
                            theme::Accent::Honey
                        } else {
                            theme::Accent::Blue
                        },
                        cx,
                    );
                    // Set before the first frame: the answer of the delivery comes later.
                    state.update(cx, |state, _| state.permission = Some(permission));

                    create_window(state, page, window_size, cx)
                });
                let window: AnyWindowHandle = match opened {
                    Ok(handle) => {
                        if name == "general" {
                            show_install_output(handle, cx);
                        }

                        if name == "prompts-reset" {
                            let asked = cx.update(|cx| {
                                handle.update(cx, |settings, _, cx| {
                                    settings.prompts.update(cx, PromptsPage::ask_for_snapshot);
                                })
                            });

                            if let Err(error) = asked {
                                log::error!("cannot ask the question: {error:#}");
                            }
                        }

                        handle.into()
                    }
                    Err(error) => {
                        log::error!("cannot open window: {error:#}");
                        continue;
                    }
                };

                cx.background_executor().timer(SETTLE).await;

                // A terminal screen arrives after the first frame, and a minimized window
                // draws no more.
                if name == "general"
                    && let Err(error) = window.update(cx, |_, window, cx| window.draw(cx).clear(cx))
                {
                    log::error!("cannot draw: {error:#}");
                }

                if let Err(error) = window.update(cx, |_, window, _| window.minimize_window()) {
                    log::error!("cannot minimize: {error:#}");
                }

                cx.background_executor().timer(SETTLE).await;

                match window.update(cx, |_, window, _| window.render_to_image()) {
                    Ok(Ok(image)) => match image.save(&path) {
                        Ok(()) => log::info!("saved {}", path.display()),
                        Err(error) => log::error!("cannot save {}: {error}", path.display()),
                    },
                    Ok(Err(error)) => log::error!("cannot render {}: {error:#}", path.display()),
                    Err(error) => log::error!("window closed: {error:#}"),
                }

                if let Err(error) = window.update(cx, |_, window, _| window.remove_window()) {
                    log::error!("cannot close window: {error:#}");
                }
            }
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use gpui::{Focusable, TestAppContext};

    use super::*;
    use crate::live_probe::{assert_named, reversed};
    use crate::notifications::harness::{Recorder, known_authorization};
    use crate::notifications::{Authorization, Delivery};

    fn setup(cx: &mut TestAppContext) {
        cx.update(|cx| {
            theme::init(cx);
            bind_keys(cx);
        });
    }

    fn open_settings(cx: &mut TestAppContext) -> WindowHandle<SettingsWindow> {
        cx.update(|cx| open_window(None, None, cx))
            .expect("window opens")
    }

    #[test]
    fn the_notification_picker_of_a_project_names_the_setting_and_the_project() {
        assert_eq!(
            pages::project_picker_label("swarm"),
            "Notifications for swarm"
        );
    }

    #[gpui::test]
    fn the_agent_fields_name_their_agent(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);

        let labels = window
            .read_with(cx, |settings, cx| {
                settings
                    .inputs
                    .agents
                    .iter()
                    .flat_map(|agent| {
                        [
                            &agent.command,
                            &agent.orchestrator_model,
                            &agent.worker_model,
                        ]
                    })
                    .map(|input| input.read(cx).label().to_string())
                    .collect::<Vec<_>>()
            })
            .expect("window is open");

        assert_eq!(
            labels,
            [
                "Claude Code command",
                "Claude Code orchestrator model",
                "Claude Code worker model",
                "Codex command",
                "Codex orchestrator model",
                "Codex worker model",
            ]
        );
    }

    #[gpui::test]
    fn opening_at_a_page_focuses_its_row_in_the_list(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);

        cx.update(|cx| {
            open(
                None,
                Vec::new(),
                WeakEntity::new_invalid(),
                Some(Page::Notifications),
                cx,
            );
        });
        cx.run_until_parked();

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let row = window
            .read_with(&visual, |settings, cx| {
                settings
                    .nav
                    .handle(settings.nav_index(Page::Notifications, cx))
            })
            .expect("window is open");
        assert!(visual.update(|window, _| row.is_focused(window)));
    }

    #[gpui::test]
    fn focus_goes_back_to_the_list_when_the_focused_control_goes_away(cx: &mut TestAppContext) {
        let (window, mut visual) = short_page(Page::Keyboard, cx);
        assert!(tab_to_the_page(window, &mut visual));

        window
            .update(&mut visual, |settings, _, cx| {
                settings.select_page(Page::Agents, cx);
            })
            .expect("window is open");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();

        let nav = window
            .read_with(&visual, |settings, cx| {
                settings.nav.handle(settings.nav_index(Page::Agents, cx))
            })
            .expect("window is open");
        assert!(visual.update(|window, _| nav.is_focused(window)));
    }

    #[gpui::test]
    fn opening_twice_keeps_one_window(cx: &mut TestAppContext) {
        setup(cx);

        let first = open_settings(cx);
        let second = open_settings(cx);

        assert_eq!(first.window_id(), second.window_id());
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn selecting_nav_rows_switches_pages(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);

        window
            .update(cx, |settings, _, cx| {
                settings.select_page(Page::Notifications, cx);
            })
            .expect("window is open");
        let section = window
            .read_with(cx, |settings, _| settings.page())
            .expect("window is open");

        window
            .update(cx, |settings, _, cx| {
                settings.select_page(Page::Project(ProjectId(1)), cx);
            })
            .expect("window is open");
        let project = window
            .read_with(cx, |settings, _| settings.page())
            .expect("window is open");

        assert_eq!(section, Page::Notifications);
        assert_eq!(project, Page::Project(ProjectId(1)));
    }

    // The test platform panics in `start_window_move`, so a panic means a move began.
    #[gpui::test]
    #[should_panic(expected = "not implemented")]
    fn dragging_the_top_of_the_page_moves_the_window(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        drag_from("settings-page-drag", &mut visual);
    }

    #[gpui::test]
    #[should_panic(expected = "not implemented")]
    fn dragging_the_gap_above_the_page_moves_the_window(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        drag_from("settings-gap-drag", &mut visual);
    }

    #[gpui::test]
    fn the_top_of_the_page_spans_the_page_and_sits_above_its_title(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();

        let strip = visual.debug_bounds("settings-page-drag");
        let page = visual.debug_bounds("settings-page");
        assert!(
            strip.is_some() && page.is_some(),
            "the page is not on screen"
        );

        if let (Some(strip), Some(page)) = (strip, page) {
            assert_eq!(strip.size.width, page.size.width);
            assert_eq!(strip.bottom(), page.top());
        }
    }

    fn drag_from(selector: &'static str, visual: &mut gpui::VisualTestContext) {
        visual.run_until_parked();
        let start = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center();

        visual.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::none());
        visual.simulate_mouse_move(
            start + point(px(6.), px(0.)),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
    }

    #[gpui::test]
    fn down_moves_to_the_next_nav_row(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        visual.dispatch_action(SelectNext);

        let page = window
            .read_with(&visual, |settings, _| settings.page())
            .expect("window is open");
        assert_eq!(page, Page::Terminals);
    }

    #[gpui::test]
    fn only_the_open_page_is_a_tab_stop_in_the_page_list(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        let stops = |visual: &mut gpui::VisualTestContext| -> Vec<bool> {
            window
                .read_with(visual, |settings, _| {
                    settings
                        .nav_handles()
                        .iter()
                        .map(|handle| handle.tab_stop)
                        .collect()
                })
                .expect("window is open")
        };
        let opening = stops(&mut visual);
        visual.dispatch_action(SelectNext);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let after = stops(&mut visual);

        assert_eq!(opening.iter().filter(|stop| **stop).count(), 1);
        assert!(opening[1], "Agents is the open page");
        assert_eq!(after.iter().filter(|stop| **stop).count(), 1);
        assert!(after[2], "Terminals is the next page");
    }

    #[gpui::test]
    fn one_tab_leaves_the_page_list(cx: &mut TestAppContext) {
        cx.update(swarm_ui::init);
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let nav = window
            .read_with(&visual, |settings, _| settings.nav_handles())
            .expect("window is open");
        visual.update(|window, cx| window.draw(cx).clear(cx));

        visual.simulate_keystrokes("tab");

        let in_nav = visual.update(|window, _| nav.iter().any(|handle| handle.is_focused(window)));
        assert!(!in_nav, "Tab stayed in the list");
    }

    #[gpui::test]
    fn home_and_end_open_the_first_and_last_page(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let page = |visual: &mut gpui::VisualTestContext| {
            window
                .read_with(visual, |settings, _| settings.page())
                .expect("window is open")
        };

        visual.simulate_keystrokes("end");
        let last = page(&mut visual);
        visual.simulate_keystrokes("home");
        let first = page(&mut visual);

        let expected_last = window
            .read_with(&visual, |settings, cx| {
                settings.page_at(settings.nav.len() - 1, cx)
            })
            .expect("window is open");
        assert!(matches!(last, Page::Project(_)));
        assert_eq!(last, expected_last);
        assert_eq!(first, Page::General);
    }

    /// Opens `page` in a tall window with focus on its row in the list, and walks Tab and
    /// Shift-Tab through the window.
    fn walk(page: impl Fn(&SettingsWindow, &App) -> Page, cx: &mut TestAppContext) -> Walk {
        cx.update(swarm_ui::init);
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1280.), px(2400.)));

        window
            .update(&mut visual, |settings, window, cx| {
                let wanted = page(settings, cx);
                settings.select_page(wanted, cx);
                settings.sync_nav(cx);
                settings.focus_nav(window, cx);
            })
            .expect("window is open");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();

        let root = window
            .read_with(&visual, |settings, _| settings.root_focus.clone())
            .expect("window is open");

        Walk {
            forward: swarm_ui::test_support::tab_walk(&mut visual, &root, "tab"),
            backward: swarm_ui::test_support::tab_walk(&mut visual, &root, "shift-tab"),
        }
    }

    struct Walk {
        forward: Vec<String>,
        backward: Vec<String>,
    }

    #[gpui::test]
    fn tab_walks_the_general_page_in_order_and_shift_tab_walks_it_back(cx: &mut TestAppContext) {
        let walk = walk(|_, _| Page::General, cx);

        assert_eq!(
            walk.forward,
            [
                "ListItem General",
                "Button Check again",
                "Button Allow notifications",
                "Switch Check for updates automatically",
                "Button Check now",
                "Switch Open as draft",
                "Link Repository, vscode-icons on GitHub",
                "Link License, CC BY-SA 4.0",
            ]
        );
        assert_eq!(walk.backward, reversed(&walk.forward));
        assert_named(&walk.forward);
    }

    #[gpui::test]
    fn tab_walks_a_project_page_and_shift_tab_walks_it_back(cx: &mut TestAppContext) {
        let walk = walk(
            |settings, cx| settings.page_at(Page::SECTIONS.len(), cx),
            cx,
        );

        assert_eq!(
            walk.forward,
            [
                "ListItem swarm",
                "ComboBox Base branch",
                "TextInput Worktree folder",
                "TextInput Port range",
                "TextInput Ports per worktree",
                "MultilineTextInput Setup script",
                "TextInput File to copy 1",
                "Button Stop copying .env",
                "TextInput File to copy 2",
                "Button Stop copying CLAUDE.local.md",
                "Button Add file",
                "TextInput Check 1",
                "Button Remove cargo test --workspace",
                "TextInput Check 2",
                "Button Remove cargo clippy --workspace",
                "Button Add check",
                "Switch Required to pass",
                "Button Run checks now",
                "Button Open worker",
                "Button Remove project\u{2026}",
            ]
        );
        assert_eq!(walk.backward, reversed(&walk.forward));
        assert_named(&walk.forward);
    }

    #[gpui::test]
    fn tab_scrolls_a_control_below_the_fold_into_view(cx: &mut TestAppContext) {
        let (window, mut visual) = short_page(Page::Appearance, cx);
        let scrolled = |visual: &mut gpui::VisualTestContext| {
            window
                .read_with(visual, |settings, _| settings.page_scroll.offset().y)
                .expect("window is open")
        };
        let before = scrolled(&mut visual);

        for _ in 0..6 {
            visual.simulate_keystrokes("tab");
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }

        assert_eq!(before, Pixels::ZERO);
        assert!(scrolled(&mut visual) < Pixels::ZERO);
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("swarm-settings-{name}-{}.toml", std::process::id()))
    }

    fn open_with(path: &Path, cx: &mut TestAppContext) -> Entity<SettingsState> {
        setup(cx);
        let window = cx
            .update(|cx| open_window(Some(path.to_path_buf()), None, cx))
            .expect("window opens");

        window
            .read_with(cx, |settings, _| settings.state.clone())
            .expect("window is open")
    }

    #[gpui::test]
    fn the_window_loads_the_settings_file(cx: &mut TestAppContext) {
        let path = temp_path("load");
        let mut saved = Settings::default();
        saved.agents.orchestrator = swarm_settings::AgentKindSetting::Codex;
        saved.agents.codex.worker_model = "gpt-6-sol".to_owned();
        swarm_settings::save(&path, &saved).expect("write the settings");

        let state = open_with(&path, cx);

        let loaded = state.read_with(cx, |state, _| state.settings.clone());
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, saved);
    }

    #[gpui::test]
    fn a_bad_value_loads_with_a_warning_on_its_page_and_a_save_corrects_the_file(
        cx: &mut TestAppContext,
    ) {
        let path = temp_path("warnings");
        std::fs::write(
            &path,
            "[agents]\nskip_permissions = \"yes\"\nmax_agents_total = 5\n\n\
             [terminals]\nconfirm_close = false\n",
        )
        .expect("write the settings");

        let state = open_with(&path, cx);
        let (agents, terminals, loaded) = state.read_with(cx, |state, _| {
            (
                state.warnings_of("agents"),
                state.warnings_of("terminals"),
                state.settings.agents.max_agents_total,
            )
        });
        state.update(cx, SettingsState::save);
        let (after, saved) = state.read_with(cx, |state, _| {
            (state.warnings.clone(), std::fs::read_to_string(&path))
        });
        let text = saved.expect("read the file");
        std::fs::remove_file(&path).ok();

        assert_eq!(
            agents,
            ["agents.skip_permissions: not a true/false value, using false"]
        );
        assert!(terminals.is_empty(), "a page shows only its own keys");
        assert_eq!(loaded, 5, "the other keys still load");
        assert!(after.is_empty());
        assert!(text.contains("skip_permissions = false"), "{text}");
    }

    #[gpui::test]
    fn a_file_that_is_not_toml_keeps_the_error_and_saves_nothing(cx: &mut TestAppContext) {
        let path = temp_path("syntax");
        std::fs::write(&path, "[agents\n").expect("write the settings");

        let state = open_with(&path, cx);
        state.update(cx, SettingsState::save);
        let (error, text) = (
            state.read_with(cx, |state, _| state.save_error.is_some()),
            std::fs::read_to_string(&path).expect("read the file"),
        );
        std::fs::remove_file(&path).ok();

        assert!(error);
        assert_eq!(text, "[agents\n");
    }

    #[test]
    fn the_agent_version_comes_from_the_check() {
        use swarm_protocol::{ToolCheck, ToolState};

        let check = |state, version: Option<&str>| ToolCheck {
            state,
            version: version.map(str::to_owned),
            detail: None,
            fix: Vec::new(),
        };

        let ready = pages::version_pill(Some(&check(ToolState::Ready, Some("2.1.314"))));
        let signed_out = pages::version_pill(Some(&check(ToolState::SignedOut, Some("0.9.0"))));
        let missing = pages::version_pill(Some(&check(ToolState::Missing, None)));
        let no_version = pages::version_pill(Some(&check(ToolState::Ready, None)));
        let unanswered = pages::version_pill(None);

        assert_eq!(ready, Some((AgentStatus::Done, "2.1.314".to_owned())));
        assert_eq!(signed_out, Some((AgentStatus::Done, "0.9.0".to_owned())));
        assert_eq!(missing, Some((AgentStatus::Error, "Not found".to_owned())));
        assert_eq!(no_version, None);
        assert_eq!(unanswered, None, "no pill before the check answers");
    }

    #[gpui::test]
    fn the_agents_page_shows_the_versions_of_the_latest_check(cx: &mut TestAppContext) {
        use swarm_protocol::{SystemCheck, ToolCheck, ToolState};

        let check = |state, version: Option<&str>| ToolCheck {
            state,
            version: version.map(str::to_owned),
            detail: None,
            fix: Vec::new(),
        };
        setup(cx);
        let window = open_settings(cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let shown = |visual: &mut gpui::VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));

            (
                visual.debug_bounds("agent-version-0").is_some(),
                visual.debug_bounds("agent-version-1").is_some(),
            )
        };
        let before = shown(&mut visual);

        window
            .update(&mut visual, |settings, _, cx| {
                settings.system_check.update(cx, |check_view, cx| {
                    check_view.show_report(
                        SystemCheck {
                            git: check(ToolState::Ready, None),
                            claude: check(ToolState::Ready, Some("2.1.314")),
                            codex: check(ToolState::Missing, None),
                            gh: check(ToolState::Ready, None),
                            github_account: check(ToolState::Ready, None),
                        },
                        cx,
                    );
                });
            })
            .expect("window is open");
        let after = shown(&mut visual);

        assert_eq!(before, (false, false), "no pill before a check answers");
        assert_eq!(after, (true, true));
    }

    #[gpui::test]
    fn an_edit_is_saved_after_the_pause(cx: &mut TestAppContext) {
        let path = temp_path("save");
        let state = open_with(&path, cx);

        state.update(cx, |state, cx| {
            state.settings.agents.skip_permissions = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        let early = swarm_settings::load(&path).expect("read the settings");

        state.update(cx, |state, cx| {
            state.settings.agents.claude_code.worker_model = "haiku".to_owned();
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        let restarted = swarm_settings::load(&path).expect("read the settings");

        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        let saved = swarm_settings::load(&path).expect("read the settings");
        std::fs::remove_file(&path).ok();

        assert_eq!(early, Settings::default());
        assert_eq!(restarted, Settings::default());
        assert!(saved.agents.skip_permissions);
        assert_eq!(saved.agents.claude_code.worker_model, "haiku");
    }

    #[gpui::test]
    fn turning_off_worker_typing_is_saved(cx: &mut TestAppContext) {
        let path = temp_path("worker-typing");
        let state = open_with(&path, cx);
        let before = cx.update(|cx| allow_worker_typing(cx));

        state.update(cx, |state, cx| {
            state.settings.terminals.allow_worker_typing = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        let saved = swarm_settings::load(&path).expect("read the settings");
        let after = cx.update(|cx| allow_worker_typing(cx));
        std::fs::remove_file(&path).ok();

        assert!(before);
        assert!(!after);
        assert!(!saved.terminals.allow_worker_typing);
    }

    #[gpui::test]
    fn closing_a_terminal_asks_until_the_settings_say_otherwise(cx: &mut TestAppContext) {
        let path = temp_path("confirm-close");
        let state = open_with(&path, cx);
        let before = cx.update(|cx| confirm_terminal_close(cx));

        state.update(cx, |state, cx| {
            state.settings.terminals.confirm_close = false;
            cx.notify();
        });
        let off = cx.update(|cx| confirm_terminal_close(cx));
        state.update(cx, |state, cx| {
            state.settings.terminals.confirm_close = true;
            cx.notify();
        });
        let on_again = cx.update(|cx| confirm_terminal_close(cx));
        std::fs::remove_file(&path).ok();

        assert!(before);
        assert!(!off);
        assert!(on_again);
    }

    #[gpui::test]
    fn do_not_show_again_is_saved_at_once_without_a_settings_window(cx: &mut TestAppContext) {
        let path = temp_path("stop-confirming");
        cx.update(|cx| load(Some(path.clone()), cx));

        cx.update(stop_confirming_terminal_close);
        let saved = swarm_settings::load(&path).expect("read the settings");
        let asks = cx.update(|cx| confirm_terminal_close(cx));
        std::fs::remove_file(&path).ok();

        assert!(!saved.terminals.confirm_close);
        assert!(!asks);
    }

    #[gpui::test]
    fn a_saved_worker_typing_choice_applies_without_opening_the_window(cx: &mut TestAppContext) {
        let path = temp_path("worker-typing-start");
        let mut saved = Settings::default();
        saved.terminals.allow_worker_typing = false;
        swarm_settings::save(&path, &saved).expect("write the settings");
        let before = cx.update(|cx| allow_worker_typing(cx));

        cx.update(|cx| load(Some(path.clone()), cx));

        let after = cx.update(|cx| allow_worker_typing(cx));
        std::fs::remove_file(&path).ok();
        assert!(before);
        assert!(!after);
    }

    #[gpui::test]
    fn a_file_that_cannot_be_read_shows_an_error(cx: &mut TestAppContext) {
        let path = temp_path("invalid");
        std::fs::write(&path, "agents = [").expect("write the file");

        let state = open_with(&path, cx);

        let error = state.read_with(cx, |state, _| state.save_error.clone());
        std::fs::remove_file(&path).ok();
        assert!(error.is_some());
    }

    #[gpui::test]
    fn a_file_that_cannot_be_read_is_never_overwritten(cx: &mut TestAppContext) {
        let path = temp_path("keep-invalid");
        std::fs::write(&path, "agents = [").expect("write the file");
        let state = open_with(&path, cx);

        state.update(cx, |state, cx| {
            state.settings.agents.skip_permissions = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        state.update(cx, SettingsState::save);

        let text = std::fs::read_to_string(&path).expect("read the file");
        let error = state.read_with(cx, |state, _| state.save_error.clone());
        std::fs::remove_file(&path).ok();
        assert_eq!(text, "agents = [");
        assert!(error.is_some());
    }

    #[gpui::test]
    fn typing_in_the_command_input_changes_the_setting(cx: &mut TestAppContext) {
        cx.update(swarm_ui::init);
        let path = temp_path("typing");
        let state = open_with(&path, cx);
        let window = cx.windows()[0]
            .downcast::<SettingsWindow>()
            .expect("settings window");
        let input = window
            .read_with(cx, |settings, _| settings.inputs.agents[1].command.clone())
            .expect("window is open");
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        input.update(&mut visual, |input, cx| input.set_text("", cx));
        let focus = input.read_with(&visual, Focusable::focus_handle);
        visual.update(|window, cx| window.focus(&focus, cx));

        visual.simulate_input("codex-dev");

        let command = state.read_with(&visual, |state, _| {
            state.settings.agents.codex.command.clone()
        });
        assert_eq!(command, "codex-dev");
    }

    fn notifications_page(
        path: &Path,
        projects: Option<Vec<Project>>,
        permission: Permission,
        cx: &mut TestAppContext,
    ) -> (Entity<SettingsState>, Rc<Recorder>, gpui::VisualTestContext) {
        cx.update(swarm_ui::init);
        setup(cx);
        let recorder = Recorder::new(permission);
        let delivery: Rc<dyn Delivery> = Rc::clone(&recorder) as Rc<dyn Delivery>;
        cx.update(|cx| notifications::use_delivery(delivery, cx));
        let window = cx
            .update(|cx| open_window(Some(path.to_path_buf()), projects, cx))
            .expect("window opens");
        window
            .update(cx, |settings, _, cx| {
                settings.select_page(Page::Notifications, cx);
            })
            .expect("window is open");
        let state = window
            .read_with(cx, |settings, _| settings.state.clone())
            .expect("window is open");

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1280.), px(2400.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));

        (state, recorder, visual)
    }

    fn click(selector: &'static str, visual: &mut gpui::VisualTestContext) {
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));

        visual.simulate_click(bounds.center(), gpui::Modifiers::none());
        visual.run_until_parked();
    }

    fn save_pause(cx: &mut gpui::VisualTestContext) {
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
    }

    fn project_at(name: &'static str, root: &str) -> Project {
        Project {
            id: ProjectId(0),
            name: name.into(),
            letter: 'D',
            tile_color: gpui::rgb(0x0058_90e0).into(),
            base_branch: "main".into(),
            daemon: None,
            root: Some(PathBuf::from(root)),
            config_error: None,
        }
    }

    #[gpui::test]
    fn a_trigger_switch_on_the_page_is_saved(cx: &mut TestAppContext) {
        let path = temp_path("notify-when");
        let (_state, _recorder, mut visual) =
            notifications_page(&path, None, Permission::Allowed, cx);

        click("notify-when-1", &mut visual);
        click("notify-when-3", &mut visual);
        save_pause(&mut visual);

        let saved = swarm_settings::load(&path).expect("read the settings");
        std::fs::remove_file(&path).ok();
        assert!(!saved.notifications.prompt);
        assert!(!saved.notifications.broke);
        assert!(saved.notifications.needs_decision);
        assert!(saved.notifications.ready_for_review);
    }

    #[gpui::test]
    fn a_delivery_switch_on_the_page_is_saved(cx: &mut TestAppContext) {
        let path = temp_path("delivery");
        let (_state, _recorder, mut visual) =
            notifications_page(&path, None, Permission::Allowed, cx);

        for index in ["delivery-0", "delivery-1", "delivery-2", "delivery-3"] {
            click(index, &mut visual);
        }
        save_pause(&mut visual);

        let saved = swarm_settings::load(&path).expect("read the settings");
        std::fs::remove_file(&path).ok();
        assert!(saved.notifications.sound);
        assert!(!saved.notifications.dock_bounce);
        assert!(!saved.notifications.mute_while_viewing);
        assert!(!saved.notifications.menu_bar_dot);
    }

    #[gpui::test]
    fn the_page_shows_the_saved_notification_settings(cx: &mut TestAppContext) {
        let path = temp_path("shows-saved");
        let mut saved = Settings::default();
        saved.notifications.sound = true;
        saved.notifications.ready_for_review = false;
        swarm_settings::save(&path, &saved).expect("write the settings");

        let (state, _recorder, _visual) = notifications_page(&path, None, Permission::Allowed, cx);

        let loaded = state.read_with(cx, |state, _| state.settings.notifications.clone());
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, saved.notifications);
    }

    #[gpui::test]
    fn the_project_mode_picker_is_saved_under_the_project_path(cx: &mut TestAppContext) {
        let path = temp_path("project-mode");
        let projects = vec![
            project_at("alpha", "/work/alpha"),
            project_at("beta", "/work/beta"),
        ];
        let (state, _recorder, mut visual) =
            notifications_page(&path, Some(projects), Permission::Allowed, cx);

        click("project-notifications-1", &mut visual);
        visual.simulate_keystrokes("down down enter");
        save_pause(&mut visual);

        let saved = swarm_settings::load(&path).expect("read the settings");
        let text = std::fs::read_to_string(&path).expect("read the file");
        let names = state.read_with(&visual, |state, _| {
            state
                .projects
                .iter()
                .map(|entry| entry.project.name.to_string())
                .collect::<Vec<_>>()
        });
        std::fs::remove_file(&path).ok();
        assert_eq!(names, ["alpha", "beta"]);
        assert_eq!(
            saved.notifications.mode(Path::new("/work/beta")),
            ProjectMode::Off
        );
        assert_eq!(
            saved.notifications.mode(Path::new("/work/alpha")),
            ProjectMode::All
        );
        assert!(text.contains("\"/work/beta\" = \"off\""));
    }

    #[gpui::test]
    fn the_picker_shows_the_saved_mode_of_a_project(cx: &mut TestAppContext) {
        let path = temp_path("project-mode-shown");
        let mut saved = Settings::default();
        saved
            .notifications
            .set_mode(Path::new("/work/alpha"), ProjectMode::ErrorsOnly);
        swarm_settings::save(&path, &saved).expect("write the settings");
        let projects = vec![project_at("alpha", "/work/alpha")];
        let (state, _recorder, mut visual) =
            notifications_page(&path, Some(projects), Permission::Allowed, cx);

        click("project-notifications-0", &mut visual);
        visual.simulate_keystrokes("down enter");
        save_pause(&mut visual);

        let again = swarm_settings::load(&path).expect("read the settings");
        let mode = state.read_with(&visual, |state, _| {
            state.settings.notifications.mode(Path::new("/work/alpha"))
        });
        std::fs::remove_file(&path).ok();
        // The menu opened on the saved mode, so the next row is Off.
        assert_eq!(mode, ProjectMode::Off);
        assert_eq!(
            again.notifications.mode(Path::new("/work/alpha")),
            ProjectMode::Off
        );
    }

    #[gpui::test]
    fn the_permission_row_offers_system_settings_only_when_denied(cx: &mut TestAppContext) {
        for (permission, offered) in [
            (Permission::Allowed, false),
            (Permission::NotAllowed, true),
            (Permission::NotAsked, false),
            (Permission::Unavailable, false),
        ] {
            let path = temp_path("permission");
            let (state, _recorder, mut visual) = notifications_page(&path, None, permission, cx);

            let shown = state.read_with(&visual, |state, _| state.permission);
            let button = visual.debug_bounds("open-system-settings").is_some();
            visual.update(|window, _| window.remove_window());
            visual.run_until_parked();
            std::fs::remove_file(&path).ok();

            assert_eq!(shown, Some(permission));
            assert_eq!(button, offered, "{permission:?}");
        }
    }

    #[gpui::test]
    fn open_system_settings_opens_the_notifications_pane(cx: &mut TestAppContext) {
        let path = temp_path("open-pane");
        let (_state, _recorder, mut visual) =
            notifications_page(&path, None, Permission::NotAllowed, cx);

        click("open-system-settings", &mut visual);

        let url = visual.opened_url();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            url.as_deref(),
            Some("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
        );
    }

    #[gpui::test]
    fn the_permission_is_read_again_when_the_user_comes_back(cx: &mut TestAppContext) {
        let path = temp_path("permission-again");
        let (state, recorder, mut visual) =
            notifications_page(&path, None, Permission::NotAllowed, cx);
        let before = state.read_with(&visual, |state, _| state.permission);

        recorder.permission.set(Permission::Allowed);
        visual.deactivate_window();
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();

        let after = state.read_with(&visual, |state, _| state.permission);
        std::fs::remove_file(&path).ok();
        assert_eq!(before, Some(Permission::NotAllowed));
        assert_eq!(after, Some(Permission::Allowed));
    }

    #[gpui::test]
    fn a_click_on_blank_space_in_a_block_keeps_focus(cx: &mut TestAppContext) {
        let path = temp_path("blank-click");
        let (_state, _recorder, mut visual) =
            notifications_page(&path, None, Permission::NotAllowed, cx);
        click("open-system-settings", &mut visual);
        let before = visual.update(|window, cx| window.focused(cx));

        let block = visual
            .debug_bounds("settings-block-0")
            .expect("block is on screen");
        let blank = block.bottom_right() - point(px(2.), px(2.));
        visual.simulate_click(blank, gpui::Modifiers::none());
        visual.run_until_parked();

        let after = visual.update(|window, cx| window.focused(cx));
        std::fs::remove_file(&path).ok();
        assert!(before.is_some());
        assert_eq!(after, before);
    }

    #[gpui::test]
    fn focus_goes_to_a_named_control_when_open_system_settings_goes_away(cx: &mut TestAppContext) {
        let path = temp_path("button-gone");
        let (_state, recorder, mut visual) =
            notifications_page(&path, None, Permission::NotAllowed, cx);
        click("open-system-settings", &mut visual);

        recorder.permission.set(Permission::Allowed);
        visual.deactivate_window();
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();

        let root = visual.update(|window, cx| {
            window
                .root::<SettingsWindow>()
                .flatten()
                .map(|settings| settings.read(cx).root_focus.clone())
        });
        let root = root.expect("window has a root");
        let stop = swarm_ui::test_support::focused_stop(&mut visual, &root);
        std::fs::remove_file(&path).ok();
        assert!(visual.debug_bounds("open-system-settings").is_none());
        assert!(
            !matches!(stop.as_str(), "unnamed" | "nothing" | "not drawn"),
            "{stop}"
        );
    }

    #[gpui::test]
    fn the_answer_of_macos_is_kept_when_the_page_opens_and_when_the_window_comes_back(
        cx: &mut TestAppContext,
    ) {
        let path = temp_path("permission-kept");
        let (_state, recorder, mut visual) =
            notifications_page(&path, None, Permission::NotAllowed, cx);
        let opened = known_authorization(&visual);

        recorder.permission.set(Permission::Allowed);
        recorder.sound_enabled.set(false);
        visual.deactivate_window();
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();

        let back = known_authorization(&visual);
        std::fs::remove_file(&path).ok();
        assert_eq!(
            opened,
            Some(Authorization {
                permission: Permission::NotAllowed,
                sound: true,
            })
        );
        assert_eq!(
            back,
            Some(Authorization {
                permission: Permission::Allowed,
                sound: false,
            })
        );
    }

    #[gpui::test]
    fn the_test_button_posts_one_notification_at_once(cx: &mut TestAppContext) {
        let path = temp_path("test-button");
        let (_state, recorder, mut visual) =
            notifications_page(&path, None, Permission::Allowed, cx);

        click("test-notification", &mut visual);

        std::fs::remove_file(&path).ok();
        assert_eq!(recorder.tags(), ["test"]);
    }

    #[gpui::test]
    fn the_appearance_control_changes_the_theme(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);

        let state = cx.update(state);

        window
            .update(cx, |_, window, cx| {
                pages::apply_theme(&state)(1, window, cx);
            })
            .expect("window is open");

        assert_eq!(
            cx.update(|cx| theme::appearance(cx)),
            theme::Appearance::Dark
        );
    }

    #[gpui::test]
    fn the_theme_choice_is_saved_in_the_settings(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let state = cx.update(state);

        window
            .update(cx, |_, window, cx| {
                pages::apply_theme(&state)(0, window, cx);
            })
            .expect("window is open");

        assert_eq!(cx.update(|cx| appearance(cx).theme), ThemeSetting::Light);
    }

    #[gpui::test]
    fn the_saved_theme_applies_when_the_settings_load(cx: &mut TestAppContext) {
        setup(cx);
        let path = temp_path("theme-at-start");
        std::fs::write(&path, "[appearance]\ntheme = \"dark\"\n").expect("write the settings");

        cx.update(|cx| load(Some(path.clone()), cx));
        std::fs::remove_file(&path).ok();

        assert_eq!(
            cx.update(|cx| theme::appearance(cx)),
            theme::Appearance::Dark
        );
    }

    fn on_appearance(
        cx: &mut TestAppContext,
    ) -> (WindowHandle<SettingsWindow>, gpui::VisualTestContext) {
        setup(cx);
        let window = open_settings(cx);
        window
            .update(cx, |settings, _, cx| {
                settings.select_page(Page::Appearance, cx);
            })
            .expect("window is open");

        let visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();

        (window, visual)
    }

    fn click_on(selector: &'static str, visual: &mut gpui::VisualTestContext) {
        let bounds = visual
            .debug_bounds(selector)
            .expect("the control is on the page");

        visual.simulate_click(bounds.center(), gpui::Modifiers::none());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn the_easter_eggs_are_on_until_the_switch_turns_them_off(cx: &mut TestAppContext) {
        let (_, mut visual) = on_appearance(cx);
        let before = cx.update(|cx| easter_eggs(cx));

        click_on("easter-eggs", &mut visual);
        let after = cx.update(|cx| easter_eggs(cx));
        let saved = cx.update(|cx| appearance(cx).easter_eggs);

        click_on("easter-eggs", &mut visual);

        assert!(before, "on by default");
        assert!(!after);
        assert!(!saved);
        assert!(cx.update(|cx| easter_eggs(cx)));
    }

    #[gpui::test]
    fn the_honeycomb_switch_needs_the_eggs_and_follows_the_setting(cx: &mut TestAppContext) {
        let (_, mut visual) = on_appearance(cx);
        assert!(!cx.update(|cx| swarm_view_honeycomb(cx)), "off by default");

        click_on("swarm-view-honeycomb", &mut visual);
        let on = cx.update(|cx| swarm_view_honeycomb(cx));

        click_on("easter-eggs", &mut visual);
        let eggs_off = cx.update(|cx| swarm_view_honeycomb(cx));
        let setting_kept = cx.update(|cx| appearance(cx).swarm_view_honeycomb);

        click_on("swarm-view-honeycomb", &mut visual);
        let disabled = cx.update(|cx| appearance(cx).swarm_view_honeycomb);

        assert!(on);
        assert!(!eggs_off, "no eggs, no honeycomb");
        assert!(setting_kept);
        assert!(disabled, "a dimmed switch does not change the setting");
    }

    #[gpui::test]
    fn the_accent_control_switches_to_honey_and_back(cx: &mut TestAppContext) {
        let (window, visual) = on_appearance(cx);
        let state = cx.update(state);
        let colors = |cx: &mut TestAppContext| cx.update(|cx| cx.theme().colors().accent);
        let blue = colors(cx);

        window
            .update(cx, |_, window, cx| {
                pages::apply_accent(&state)(1, window, cx);
            })
            .expect("window is open");
        visual.run_until_parked();
        let (honey, kept) = (
            colors(cx),
            cx.update(|cx| appearance(cx).accent == AccentSetting::Honey),
        );

        window
            .update(cx, |_, window, cx| {
                pages::apply_accent(&state)(0, window, cx);
            })
            .expect("window is open");

        assert_ne!(honey, blue);
        assert!(kept);
        assert_eq!(colors(cx), blue);
        assert_eq!(cx.update(|cx| theme::accent(cx)), theme::Accent::Blue);
    }

    #[gpui::test]
    fn the_appearance_page_lists_theme_accent_and_easter_eggs(cx: &mut TestAppContext) {
        let (_, mut visual) = on_appearance(cx);

        for selector in ["theme", "accent", "easter-eggs", "swarm-view-honeycomb"] {
            assert!(visual.debug_bounds(selector).is_some(), "{selector}");
        }
    }

    /// A short window on `page`, so the page is taller than the window.
    fn short_page(
        page: Page,
        cx: &mut TestAppContext,
    ) -> (WindowHandle<SettingsWindow>, gpui::VisualTestContext) {
        cx.update(swarm_ui::init);
        setup(cx);
        let window = open_settings(cx);
        window
            .update(cx, |settings, _, cx| settings.select_page(page, cx))
            .expect("window is open");

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1280.), px(160.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));

        (window, visual)
    }

    /// Presses Tab until the page container has focus. Returns whether it ever did.
    fn tab_to_the_page(
        window: WindowHandle<SettingsWindow>,
        visual: &mut gpui::VisualTestContext,
    ) -> bool {
        let focus = window
            .read_with(visual, |settings, _| settings.page_focus.clone())
            .expect("window is open");

        (0..40).any(|_| {
            visual.simulate_keystrokes("tab");

            visual.update(|window, _| focus.is_focused(window))
        })
    }

    fn top_of(selector: &'static str, visual: &mut gpui::VisualTestContext) -> f32 {
        visual.update(|window, cx| window.draw(cx).clear(cx));

        visual
            .debug_bounds(selector)
            .map_or(f32::NAN, |bounds| f32::from(bounds.origin.y))
    }

    #[gpui::test]
    fn the_keyboard_and_usage_pages_scroll_with_the_arrows_page_keys_home_and_end(
        cx: &mut TestAppContext,
    ) {
        for (page, row) in [
            (Page::Keyboard, "shortcut-open-settings"),
            (Page::Usage, "popover-usage-claude"),
        ] {
            let (window, mut visual) = short_page(page, cx);
            let reached = tab_to_the_page(window, &mut visual);

            let start = top_of(row, &mut visual);
            visual.simulate_keystrokes("down");
            let one_line = top_of(row, &mut visual);
            visual.simulate_keystrokes("pagedown");
            let one_page = top_of(row, &mut visual);
            visual.simulate_keystrokes("pageup");
            let back = top_of(row, &mut visual);
            visual.simulate_keystrokes("end");
            let end = top_of(row, &mut visual);
            visual.simulate_keystrokes("home");
            let home = top_of(row, &mut visual);
            visual.simulate_keystrokes("up");
            let clamped = top_of(row, &mut visual);

            assert!(reached, "{page:?}: Tab never reached the page");
            assert!(
                (one_line - (start - 40.)).abs() < 0.5,
                "{page:?}: down moves a line"
            );
            assert!(
                one_page < one_line - 40.,
                "{page:?}: page down moves further"
            );
            assert!(back > one_page, "{page:?}: page up goes back");
            assert!(end < one_page, "{page:?}: end goes to the bottom");
            assert!((home - start).abs() < 0.5, "{page:?}: home goes to the top");
            assert!(
                (clamped - start).abs() < 0.5,
                "{page:?}: it stops at the top"
            );

            visual.update(|window, _| window.remove_window());
            visual.run_until_parked();
        }
    }

    #[gpui::test]
    fn a_page_with_controls_adds_no_stop_for_the_page_itself(cx: &mut TestAppContext) {
        let (window, mut visual) = short_page(Page::Agents, cx);

        assert!(!tab_to_the_page(window, &mut visual));
    }

    #[gpui::test]
    fn a_page_opens_at_its_top(cx: &mut TestAppContext) {
        let (window, mut visual) = short_page(Page::Keyboard, cx);
        tab_to_the_page(window, &mut visual);
        visual.simulate_keystrokes("end");

        window
            .update(&mut visual, |settings, _, cx| {
                settings.select_page(Page::Usage, cx);
            })
            .expect("window is open");
        let on_opening = top_of("popover-usage-claude", &mut visual);
        tab_to_the_page(window, &mut visual);
        visual.simulate_keystrokes("home");
        let at_the_top = top_of("popover-usage-claude", &mut visual);

        assert!(
            (on_opening - at_the_top).abs() < 0.5,
            "the page opened scrolled: {on_opening} against {at_the_top}"
        );
    }

    fn numeric_input(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TextInput>,
        Entity<SettingsState>,
        gpui::VisualTestContext,
    ) {
        cx.update(swarm_ui::init);
        setup(cx);
        let window = open_settings(cx);
        let (input, state) = window
            .read_with(cx, |settings, _| {
                (
                    settings.inputs.max_agents_per_task.clone(),
                    settings.state.clone(),
                )
            })
            .expect("window is open");
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);

        input.update(&mut visual, |input, cx| input.set_text("", cx));
        let focus = input.read_with(&visual, Focusable::focus_handle);
        visual.update(|window, cx| window.focus(&focus, cx));

        (input, state, visual)
    }

    #[gpui::test]
    fn numeric_inputs_reject_letters_when_typed(cx: &mut TestAppContext) {
        let (input, state, mut visual) = numeric_input(cx);

        visual.simulate_input("1a2b");

        let text = input.read_with(&visual, |input, _| input.text());
        let stored = state.read_with(&visual, |state, _| {
            state.settings.agents.max_agents_per_task
        });
        assert_eq!(text.as_ref(), "12");
        assert_eq!(stored, 12);
    }

    #[gpui::test]
    fn a_limit_below_one_shows_the_error_and_is_not_saved(cx: &mut TestAppContext) {
        let (_, state, mut visual) = numeric_input(cx);
        let read = |visual: &gpui::VisualTestContext| {
            state.read_with(visual, |state, _| {
                (
                    state.settings.agents.max_agents_per_task,
                    state.max_agents_per_task_invalid,
                )
            })
        };

        visual.simulate_input("4");
        let valid = read(&visual);
        visual.simulate_keystrokes("backspace");
        let empty = read(&visual);
        visual.simulate_input("0");
        let zero = read(&visual);
        visual.simulate_input("5");
        let fixed = read(&visual);

        assert_eq!(valid, (4, false));
        assert_eq!(empty, (4, true), "no number keeps the last good limit");
        assert_eq!(zero, (4, true));
        assert_eq!(fixed, (5, false), "05 is the number 5");
    }

    #[gpui::test]
    fn an_invalid_limit_puts_the_field_in_an_error_state_until_fixed(cx: &mut TestAppContext) {
        let (input, _, mut visual) = numeric_input(cx);
        let error = |visual: &gpui::VisualTestContext| {
            input.read_with(visual, |input, _| input.error().cloned())
        };

        visual.simulate_input("0");
        let invalid = error(&visual);
        visual.simulate_input("5");
        let fixed = error(&visual);

        assert_eq!(invalid.as_deref(), Some(MAX_AGENTS_PER_TASK_ERROR));
        assert_eq!(fixed, None);
    }

    #[gpui::test]
    fn the_limit_fields_show_the_settings_and_a_save_keeps_them(cx: &mut TestAppContext) {
        cx.update(swarm_ui::init);
        setup(cx);
        let path = temp_path("limits");
        std::fs::write(
            &path,
            "[agents]\nmax_agents_per_task = 3\nmax_agents_total = 9\nidle_stop_minutes = 60\n",
        )
        .expect("write the settings");
        let window = cx
            .update(|cx| open_window(Some(path.clone()), None, cx))
            .expect("window opens");

        let (per_task, total) = window
            .read_with(cx, |settings, cx| {
                (
                    settings
                        .inputs
                        .max_agents_per_task
                        .read(cx)
                        .text()
                        .to_string(),
                    settings.inputs.max_agents_total.read(cx).text().to_string(),
                )
            })
            .expect("window is open");
        std::fs::remove_file(&path).ok();

        assert_eq!(per_task, "3");
        assert_eq!(total, "9");
    }

    #[test]
    fn the_idle_options_map_to_minutes_and_back() {
        let minutes: Vec<Option<u32>> = (0..5).map(idle_minutes).collect();
        let indexes: Vec<usize> = [10, 30, 60, 0].map(idle_index).to_vec();

        assert_eq!(minutes, [Some(10), Some(30), Some(60), Some(0), None]);
        assert_eq!(indexes, [0, 1, 2, 3]);
    }

    #[test]
    fn a_stored_idle_value_that_is_not_an_option_shows_as_the_nearest_one() {
        let nearest: Vec<usize> = [1, 14, 25, 44, 46, 200].map(idle_index).to_vec();

        assert_eq!(nearest, [0, 0, 1, 1, 2, 2]);
    }

    #[gpui::test]
    fn picking_an_idle_option_saves_its_minutes_and_a_stored_value_stays_until_then(
        cx: &mut TestAppContext,
    ) {
        setup(cx);
        let window = open_settings(cx);
        let state = window
            .read_with(cx, |settings, _| settings.state.clone())
            .expect("window is open");
        state.update(cx, |state, _| state.settings.agents.idle_stop_minutes = 45);
        let kept = state.read_with(cx, |state, _| state.settings.agents.idle_stop_minutes);

        state.update(cx, |state, _| pages::set_idle_stop(state, 3));
        let never = state.read_with(cx, |state, _| state.settings.agents.idle_stop_minutes);

        assert_eq!(kept, 45);
        assert_eq!(never, 0);
    }

    #[gpui::test]
    fn numeric_inputs_filter_pasted_text(cx: &mut TestAppContext) {
        let (input, _, mut visual) = numeric_input(cx);

        visual.write_to_clipboard(gpui::ClipboardItem::new_string("3x4 5".to_owned()));
        visual.simulate_keystrokes("cmd-v");

        let text = input.read_with(&visual, |input, _| input.text());
        assert_eq!(text.as_ref(), "345");
    }

    #[gpui::test]
    fn undo_still_works_after_rejected_letters(cx: &mut TestAppContext) {
        let (input, _, mut visual) = numeric_input(cx);

        visual.simulate_input("12");
        visual.simulate_input("z");
        visual.simulate_keystrokes("cmd-z");

        let text = input.read_with(&visual, |input, _| input.text());
        assert_eq!(text.as_ref(), "");
    }
}
