//! The swarm's counts for the easter eggs: `swarm buzz` and the queen bee.

use anyhow::Result;
use swarm_protocol::{Event, SwarmStats, TaskId};

use crate::Services;
use crate::agents::lock;

/// The counts now. The request, the agent call and the broadcast all use this.
pub(crate) fn current(services: &Services) -> Result<SwarmStats> {
    let (open_tasks, merged_prs, hundredth_merge_at) = {
        let store = lock(&services.store);

        (
            store.open_task_count()?,
            store.merge_count()?,
            store.hundredth_merge_at()?,
        )
    };

    Ok(SwarmStats {
        running_agents: services.agents.running_count(),
        open_tasks,
        merged_prs,
        hundredth_merge_at,
    })
}

/// Counts the merge of the task's pull request and tells every client. The pull request
/// is merged either way, so a failure here is only logged.
pub(crate) fn record_merge(services: &Services, task: TaskId) {
    let counted = lock(&services.store).record_merge(task);
    match counted {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            log::warn!("cannot count the merge of task {}: {error}", task.0);

            return;
        }
    }

    match current(services) {
        Ok(stats) => services.clients.broadcast(&Event::StatsChanged { stats }),
        Err(error) => log::warn!("cannot read the stats after a merge: {error:#}"),
    }
}
