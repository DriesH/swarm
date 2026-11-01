//! User settings in `~/.config/swarm/settings.toml`. People edit the file by hand or in
//! the app's Settings window; the daemon reads it when it starts an agent.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::{self, DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize};
use swarm_protocol::{AgentKind, AgentRole, NoticeKind};
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Settings {
    /// The welcome screen was dealt with: skipped, or left to open a project. It shows
    /// only while this is false.
    pub welcome_done: bool,
    pub agents: AgentSettings,
    pub terminals: TerminalSettings,
    pub notifications: NotificationSettings,
}

/// Which notices become system notifications, and how they are delivered. Reading never
/// fails: a value that is not valid falls back to its default, and a line in the log
/// says so. A typo here must not stop the daemon from starting agents.
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

impl<'de> Deserialize<'de> for NotificationSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        toml::Value::deserialize(deserializer).map(Self::from_value)
    }
}

impl NotificationSettings {
    fn from_value(value: toml::Value) -> Self {
        let toml::Value::Table(mut table) = value else {
            log::warn!("[notifications] is not a table, so it uses the defaults");

            return Self::default();
        };
        let defaults = Self::default();

        Self {
            needs_decision: take_or(&mut table, "needs_decision", defaults.needs_decision),
            prompt: take_or(&mut table, "prompt", defaults.prompt),
            ready_for_review: take_or(&mut table, "ready_for_review", defaults.ready_for_review),
            broke: take_or(&mut table, "broke", defaults.broke),
            sound: take_or(&mut table, "sound", defaults.sound),
            dock_bounce: take_or(&mut table, "dock_bounce", defaults.dock_bounce),
            mute_while_viewing: take_or(
                &mut table,
                "mute_while_viewing",
                defaults.mute_while_viewing,
            ),
            menu_bar_dot: take_or(&mut table, "menu_bar_dot", defaults.menu_bar_dot),
            projects: take_projects(&mut table),
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

/// The value under `key`, or `default` when it is missing or not valid.
fn take_or<T: DeserializeOwned>(table: &mut toml::Table, key: &str, default: T) -> T {
    let Some(value) = table.remove(key) else {
        return default;
    };

    T::deserialize(value).unwrap_or_else(|error| {
        log::warn!("notifications.{key} is not valid, so it uses the default: {error}");

        default
    })
}

/// The projects with a mode that is valid. A project whose mode is not valid is left out,
/// which makes it [`ProjectMode::All`].
fn take_projects(table: &mut toml::Table) -> BTreeMap<String, ProjectMode> {
    let Some(value) = table.remove("projects") else {
        return BTreeMap::new();
    };
    let toml::Value::Table(entries) = value else {
        log::warn!("notifications.projects is not a table, so it uses the default");

        return BTreeMap::new();
    };

    entries
        .into_iter()
        .filter_map(|(path, mode)| match ProjectMode::deserialize(mode) {
            Ok(mode) => Some((path, mode)),
            Err(error) => {
                log::warn!("notifications.projects has no valid mode for {path}: {error}");

                None
            }
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct TerminalSettings {
    /// Lets the user type into worker terminals.
    pub allow_worker_typing: bool,
    /// The font size of every terminal, in points. A value outside
    /// [`FONT_SIZE_MIN`]..=[`FONT_SIZE_MAX`] in the file is brought into it.
    #[serde(deserialize_with = "read_font_size")]
    pub font_size: u8,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            allow_worker_typing: true,
            font_size: FONT_SIZE_DEFAULT,
        }
    }
}

/// `points` brought into the range the settings allow.
pub fn clamp_font_size(points: u8) -> u8 {
    points.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX)
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(from = "PartialAgentSettings")]
pub struct AgentSettings {
    /// The CLI that new orchestrators run.
    pub orchestrator: AgentKindSetting,
    /// Starts agents without permission prompts (Claude Code
    /// `--dangerously-skip-permissions`, Codex `--yolo`).
    pub skip_permissions: bool,
    pub claude_code: CliSettings,
    pub codex: CliSettings,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            orchestrator: AgentKindSetting::ClaudeCode,
            skip_permissions: false,
            claude_code: CliSettings::claude_code(),
            codex: CliSettings::codex(),
        }
    }
}

/// The file as written: every key can be left out.
#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialAgentSettings {
    orchestrator: Option<AgentKindSetting>,
    skip_permissions: Option<bool>,
    claude_code: PartialCliSettings,
    codex: PartialCliSettings,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialCliSettings {
    command: Option<String>,
    orchestrator_model: Option<String>,
    worker_model: Option<String>,
}

impl PartialCliSettings {
    fn over(self, defaults: CliSettings) -> CliSettings {
        CliSettings {
            command: self.command.unwrap_or(defaults.command),
            orchestrator_model: self
                .orchestrator_model
                .unwrap_or(defaults.orchestrator_model),
            worker_model: self.worker_model.unwrap_or(defaults.worker_model),
        }
    }
}

impl From<PartialAgentSettings> for AgentSettings {
    fn from(partial: PartialAgentSettings) -> Self {
        let defaults = Self::default();

        Self {
            orchestrator: partial.orchestrator.unwrap_or(defaults.orchestrator),
            skip_permissions: partial
                .skip_permissions
                .unwrap_or(defaults.skip_permissions),
            claude_code: partial.claude_code.over(defaults.claude_code),
            codex: partial.codex.over(defaults.codex),
        }
    }
}

impl AgentSettings {
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

/// A missing file gives the defaults; keys left out keep their defaults.
pub fn load(path: &Path) -> Result<Settings, SettingsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(source) => {
            return Err(SettingsError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };

    toml::from_str(&text).map_err(|source| SettingsError::Parse {
        path: path.to_owned(),
        source,
    })
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

    set_notifications(
        table_mut(document.as_table_mut(), "notifications"),
        &settings.notifications,
    );

    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(io_error)?;
    }

    let temporary = path.with_extension("toml.tmp");

    std::fs::write(&temporary, document.to_string()).map_err(io_error)?;
    std::fs::rename(&temporary, path).map_err(io_error)
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

    #[test]
    fn a_bad_value_outside_the_notifications_section_still_fails() {
        for text in [
            "[agents]\norchestrator = \"gemini\"\n",
            "[agents]\nskip_permissions = \"yes\"\n",
            "[terminals]\nallow_worker_typing = \"yes\"\n",
            "[terminals]\nfont_size = \"big\"\n",
        ] {
            let result: Result<Settings, _> = toml::from_str(text);

            assert!(result.is_err(), "{text}");
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
}
