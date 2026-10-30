//! The settings window: a nav of sections and projects, and one page at a time.

mod pages;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Global, KeyBinding, Pixels, Role, SharedString,
    Size, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions,
    actions, div, px, size,
};
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;
use swarm_ui::theme;
use swarm_ui::{ROOT_CONTEXT, SelectNext, SelectPrevious, TextFieldSize};

use crate::model::{FakeData, Project};
use crate::window_drag::drag_region;

actions!(swarm_app, [CloseSettings]);

const SETTINGS_CONTEXT: &str = "Settings";
const NAV_CONTEXT: &str = "SettingsNav";

const WINDOW_SIZE: Size<Pixels> = size(px(1280.), px(900.));
const MIN_WINDOW_SIZE: Size<Pixels> = size(px(900.), px(600.));

pub(crate) struct AgentInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub icon: IconName,
    pub models: &'static [&'static str],
}

pub(crate) const AGENTS: [AgentInfo; 2] = [
    AgentInfo {
        name: "Claude Code",
        version: "2.1.314",
        icon: IconName::ClaudeCode,
        models: &["Opus 5.5", "Sonnet 5.5"],
    },
    AgentInfo {
        name: "Codex",
        version: "0.154.0",
        icon: IconName::Codex,
        models: &["Default"],
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

#[derive(Clone, Debug)]
pub(crate) struct AgentSettings {
    pub command: String,
    pub model: usize,
    pub skip_permissions: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectSettings {
    pub project: Project,
    pub base_branches: Vec<String>,
    pub base_branch: usize,
    pub worktree_folder: String,
    pub port_range: String,
    pub ports_per_worktree: String,
    pub setup_script: String,
    pub copy_files: Vec<String>,
    pub checks: Vec<String>,
    pub checks_required: bool,
    pub notifications: usize,
}

/// Settings shared by every page. In memory only for now.
#[derive(Clone, Debug)]
pub(crate) struct SettingsState {
    pub agents: [AgentSettings; 2],
    pub orchestrator: usize,
    pub allow_worker_typing: bool,
    pub idle_stop: usize,
    pub max_agents_per_task: String,
    pub max_agents_total: String,
    pub notify_when: [bool; 4],
    pub delivery: [bool; 4],
    pub projects: Vec<ProjectSettings>,
}

impl SettingsState {
    pub(crate) fn sample() -> Self {
        let agent = |command: &str| AgentSettings {
            command: command.to_owned(),
            model: 0,
            skip_permissions: true,
        };

        let projects = FakeData::sample()
            .projects
            .into_iter()
            .enumerate()
            .map(|(index, project)| {
                let mut base_branches = vec![project.base_branch.to_string()];
                if project.base_branch != "main" {
                    base_branches.push("main".to_owned());
                }

                ProjectSettings {
                    project,
                    base_branches,
                    base_branch: 0,
                    worktree_folder: ".worktrees".to_owned(),
                    port_range: "5170–5199".to_owned(),
                    ports_per_worktree: "3".to_owned(),
                    setup_script: "cargo fetch\npnpm --dir docs install --frozen-lockfile\ncp .env.example .env".to_owned(),
                    copy_files: vec![".env".to_owned(), "CLAUDE.local.md".to_owned()],
                    checks: vec![
                        "cargo test --workspace".to_owned(),
                        "cargo clippy --workspace".to_owned(),
                    ],
                    checks_required: true,
                    notifications: index.min(2),
                }
            })
            .collect();

        Self {
            agents: [agent("claude"), agent("codex")],
            orchestrator: 0,
            allow_worker_typing: true,
            idle_stop: 1,
            max_agents_per_task: "6".to_owned(),
            max_agents_total: "16".to_owned(),
            notify_when: [true; 4],
            delivery: [false, true, true, true],
            projects,
        }
    }

    pub(crate) fn remove_copy_file(
        &mut self,
        project: usize,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(files) = self.projects.get_mut(project).map(|p| &mut p.copy_files)
            && index < files.len()
        {
            files.remove(index);
            cx.notify();
        }
    }

    pub(crate) fn add_copy_file(&mut self, project: usize, cx: &mut Context<Self>) {
        if let Some(settings) = self.projects.get_mut(project) {
            settings.copy_files.push("new-file".to_owned());
            cx.notify();
        }
    }

    pub(crate) fn add_check(&mut self, project: usize, cx: &mut Context<Self>) {
        if let Some(settings) = self.projects.get_mut(project) {
            settings.checks.push("new-check".to_owned());
            cx.notify();
        }
    }

    pub(crate) fn remove_check(&mut self, project: usize, index: usize, cx: &mut Context<Self>) {
        if let Some(checks) = self.projects.get_mut(project).map(|p| &mut p.checks)
            && index < checks.len()
        {
            checks.remove(index);
            cx.notify();
        }
    }

    pub(crate) fn set_check(
        &mut self,
        project: usize,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let check = self
            .projects
            .get_mut(project)
            .and_then(|p| p.checks.get_mut(index));

        if let Some(check) = check {
            *check = text;
            cx.notify();
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

struct ProjectInputs {
    worktree_folder: Entity<TextInput>,
    port_range: Entity<TextInput>,
    ports_per_worktree: Entity<TextInput>,
    setup_script: Entity<TextArea>,
    checks: Vec<Entity<TextInput>>,
}

struct Inputs {
    agent_commands: [Entity<TextInput>; 2],
    max_agents_per_task: Entity<TextInput>,
    max_agents_total: Entity<TextInput>,
    projects: Vec<ProjectInputs>,
}

pub(crate) struct SettingsWindow {
    state: Entity<SettingsState>,
    page: Page,
    nav_focus: Vec<FocusHandle>,
    inputs: Inputs,
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

            state.update(cx, |state, _| apply(state, text.to_string()));
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
            cx.observe(&state, |_, _, cx| cx.notify()),
            theme::follow_system_appearance(window, cx),
        ];

        let settings = state.read(cx).clone();

        let agent_commands = [0, 1].map(|index| {
            let input = field("Command", &settings.agents[index].command, true, cx);
            bind_text(
                &mut subscriptions,
                &input,
                &state,
                move |state, text| state.agents[index].command = text,
                cx,
            );
            input
        });

        let max_agents_per_task =
            numeric_field("Max agents per task", &settings.max_agents_per_task, cx);
        bind_text(
            &mut subscriptions,
            &max_agents_per_task,
            &state,
            |state, text| state.max_agents_per_task = text,
            cx,
        );

        let max_agents_total = numeric_field("Max agents in total", &settings.max_agents_total, cx);
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
            nav_focus: (0..Page::SECTIONS.len() + settings.projects.len())
                .map(|_| cx.focus_handle().tab_stop(true))
                .collect(),
            inputs: Inputs {
                agent_commands,
                max_agents_per_task,
                max_agents_total,
                projects: Vec::new(),
            },
            subscriptions,
        };

        for (index, project) in settings.projects.iter().enumerate() {
            let inputs = this.project_inputs(index, project, cx);
            this.inputs.projects.push(inputs);
        }

        this.focus_nav(window, cx);

        this
    }

    fn project_inputs(
        &mut self,
        index: usize,
        project: &ProjectSettings,
        cx: &mut Context<Self>,
    ) -> ProjectInputs {
        let state = self.state.clone();

        let worktree_folder = field("Worktree folder", &project.worktree_folder, true, cx);
        bind_text(
            &mut self.subscriptions,
            &worktree_folder,
            &state,
            move |state, text| {
                if let Some(project) = state.projects.get_mut(index) {
                    project.worktree_folder = text;
                }
            },
            cx,
        );

        let port_range = field("Port range", &project.port_range, true, cx);
        bind_text(
            &mut self.subscriptions,
            &port_range,
            &state,
            move |state, text| {
                if let Some(project) = state.projects.get_mut(index) {
                    project.port_range = text;
                }
            },
            cx,
        );

        let ports_per_worktree =
            numeric_field("Ports per worktree", &project.ports_per_worktree, cx);
        bind_text(
            &mut self.subscriptions,
            &ports_per_worktree,
            &state,
            move |state, text| {
                if let Some(project) = state.projects.get_mut(index) {
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
                    this.state.update(cx, |state, _| {
                        if let Some(project) = state.projects.get_mut(index) {
                            project.setup_script = text;
                        }
                    });
                }
            },
        ));

        let checks = project
            .checks
            .iter()
            .map(|check| self.check_input(index, check, cx))
            .collect();

        ProjectInputs {
            worktree_folder,
            port_range,
            ports_per_worktree,
            setup_script,
            checks,
        }
    }

    fn check_input(
        &mut self,
        project: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        let input = field("Check", text, true, cx);

        self.subscriptions.push(cx.subscribe(
            &input,
            move |this, input, event: &TextInputEvent, cx| {
                let TextInputEvent::Changed(text) = event else {
                    return;
                };

                let index = this
                    .inputs
                    .projects
                    .get(project)
                    .and_then(|inputs| inputs.checks.iter().position(|check| *check == input));

                if let Some(index) = index {
                    let text = text.to_string();
                    this.state
                        .update(cx, |state, cx| state.set_check(project, index, text, cx));
                }
            },
        ));

        input
    }

    pub(crate) fn add_check(&mut self, project: usize, cx: &mut Context<Self>) {
        let input = self.check_input(project, "new-check", cx);

        if let Some(inputs) = self.inputs.projects.get_mut(project) {
            inputs.checks.push(input);
        }

        self.state
            .update(cx, |state, cx| state.add_check(project, cx));
    }

    pub(crate) fn remove_check(&mut self, project: usize, index: usize, cx: &mut Context<Self>) {
        if let Some(inputs) = self.inputs.projects.get_mut(project)
            && index < inputs.checks.len()
        {
            inputs.checks.remove(index);
        }

        self.state
            .update(cx, |state, cx| state.remove_check(project, index, cx));
    }

    #[cfg(test)]
    pub(crate) fn page(&self) -> Page {
        self.page
    }

    pub(crate) fn select_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
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
        let (ground, panel, line, fg) = (colors.ground, colors.panel, colors.line, colors.fg);

        let nav = self.nav(window, cx);
        let page = self.render_page(cx);

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
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .py(px(8.))
                        .pr(px(8.))
                        .child(
                            div()
                                .id("settings-page")
                                .size_full()
                                .bg(panel)
                                .border_1()
                                .border_color(line)
                                .rounded(px(10.))
                                .overflow_y_scroll()
                                .child(
                                    v_flex()
                                        .max_w(px(756.))
                                        .px(px(48.))
                                        .py(px(36.))
                                        .gap(px(26.))
                                        .child(page),
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

fn shared_state(cx: &mut App) -> Entity<SettingsState> {
    if let Some(registry) = cx.try_global::<Registry>() {
        return registry.state.clone();
    }

    let state = cx.new(|_| SettingsState::sample());
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

fn open_window(cx: &mut App) -> anyhow::Result<WindowHandle<SettingsWindow>> {
    bind_keys(cx);
    let state = shared_state(cx);

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

    let handle = create_window(state, Page::Agents, WINDOW_SIZE, cx)?;
    cx.global_mut::<Registry>().window = Some(handle);

    Ok(handle)
}

/// Opens the settings window, or focuses it when it is already open.
pub(crate) fn open(_window: &mut Window, cx: &mut App) {
    if let Err(error) = open_window(cx) {
        log::error!("could not open the settings window: {error:#}");
    }
}

#[cfg(feature = "snapshot")]
pub(crate) fn snapshot(directory: std::path::PathBuf, cx: &mut App) {
    use std::time::Duration;

    use gpui::{AnyWindowHandle, AsyncApp};
    use swarm_ui::theme::Appearance;

    const SETTLE: Duration = Duration::from_millis(700);

    let pages = [
        (Page::Agents, "agents", size(px(1280.), px(900.))),
        (
            Page::Notifications,
            "notifications",
            size(px(1280.), px(800.)),
        ),
        (Page::Project(0), "project", size(px(1280.), px(1180.))),
    ];

    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        for (appearance, suffix) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
            cx.update(|cx| theme::set_appearance(appearance, cx));

            for (page, name, window_size) in pages {
                let path = directory.join(format!("settings-{name}-{suffix}.png"));

                let opened = cx.update(|cx| {
                    let state = cx.new(|_| SettingsState::sample());
                    create_window(state, page, window_size, cx)
                });
                let window: AnyWindowHandle = match opened {
                    Ok(handle) => handle.into(),
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

    fn setup(cx: &mut TestAppContext) {
        cx.update(|cx| {
            theme::init(cx);
            bind_keys(cx);
        });
    }

    fn open_settings(cx: &mut TestAppContext) -> WindowHandle<SettingsWindow> {
        cx.update(open_window).expect("window opens")
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

    #[gpui::test]
    fn removing_a_copy_file_row_removes_it_from_state(cx: &mut TestAppContext) {
        setup(cx);
        let window = open_settings(cx);
        let state = window
            .read_with(cx, |settings, _| settings.state.clone())
            .expect("window is open");

        state.update(cx, |state, cx| state.remove_copy_file(0, 0, cx));

        let files = state.read_with(cx, |state, _| state.projects[0].copy_files.clone());
        assert_eq!(files, vec!["CLAUDE.local.md".to_owned()]);
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
