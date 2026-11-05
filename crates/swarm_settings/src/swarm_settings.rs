//! User settings in `~/.config/swarm/settings.toml`. People edit the file by hand or in
//! the app's Settings window; the daemon reads it when it starts an agent.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::{self, DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize};
use swarm_protocol::{AgentKind, AgentRole, NoticeKind};
use toml_edit::{DocumentMut, Item, Table, Value};

/// Every section reads the same way: a key whose value has the wrong type or is out of
/// range falls back to that key's default, and the other keys still load. Only a file that
/// is not valid TOML fails, in [`load`]. A typo must not stop the daemon from starting agents.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// The welcome screen was dealt with: skipped, or left to open a project. It shows
    /// only while this is false.
    pub welcome_done: bool,
    pub agents: AgentSettings,
    pub terminals: TerminalSettings,
    pub notifications: NotificationSettings,
    pub appearance: AppearanceSettings,
    pub updates: UpdateSettings,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let table = toml::Table::deserialize(deserializer)?;
        let mut warnings = Vec::new();
        let settings = Self::from_table(table, &mut warnings);

        for warning in &warnings {
            log::warn!("settings: {warning}");
        }

        Ok(settings)
    }
}

impl Settings {
    fn from_table(table: toml::Table, warnings: &mut Vec<String>) -> Self {
        let mut root = Section {
            name: String::new(),
            table,
            warnings,
        };

        Self {
            welcome_done: root.take("welcome_done", false, BOOL),
            agents: AgentSettings::read(root.child("agents")),
            terminals: TerminalSettings::read(root.child("terminals")),
            notifications: NotificationSettings::read(root.child("notifications")),
            appearance: AppearanceSettings::read(root.child("appearance")),
            updates: UpdateSettings::read(root.child("updates")),
        }
    }
}

const BOOL: &str = "a true/false value";
const NUMBER: &str = "a whole number";
const TEXT: &str = "text";

/// One table of the file, read key by key. A key that is missing keeps its default; one
/// that is not valid keeps it too and adds a warning that names the key.
struct Section<'a> {
    name: String,
    table: toml::Table,
    warnings: &'a mut Vec<String>,
}

impl<'a> Section<'a> {
    fn new(name: &str, value: Option<toml::Value>, warnings: &'a mut Vec<String>) -> Self {
        let table = match value {
            None => toml::Table::new(),
            Some(toml::Value::Table(table)) => table,
            Some(_) => {
                warnings.push(format!("{name}: not a table, using the defaults"));

                toml::Table::new()
            }
        };

        Self {
            name: name.to_owned(),
            table,
            warnings,
        }
    }

    fn path(&self, key: &str) -> String {
        if self.name.is_empty() {
            key.to_owned()
        } else {
            format!("{}.{key}", self.name)
        }
    }

    /// The table under `key` as a section of its own. Use it before `self` again.
    fn child(&mut self, key: &str) -> Section<'_> {
        let value = self.table.remove(key);
        let name = self.path(key);

        Section::new(&name, value, self.warnings)
    }

    /// The value under `key`, or `default` when it is missing or not valid.
    fn take<T: DeserializeOwned + Serialize>(
        &mut self,
        key: &str,
        default: T,
        expected: &str,
    ) -> T {
        self.take_if(key, default, expected, |_| true)
    }

    /// Like [`Self::take`], and a value that `accepts` refuses falls back as well.
    fn take_if<T: DeserializeOwned + Serialize>(
        &mut self,
        key: &str,
        default: T,
        expected: &str,
        accepts: impl FnOnce(&T) -> bool,
    ) -> T {
        let Some(value) = self.table.remove(key) else {
            return default;
        };

        match T::deserialize(value) {
            Ok(read) if accepts(&read) => read,
            _ => {
                let shown = toml::Value::try_from(&default)
                    .map_or_else(|_| "the default".to_owned(), |value| value.to_string());
                let path = self.path(key);
                self.warnings
                    .push(format!("{path}: not {expected}, using {shown}"));

                default
            }
        }
    }
}

/// How the app looks for new versions of itself.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSettings {
    /// Checks for an update after launch and every few hours. Off leaves only the manual
    /// checks.
    pub auto_check: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

impl UpdateSettings {
    fn read(mut section: Section<'_>) -> Self {
        let defaults = Self::default();

        Self {
            auto_check: section.take("auto_check", defaults.auto_check, BOOL),
        }
    }
}

/// The accent color of the app.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccentSetting {
    #[default]
    Blue,
    Honey,
}

/// Light, dark, or whatever macOS is set to.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeSetting {
    #[default]
    System,
    Light,
    Dark,
}

/// What the app looks like.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceSettings {
    pub theme: ThemeSetting,
    /// Turns every easter egg off when false.
    pub easter_eggs: bool,
    pub accent: AccentSetting,
    /// Shows a honeycomb behind the Swarm view, in place of the dots.
    pub swarm_view_honeycomb: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeSetting::System,
            easter_eggs: true,
            accent: AccentSetting::Blue,
            swarm_view_honeycomb: false,
        }
    }
}

impl AppearanceSettings {
    fn read(mut section: Section<'_>) -> Self {
        let defaults = Self::default();

        Self {
            theme: section.take("theme", defaults.theme, "light, dark or system"),
            easter_eggs: section.take("easter_eggs", defaults.easter_eggs, BOOL),
            accent: section.take("accent", defaults.accent, "blue or honey"),
            swarm_view_honeycomb: section.take(
                "swarm_view_honeycomb",
                defaults.swarm_view_honeycomb,
                BOOL,
            ),
        }
    }
}

