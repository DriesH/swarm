//! The settings window: a nav of sections and projects, and one page at a time.

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
    App, Bounds, Context, Entity, FocusHandle, Global, KeyBinding, Pixels, Role, ScrollHandle,
    SharedString, Size, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, point, px, size,
};
use swarm_settings::{CliSettings, NotificationSettings, ProjectMode, Settings};
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::theme;
use swarm_ui::{ROOT_CONTEXT, SelectNext, SelectPrevious, TextFieldSize, scroll_for_key};

use self::prompts_page::PromptsPage;
use self::usage_page::UsagePage;
use crate::model::{FakeData, Project};
use crate::notifications::{self, Permission};
use crate::window_drag::drag_region;
use crate::workspace::Workspace;
use project::{ProjectInputs, ProjectSave, ProjectSettings};

actions!(swarm_app, [CloseSettings]);

const SETTINGS_CONTEXT: &str = "Settings";
const NAV_CONTEXT: &str = "SettingsNav";

/// Edits are saved once typing pauses this long.
const SAVE_DELAY: Duration = Duration::from_millis(300);

const NOT_CONNECTED: &str = "Not connected to the daemon";

/// How far an arrow key scrolls a page without controls.
const PAGE_LINE: Pixels = px(40.);

const WINDOW_SIZE: Size<Pixels> = size(px(1280.), px(900.));
const MIN_WINDOW_SIZE: Size<Pixels> = size(px(900.), px(600.));

pub(crate) struct AgentInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub icon: IconName,
}

pub(crate) const AGENTS: [AgentInfo; 2] = [
    AgentInfo {
        name: "Claude Code",
        version: "2.1.314",
        icon: IconName::ClaudeCode,
    },
    AgentInfo {
        name: "Codex",
        version: "0.154.0",
        icon: IconName::Codex,
    },
];

