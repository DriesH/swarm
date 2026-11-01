//! The system check request: the tools Swarm needs, as the daemon and its agents see them
//! (PLAN.md, System check).

use std::path::PathBuf;

use swarm_core::github::Github;
use swarm_core::system_check::{self, AgentCommands, Tools};
use swarm_protocol::{AgentKind, SystemCheck};

use crate::usage::configured_command;

pub(crate) struct SystemChecks {
    tools: Tools,
    /// The settings file that names the agent commands.
    settings: Option<PathBuf>,
    github: Github,
}

impl SystemChecks {
    pub(crate) fn new(tools: Tools, settings: Option<PathBuf>, github: Github) -> Self {
        Self {
            tools,
            settings,
            github,
        }
    }

    /// Checks every tool now. The settings are read again, so a command the user changed
    /// counts at once.
    pub(crate) fn run(&self) -> SystemCheck {
        let claude = configured_command(self.settings.as_deref(), AgentKind::ClaudeCode);
        let codex = configured_command(self.settings.as_deref(), AgentKind::Codex);

        system_check::check(
            &self.tools,
            &AgentCommands {
                claude: &claude,
                codex: &codex,
            },
            &self.github,
        )
    }
}