/// Which notices become system notifications, and how they are delivered.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each one is a switch in the settings file"
)]
pub struct NotificationSettings {
    /// The orchestrator asked for a decision.
    pub needs_decision: bool,
    /// An agent shows a permission or question prompt.
    pub prompt: bool,
    /// Every worker is done and the orchestrator is idle.
    pub ready_for_review: bool,
    /// An agent failed or ended with an error.
    pub broke: bool,
    /// Plays a sound with a notification.
    pub sound: bool,
    /// Bounces the Dock icon for urgent notices.
    pub dock_bounce: bool,
    /// Posts nothing while the user looks at the task.
    pub mute_while_viewing: bool,
    /// Shows the dot on the menu bar icon while a task needs the user.
    pub menu_bar_dot: bool,
    /// The mode of each project, keyed by the path of its repository. A project that is
    /// not listed is [`ProjectMode::All`].
    pub projects: BTreeMap<String, ProjectMode>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            needs_decision: true,
            prompt: true,
            ready_for_review: true,
            broke: true,
            sound: false,
            dock_bounce: true,
            mute_while_viewing: true,
            menu_bar_dot: true,
            projects: BTreeMap::new(),
        }
    }
}

impl NotificationSettings {
    fn read(mut section: Section<'_>) -> Self {
        let defaults = Self::default();

        Self {
            needs_decision: section.take("needs_decision", defaults.needs_decision, BOOL),
            prompt: section.take("prompt", defaults.prompt, BOOL),
            ready_for_review: section.take("ready_for_review", defaults.ready_for_review, BOOL),
            broke: section.take("broke", defaults.broke, BOOL),
            sound: section.take("sound", defaults.sound, BOOL),
            dock_bounce: section.take("dock_bounce", defaults.dock_bounce, BOOL),
            mute_while_viewing: section.take(
                "mute_while_viewing",
                defaults.mute_while_viewing,
                BOOL,
            ),
            menu_bar_dot: section.take("menu_bar_dot", defaults.menu_bar_dot, BOOL),
            projects: take_projects(section.child("projects")),
        }
    }

    /// The trigger switch of this kind of notice.
    pub fn triggers(&self, kind: NoticeKind) -> bool {
        match kind {
            NoticeKind::NeedsDecision => self.needs_decision,
            NoticeKind::Prompt => self.prompt,
            NoticeKind::ReadyForReview => self.ready_for_review,
            NoticeKind::Broke => self.broke,
        }
    }

    pub fn mode(&self, project: &Path) -> ProjectMode {
        self.projects
            .get(project.to_string_lossy().as_ref())
            .copied()
            .unwrap_or_default()
    }

    /// The default mode is not stored: the project's entry goes away.
    pub fn set_mode(&mut self, project: &Path, mode: ProjectMode) {
        let key = project.to_string_lossy().into_owned();

        if mode == ProjectMode::default() {
            self.projects.remove(&key);
        } else {
            self.projects.insert(key, mode);
        }
    }
}

/// The projects with a mode that is valid. A project whose mode is not valid is left out,
/// which makes it [`ProjectMode::All`].
fn take_projects(section: Section<'_>) -> BTreeMap<String, ProjectMode> {
    let Section {
        name,
        table,
        warnings,
    } = section;

    table
        .into_iter()
        .filter_map(|(path, mode)| {
            let Ok(mode) = ProjectMode::deserialize(mode) else {
                warnings.push(format!(
                    "{name}.{path}: not all, errors_only or off, leaving it out"
                ));

                return None;
            };

            Some((path, mode))
        })
        .collect()
}

/// What a project may notify about.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMode {
    #[default]
    All,
    /// Only when something broke.
    ErrorsOnly,
    Off,
}

/// The smallest terminal font size, in points.
pub const FONT_SIZE_MIN: u8 = 11;

/// The largest terminal font size, in points.
pub const FONT_SIZE_MAX: u8 = 20;

/// The terminal font size when the settings say nothing, in points.
pub const FONT_SIZE_DEFAULT: u8 = 13;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalSettings {
    /// Lets the user type into worker terminals.
    pub allow_worker_typing: bool,
    /// The font size of every terminal, in points. A value outside
    /// [`FONT_SIZE_MIN`]..=[`FONT_SIZE_MAX`] in the file is brought into it.
    pub font_size: u8,
    /// Asks before a user terminal with a running program closes.
    pub confirm_close: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            allow_worker_typing: true,
            font_size: FONT_SIZE_DEFAULT,
            confirm_close: true,
        }
    }
}

impl TerminalSettings {
    fn read(mut section: Section<'_>) -> Self {
        let defaults = Self::default();

        Self {
            allow_worker_typing: section.take(
                "allow_worker_typing",
                defaults.allow_worker_typing,
                BOOL,
            ),
            font_size: section
                .take(
                    "font_size",
                    FontSize(defaults.font_size),
                    "a number of points",
                )
                .0,
            confirm_close: section.take("confirm_close", defaults.confirm_close, BOOL),
        }
    }
}

/// `points` brought into the range the settings allow.
pub fn clamp_font_size(points: u8) -> u8 {
    points.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX)
}

/// A font size as the file may spell it: whole or fractional, and out of range.
#[derive(Serialize)]
#[serde(transparent)]
struct FontSize(u8);

impl<'de> Deserialize<'de> for FontSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        read_font_size(deserializer).map(Self)
    }
}

fn read_font_size<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    struct Points;

    impl de::Visitor<'_> for Points {
        type Value = u8;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a font size in points")
        }

        fn visit_i64<E: de::Error>(self, points: i64) -> Result<u8, E> {
            let clamped = points.clamp(i64::from(FONT_SIZE_MIN), i64::from(FONT_SIZE_MAX));

            Ok(u8::try_from(clamped).unwrap_or(FONT_SIZE_DEFAULT))
        }

        fn visit_u64<E: de::Error>(self, points: u64) -> Result<u8, E> {
            self.visit_i64(i64::try_from(points).unwrap_or(i64::MAX))
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "rounded and clamped into the u8 range first"
        )]
        fn visit_f64<E: de::Error>(self, points: f64) -> Result<u8, E> {
            if points.is_nan() {
                return Ok(FONT_SIZE_DEFAULT);
            }

            self.visit_i64(points.round().clamp(-1e6, 1e6) as i64)
        }
    }

    deserializer.deserialize_any(Points)
}

