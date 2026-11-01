//! `.swarm/config.toml`: per-repository settings, edited by hand.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::Deserialize;
use swarm_protocol::ProjectConfigFields;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::Error;

const DEFAULT_FOLDER: &str = ".worktrees";

/// Saves read the file and write it back, so two saves must not interleave.
static SAVING: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub worktrees: WorktreeConfig,
    pub checks: ChecksConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WorktreeConfig {
    pub base_branch: Option<String>,
    /// Relative to the repository root.
    pub folder: String,
    pub ports: Option<PortRange>,
    /// Files copied from the repository root into each task worktree.
    pub copy: Vec<String>,
    pub setup: String,
    pub teardown: String,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            base_branch: None,
            folder: DEFAULT_FOLDER.to_owned(),
            ports: None,
            copy: Vec::new(),
            setup: String::new(),
            teardown: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PortRange {
    /// `"5170-5199"`, both ends included.
    pub range: String,
    pub per_worktree: u16,
}

impl PortRange {
    /// The first and last port of the range.
    pub fn bounds(&self) -> Result<(u16, u16), String> {
        let invalid = || format!("ports.range {:?} must look like \"5170-5199\"", self.range);

        let (first, last) = self.range.split_once('-').ok_or_else(invalid)?;
        let first: u16 = first.trim().parse().map_err(|_| invalid())?;
        let last: u16 = last.trim().parse().map_err(|_| invalid())?;

        if first == 0 || first > last {
            return Err(invalid());
        }

        Ok((first, last))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ChecksConfig {
    pub before_pr: Vec<String>,
    pub required: bool,
}

impl ChecksConfig {
    /// The commands to run: trimmed, without empty ones.
    pub fn commands(&self) -> Vec<String> {
        self.before_pr
            .iter()
            .map(|command| command.trim().to_owned())
            .filter(|command| !command.is_empty())
            .collect()
    }
}

/// What reading the config file gave: the settings to use and why the file failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: ProjectConfig,
    pub error: Option<String>,
}

impl ProjectConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        check_folder(&self.worktrees.folder)?;

        if let Some(ports) = &self.worktrees.ports {
            check_ports(ports)?;
        }

        Ok(())
    }

    /// What Settings › Projects shows.
    pub fn fields(&self) -> ProjectConfigFields {
        let (port_range, ports_per_worktree) = match &self.worktrees.ports {
            Some(ports) => (ports.range.clone(), ports.per_worktree),
            None => (String::new(), 0),
        };

        ProjectConfigFields {
            base_branch: self
                .worktrees
                .base_branch
                .clone()
                .filter(|branch| !branch.trim().is_empty()),
            worktree_folder: self.worktrees.folder.clone(),
            port_range,
            ports_per_worktree,
            copy: self.worktrees.copy.clone(),
            setup: self.worktrees.setup.clone(),
            teardown: self.worktrees.teardown.clone(),
            checks: self.checks.before_pr.clone(),
            checks_required: self.checks.required,
        }
    }
}

fn check_folder(folder: &str) -> Result<(), String> {
    if is_inside_repository(folder) {
        return Ok(());
    }

    Err(format!(
        "worktrees.folder {folder:?} must be a folder inside the repository"
    ))
}

fn check_ports(ports: &PortRange) -> Result<(), String> {
    let (first, last) = ports.bounds()?;

    let size = u32::from(last) - u32::from(first) + 1;
    if ports.per_worktree == 0 || u32::from(ports.per_worktree) > size {
        return Err(format!(
            "ports.per_worktree must be between 1 and {size}, the size of the range"
        ));
    }

    Ok(())
}

/// A relative path that stays inside the folder it is relative to.
pub(crate) fn is_inside_repository(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn config_path(root: &Path) -> PathBuf {
    root.join(".swarm").join("config.toml")
}

/// Reads `<root>/.swarm/config.toml`. A missing file gives the defaults; an invalid
/// one gives the defaults and the reason.
pub fn load(root: &Path) -> LoadedConfig {
    let path = config_path(root);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedConfig::default();
        }
        Err(error) => {
            return LoadedConfig {
                config: ProjectConfig::default(),
                error: Some(format!("cannot read {}: {error}", path.display())),
            };
        }
    };

    match ProjectConfig::parse(&text) {
        Ok(config) => LoadedConfig {
            config,
            error: None,
        },
        Err(error) => LoadedConfig {
            config: ProjectConfig::default(),
            error: Some(error),
        },
    }
}

/// What Settings › Projects shows: the fields of `<root>/.swarm/config.toml` and, when the
/// file has a problem, why. A file with a problem gives the defaults.
pub fn read_fields(root: &Path) -> (ProjectConfigFields, Option<String>) {
    let LoadedConfig { config, error } = load(root);

    (config.fields(), error)
}