pub(crate) const IDLE_OPTIONS: [&str; 4] = ["10 minutes", "30 minutes", "1 hour", "Never"];
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
    pub idle_stop: usize,
    pub max_agents_per_task: String,
    pub max_agents_total: String,
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
            idle_stop: 1,
            max_agents_per_task: "6".to_owned(),
            max_agents_total: "16".to_owned(),
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

        match swarm_settings::load(path) {
            Ok(settings) => {
                self.settings = settings;
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

        self.save_error = swarm_settings::save(path, &self.settings)
            .err()
            .map(|error| error.to_string().into());
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
    Project(usize),
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

    /// Position in the nav: sections first, then projects.
    fn nav_index(self) -> usize {
        match self {
            Self::Project(index) => Self::SECTIONS.len() + index,
            section => Self::SECTIONS
                .iter()
                .position(|candidate| *candidate == section)
                .unwrap_or(0),
        }
    }

    fn from_nav_index(index: usize) -> Self {
        Self::SECTIONS
            .get(index)
            .copied()
            .unwrap_or_else(|| Self::Project(index - Self::SECTIONS.len()))
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
    nav_focus: Vec<FocusHandle>,
    inputs: Inputs,
    prompts: Entity<PromptsPage>,
    usage: Entity<UsagePage>,
    /// Where a page without controls takes keyboard focus, to scroll with the keys.
    page_focus: FocusHandle,
    page_scroll: ScrollHandle,
    subscriptions: Vec<Subscription>,
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
            cx.observe_window_activation(window, |this, window, cx| {
                // The user may come back from System Settings with a new answer.
                if window.is_window_active() {
                    this.refresh_permission(cx);
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
        let settings = state.read(cx);
        let (max_agents_per_task, max_agents_total) = (
            settings.max_agents_per_task.clone(),
            settings.max_agents_total.clone(),
        );
        let projects = settings.projects.clone();

        let agent_inputs = [0, 1].map(|index| {
            let current = cli(&agents, index);
            let mut input = |label: &str,
                             text: &str,
                             apply: fn(&mut CliSettings) -> &mut String,
                             cx: &mut Context<Self>| {
                let input = field(label.to_owned(), text, true, cx);
                bind_agent_text(
                    &mut subscriptions,
                    &input,
                    &state,
                    move |agents, text| *apply(cli_mut(agents, index)) = text,
                    cx,
                );
                input
            };

            CliInputs {
                command: input("Command", &current.command, |cli| &mut cli.command, cx),
                orchestrator_model: input(
                    "Orchestrator model",
                    &current.orchestrator_model,
                    |cli| &mut cli.orchestrator_model,
                    cx,
                ),
                worker_model: input(
                    "Worker model",
                    &current.worker_model,
                    |cli| &mut cli.worker_model,
                    cx,
                ),
            }
        });

        let max_agents_per_task = numeric_field("Max agents per task", &max_agents_per_task, cx);
        bind_text(
            &mut subscriptions,
            &max_agents_per_task,
            &state,
            |state, text| state.max_agents_per_task = text,
            cx,
        );

        let max_agents_total = numeric_field("Max agents in total", &max_agents_total, cx);
        bind_text(
            &mut subscriptions,
            &max_agents_total,
            &state,
            |state, text| state.max_agents_total = text,
            cx,
        );

        let mut this = Self {
            state: state.clone(),
            page,
            nav_focus: (0..Page::SECTIONS.len() + projects.len())
                .map(|_| cx.focus_handle().tab_stop(true))
                .collect(),
            inputs: Inputs {
                agents: agent_inputs,
                max_agents_per_task,
                max_agents_total,
                projects: Vec::new(),
            },
            prompts: cx.new(|cx| PromptsPage::new(state.clone(), cx)),
            usage: cx.new(|cx| UsagePage::new(state.clone(), cx)),
            page_focus: cx.focus_handle().tab_stop(true),
            page_scroll: ScrollHandle::new(),
            subscriptions,
        };

        for index in 0..projects.len() {
            if let Some(inputs) = this.project_inputs(index, cx) {
                this.inputs.projects.push(inputs);
                this.sync_inputs(index, cx);
            }
        }

        this.focus_nav(window, cx);
        this.refresh_permission(cx);
        this.refresh_prompts(cx);
        this.open_project(cx);

        this
    }

    #[cfg(test)]
    pub(crate) fn page(&self) -> Page {
        self.page
    }

    pub(crate) fn select_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.page_scroll.set_offset(point(px(0.), px(0.)));
        self.refresh_permission(cx);
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

    fn focus_nav(&self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.nav_focus.get(self.page.nav_index()) {
            window.focus(handle, cx);
        }
    }

    fn move_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.nav_focus.len().saturating_sub(1);
        let next = self.page.nav_index().saturating_add_signed(step).min(last);

        self.page = Page::from_nav_index(next);
        self.page_scroll.set_offset(point(px(0.), px(0.)));
        self.refresh_permission(cx);
        self.refresh_prompts(cx);
        self.open_project(cx);
        self.focus_nav(window, cx);
        cx.notify();
    }

    fn nav_row(&self, index: usize, cx: &Context<Self>) -> impl IntoElement + use<> {
        let page = Page::from_nav_index(index);
        let projects = self.state.read(cx).projects.clone();

        let (label, tile) = match page {
            Page::Project(project) => match projects.get(project) {
                Some(settings) => (
                    settings.project.name.clone(),
                    Some(
                        ProjectTile::new(settings.project.letter, settings.project.tile_color)
                            .size(px(16.)),
                    ),
                ),
                None => (SharedString::default(), None),
            },
            section => (SharedString::from(section.title()), None),
        };

        let mut row = ListRow::new(("settings-nav", index), label)
            .toggle_state(self.page == page)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_page(page, cx);
                this.focus_nav(window, cx);
            }));

        if let Some(handle) = self.nav_focus.get(index) {
            row = row.track_focus(handle);
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

        let nav = self.nav(window, cx);
        let page = self.render_page(cx);
        let scrolls_with_keys = matches!(self.page, Page::Keyboard | Page::Usage);
        let scroll = self.page_scroll.clone();
        let gap_drag = drag_region("settings-gap-drag", window, cx);
        let page_drag = drag_region("settings-page-drag", window, cx);

        swarm_ui::tab_navigation(
            div()
                .id("settings")
                .key_context(ROOT_CONTEXT)
                .size_full()
                .bg(ground)
                .text_color(fg)
                .font_family(UI_FONT)
                .text_size(px(13.)),
        )
        .child(
            h_flex()
                .key_context(SETTINGS_CONTEXT)
                .on_action(cx.listener(|_, _: &CloseSettings, window, _| window.remove_window()))
                .size_full()
                .child(nav)
                .child(
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
                                    div()
                                        .id("settings-page")
                                        .debug_selector(|| "settings-page".to_owned())
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
                                        })
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_y_scroll()
                                        .track_scroll(&self.page_scroll)
                                        .child(
                                            v_flex()
                                                .max_w(px(756.))
                                                .px(px(48.))
                                                .pb(px(36.))
                                                .gap(px(26.))
                                                .child(page),
                                        ),
                                ),
                        ),
                ),
        )
    }
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

