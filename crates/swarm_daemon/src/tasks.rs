//! Starting a task: the work in `swarm_core`, then the setup script as a terminal.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use swarm_core::{PreparedTask, Projects};
use swarm_protocol::{CreateTask, CreateTerminal, TaskInfo, TerminalId, TerminalKind};
use swarm_term::{Palette, Rgb, Size};

use crate::hub::{Extras, Terminals};

/// Default size and colors of a setup terminal; the app resizes it when it attaches.
const SETUP_SIZE: Size = Size {
    cols: 100,
    rows: 30,
    cell_width: 8,
    cell_height: 16,
};

const SETUP_PALETTE: Palette = Palette {
    fg: Rgb(230, 230, 230),
    bg: Rgb(10, 10, 12),
    cursor: Rgb(255, 255, 255),
    ansi: [
        Rgb(0, 0, 0),
        Rgb(205, 49, 49),
        Rgb(13, 188, 121),
        Rgb(229, 229, 16),
        Rgb(36, 114, 200),
        Rgb(188, 63, 188),
        Rgb(17, 168, 205),
        Rgb(229, 229, 229),
        Rgb(102, 102, 102),
        Rgb(241, 76, 76),
        Rgb(35, 209, 139),
        Rgb(245, 245, 67),
        Rgb(59, 142, 234),
        Rgb(214, 112, 214),
        Rgb(41, 184, 219),
        Rgb(255, 255, 255),
    ],
};

/// Makes the task, then starts its setup script without waiting for it to finish.
pub(crate) fn create(
    terminals: &Arc<Terminals>,
    projects: &Arc<Projects>,
    request: &CreateTask,
) -> Result<TaskInfo> {
    let PreparedTask {
        mut task,
        setup,
        env,
    } = projects.create_task(request)?;

    if let Some(script) = setup {
        match start_setup(terminals, projects, &task, script, env) {
            Ok(terminal) => task.setup_terminal = Some(terminal),
            Err(error) => log::error!("cannot start the setup of task {}: {error:#}", task.id.0),
        }
    }

    Ok(task)
}

fn start_setup(
    terminals: &Arc<Terminals>,
    projects: &Arc<Projects>,
    task: &TaskInfo,
    script: String,
    env: Vec<(String, String)>,
) -> Result<TerminalId> {
    let request = CreateTerminal {
        kind: TerminalKind::Process,
        command: Some(vec!["/bin/sh".to_owned(), "-lc".to_owned(), script]),
        cwd: Some(task.worktree.clone()),
        size: SETUP_SIZE,
        palette: SETUP_PALETTE,
        task: Some(task.id.terminal_key()),
    };

    let (started, finished) = (Arc::clone(projects), Arc::clone(projects));
    let task_id = task.id;
    let extras = Extras {
        env,
        on_started: Some(Box::new(move |terminal| {
            if let Err(error) = started.set_setup_terminal(task_id, Some(terminal)) {
                log::error!("cannot record the setup terminal: {error}");
            }
        })),
        on_exit: Some(Box::new(move |exit| {
            if let Err(error) = finished.finish_setup(task_id, exit.as_ref()) {
                log::error!("cannot record the end of the setup: {error}");
            }
        })),
    };

    let hub = terminals
        .create_with(request, extras)
        .context("cannot start the setup terminal")?;

    Ok(hub.id())
}