/// Writes the fields that differ from the file into `<root>/.swarm/config.toml` and keeps
/// everything else: comments, key order and the keys Swarm does not show. A file that
/// does not parse is left as it is.
pub fn save(root: &Path, fields: &ProjectConfigFields) -> Result<(), Error> {
    let wanted = ProjectConfigFields {
        base_branch: fields
            .base_branch
            .clone()
            .filter(|branch| !branch.trim().is_empty()),
        ..fields.clone()
    };
    validate_fields(&wanted).map_err(Error::Invalid)?;

    let _saving = SAVING.lock().unwrap_or_else(PoisonError::into_inner);

    let path = config_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::Io {
                action: format!("read {}", path.display()),
                source,
            });
        }
    };

    let refuse = |problem: String| {
        Error::Invalid(format!(
            "{} has a problem, so Swarm leaves it alone: {problem}",
            path.display()
        ))
    };
    let current = ProjectConfig::parse(&text).map_err(refuse)?;
    let mut document: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| refuse(error.to_string()))?;

    let changes = changes(&current.fields(), &wanted);
    if changes.is_empty() {
        return Ok(());
    }

    for change in changes {
        change.apply(&mut document);
    }

    write_atomic(&path, &document.to_string())
}

fn validate_fields(fields: &ProjectConfigFields) -> Result<(), String> {
    check_folder(&fields.worktree_folder)?;

    if !fields.port_range.is_empty() {
        check_ports(&PortRange {
            range: fields.port_range.clone(),
            per_worktree: fields.ports_per_worktree,
        })?;
    }

    if let Some(entry) = fields
        .copy
        .iter()
        .find(|entry| !is_inside_repository(entry))
    {
        return Err(format!(
            "worktrees.copy {entry:?} must be a path inside the repository"
        ));
    }

    if fields.checks.iter().any(|check| check.trim().is_empty()) {
        return Err("checks.before_pr must not hold an empty command".to_owned());
    }

    Ok(())
}

const WORKTREES: &str = "worktrees";
const CHECKS: &str = "checks";

/// One edit of the document.
enum Change {
    Set {
        table: &'static str,
        key: &'static str,
        value: Value,
    },
    Remove {
        table: &'static str,
        key: &'static str,
    },
    /// Sets `ports`, keeping its other keys when the file has them.
    Ports { range: String, per_worktree: u16 },
}

impl Change {
    fn set(table: &'static str, key: &'static str, value: Value) -> Self {
        Self::Set { table, key, value }
    }

    fn apply(self, document: &mut DocumentMut) {
        match self {
            Self::Set { table, key, value } => {
                with_table(document, table, true, |table| set_value(table, key, value));
            }
            Self::Remove { table, key } => {
                with_table(document, table, false, |table| {
                    table.remove(key);
                });
            }
            Self::Ports {
                range,
                per_worktree,
            } => with_table(document, WORKTREES, true, |table| {
                let per_worktree = Value::from(i64::from(per_worktree));

                if let Some(ports) = table.get_mut("ports").and_then(Item::as_table_like_mut) {
                    set_value(ports, "range", Value::from(range));
                    set_value(ports, "per_worktree", per_worktree);

                    return;
                }

                let mut ports = InlineTable::new();
                ports.insert("range", Value::from(range));
                ports.insert("per_worktree", per_worktree);
                ports.fmt();
                table.insert("ports", Item::Value(Value::InlineTable(ports)));
            }),
        }
    }
}

/// The edits that turn what the file holds into what is wanted, in file order.
fn changes(current: &ProjectConfigFields, wanted: &ProjectConfigFields) -> Vec<Change> {
    let mut changes = Vec::new();

    if wanted.base_branch != current.base_branch {
        changes.push(match &wanted.base_branch {
            Some(branch) => Change::set(WORKTREES, "base_branch", Value::from(branch.as_str())),
            None => Change::Remove {
                table: WORKTREES,
                key: "base_branch",
            },
        });
    }

    if wanted.worktree_folder != current.worktree_folder {
        let folder = Value::from(wanted.worktree_folder.as_str());
        changes.push(Change::set(WORKTREES, "folder", folder));
    }

    if reserved_ports(wanted) != reserved_ports(current) {
        changes.push(match reserved_ports(wanted) {
            Some((range, per_worktree)) => Change::Ports {
                range: range.to_owned(),
                per_worktree,
            },
            None => Change::Remove {
                table: WORKTREES,
                key: "ports",
            },
        });
    }

    if wanted.copy != current.copy {
        changes.push(Change::set(WORKTREES, "copy", string_array(&wanted.copy)));
    }

    if wanted.setup != current.setup {
        changes.push(Change::set(WORKTREES, "setup", string_value(&wanted.setup)));
    }

    if wanted.teardown != current.teardown {
        changes.push(Change::set(
            WORKTREES,
            "teardown",
            string_value(&wanted.teardown),
        ));
    }

    if wanted.checks != current.checks {
        changes.push(Change::set(
            CHECKS,
            "before_pr",
            string_array(&wanted.checks),
        ));
    }

    if wanted.checks_required != current.checks_required {
        let required = Value::from(wanted.checks_required);
        changes.push(Change::set(CHECKS, "required", required));
    }

    changes
}

/// The range and its size per worktree; none when the range is empty.
fn reserved_ports(fields: &ProjectConfigFields) -> Option<(&str, u16)> {
    (!fields.port_range.is_empty())
        .then_some((fields.port_range.as_str(), fields.ports_per_worktree))
}