/// Loads the settings file at startup, so the settings apply before the window opens.
pub(crate) fn load(path: Option<PathBuf>, cx: &mut App) {
    let state = shared_state(path, cx);

    state.update(cx, SettingsState::reload);
}

/// The notification settings. They stay the defaults until the settings load.
pub(crate) fn notifications(cx: &App) -> NotificationSettings {
    cx.try_global::<Registry>()
        .map_or_else(NotificationSettings::default, |registry| {
            registry.state.read(cx).settings.notifications.clone()
        })
}

/// The shared settings state, for what must follow a change in the settings.
pub(crate) fn state(cx: &mut App) -> Entity<SettingsState> {
    shared_state(None, cx)
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
            .update(cx, |_, window, _| window.activate_window())
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
/// open tasks the Projects pages use.
pub(crate) fn open(
    path: Option<PathBuf>,
    projects: Vec<Project>,
    workspace: WeakEntity<Workspace>,
    _window: &mut Window,
    cx: &mut App,
) {
    link_workspace(workspace, cx);

    if let Err(error) = open_window(path, Some(projects), cx) {
        log::error!("could not open the settings window: {error:#}");
    }
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

#[cfg(feature = "snapshot")]
pub(crate) fn snapshot(directory: PathBuf, cx: &mut App) {
    use std::time::Duration;

    use gpui::{AnyWindowHandle, AsyncApp};
    use swarm_ui::theme::Appearance;

    use crate::notifications::SampleDelivery;

    const SETTLE: Duration = Duration::from_millis(700);

    // The permission is the answer the sample delivery gives to every page.
    let pages = [
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
            Page::Project(0),
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
    ];

    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        for (appearance, suffix) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
            cx.update(|cx| theme::set_appearance(appearance, cx));

            for (page, name, window_size, permission) in pages {
                let path = directory.join(format!("settings-{name}-{suffix}.png"));

                let opened = cx.update(|cx| {
                    notifications::use_delivery(Rc::new(SampleDelivery(permission)), cx);

                    let state = cx.new(|_| SettingsState::sample(None));
                    state.update(cx, SettingsState::sample_modes);
                    // Set before the first frame: the answer of the delivery comes later.
                    state.update(cx, |state, _| state.permission = Some(permission));

                    create_window(state, page, window_size, cx)
                });
                let window: AnyWindowHandle = match opened {
                    Ok(handle) => {
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
                settings.select_page(Page::Project(1), cx);
            })
            .expect("window is open");
        let project = window
            .read_with(cx, |settings, _| settings.page())
            .expect("window is open");

        assert_eq!(section, Page::Notifications);
        assert_eq!(project, Page::Project(1));
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
            id: crate::model::ProjectId(0),
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

        window
            .update(cx, |_, _, cx| pages::apply_appearance(1, cx))
            .expect("window is open");

        assert_eq!(
            cx.update(|cx| theme::appearance(cx)),
            theme::Appearance::Dark
        );
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
        let stored = state.read_with(&visual, |state, _| state.max_agents_per_task.clone());
        assert_eq!(text.as_ref(), "12");
        assert_eq!(stored, "12");
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