/// How long an idle agent runs before the daemon stops it, in minutes, when the settings
/// say nothing.
pub const IDLE_STOP_MINUTES_DEFAULT: u32 = 30;

/// The most agents one task runs at once, when the settings say nothing.
pub const MAX_AGENTS_PER_TASK_DEFAULT: u32 = 6;

/// The most agents all tasks run at once, when the settings say nothing.
pub const MAX_AGENTS_TOTAL_DEFAULT: u32 = 16;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentSettings {
    /// The CLI that new orchestrators run.
    pub orchestrator: AgentKindSetting,
    /// Starts agents without permission prompts (Claude Code
    /// `--dangerously-skip-permissions`, Codex `--yolo`).
    pub skip_permissions: bool,
    /// Stops an agent that has been idle this many minutes. `0` never stops one.
    pub idle_stop_minutes: u32,
    /// The most agents one task runs at once. At least 1.
    pub max_agents_per_task: u32,
    /// The most agents all tasks run at once. At least 1.
    pub max_agents_total: u32,
    pub claude_code: CliSettings,
    pub codex: CliSettings,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            orchestrator: AgentKindSetting::ClaudeCode,
            skip_permissions: false,
            idle_stop_minutes: IDLE_STOP_MINUTES_DEFAULT,
            max_agents_per_task: MAX_AGENTS_PER_TASK_DEFAULT,
            max_agents_total: MAX_AGENTS_TOTAL_DEFAULT,
            claude_code: CliSettings::claude_code(),
            codex: CliSettings::codex(),
        }
    }
}

const AT_LEAST_ONE: &str = "a number of at least 1";

impl CliSettings {
    fn read(mut section: Section<'_>, defaults: Self) -> Self {
        Self {
            command: section.take("command", defaults.command, TEXT),
            orchestrator_model: section.take(
                "orchestrator_model",
                defaults.orchestrator_model,
                TEXT,
            ),
            worker_model: section.take("worker_model", defaults.worker_model, TEXT),
        }
    }
}

impl AgentSettings {
    fn read(mut section: Section<'_>) -> Self {
        let defaults = Self::default();

        Self {
            orchestrator: section.take(
                "orchestrator",
                defaults.orchestrator,
                "claude-code or codex",
            ),
            skip_permissions: section.take("skip_permissions", defaults.skip_permissions, BOOL),
            idle_stop_minutes: section.take(
                "idle_stop_minutes",
                defaults.idle_stop_minutes,
                NUMBER,
            ),
            max_agents_per_task: section.take_if(
                "max_agents_per_task",
                defaults.max_agents_per_task,
                AT_LEAST_ONE,
                |limit| *limit >= 1,
            ),
            max_agents_total: section.take_if(
                "max_agents_total",
                defaults.max_agents_total,
                AT_LEAST_ONE,
                |limit| *limit >= 1,
            ),
            claude_code: CliSettings::read(section.child("claude_code"), defaults.claude_code),
            codex: CliSettings::read(section.child("codex"), defaults.codex),
        }
    }

    pub fn cli(&self, kind: AgentKind) -> &CliSettings {
        match kind {
            AgentKind::ClaudeCode => &self.claude_code,
            AgentKind::Codex => &self.codex,
        }
    }

