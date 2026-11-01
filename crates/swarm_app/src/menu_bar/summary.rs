//! What the status item and the popover show, worked out from the workspace's data. Plain
//! data in and out, so all of it runs without `AppKit` or a window.

use swarm_protocol::Usage;
use swarm_ui::AgentStatus;

use crate::model::{Agent, FakeData, TaskId, TaskStatus};
use crate::usage_text::{UsageSection, shown_percent, usage_sections};

/// Which of the bee's two wing frames the status item shows. Flipping between them is the
/// buzz.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BeeFrame {
    /// The wings spread: the bee at rest.
    #[default]
    Open,
    Flat,
}

impl BeeFrame {
    pub(crate) fn flipped(self) -> Self {
        match self {
            Self::Open => Self::Flat,
            Self::Flat => Self::Open,
        }
    }
}

/// How the status item looks: the text next to the bee, the bee with or without the dot,
/// and what VoiceOver says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    pub title: Option<String>,
    pub dot: bool,
    pub label: String,
    /// The wing frame. It never changes what VoiceOver says.
    pub frame: BeeFrame,
}

impl Look {
    pub(crate) fn new(percent: Option<u8>, needs_you: bool) -> Self {
        let label = match (percent, needs_you) {
            (Some(percent), true) => format!("Swarm, {percent}% used, needs you"),
            (Some(percent), false) => format!("Swarm, {percent}% used"),
            (None, true) => "Swarm, needs you".to_owned(),
            (None, false) => "Swarm".to_owned(),
        };

        Self {
            title: percent.map(|percent| format!(" {percent}%")),
            dot: needs_you,
            label,
            frame: BeeFrame::Open,
        }
    }

    pub(crate) fn with_frame(mut self, frame: BeeFrame) -> Self {
        self.frame = frame;
        self
    }
}

/// A task that waits for the user, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NeedsYou {
    pub task: TaskId,
    pub project: String,
    pub name: String,
    /// For example "permission \u{b7} Allow cargo test?". Empty when the agent gave none.
    pub reason: String,
}

impl NeedsYou {
    /// For example "swarm \u{b7} terminal-renderer".
    pub(crate) fn title(&self) -> String {
        format!("{} \u{b7} {}", self.project, self.name)
    }

