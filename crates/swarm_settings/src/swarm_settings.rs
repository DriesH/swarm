//! User settings in `~/.config/swarm/settings.toml`. People edit the file by hand or in
//! the app's Settings window; the daemon reads it when it starts an agent.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use swarm_protocol::{AgentKind, AgentRole};
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Settings {
    pub agents: AgentSettings,
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
            AgentRole::Orchestrator => &cli.orchestrator_model,
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

    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(io_error)?;
    }

    let temporary = path.with_extension("toml.tmp");

    std::fs::write(&temporary, document.to_string()).map_err(io_error)?;
    std::fs::rename(&temporary, path).map_err(io_error)
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
}