    pub fn model(&self, kind: AgentKind, role: AgentRole) -> &str {
        let cli = self.cli(kind);

        match role {
            AgentRole::Orchestrator | AgentRole::StartOrchestrator => &cli.orchestrator_model,
            AgentRole::Worker => &cli.worker_model,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKindSetting {
    ClaudeCode,
    Codex,
}

impl From<AgentKindSetting> for AgentKind {
    fn from(setting: AgentKindSetting) -> Self {
        match setting {
            AgentKindSetting::ClaudeCode => Self::ClaudeCode,
            AgentKindSetting::Codex => Self::Codex,
        }
    }
}

impl From<AgentKind> for AgentKindSetting {
    fn from(kind: AgentKind) -> Self {
        match kind {
            AgentKind::ClaudeCode => Self::ClaudeCode,
            AgentKind::Codex => Self::Codex,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CliSettings {
    /// Looked up in `PATH` and the usual install folders when not absolute.
    pub command: String,
    pub orchestrator_model: String,
    pub worker_model: String,
}

impl CliSettings {
    fn claude_code() -> Self {
        Self {
            command: "claude".to_owned(),
            orchestrator_model: "opus".to_owned(),
            worker_model: "sonnet".to_owned(),
        }
    }

    // Sol and Astra are the goal; a ChatGPT sign-in cannot use them yet.
    fn codex() -> Self {
        Self {
            command: "codex".to_owned(),
            orchestrator_model: "gpt-reserve".to_owned(),
            worker_model: "gpt-6-luna".to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("cannot read or write {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path} is not valid: {source}")]
    Edit {
        path: PathBuf,
        source: toml_edit::TomlError,
    },
}

/// `~/.config/swarm/settings.toml`.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/swarm/settings.toml"))
}

/// A missing file gives the defaults; keys left out keep their defaults, and so do keys
/// whose value is not valid. Only a file that is not TOML fails. It logs nothing, so a
/// caller that reads often does not repeat itself: [`load_with_warnings`] gives the
/// warnings.
pub fn load(path: &Path) -> Result<Settings, SettingsError> {
    load_with_warnings(path).map(|(settings, _)| settings)
}

/// Like [`load`], and says which keys fell back to their default, for example
/// `agents.skip_permissions: not a true/false value, using false`.
pub fn load_with_warnings(path: &Path) -> Result<(Settings, Vec<String>), SettingsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Settings::default(), Vec::new()));
        }
        Err(source) => {
            return Err(SettingsError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };

    let table: toml::Table = toml::from_str(&text).map_err(|source| SettingsError::Parse {
        path: path.to_owned(),
        source,
    })?;
    let mut warnings = Vec::new();
    let settings = Settings::from_table(table, &mut warnings);

    Ok((settings, warnings))
}

/// Sets the keys Swarm owns in the existing file and keeps everything else, comments
/// and unknown keys included. Writes through a temp file and a rename, so a reader
/// never sees half a file.
pub fn save(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let io_error = |source| SettingsError::Io {
        path: path.to_owned(),
        source,
    };

    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(io_error(source)),
    };
    let mut document: DocumentMut = existing.parse().map_err(|source| SettingsError::Edit {
        path: path.to_owned(),
        source,
    })?;

    // An unset flag stays out of a file that has none.
    if settings.welcome_done || document.contains_key("welcome_done") {
        set_value(
            document.as_table_mut(),
            "welcome_done",
            Value::from(settings.welcome_done),
        );
    }

    let agents = table_mut(document.as_table_mut(), "agents");
    let kind = match settings.agents.orchestrator {
        AgentKindSetting::ClaudeCode => "claude-code",
        AgentKindSetting::Codex => "codex",
    };
    set_value(agents, "orchestrator", Value::from(kind));
    set_value(
        agents,
        "skip_permissions",
        Value::from(settings.agents.skip_permissions),
    );
    for (key, number) in [
        ("idle_stop_minutes", settings.agents.idle_stop_minutes),
        ("max_agents_per_task", settings.agents.max_agents_per_task),
        ("max_agents_total", settings.agents.max_agents_total),
    ] {
        set_value(agents, key, Value::from(i64::from(number)));
    }
    set_cli(
        table_mut(agents, "claude_code"),
        &settings.agents.claude_code,
    );
    set_cli(table_mut(agents, "codex"), &settings.agents.codex);

    let terminals = table_mut(document.as_table_mut(), "terminals");
    set_value(
        terminals,
        "allow_worker_typing",
        Value::from(settings.terminals.allow_worker_typing),
    );
    set_value(
        terminals,
        "font_size",
        Value::from(i64::from(clamp_font_size(settings.terminals.font_size))),
    );
    set_value(
        terminals,
        "confirm_close",
        Value::from(settings.terminals.confirm_close),
    );

    set_notifications(
        table_mut(document.as_table_mut(), "notifications"),
        &settings.notifications,
    );
    set_appearance(
        table_mut(document.as_table_mut(), "appearance"),
        settings.appearance,
    );
    set_value(
        table_mut(document.as_table_mut(), "updates"),
        "auto_check",
        Value::from(settings.updates.auto_check),
    );

    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(io_error)?;
    }

    let temporary = path.with_extension("toml.tmp");

    std::fs::write(&temporary, document.to_string()).map_err(io_error)?;
    std::fs::rename(&temporary, path).map_err(io_error)
}

fn set_appearance(table: &mut Table, appearance: AppearanceSettings) {
    let accent = match appearance.accent {
        AccentSetting::Blue => "blue",
        AccentSetting::Honey => "honey",
    };

    let theme = match appearance.theme {
        ThemeSetting::System => "system",
        ThemeSetting::Light => "light",
        ThemeSetting::Dark => "dark",
    };

    set_value(table, "theme", Value::from(theme));
    set_value(table, "easter_eggs", Value::from(appearance.easter_eggs));
    set_value(table, "accent", Value::from(accent));
    set_value(
        table,
        "swarm_view_honeycomb",
        Value::from(appearance.swarm_view_honeycomb),
    );
}

fn set_notifications(table: &mut Table, notifications: &NotificationSettings) {
    for (key, on) in [
        ("needs_decision", notifications.needs_decision),
        ("prompt", notifications.prompt),
        ("ready_for_review", notifications.ready_for_review),
        ("broke", notifications.broke),
        ("sound", notifications.sound),
        ("dock_bounce", notifications.dock_bounce),
        ("mute_while_viewing", notifications.mute_while_viewing),
        ("menu_bar_dot", notifications.menu_bar_dot),
    ] {
        set_value(table, key, Value::from(on));
    }

    if notifications.projects.is_empty() {
        table.remove("projects");
        return;
    }

    let projects = table_mut(table, "projects");
    let stale: Vec<String> = projects
        .iter()
        .map(|(key, _)| key.to_owned())
        .filter(|key| !notifications.projects.contains_key(key))
        .collect();
    for key in stale {
        projects.remove(&key);
    }

    for (path, mode) in &notifications.projects {
        let name = match mode {
            ProjectMode::All => "all",
            ProjectMode::ErrorsOnly => "errors_only",
            ProjectMode::Off => "off",
        };
        set_value(projects, path, Value::from(name));
    }
}

fn set_cli(table: &mut Table, cli: &CliSettings) {
    set_value(table, "command", Value::from(cli.command.as_str()));
    set_value(
        table,
        "orchestrator_model",
        Value::from(cli.orchestrator_model.as_str()),
    );
    set_value(
        table,
        "worker_model",
        Value::from(cli.worker_model.as_str()),
    );
}

/// The table under `key`. An inline table becomes a normal one; anything else is replaced.
fn table_mut<'a>(parent: &'a mut Table, key: &str) -> &'a mut Table {
    let item = parent.entry(key).or_insert(Item::Table(Table::new()));

    if !item.is_table() {
        let taken = std::mem::take(item);
        *item = Item::Table(taken.into_table().unwrap_or_default());
    }

    match item {
        Item::Table(table) => table,
        _ => unreachable!("the item was made a table above"),
    }
}