/// Runs `edit` on the table under `name`. A missing table is made when `create` is set.
/// Inline tables and dotted keys count as tables.
fn with_table(
    document: &mut DocumentMut,
    name: &str,
    create: bool,
    edit: impl FnOnce(&mut dyn TableLike),
) {
    if !document.get(name).is_some_and(Item::is_table_like) {
        if !create {
            return;
        }

        let mut table = Table::new();
        table.decor_mut().set_prefix(new_table_prefix(document));
        document.insert(name, Item::Table(table));
    }

    if let Some(table) = document.get_mut(name).and_then(Item::as_table_like_mut) {
        edit(table);
    }
}

/// A new table follows a blank line. Comments at the end of the file move above it, so
/// they stay after what they followed.
fn new_table_prefix(document: &mut DocumentMut) -> String {
    let comments = document.trailing().as_str().unwrap_or_default().to_owned();

    if !comments.trim().is_empty() {
        document.set_trailing("");

        let newline = if comments.ends_with('\n') { "" } else { "\n" };

        return format!("{comments}{newline}\n");
    }

    if document.is_empty() {
        String::new()
    } else {
        "\n".to_owned()
    }
}

/// Replaces a value and keeps the comment and spacing around it.
fn set_value(table: &mut dyn TableLike, key: &str, mut value: Value) {
    if let Some(Item::Value(existing)) = table.get_mut(key) {
        *value.decor_mut() = existing.decor().clone();
        *existing = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

fn string_array(entries: &[String]) -> Value {
    Value::Array(entries.iter().map(String::as_str).collect::<Array>())
}

/// A text with several lines becomes a multi-line basic string, like the PLAN example.
fn string_value(text: &str) -> Value {
    if !text.contains('\n') {
        return Value::from(text);
    }

    let mut raw = String::from("\"\"\"\n");
    let mut quotes = 0;
    for character in text.chars() {
        quotes = if character == '"' { quotes + 1 } else { 0 };

        match character {
            '\\' => raw.push_str("\\\\"),
            '"' if quotes == 3 => {
                raw.push_str("\\\"");
                quotes = 0;
            }
            '\r' => raw.push_str("\\r"),
            '\n' | '\t' => raw.push(character),
            other if other.is_control() => {
                write!(raw, "\\u{:04X}", u32::from(other)).ok();
            }
            other => raw.push(other),
        }
    }

    // Quotes right before the closing delimiter would join it.
    raw.truncate(raw.len() - quotes);
    raw.push_str(&"\\\"".repeat(quotes));
    raw.push_str("\"\"\"");

    raw.parse::<Value>().unwrap_or_else(|_| Value::from(text))
}

/// Writes the file whole or not at all: a temp file next to it, then a rename. A
/// symlink is followed, so the link stays.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), Error> {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let io_error = |source| Error::Io {
        action: format!("write {}", path.display()),
        source,
    };

    let target = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let name = target
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy());
    let temporary = target.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));

    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(io_error)?;
    }

    let written = std::fs::File::create(&temporary).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        if let Ok(existing) = std::fs::metadata(&target) {
            file.set_permissions(existing.permissions())?;
        }

        file.sync_all()
    });
    let renamed = written.and_then(|()| std::fs::rename(&temporary, &target));

    if let Err(source) = renamed {
        if let Err(error) = std::fs::remove_file(&temporary) {
            log::debug!("cannot remove {}: {error}", temporary.display());
        }

        return Err(io_error(source));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN_EXAMPLE: &str = r#"
[worktrees]
base_branch = "main"
folder = ".worktrees"
ports = { range = "5170-5199", per_worktree = 3 }
copy = [".env", "CLAUDE.local.md"]
setup = """
cargo fetch
pnpm --dir docs install --frozen-lockfile
"""
teardown = ""

[checks]
before_pr = ["cargo test --workspace", "cargo clippy -- -D warnings", "cargo fmt --check"]
required = true
"#;

    #[test]
    fn the_plan_example_parses() {
        let config = ProjectConfig::parse(PLAN_EXAMPLE).expect("config");

        assert_eq!(config.worktrees.base_branch.as_deref(), Some("main"));
        assert_eq!(config.worktrees.copy, [".env", "CLAUDE.local.md"]);
        assert_eq!(
            config.worktrees.setup,
            "cargo fetch\npnpm --dir docs install --frozen-lockfile\n"
        );
        assert_eq!(
            config.worktrees.ports,
            Some(PortRange {
                range: "5170-5199".to_owned(),
                per_worktree: 3
            })
        );
        assert_eq!(config.checks.before_pr.len(), 3);
        assert!(config.checks.required);
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = ProjectConfig::parse("").expect("config");

        assert_eq!(config, ProjectConfig::default());
        assert_eq!(config.worktrees.folder, ".worktrees");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let config = ProjectConfig::parse("[worktrees]\nfuture = 1\n[other]\na = 2\n");

        assert_eq!(config, Ok(ProjectConfig::default()));
    }

    #[test]
    fn a_missing_file_gives_the_defaults_without_an_error() {
        let loaded = load(Path::new("/nonexistent/swarm-config-test"));

        assert_eq!(loaded, LoadedConfig::default());
    }

    #[test]
    fn invalid_toml_gives_the_defaults_and_an_error() {
        let root = std::env::temp_dir().join(format!("swarm-config-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".swarm")).expect("folder");
        std::fs::write(root.join(".swarm/config.toml"), "[worktrees\nbroken").expect("file");

        let loaded = load(&root);
        std::fs::remove_dir_all(&root).expect("cleanup");

        assert_eq!(loaded.config, ProjectConfig::default());
        assert!(loaded.error.is_some());
    }

    #[test]
    fn a_wrong_type_is_an_error() {
        assert!(ProjectConfig::parse("[worktrees]\ncopy = 3\n").is_err());
    }

    #[test]
    fn a_bad_port_range_is_an_error() {
        let text = "[worktrees]\nports = { range = \"9-3\", per_worktree = 1 }\n";

        assert!(ProjectConfig::parse(text).is_err());
    }

    #[test]
    fn more_ports_per_worktree_than_the_range_holds_is_an_error() {
        let text = "[worktrees]\nports = { range = \"10-11\", per_worktree = 3 }\n";

        assert!(ProjectConfig::parse(text).is_err());
    }

    #[test]
    fn a_folder_outside_the_repository_is_an_error() {
        assert!(ProjectConfig::parse("[worktrees]\nfolder = \"../x\"\n").is_err());
        assert!(ProjectConfig::parse("[worktrees]\nfolder = \"/x\"\n").is_err());
    }

    struct Repo {
        root: PathBuf,
    }

    impl Repo {
        fn new(name: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);

            let root = std::env::temp_dir().join(format!(
                "swarm-config-{}-{}-{name}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).expect("folder");

            Self { root }
        }

        fn with_config(name: &str, text: &str) -> Self {
            let repo = Self::new(name);
            std::fs::create_dir_all(repo.root.join(".swarm")).expect("folder");
            std::fs::write(repo.file(), text).expect("file");

            repo
        }

        fn file(&self) -> PathBuf {
            config_path(&self.root)
        }

        fn text(&self) -> String {
            std::fs::read_to_string(self.file()).expect("config file")
        }

        fn fields(&self) -> ProjectConfigFields {
            read_fields(&self.root).0
        }

        /// What the app does: read the fields, change some, save all of them.
        fn save(&self, edit: impl FnOnce(&mut ProjectConfigFields)) -> Result<(), Error> {
            let mut fields = self.fields();
            edit(&mut fields);

            save(&self.root, &fields)
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    type Edit = fn(&mut ProjectConfigFields);

    fn refusal(result: Result<(), Error>) -> String {
        match result {
            Err(Error::Invalid(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn every_field_set() -> ProjectConfigFields {
        ProjectConfigFields {
            base_branch: Some("develop".to_owned()),
            worktree_folder: ".trees/tasks".to_owned(),
            port_range: "6000-6099".to_owned(),
            ports_per_worktree: 4,
            copy: vec![".env".to_owned(), "config/local.toml".to_owned()],
            setup: "cargo fetch\npnpm install\n".to_owned(),
            teardown: "docker compose down".to_owned(),
            checks: vec!["cargo test".to_owned(), "cargo fmt --check".to_owned()],
            checks_required: true,
        }
    }

    const RICH: &str = r#"# Swarm settings for this repo

[worktrees] # where tasks live
base_branch = "main"
folder   =   ".worktrees"   # keep it out of the way
future_key = 42
copy = [
    ".env",   # secrets
    "CLAUDE.local.md",
]
setup = "cargo fetch"

# my own tool
[tools.fmt]
enabled = true

[checks]
before_pr = ["cargo test"]
required = false # flip when stable
"#;

    #[test]
    fn the_fields_of_the_plan_example_are_the_file_values() {
        let config = ProjectConfig::parse(PLAN_EXAMPLE).expect("config");

        assert_eq!(
            config.fields(),
            ProjectConfigFields {
                base_branch: Some("main".to_owned()),
                worktree_folder: ".worktrees".to_owned(),
                port_range: "5170-5199".to_owned(),
                ports_per_worktree: 3,
                copy: vec![".env".to_owned(), "CLAUDE.local.md".to_owned()],
                setup: "cargo fetch\npnpm --dir docs install --frozen-lockfile\n".to_owned(),
                teardown: String::new(),
                checks: vec![
                    "cargo test --workspace".to_owned(),
                    "cargo clippy -- -D warnings".to_owned(),
                    "cargo fmt --check".to_owned(),
                ],
                checks_required: true,
            }
        );
    }

    #[test]
    fn without_ports_the_fields_have_an_empty_range_and_zero_ports() {
        let fields = ProjectConfig::default().fields();

        assert_eq!(fields.port_range, "");
        assert_eq!(fields.ports_per_worktree, 0);
        assert_eq!(fields.worktree_folder, ".worktrees");
        assert_eq!(fields.base_branch, None);
    }

    #[test]
    fn a_missing_file_reads_as_the_defaults_without_an_error() {
        let repo = Repo::new("read-missing");

        let (fields, error) = read_fields(&repo.root);

        assert_eq!(fields, ProjectConfig::default().fields());
        assert_eq!(error, None);
    }

    #[test]
    fn a_file_that_does_not_parse_reads_as_the_defaults_with_the_message() {
        let repo = Repo::with_config("read-broken", "[worktrees\nbroken");

        let (fields, error) = read_fields(&repo.root);

        assert_eq!(fields, ProjectConfig::default().fields());
        assert!(error.is_some_and(|message| !message.is_empty()));
    }

    #[test]
    fn a_blank_base_branch_reads_as_none() {
        let repo = Repo::with_config("read-blank-branch", "[worktrees]\nbase_branch = \"\"\n");

        assert_eq!(repo.fields().base_branch, None);
    }

    #[test]
    fn every_field_survives_a_save_into_a_missing_file() {
        let repo = Repo::new("round-trip");

        save(&repo.root, &every_field_set()).expect("save");

        assert_eq!(repo.fields(), every_field_set());
        assert_eq!(read_fields(&repo.root).1, None);
    }

    #[test]
    fn each_field_survives_a_save_on_its_own() {
        let edits: [(&str, Edit); 9] = [
            ("base_branch", |fields| {
                fields.base_branch = Some("develop".to_owned());
            }),
            ("folder", |fields| {
                fields.worktree_folder = ".trees".to_owned();
            }),
            ("ports", |fields| {
                fields.port_range = "6000-6009".to_owned();
                fields.ports_per_worktree = 2;
            }),
            ("copy", |fields| fields.copy = vec![".env".to_owned()]),
            ("setup", |fields| fields.setup = "make setup".to_owned()),
            ("teardown", |fields| {
                fields.teardown = "make clean\nmake reset\n".to_owned();
            }),
            ("checks", |fields| {
                fields.checks = vec!["cargo test".to_owned()];
            }),
            ("required", |fields| fields.checks_required = true),
            ("branch_none", |fields| fields.base_branch = None),
        ];

        for (name, edit) in edits {
            let repo = Repo::with_config(name, RICH);
            let mut expected = repo.fields();
            edit(&mut expected);

            repo.save(edit).expect(name);

            assert_eq!(repo.fields(), expected, "{name}");
        }
    }

    #[test]
    fn a_save_that_changes_one_field_keeps_comments_unknown_keys_and_formatting() {
        let repo = Repo::with_config("keeps", RICH);

        repo.save(|fields| fields.checks_required = true)
            .expect("save");

        assert_eq!(
            repo.text(),
            RICH.replace(
                "required = false # flip when stable",
                "required = true # flip when stable"
            )
        );
    }

    #[test]
    fn a_changed_value_keeps_the_comment_and_spacing_around_it() {
        let repo = Repo::with_config("decor", RICH);

        repo.save(|fields| fields.worktree_folder = ".trees".to_owned())
            .expect("save");

        assert_eq!(
            repo.text(),
            RICH.replace(
                "folder   =   \".worktrees\"   # keep it out of the way",
                "folder   =   \".trees\"   # keep it out of the way"
            )
        );
    }

    #[test]
    fn a_changed_list_is_replaced_in_place() {
        let repo = Repo::with_config("list", RICH);

        repo.save(|fields| fields.copy = vec![".env".to_owned(), "a/b".to_owned()])
            .expect("save");

        assert_eq!(
            repo.text(),
            RICH.replace(
                "copy = [\n    \".env\",   # secrets\n    \"CLAUDE.local.md\",\n]",
                "copy = [\".env\", \"a/b\"]"
            )
        );
    }

    #[test]
    fn saving_the_fields_as_they_are_changes_nothing() {
        let repo = Repo::with_config("same", RICH);

        repo.save(|_| {}).expect("save");

        assert_eq!(repo.text(), RICH);
    }

    #[test]
    fn a_field_equal_to_its_default_is_not_added() {
        let repo = Repo::with_config("defaults", "[checks]\nrequired = true\n");

        repo.save(|fields| fields.copy = Vec::new()).expect("save");
        repo.save(|fields| fields.teardown = String::new())
            .expect("save");
        repo.save(|fields| fields.worktree_folder = DEFAULT_FOLDER.to_owned())
            .expect("save");

        assert_eq!(repo.text(), "[checks]\nrequired = true\n");
    }

    #[test]
    fn a_new_field_is_added_next_to_the_ones_in_the_file() {
        let repo = Repo::with_config("add", "[checks]\nrequired = true\n");

        repo.save(|fields| {
            fields.base_branch = Some("main".to_owned());
            fields.teardown = "echo bye".to_owned();
        })
        .expect("save");

        assert_eq!(
            repo.text(),
            "[checks]\nrequired = true\n\n[worktrees]\nbase_branch = \"main\"\nteardown = \"echo bye\"\n"
        );
    }

    #[test]
    fn a_missing_file_and_folder_are_made_when_something_changes() {
        let repo = Repo::new("missing");
        assert!(!repo.root.join(".swarm").exists());

        repo.save(|fields| {
            fields.base_branch = Some("main".to_owned());
            fields.checks = vec!["cargo test".to_owned()];
            fields.checks_required = true;
        })
        .expect("save");

        assert_eq!(
            repo.text(),
            "[worktrees]\nbase_branch = \"main\"\n\n[checks]\nbefore_pr = [\"cargo test\"]\nrequired = true\n"
        );
    }

    #[test]
    fn saving_the_defaults_makes_no_file() {
        let repo = Repo::new("missing-defaults");

        repo.save(|_| {}).expect("save");

        assert!(!repo.root.join(".swarm").exists());
    }

    #[test]
    fn a_range_of_ports_is_written_as_an_inline_table() {
        let repo = Repo::new("ports");

        repo.save(|fields| {
            fields.port_range = "5170-5199".to_owned();
            fields.ports_per_worktree = 3;
        })
        .expect("save");

        assert_eq!(
            repo.text(),
            "[worktrees]\nports = { range = \"5170-5199\", per_worktree = 3 }\n"
        );
    }

    #[test]
    fn changing_the_ports_keeps_the_table_they_are_in() {
        let inline = Repo::with_config(
            "ports-inline",
            "[worktrees]\nports = { range = \"5170-5199\", per_worktree = 3 } # dev servers\n",
        );
        let standard = Repo::with_config(
            "ports-standard",
            "[worktrees.ports]\nrange = \"5170-5199\"\nper_worktree = 3\n",
        );

        inline
            .save(|fields| fields.ports_per_worktree = 5)
            .expect("save");
        standard
            .save(|fields| fields.port_range = "5000-5099".to_owned())
            .expect("save");

        assert_eq!(
            inline.text(),
            "[worktrees]\nports = { range = \"5170-5199\", per_worktree = 5 } # dev servers\n"
        );
        assert_eq!(
            standard.text(),
            "[worktrees.ports]\nrange = \"5000-5099\"\nper_worktree = 3\n"
        );
    }

    #[test]
    fn an_empty_range_removes_the_ports() {
        let inline = Repo::with_config(
            "no-ports-inline",
            "[worktrees]\nfolder = \".trees\"\nports = { range = \"5170-5199\", per_worktree = 3 }\ncopy = [\".env\"]\n",
        );
        let standard = Repo::with_config(
            "no-ports-standard",
            "[worktrees]\nfolder = \".trees\"\n\n[worktrees.ports]\nrange = \"5170-5199\"\nper_worktree = 3\n\n[checks]\nrequired = true\n",
        );

        for repo in [&inline, &standard] {
            repo.save(|fields| {
                fields.port_range = String::new();
                fields.ports_per_worktree = 0;
            })
            .expect("save");
        }

        assert_eq!(
            inline.text(),
            "[worktrees]\nfolder = \".trees\"\ncopy = [\".env\"]\n"
        );
        assert_eq!(
            standard.text(),
            "[worktrees]\nfolder = \".trees\"\n\n[checks]\nrequired = true\n"
        );
        assert_eq!(inline.fields().port_range, "");
        assert_eq!(standard.fields().ports_per_worktree, 0);
    }

    #[test]
    fn several_lines_of_setup_are_written_as_a_multi_line_basic_string() {
        let repo = Repo::new("multi-line");

        repo.save(|fields| {
            fields.setup = "cargo fetch\npnpm --dir docs install --frozen-lockfile\n".to_owned();
            fields.teardown = "docker compose down\nrm -rf tmp".to_owned();
        })
        .expect("save");

        assert_eq!(
            repo.text(),
            "[worktrees]\nsetup = \"\"\"\ncargo fetch\npnpm --dir docs install --frozen-lockfile\n\"\"\"\nteardown = \"\"\"\ndocker compose down\nrm -rf tmp\"\"\"\n"
        );
        assert_eq!(
            repo.fields().setup,
            "cargo fetch\npnpm --dir docs install --frozen-lockfile\n"
        );
        assert_eq!(repo.fields().teardown, "docker compose down\nrm -rf tmp");
    }

    #[test]
    fn several_lines_with_quotes_and_backslashes_are_still_a_basic_string() {
        let repo = Repo::new("basic-string");

        repo.save(|fields| fields.setup = "echo \"hi\"\nprintf 'a\\n'\n".to_owned())
            .expect("save");

        assert_eq!(
            repo.text(),
            "[worktrees]\nsetup = \"\"\"\necho \"hi\"\nprintf 'a\\\\n'\n\"\"\"\n"
        );
    }

    #[test]
    fn a_multi_line_value_keeps_the_comment_after_the_key() {
        let repo = Repo::with_config("multi-line-comment", RICH);

        repo.save(|fields| fields.setup = "cargo fetch\ncargo build\n".to_owned())
            .expect("save");

        assert_eq!(
            repo.text(),
            RICH.replace(
                "setup = \"cargo fetch\"",
                "setup = \"\"\"\ncargo fetch\ncargo build\n\"\"\""
            )
        );
    }

    #[test]
    fn special_characters_in_several_lines_survive() {
        let setups = [
            "echo \"a\" \"\"\"b\"\"\" \"\"\"\"\nx\n",
            "one\ntwo \\ three\\\nfour\n\"",
            "\"\"\n\"\"",
            "tab\there\r\nwindows\n\u{1}bell\u{7f}",
            "\n\nleading newlines",
            "multi\nline with 'single' quotes and é ü 🐝\n",
        ];

        for (number, setup) in setups.into_iter().enumerate() {
            let repo = Repo::new(&format!("special-{number}"));

            repo.save(|fields| fields.setup = setup.to_owned())
                .expect("save");

            assert_eq!(repo.fields().setup, setup, "{setup:?}");
            assert_eq!(read_fields(&repo.root).1, None, "{setup:?}");
        }
    }

    #[test]
    fn a_blank_base_branch_is_saved_as_none() {
        let repo = Repo::with_config("blank-branch", RICH);

        repo.save(|fields| fields.base_branch = Some("  ".to_owned()))
            .expect("save");

        assert!(!repo.text().contains("base_branch"));
        assert_eq!(repo.fields().base_branch, None);
    }

    #[test]
    fn a_folder_outside_the_repository_is_refused() {
        for folder in ["../x", "/x", "", "a/../../b", "./x"] {
            let repo = Repo::new("folder");

            let message = refusal(repo.save(|fields| fields.worktree_folder = folder.to_owned()));

            assert!(message.contains("worktrees.folder"), "{folder}: {message}");
            assert!(!repo.root.join(".swarm").exists(), "{folder}");
        }
    }

    #[test]
    fn a_port_range_that_does_not_look_like_a_range_is_refused() {
        for range in [
            "5199-5170",
            "abc",
            "5170",
            "5170-",
            "0-10",
            "5170-70000",
            " ",
        ] {
            let repo = Repo::new("range");

            let message = refusal(repo.save(|fields| {
                fields.port_range = range.to_owned();
                fields.ports_per_worktree = 1;
            }));

            assert!(message.contains("ports.range"), "{range}: {message}");
            assert!(!repo.root.join(".swarm").exists(), "{range}");
        }
    }

    #[test]
    fn ports_per_worktree_must_be_at_least_one_and_fit_the_range() {
        for per_worktree in [0, 11] {
            let repo = Repo::new("per-worktree");

            let message = refusal(repo.save(|fields| {
                fields.port_range = "5170-5179".to_owned();
                fields.ports_per_worktree = per_worktree;
            }));

            assert!(message.contains("ports.per_worktree"), "{message}");
            assert!(message.contains("between 1 and 10"), "{message}");
        }

        let repo = Repo::new("per-worktree-fits");
        repo.save(|fields| {
            fields.port_range = "5170-5179".to_owned();
            fields.ports_per_worktree = 10;
        })
        .expect("a block as big as the range fits");
    }

    #[test]
    fn a_copy_path_that_is_not_inside_the_repository_is_refused() {
        for entry in ["../secret", "/etc/passwd", "", "a/../../b"] {
            let repo = Repo::new("copy");

            let message = refusal(repo.save(|fields| {
                fields.copy = vec![".env".to_owned(), entry.to_owned()];
            }));

            assert!(message.contains("worktrees.copy"), "{entry}: {message}");
            assert!(!repo.root.join(".swarm").exists(), "{entry}");
        }
    }

    #[test]
    fn an_empty_check_command_is_refused() {
        for check in ["", "   "] {
            let repo = Repo::new("check");

            let message = refusal(repo.save(|fields| {
                fields.checks = vec!["cargo test".to_owned(), check.to_owned()];
            }));

            assert!(message.contains("checks.before_pr"), "{check:?}: {message}");
            assert!(!repo.root.join(".swarm").exists(), "{check:?}");
        }
    }

    #[test]
    fn a_refused_save_leaves_the_file_as_it_was() {
        let repo = Repo::with_config("refused", RICH);

        let message = refusal(repo.save(|fields| {
            fields.teardown = "echo bye".to_owned();
            fields.worktree_folder = "../out".to_owned();
        }));

        assert!(message.contains("worktrees.folder"));
        assert_eq!(repo.text(), RICH);
    }

    #[test]
    fn a_file_that_does_not_parse_is_refused_and_never_overwritten() {
        for broken in [
            "[worktrees\nbroken = ",
            "[worktrees]\ncopy = 3\n",
            "[worktrees]\nfolder = \"../x\"\n",
            "[worktrees]\nports = { range = \"9-3\", per_worktree = 1 }\n",
        ] {
            let repo = Repo::with_config("broken", broken);

            let message = refusal(save(&repo.root, &every_field_set()));

            assert!(message.contains("config.toml"), "{message}");
            assert_eq!(repo.text(), broken);
        }
    }

    #[test]
    fn a_save_leaves_no_temporary_file_behind() {
        let repo = Repo::with_config("no-temp", RICH);

        repo.save(|fields| fields.checks_required = true)
            .expect("save");
        repo.save(|fields| fields.teardown = "echo bye".to_owned())
            .expect("save");

        let names: Vec<_> = std::fs::read_dir(repo.root.join(".swarm"))
            .expect("folder")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    #[test]
    fn a_failed_write_leaves_the_file_as_it_was_and_no_temporary_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let repo = Repo::with_config("read-only-folder", RICH);
        let folder = repo.root.join(".swarm");
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).expect("mode");

        let result = repo.save(|fields| fields.checks_required = true);

        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).expect("mode");
        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
        assert_eq!(repo.text(), RICH);
        assert_eq!(std::fs::read_dir(&folder).expect("folder").count(), 1);
    }

    #[test]
    fn a_symlinked_config_file_stays_a_link() {
        use std::os::unix::fs::symlink;

        let repo = Repo::new("link");
        let real = repo.root.join("dotfiles-config.toml");
        std::fs::write(&real, RICH).expect("file");
        std::fs::create_dir_all(repo.root.join(".swarm")).expect("folder");
        symlink(&real, repo.file()).expect("link");

        repo.save(|fields| fields.checks_required = true)
            .expect("save");

        assert!(
            repo.file()
                .symlink_metadata()
                .expect("metadata")
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&real)
                .expect("real file")
                .contains("required = true")
        );
    }

    #[test]
    fn a_save_keeps_the_mode_of_the_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let repo = Repo::with_config("mode", RICH);
        std::fs::set_permissions(repo.file(), std::fs::Permissions::from_mode(0o600))
            .expect("mode");

        repo.save(|fields| fields.checks_required = true)
            .expect("save");

        let mode = std::fs::metadata(repo.file())
            .expect("metadata")
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
    }

    #[test]
    fn a_dotted_or_inline_worktrees_table_stays_as_it_is() {
        let inline = Repo::with_config("inline-worktrees", "worktrees = { folder = \".trees\" }\n");
        let dotted = Repo::with_config("dotted-worktrees", "worktrees.folder = \".trees\"\n");

        for repo in [&inline, &dotted] {
            repo.save(|fields| fields.teardown = "echo bye".to_owned())
                .expect("save");
        }

        assert!(
            inline
                .text()
                .starts_with("worktrees = { folder = \".trees\"")
        );
        assert_eq!(inline.fields().teardown, "echo bye");
        assert_eq!(
            dotted.text(),
            "worktrees.folder = \".trees\"\nworktrees.teardown = \"echo bye\"\n"
        );
    }

    #[test]
    fn a_worktrees_table_that_only_holds_ports_gets_its_new_key() {
        let repo = Repo::with_config(
            "implicit",
            "[worktrees.ports]\nrange = \"5170-5199\"\nper_worktree = 3\n",
        );

        repo.save(|fields| fields.teardown = "echo bye".to_owned())
            .expect("save");

        assert_eq!(repo.fields().teardown, "echo bye");
        assert_eq!(repo.fields().port_range, "5170-5199");
    }

    #[test]
    fn comments_at_the_end_of_the_file_stay_above_a_table_that_is_added() {
        let repo = Repo::with_config(
            "trailing-comments",
            "[checks]\nrequired = true\n# ports = { range = \"5170-5199\", per_worktree = 3 }\n",
        );

        repo.save(|fields| fields.teardown = "echo bye".to_owned())
            .expect("save");

        assert_eq!(
            repo.text(),
            "[checks]\nrequired = true\n# ports = { range = \"5170-5199\", per_worktree = 3 }\n\n[worktrees]\nteardown = \"echo bye\"\n"
        );
    }

    #[test]
    fn a_file_with_only_comments_keeps_them_above_the_new_tables() {
        let repo = Repo::with_config("only-comments", "# folder = \".trees\"\n# copy = []");

        repo.save(|fields| fields.teardown = "echo bye".to_owned())
            .expect("save");

        assert_eq!(
            repo.text(),
            "# folder = \".trees\"\n# copy = []\n\n[worktrees]\nteardown = \"echo bye\"\n"
        );
    }

    #[test]
    fn a_file_without_a_final_newline_gets_its_new_key_on_a_new_line() {
        let repo = Repo::with_config("no-final-newline", "[checks]\nrequired = true");

        repo.save(|fields| fields.checks = vec!["cargo test".to_owned()])
            .expect("save");

        assert_eq!(
            repo.text(),
            "[checks]\nrequired = true\nbefore_pr = [\"cargo test\"]\n"
        );
    }

    #[test]
    fn a_file_with_windows_line_endings_is_not_rewritten_when_nothing_changes() {
        let text = "[worktrees]\r\nfolder = \".trees\"\r\n";
        let repo = Repo::with_config("crlf", text);

        repo.save(|_| {}).expect("save");

        assert_eq!(repo.text(), text);
    }

    #[test]
    fn a_failed_rename_removes_the_temporary_file() {
        let repo = Repo::new("failed-rename");
        let target = repo.root.join("config.toml");
        std::fs::create_dir_all(target.join("inside")).expect("a folder where the file should go");

        let result = write_atomic(&target, "text");

        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
        let names: Vec<_> = std::fs::read_dir(&repo.root)
            .expect("folder")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }
}