    pub(crate) fn spoken(&self) -> String {
        if self.reason.is_empty() {
            format!("{}, {}, needs you", self.project, self.name)
        } else {
            format!(
                "{}, {}, needs you, {}",
                self.project, self.name, self.reason
            )
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// The fullest window of any agent kind: the one that stops the user first.
    pub percent: Option<u8>,
    pub running: usize,
    /// Running agents that are at work: the bee buzzes while there is one.
    pub working: usize,
    pub needs_you: Vec<NeedsYou>,
    pub usage: Vec<UsageSection>,
}

impl Summary {
    /// `now` is in seconds since the Unix epoch.
    pub(crate) fn of(data: &FakeData, now: u64) -> Self {
        Self {
            percent: highest_percent(&data.usage, now),
            running: data
                .agents
                .iter()
                .filter(|agent| agent.is_running())
                .count(),
            working: data
                .agents
                .iter()
                .filter(|agent| agent.is_running() && agent.status == AgentStatus::Working)
                .count(),
            needs_you: needs_you(data),
            usage: usage_sections(&data.usage, now),
        }
    }

    /// `dot_allowed` is the menu bar dot setting: off, the icon never shows the dot.
    pub(crate) fn look(&self, dot_allowed: bool) -> Look {
        Look::new(self.percent, dot_allowed && !self.needs_you.is_empty())
    }
}

/// The highest used share across every window of both agent kinds.
fn highest_percent(usage: &Usage, now: u64) -> Option<u8> {
    [&usage.claude, &usage.codex]
        .into_iter()
        .flat_map(|reading| &reading.windows)
        .map(|window| shown_percent(window, now))
        .max()
}

fn needs_you(data: &FakeData) -> Vec<NeedsYou> {
    data.tasks
        .iter()
        .filter(|task| task.status == TaskStatus::NeedsYou)
        .filter_map(|task| {
            let project = data.project(task.project)?;
            let reason = data
                .agents_of(task.id)
                .find_map(Agent::attention)
                .unwrap_or_default();

            Some(NeedsYou {
                task: task.id,
                project: project.name.to_string(),
                name: task.name.to_string(),
                reason,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use swarm_protocol::{SubscriptionUsage, UsageProblem, UsageWindow};

    use super::*;
    use crate::model::SAMPLE_NOW;

    const NOW: u64 = SAMPLE_NOW / 1_000;

    fn window(label: &str, used_percent: u8, resets_at: Option<u64>) -> UsageWindow {
        UsageWindow {
            label: label.to_owned(),
            used_percent,
            resets_at,
            length_secs: None,
        }
    }

    fn reading(
        plan: Option<&str>,
        windows: Vec<UsageWindow>,
        problem: Option<UsageProblem>,
    ) -> SubscriptionUsage {
        SubscriptionUsage {
            windows,
            plan: plan.map(str::to_owned),
            problem,
            ..Default::default()
        }
    }

    #[test]
    fn the_title_is_the_highest_percent_of_every_window_of_both_agent_kinds() {
        let usage = Usage {
            claude: reading(
                None,
                vec![window("5h", 42, None), window("7d", 18, None)],
                None,
            ),
            codex: reading(None, vec![window("5h", 77, None)], None),
        };

        assert_eq!(highest_percent(&usage, NOW), Some(77));
        assert_eq!(
            Look::new(highest_percent(&usage, NOW), false)
                .title
                .as_deref(),
            Some(" 77%")
        );
    }

    #[test]
    fn a_window_of_the_other_kind_can_be_the_highest() {
        let usage = Usage {
            claude: reading(
                None,
                vec![window("5h", 42, None), window("7d", 91, None)],
                None,
            ),
            codex: reading(None, vec![window("5h", 7, None)], None),
        };

        assert_eq!(highest_percent(&usage, NOW), Some(91));
    }

    #[test]
    fn a_window_that_has_reset_counts_as_empty() {
        let usage = Usage {
            claude: reading(
                None,
                vec![
                    window("5h", 97, Some(NOW)),
                    window("7d", 30, Some(NOW + 60)),
                ],
                None,
            ),
            ..Default::default()
        };

        assert_eq!(highest_percent(&usage, NOW), Some(30));
    }

    #[test]
    fn there_is_no_title_without_usage() {
        let look = Look::new(highest_percent(&Usage::default(), NOW), false);

        assert_eq!(highest_percent(&Usage::default(), NOW), None);
        assert_eq!(look.title, None);
    }

    #[test]
    fn the_title_shows_zero_when_nothing_is_used() {
        assert_eq!(Look::new(Some(0), false).title.as_deref(), Some(" 0%"));
    }

    #[test]
    fn the_label_says_the_percent_and_that_an_agent_needs_you() {
        assert_eq!(
            Look::new(Some(42), true).label,
            "Swarm, 42% used, needs you"
        );
        assert_eq!(Look::new(Some(42), false).label, "Swarm, 42% used");
        assert_eq!(Look::new(None, true).label, "Swarm, needs you");
        assert_eq!(Look::new(None, false).label, "Swarm");
    }

    #[test]
    fn the_dot_shows_while_an_agent_needs_you() {
        assert!(Look::new(Some(42), true).dot);
        assert!(!Look::new(Some(42), false).dot);
    }

    fn sample() -> FakeData {
        FakeData::sample()
    }

    #[test]
    fn the_sample_needs_you_state_follows_the_tasks() {
        let mut data = sample();
        for task in &mut data.tasks {
            task.status = TaskStatus::Idle;
        }
        let quiet = Summary::of(&data, NOW);

        if let Some(task) = data.tasks.first_mut() {
            task.status = TaskStatus::NeedsYou;
        }
        let waiting = Summary::of(&data, NOW);

        assert!(quiet.needs_you.is_empty());
        assert!(!quiet.look(true).dot);
        assert_eq!(waiting.needs_you.len(), 1);
        assert!(waiting.look(true).dot);
        assert!(!waiting.look(false).dot);
    }

    #[test]
    fn a_needs_you_entry_names_the_project_the_task_and_the_reason() {
        let mut data = sample();
        for task in &mut data.tasks {
            task.status = TaskStatus::Idle;
        }
        let project = data
            .projects
            .first()
            .map(|project| project.name.to_string());
        let task = data
            .tasks
            .first()
            .map(|task| (task.id, task.name.to_string()));
        let blocked = task.as_ref().and_then(|(id, _)| {
            data.agents
                .iter_mut()
                .find(|agent| agent.task == *id && agent.daemon.is_none())
        });
        if let Some(agent) = blocked {
            agent.status = AgentStatus::Blocked;
            agent.note = Some("Allow cargo test?".into());
        }
        if let Some(task) = data.tasks.first_mut() {
            task.status = TaskStatus::NeedsYou;
        }

        let summary = Summary::of(&data, NOW);

        let entry = summary.needs_you.first();
        assert_eq!(
            entry.map(|entry| entry.task),
            task.as_ref().map(|task| task.0)
        );
        assert_eq!(entry.map(|entry| entry.project.clone()), project);
        assert_eq!(
            entry.map(|entry| entry.name.clone()),
            task.map(|task| task.1)
        );
        assert_eq!(
            entry.map(|entry| entry.reason.as_str()),
            Some("Allow cargo test?")
        );
    }

    #[test]
    fn running_counts_the_agents_whose_process_runs() {
        let mut data = sample();
        let before = Summary::of(&data, NOW).running;

        let first = data.agents.first_mut();
        if let Some(agent) = first {
            agent.status = AgentStatus::Done;
        }
        let after = Summary::of(&data, NOW).running;

        assert!(before > 0);
        assert_eq!(after, before - 1);
    }
}
