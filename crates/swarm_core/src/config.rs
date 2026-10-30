//! `.swarm/config.toml`: per-repository settings, edited by hand.

use std::path::{Component, Path};

use serde::Deserialize;

const DEFAULT_FOLDER: &str = ".worktrees";

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
        let folder = Path::new(&self.worktrees.folder);
        let inside = !self.worktrees.folder.is_empty()
            && folder
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        if !inside {
            return Err(format!(
                "worktrees.folder {:?} must be a folder inside the repository",
                self.worktrees.folder
            ));
        }

        if let Some(ports) = &self.worktrees.ports {
            let (first, last) = ports.bounds()?;
            let size = u32::from(last) - u32::from(first) + 1;
            if ports.per_worktree == 0 || u32::from(ports.per_worktree) > size {
                return Err(format!(
                    "ports.per_worktree must be between 1 and {size}, the size of the range"
                ));
            }
        }

        Ok(())
    }
}

/// Reads `<root>/.swarm/config.toml`. A missing file gives the defaults; an invalid
/// one gives the defaults and the reason.
pub fn load(root: &Path) -> LoadedConfig {
    let path = root.join(".swarm").join("config.toml");

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
}
