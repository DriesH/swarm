//! The swarm's counts for the easter eggs: `swarm buzz` and the queen bee. The creatures
//! the user caught for the Field guide are kept here too.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use swarm_protocol::creatures::is_creature_slug;
use swarm_protocol::{Catch, Event, SwarmStats, TaskId};

use crate::Services;
use crate::agents::lock;

const DAY_MILLIS: u64 = 24 * 60 * 60 * 1000;

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

/// The creatures the user caught.
pub(crate) fn catches(services: &Services) -> Result<Vec<Catch>> {
    Ok(lock(&services.store).catches()?)
}

/// When the creatures of the last 24 hours came.
pub(crate) fn visits(services: &Services) -> Result<Vec<u64>> {
    let since = now_millis().saturating_sub(DAY_MILLIS);

    Ok(lock(&services.store).visits_since(since)?)
}

/// Records that a creature came at `at`.
pub(crate) fn record_visit(services: &Services, at: u64) -> Result<()> {
    Ok(lock(&services.store).record_visit(at)?)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Records a catch. A slug that is not in the list is refused.
pub(crate) fn record_catch(services: &Services, creature: &str) -> Result<Catch> {
    if !is_creature_slug(creature) {
        bail!("{creature} is not a creature of the Field guide");
    }

    Ok(lock(&services.store).record_catch(creature)?)
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