/// Replaces a value and keeps the comments around it.
fn set_value(table: &mut Table, key: &str, mut value: Value) {
    if let Some(Item::Value(existing)) = table.get_mut(key) {
        *value.decor_mut() = existing.decor().clone();
        *existing = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let settings: Settings = toml::from_str("").unwrap();

        assert_eq!(settings, Settings::default());
        assert_eq!(
            settings
                .agents
                .model(AgentKind::ClaudeCode, AgentRole::Orchestrator),
            "opus"
        );
        assert_eq!(
            settings
                .agents
                .model(AgentKind::ClaudeCode, AgentRole::Worker),
            "sonnet"
        );
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let settings: Settings =
            toml::from_str("[agents]\norchestrator = \"codex\"\nskip_permissions = true\n")
                .unwrap();

        assert_eq!(settings.agents.orchestrator, AgentKindSetting::Codex);
        assert!(settings.agents.skip_permissions);
        assert_eq!(
            settings
                .agents
                .model(AgentKind::Codex, AgentRole::Orchestrator),
            "gpt-reserve"
        );
    }

    #[test]
    fn settings_round_trip_through_a_file() {
        let folder = std::env::temp_dir().join(format!("swarm-settings-{}", std::process::id()));
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.agents.codex.worker_model = "gpt-6-sol".to_owned();

        save(&path, &settings).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(loaded, settings);
    }

    #[test]
    fn worker_typing_is_allowed_by_default() {
        let settings: Settings = toml::from_str("[agents]\nskip_permissions = true\n").unwrap();

        assert!(settings.terminals.allow_worker_typing);
    }

    #[test]
    fn turning_worker_typing_off_is_saved_and_loaded() {
        let folder =
            std::env::temp_dir().join(format!("swarm-settings-typing-{}", std::process::id()));
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.terminals.allow_worker_typing = false;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("[terminals]\nallow_worker_typing = false"));
        assert!(!loaded.terminals.allow_worker_typing);
    }

    #[test]
    fn closing_a_terminal_asks_by_default() {
        let empty: Settings = toml::from_str("").unwrap();
        let old: Settings = toml::from_str("[terminals]\nallow_worker_typing = false\n").unwrap();

        assert!(Settings::default().terminals.confirm_close);
        assert!(empty.terminals.confirm_close);
        assert!(old.terminals.confirm_close);
    }

    #[test]
    fn turning_the_close_question_off_is_saved_and_loaded() {
        let folder = temp_folder("confirm-close");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.terminals.confirm_close = false;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("[terminals]"));
        assert!(text.contains("confirm_close = false"), "{text}");
        assert!(!loaded.terminals.confirm_close);
    }

    #[test]
    fn saving_the_close_question_keeps_comments_and_other_keys() {
        let folder = temp_folder("confirm-close-keep");
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            &path,
            "# my notes\n\n[terminals]\nconfirm_close = true # ask\nfont_size = 15\nextra = \"x\"\n",
        )
        .unwrap();
        let mut settings = load(&path).unwrap();
        settings.terminals.confirm_close = false;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("# my notes"));
        assert!(text.contains("confirm_close = false # ask"), "{text}");
        assert!(text.contains("extra = \"x\""));
        assert_eq!(loaded.terminals.font_size, 15);
        assert!(!loaded.terminals.confirm_close);
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let loaded = load(Path::new("/nonexistent/swarm/settings.toml")).unwrap();

        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn a_partial_claude_code_table_keeps_the_other_defaults() {
        let settings: Settings =
            toml::from_str("[agents.claude_code]\norchestrator_model = \"sonnet\"\n").unwrap();

        let cli = &settings.agents.claude_code;
        assert_eq!(cli.command, "claude");
        assert_eq!(cli.orchestrator_model, "sonnet");
        assert_eq!(cli.worker_model, "sonnet");
        assert_eq!(settings.agents.codex, CliSettings::codex());
    }

    #[test]
    fn a_partial_codex_table_keeps_the_other_defaults() {
        let settings: Settings =
            toml::from_str("[agents.codex]\nworker_model = \"gpt-6-sol\"\n").unwrap();

        let cli = &settings.agents.codex;
        assert_eq!(cli.command, "codex");
        assert_eq!(cli.orchestrator_model, "gpt-reserve");
        assert_eq!(cli.worker_model, "gpt-6-sol");
        assert_eq!(settings.agents.claude_code, CliSettings::claude_code());
    }

    #[test]
    fn the_easter_eggs_are_on_by_default() {
        let settings: Settings = toml::from_str("").unwrap();

        assert!(settings.appearance.easter_eggs);
        assert_eq!(settings.appearance.accent, AccentSetting::Blue);
        assert!(!settings.appearance.swarm_view_honeycomb);
    }

    #[test]
    fn an_old_appearance_section_keeps_the_eggs_on() {
        let settings: Settings = toml::from_str("[appearance]\naccent = \"honey\"\n").unwrap();

        assert!(settings.appearance.easter_eggs);
        assert_eq!(settings.appearance.accent, AccentSetting::Honey);
    }

    #[test]
    fn appearance_round_trips_through_a_file() {
        let folder = temp_folder("appearance");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.appearance.easter_eggs = false;
        settings.appearance.accent = AccentSetting::Honey;
        settings.appearance.swarm_view_honeycomb = true;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("easter_eggs = false"));
        assert!(text.contains("accent = \"honey\""));
        assert_eq!(loaded.appearance, settings.appearance);
    }

    #[test]
    fn updates_check_automatically_by_default() {
        let settings: Settings = toml::from_str("").unwrap();

        assert!(settings.updates.auto_check);
    }

    #[test]
    fn a_bad_auto_check_falls_back_and_the_other_sections_still_load() {
        let settings: Settings =
            toml::from_str("[updates]\nauto_check = \"yes\"\n[terminals]\nfont_size = 15\n")
                .unwrap();

        assert!(settings.updates.auto_check);
        assert_eq!(settings.terminals.font_size, 15);
    }

    #[test]
    fn auto_check_round_trips_through_a_file() {
        let folder = temp_folder("updates");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.updates.auto_check = false;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("auto_check = false"), "{text}");
        assert_eq!(loaded.updates, settings.updates);
    }

    fn temp_folder(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("swarm-settings-{name}-{}", std::process::id()))
    }

    #[test]
    fn notifications_default_to_the_plan() {
        let settings: Settings = toml::from_str("").unwrap();

        let notifications = settings.notifications;
        assert!(notifications.needs_decision);
        assert!(notifications.prompt);
        assert!(notifications.ready_for_review);
        assert!(notifications.broke);
        assert!(!notifications.sound);
        assert!(notifications.dock_bounce);
        assert!(notifications.mute_while_viewing);
        assert!(notifications.menu_bar_dot);
        assert!(notifications.projects.is_empty());
    }

    #[test]
    fn an_old_file_without_a_notifications_section_still_loads() {
        let folder = temp_folder("old-file");
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            &path,
            "[agents]\nskip_permissions = true\n\n[terminals]\nallow_worker_typing = false\n",
        )
        .unwrap();

        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(loaded.notifications, NotificationSettings::default());
        assert!(loaded.agents.skip_permissions);
    }

    #[test]
    fn a_partial_notifications_section_keeps_the_other_defaults() {
        let settings: Settings =
            toml::from_str("[notifications]\nsound = true\nprompt = false\n").unwrap();

        let notifications = settings.notifications;
        assert!(notifications.sound);
        assert!(!notifications.prompt);
        assert!(notifications.needs_decision);
        assert!(notifications.menu_bar_dot);
    }

    #[test]
    fn each_trigger_reads_its_own_switch() {
        let notifications = NotificationSettings {
            needs_decision: false,
            ready_for_review: false,
            ..NotificationSettings::default()
        };

        assert!(!notifications.triggers(NoticeKind::NeedsDecision));
        assert!(notifications.triggers(NoticeKind::Prompt));
        assert!(!notifications.triggers(NoticeKind::ReadyForReview));
        assert!(notifications.triggers(NoticeKind::Broke));
    }

    #[test]
    fn notifications_round_trip_through_a_file() {
        let folder = temp_folder("notifications");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.notifications.sound = true;
        settings.notifications.broke = false;
        settings.notifications.menu_bar_dot = false;
        settings
            .notifications
            .set_mode(Path::new("/work/swarm"), ProjectMode::ErrorsOnly);
        settings
            .notifications
            .set_mode(Path::new("/work/with space"), ProjectMode::Off);

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("[notifications]"));
        assert!(text.contains("\"/work/swarm\" = \"errors_only\""));
        assert!(text.contains("\"/work/with space\" = \"off\""));
        assert_eq!(loaded, settings);
    }

    #[test]
    fn saving_an_old_file_adds_the_section_and_keeps_the_rest() {
        let folder = temp_folder("old-save");
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&path, "# mine\n[agents]\nskip_permissions = true\n").unwrap();
        let mut settings = load(&path).unwrap();
        settings.notifications.dock_bounce = false;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.starts_with("# mine\n"));
        assert!(text.contains("dock_bounce = false"));
        assert!(!loaded.notifications.dock_bounce);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn the_mode_of_an_unlisted_project_is_all() {
        let notifications = NotificationSettings::default();

        assert_eq!(
            notifications.mode(Path::new("/work/none")),
            ProjectMode::All
        );
    }

    #[test]
    fn choosing_all_again_removes_the_project_from_the_file() {
        let folder = temp_folder("mode-all");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        let project = Path::new("/work/swarm");
        settings.notifications.set_mode(project, ProjectMode::Off);
        settings
            .notifications
            .set_mode(Path::new("/work/other"), ProjectMode::ErrorsOnly);
        save(&path, &settings).unwrap();

        settings.notifications.set_mode(project, ProjectMode::All);
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(!text.contains("/work/swarm"));
        assert_eq!(loaded.notifications.mode(project), ProjectMode::All);
        assert_eq!(
            loaded.notifications.mode(Path::new("/work/other")),
            ProjectMode::ErrorsOnly
        );
    }

    #[test]
    fn a_bad_value_in_the_notifications_section_falls_back_and_the_rest_still_loads() {
        let folder = temp_folder("bad-notifications");
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            &path,
            "[agents]\norchestrator = \"codex\"\n\n[notifications]\nsound = \"yes\"\n\
             broke = \"no\"\nprompt = false\n\n[notifications.projects]\n\
             \"/repo\" = \"errors-only\"\n\"/work/swarm\" = \"off\"\n",
        )
        .unwrap();

        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(loaded.agents.orchestrator, AgentKindSetting::Codex);
        assert!(!loaded.notifications.sound, "the default of sound");
        assert!(loaded.notifications.broke, "the default of broke");
        assert!(!loaded.notifications.prompt, "a valid value stays");
        assert_eq!(
            loaded.notifications.mode(Path::new("/repo")),
            ProjectMode::All
        );
        assert_eq!(
            loaded.notifications.mode(Path::new("/work/swarm")),
            ProjectMode::Off
        );
    }

    #[test]
    fn a_notifications_section_of_the_wrong_shape_gives_the_defaults() {
        for text in [
            "notifications = 5\n",
            "notifications = \"loud\"\n",
            "[notifications]\nprojects = 3\n",
        ] {
            let settings: Settings = toml::from_str(text).unwrap();

            assert_eq!(
                settings.notifications,
                NotificationSettings::default(),
                "{text}"
            );
        }
    }

    fn font_size_of(text: &str) -> u8 {
        let settings: Settings = toml::from_str(text).unwrap();

        settings.terminals.font_size
    }

    #[test]
    fn the_font_size_is_13_points_by_default() {
        assert_eq!(Settings::default().terminals.font_size, 13);
        assert_eq!(font_size_of(""), 13);
        assert_eq!(
            font_size_of("[terminals]\nallow_worker_typing = false\n"),
            13
        );
    }

    #[test]
    fn a_font_size_in_range_is_kept() {
        for points in [11, 13, 16, 20] {
            assert_eq!(
                font_size_of(&format!("[terminals]\nfont_size = {points}\n")),
                points
            );
        }
    }

    #[test]
    fn a_font_size_out_of_range_is_clamped_on_load() {
        let sizes: Vec<u8> = [
            "font_size = 0",
            "font_size = 10",
            "font_size = 21",
            "font_size = 300",
            "font_size = -4",
            "font_size = 99999999999",
        ]
        .iter()
        .map(|line| font_size_of(&format!("[terminals]\n{line}\n")))
        .collect();

        assert_eq!(sizes, [11, 11, 20, 20, 11, 20]);
    }

    #[test]
    fn a_fractional_font_size_is_rounded_into_range() {
        let sizes: Vec<u8> = ["14.4", "14.6", "3.2", "40.5"]
            .iter()
            .map(|points| font_size_of(&format!("[terminals]\nfont_size = {points}\n")))
            .collect();

        assert_eq!(sizes, [14, 15, 11, 20]);
    }

    #[test]
    fn the_font_size_is_saved_and_loaded() {
        let folder =
            std::env::temp_dir().join(format!("swarm-settings-font-{}", std::process::id()));
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.terminals.font_size = 16;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("font_size = 16"));
        assert_eq!(loaded.terminals.font_size, 16);
    }

    #[test]
    fn saving_never_writes_a_font_size_out_of_range() {
        let folder =
            std::env::temp_dir().join(format!("swarm-settings-font-range-{}", std::process::id()));
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.terminals.font_size = 99;

        save(&path, &settings).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(loaded.terminals.font_size, 20);
    }

    fn load_text(name: &str, text: &str) -> Result<(Settings, Vec<String>), SettingsError> {
        let folder = temp_folder(name);
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&path, text).unwrap();

        let loaded = load_with_warnings(&path);
        std::fs::remove_dir_all(&folder).unwrap();

        loaded
    }

    #[test]
    fn a_bad_value_in_each_section_falls_back_per_key_and_keeps_the_rest() {
        let text = "welcome_done = \"yes\"\n\n\
                    [agents]\norchestrator = \"gemini\"\nskip_permissions = true\n\
                    idle_stop_minutes = \"soon\"\nmax_agents_per_task = 0\nmax_agents_total = 3\n\n\
                    [agents.codex]\nworker_model = 5\ncommand = \"cx\"\n\n\
                    [terminals]\nallow_worker_typing = \"yes\"\nfont_size = \"big\"\nconfirm_close = false\n\n\
                    [appearance]\ntheme = \"dark\"\naccent = \"green\"\neaster_eggs = 1\n";

        let (settings, _) = load_text("bad-each", text).unwrap();

        assert!(!settings.welcome_done);
        assert_eq!(settings.agents.orchestrator, AgentKindSetting::ClaudeCode);
        assert!(settings.agents.skip_permissions, "a valid value stays");
        assert_eq!(settings.agents.idle_stop_minutes, IDLE_STOP_MINUTES_DEFAULT);
        assert_eq!(
            settings.agents.max_agents_per_task,
            MAX_AGENTS_PER_TASK_DEFAULT
        );
        assert_eq!(settings.agents.max_agents_total, 3);
        assert_eq!(settings.agents.codex.worker_model, "gpt-6-luna");
        assert_eq!(settings.agents.codex.command, "cx");
        assert!(settings.terminals.allow_worker_typing);
        assert_eq!(settings.terminals.font_size, FONT_SIZE_DEFAULT);
        assert!(!settings.terminals.confirm_close);
        assert_eq!(settings.appearance.theme, ThemeSetting::Dark);
        assert_eq!(settings.appearance.accent, AccentSetting::Blue);
        assert!(settings.appearance.easter_eggs);
    }

    #[test]
    fn a_warning_names_the_key_that_fell_back() {
        let text = "[agents]\nskip_permissions = \"yes\"\n\n[agents.claude_code]\ncommand = 3\n\n\
                    [terminals]\nfont_size = \"big\"\n\n[notifications.projects]\n\"/repo\" = \"loud\"\n";

        let (_, warnings) = load_text("warnings", text).unwrap();

        assert!(
            warnings.contains(
                &"agents.skip_permissions: not a true/false value, using false".to_owned()
            ),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with("agents.claude_code.command: not text")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with("terminals.font_size: ")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with("notifications.projects./repo: ")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_valid_file_has_no_warnings() {
        let (_, warnings) =
            load_text("no-warnings", "[agents]\nskip_permissions = true\n").unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_section_of_the_wrong_shape_gives_its_defaults_and_a_warning() {
        let (settings, warnings) = load_text("shape", "agents = 5\nterminals = \"x\"\n").unwrap();

        assert_eq!(settings.agents, AgentSettings::default());
        assert_eq!(settings.terminals, TerminalSettings::default());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with("agents: "))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with("terminals: "))
        );
    }

    #[test]
    fn a_file_that_is_not_toml_is_still_an_error() {
        let loaded = load_text("syntax", "[agents\nskip_permissions = \n");

        assert!(matches!(loaded, Err(SettingsError::Parse { .. })));
    }

    #[test]
    fn load_returns_the_same_settings_as_load_with_warnings() {
        let folder = temp_folder("load-same");
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            &path,
            "[agents]\nskip_permissions = \"yes\"\nmax_agents_total = 2\n",
        )
        .unwrap();

        let plain = load(&path).unwrap();
        let (with_warnings, _) = load_with_warnings(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(plain, with_warnings);
        assert_eq!(plain.agents.max_agents_total, 2);
    }

    #[test]
    fn saving_after_a_bad_value_writes_a_corrected_file() {
        let folder = temp_folder("corrected");
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&path, "[agents]\nskip_permissions = \"yes\"\n").unwrap();
        let (settings, _) = load_with_warnings(&path).unwrap();

        save(&path, &settings).unwrap();
        let (_, warnings) = load_with_warnings(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(text.contains("skip_permissions = false"), "{text}");
    }

    #[test]
    fn the_agent_limits_and_idle_stop_have_their_defaults() {
        let settings: Settings = toml::from_str("").unwrap();

        assert_eq!(settings.agents.idle_stop_minutes, 30);
        assert_eq!(settings.agents.max_agents_per_task, 6);
        assert_eq!(settings.agents.max_agents_total, 16);
    }

    #[test]
    fn the_agent_limits_and_idle_stop_round_trip_through_a_file() {
        let folder = temp_folder("limits");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        settings.agents.idle_stop_minutes = 0;
        settings.agents.max_agents_per_task = 3;
        settings.agents.max_agents_total = 9;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("idle_stop_minutes = 0"), "{text}");
        assert!(text.contains("max_agents_per_task = 3"), "{text}");
        assert!(text.contains("max_agents_total = 9"), "{text}");
        assert_eq!(loaded.agents, settings.agents);
    }

    #[test]
    fn a_limit_below_one_falls_back_to_its_default() {
        let settings: Settings =
            toml::from_str("[agents]\nmax_agents_per_task = 0\nmax_agents_total = -2\n").unwrap();

        assert_eq!(settings.agents.max_agents_per_task, 6);
        assert_eq!(settings.agents.max_agents_total, 16);
    }

    #[test]
    fn the_theme_is_system_by_default_and_round_trips() {
        let folder = temp_folder("theme");
        let path = folder.join("settings.toml");
        let mut settings = Settings::default();
        let default_theme = settings.appearance.theme;
        settings.appearance.theme = ThemeSetting::Light;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(default_theme, ThemeSetting::System);
        assert!(text.contains("theme = \"light\""), "{text}");
        assert_eq!(loaded.appearance.theme, ThemeSetting::Light);
    }

    #[test]
    fn an_unknown_theme_falls_back_to_system() {
        for text in [
            "[appearance]\ntheme = \"sepia\"\n",
            "[appearance]\ntheme = 4\n",
        ] {
            let settings: Settings = toml::from_str(text).unwrap();

            assert_eq!(settings.appearance.theme, ThemeSetting::System, "{text}");
        }
    }

    #[test]
    fn saving_keeps_comments_and_unknown_keys() {
        let folder =
            std::env::temp_dir().join(format!("swarm-settings-keep-{}", std::process::id()));
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            &path,
            "# my notes\nfuture = 1\n\n[agents]\nskip_permissions = false # careful\nextra = \"x\"\n",
        )
        .unwrap();
        let mut settings = load(&path).unwrap();
        settings.agents.skip_permissions = true;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("# my notes"));
        assert!(text.contains("future = 1"));
        assert!(text.contains("extra = \"x\""));
        assert!(text.contains("skip_permissions = true # careful"));
        assert_eq!(loaded, settings);
    }

    #[test]
    fn the_welcome_is_not_done_by_default_and_an_old_file_still_loads() {
        let empty: Settings = toml::from_str("").unwrap();
        let old: Settings = toml::from_str("[agents]\nskip_permissions = true\n").unwrap();

        assert!(!empty.welcome_done);
        assert!(!old.welcome_done);
    }

    #[test]
    fn the_welcome_flag_is_saved_at_the_top_and_loaded() {
        let folder =
            std::env::temp_dir().join(format!("swarm-settings-welcome-{}", std::process::id()));
        let path = folder.join("settings.toml");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&path, "# my notes\n\n[agents]\nskip_permissions = true\n").unwrap();
        let mut settings = load(&path).unwrap();
        settings.welcome_done = true;

        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = load(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(text.contains("# my notes"));
        assert!(
            text.find("welcome_done = true") < text.find("[agents]"),
            "{text}"
        );
        assert!(loaded.welcome_done);
        assert!(loaded.agents.skip_permissions);
    }

    #[test]
    fn saving_with_the_welcome_open_adds_no_key() {
        let folder = std::env::temp_dir().join(format!(
            "swarm-settings-welcome-open-{}",
            std::process::id()
        ));
        let path = folder.join("settings.toml");

        save(&path, &Settings::default()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();

        assert!(!text.contains("welcome_done"), "{text}");
    }

    struct Capture;

    static CAPTURED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            CAPTURED.lock().unwrap().push(record.args().to_string());
        }

        fn flush(&self) {}
    }

    #[test]
    fn load_logs_nothing_and_load_with_warnings_still_reports() {
        log::set_logger(&Capture).ok();
        log::set_max_level(log::LevelFilter::Warn);
        let folder =
            std::env::temp_dir().join(format!("swarm-settings-quiet-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("settings.toml");
        std::fs::write(&path, "[agents]\nskip_permissions = \"yes\"\n").unwrap();

        let (_, warnings) = load_with_warnings(&path).unwrap();
        load(&path).unwrap();
        let logged = CAPTURED
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(&*folder.to_string_lossy()));
        std::fs::remove_dir_all(&folder).unwrap();

        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(!logged, "load logged a warning");
    }
}
