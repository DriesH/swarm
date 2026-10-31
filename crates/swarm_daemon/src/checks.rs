//! The project's local checks (`[checks] before_pr`): one process terminal per command, in
//! the task worktree, one after the other (PLAN.md, Pull requests).
//!
//! The latest run of each task lives in memory only. A daemon restart forgets it, and the
//! user runs the checks again.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use swarm_core::{Projects, git, task_env};
use swarm_protocol::{
    CheckState, ChecksRun, CreateTerminal, Event, LocalCheck, TaskId, TaskInfo, TerminalId,
    TerminalKind,
};
use swarm_term::{ExitStatus, TerminalInput};

use crate::agents::Agents;
use crate::hub::{Clients, Extras, Terminals};
use crate::tasks::{TERMINAL_PALETTE, TERMINAL_SIZE};

/// How long a new run waits for the run it replaced to let go of the worktree.
const SETTLE: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The latest run of one task.
struct Slot {
    /// Tells a runner whether it still owns the slot.
    generation: u64,
    run: ChecksRun,
    /// The terminal of the check that runs now.
    current: Option<TerminalId>,
    /// Running terminals that the user closed.
    closed_by_user: HashSet<TerminalId>,
    /// Disconnects when the runner thread ends.
    finished: flume::Receiver<()>,
}

/// What a replaced run leaves behind for the run that follows it.
struct Replaced {
    finished: flume::Receiver<()>,
    terminal: Option<TerminalId>,
}

pub(crate) struct Checks {
    projects: Arc<Projects>,
    terminals: Arc<Terminals>,
    clients: Arc<Clients>,
    agents: Arc<Agents>,
    slots: Mutex<HashMap<TaskId, Slot>>,
    generations: AtomicU64,
}

impl Checks {
    pub(crate) fn new(
        projects: Arc<Projects>,
        terminals: Arc<Terminals>,
        clients: Arc<Clients>,
        agents: Arc<Agents>,
    ) -> Arc<Self> {
        Arc::new(Self {
            projects,
            terminals,
            clients,
            agents,
            slots: Mutex::default(),
            generations: AtomicU64::new(0),
        })
    }

    /// The task's latest run, if it ever ran its checks since the daemon started.
    pub(crate) fn latest(&self, task: TaskId) -> Option<ChecksRun> {
        lock(&self.slots).get(&task).map(|slot| slot.run.clone())
    }

    /// Forgets the run of a task that closed.
    pub(crate) fn forget(&self, task: TaskId) {
        lock(&self.slots).remove(&task);
    }

    /// The user closed a terminal. When it is a running check, the run stops.
    pub(crate) fn user_closed(&self, terminal: TerminalId) {
        let mut slots = lock(&self.slots);

        if let Some(slot) = slots
            .values_mut()
            .find(|slot| slot.current == Some(terminal))
        {
            slot.closed_by_user.insert(terminal);
        }
    }

    /// Starts the checks of the task, replacing a run that still goes. Progress arrives
    /// as `ChecksChanged`. A project without checks gets a finished run with none.
    pub(crate) fn start(self: &Arc<Self>, id: TaskId) -> Result<()> {
        let task = self.projects.task(id)?;
        if self.agents.is_closing(Some(id)) {
            bail!("the task is closing");
        }

        let config = self.projects.checks_config(task.project)?;
        let commands = config.commands();

        let commit = git::commit_of(&task.worktree, "HEAD")?;
        let run = ChecksRun {
            task: id,
            clean: untouched(&task, &commit),
            commit,
            required: config.required,
            checks: commands
                .iter()
                .map(|command| LocalCheck {
                    command: command.clone(),
                    state: CheckState::Waiting,
                    terminal: None,
                    exit: None,
                    duration_ms: None,
                })
                .collect(),
            pre_push_hook: git::pre_push_hook(&task.worktree),
        };

        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        let (done, finished) = flume::bounded::<()>(1);

        let replaced = {
            let mut slots = lock(&self.slots);
            let replaced = slots.remove(&id).map(|old| self.stop(old));

            slots.insert(
                id,
                Slot {
                    generation,
                    run: run.clone(),
                    current: None,
                    closed_by_user: HashSet::new(),
                    finished,
                },
            );
            self.clients.broadcast(&Event::ChecksChanged { run });

            replaced
        };
        if commands.is_empty() {
            return Ok(());
        }

        let checks = Arc::clone(self);
        thread::Builder::new()
            .name(format!("checks-{}", id.0))
            .spawn(move || {
                checks.settle(replaced);
                checks.run_all(&task, &commands, generation);
                drop(done);
            })
            .context("cannot start the checks")?;

        Ok(())
    }

    /// Ends a run that a new one replaces: its unfinished checks count as stopped, and
    /// the check that runs gets a SIGTERM.
    fn stop(&self, mut old: Slot) -> Replaced {
        let unfinished = old
            .run
            .checks
            .iter()
            .any(|check| matches!(check.state, CheckState::Waiting | CheckState::Running));

        for check in &mut old.run.checks {
            if matches!(check.state, CheckState::Waiting | CheckState::Running) {
                check.state = CheckState::Stopped;
            }
        }
        if let Some(terminal) = old.current {
            self.end(terminal, libc::SIGTERM);
        }
        if unfinished {
            self.clients
                .broadcast(&Event::ChecksChanged { run: old.run });
        }

        Replaced {
            finished: old.finished,
            terminal: old.current,
        }
    }

    /// Sends the signal to the terminal's process group, or kills the terminal when that
    /// is not possible.
    fn end(&self, terminal: TerminalId, signal: libc::c_int) {
        let Some(hub) = self.terminals.get(terminal) else {
            return;
        };

        if !hub.signal(signal) {
            hub.send_input(TerminalInput::Kill);
        }
    }

    /// Waits until the replaced run let go of the worktree, killing it when it takes long.
    fn settle(&self, replaced: Option<Replaced>) {
        let Some(replaced) = replaced else {
            return;
        };

        if matches!(
            replaced.finished.recv_timeout(SETTLE),
            Err(flume::RecvTimeoutError::Timeout)
        ) {
            if let Some(terminal) = replaced.terminal {
                self.end(terminal, libc::SIGKILL);
            }

            if replaced.finished.recv_timeout(SETTLE).is_err() {
                log::warn!("the replaced checks did not end");
            }
        }
    }

    fn run_all(&self, task: &TaskInfo, commands: &[String], generation: u64) {
        let Some(commit) = lock(&self.slots)
            .get(&task.id)
            .filter(|slot| slot.generation == generation)
            .map(|slot| slot.run.commit.clone())
        else {
            return;
        };

        for (index, command) in commands.iter().enumerate() {
            if !self.owns(task.id, generation) {
                return;
            }

            if !self.run_one(task, &commit, index, command, generation) {
                return;
            }
        }
    }

    /// True when the check passed and the next one may run. A check that does not run
    /// leaves the later ones skipped. After each check the worktree must still be at
    /// `commit` with no uncommitted files, or the run stops counting as clean.
    fn run_one(
        &self,
        task: &TaskInfo,
        commit: &str,
        index: usize,
        command: &str,
        generation: u64,
    ) -> bool {
        let started = Instant::now();
        let (exited, exit_receiver) = flume::bounded(1);

        let created = self.spawn_terminal(task, command, exited);
        let hub = match created {
            Ok(hub) => hub,
            Err(error) => {
                log::error!("cannot start the check {command:?}: {error:#}");
                self.update(task.id, generation, |slot| {
                    slot.run.checks[index].state = CheckState::Failed;
                    slot.run.clean &= untouched(task, commit);
                    skip_rest(&mut slot.run, index);
                });

                return false;
            }
        };
        let terminal = hub.id();

        let running = self.update(task.id, generation, |slot| {
            slot.current = Some(terminal);
            let check = &mut slot.run.checks[index];
            check.state = CheckState::Running;
            check.terminal = Some(terminal);
        });
        if !running {
            hub.send_input(TerminalInput::Kill);

            return false;
        }

        let exit = exit_receiver.recv().ok().flatten();
        let duration = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let closing = self.agents.is_closing(Some(task.id));
        let untouched = untouched(task, commit);

        let mut passed = false;
        self.update(task.id, generation, |slot| {
            let closed = slot.closed_by_user.remove(&terminal);
            slot.current = None;

            let state = if closing || closed {
                CheckState::Stopped
            } else if exit == Some(ExitStatus::Code(0)) {
                CheckState::Passed
            } else {
                CheckState::Failed
            };
            passed = state == CheckState::Passed;

            let check = &mut slot.run.checks[index];
            check.state = state;
            check.exit = exit;
            check.duration_ms = Some(duration);
            slot.run.clean &= untouched;

            match state {
                CheckState::Stopped => {
                    for later in &mut slot.run.checks[index + 1..] {
                        later.state = CheckState::Stopped;
                    }
                }
                CheckState::Failed => skip_rest(&mut slot.run, index),
                _ => {}
            }
        });

        passed
    }

    fn spawn_terminal(
        &self,
        task: &TaskInfo,
        command: &str,
        exited: flume::Sender<Option<ExitStatus>>,
    ) -> Result<Arc<crate::hub::Hub>> {
        if self.agents.is_closing(Some(task.id)) {
            bail!("the task is closing");
        }

        let request = CreateTerminal {
            kind: TerminalKind::Process,
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-lc".to_owned(),
                command.to_owned(),
            ]),
            cwd: Some(task.worktree.clone()),
            size: TERMINAL_SIZE,
            palette: TERMINAL_PALETTE,
            task: Some(task.id.terminal_key()),
        };
        let extras = Extras {
            env: task_env(task),
            on_started: None,
            on_exit: Some(Box::new(move |exit| {
                if exited.send(exit).is_err() {
                    log::debug!("nobody waits for the check any more");
                }
            })),
            on_output: None,
            process: None,
        };

        self.terminals
            .create_with(request, extras)
            .context("cannot start the check terminal")
    }

    fn owns(&self, task: TaskId, generation: u64) -> bool {
        lock(&self.slots)
            .get(&task)
            .is_some_and(|slot| slot.generation == generation)
    }

    /// Changes the run and tells every client. False when a newer run replaced this one.
    fn update(&self, task: TaskId, generation: u64, change: impl FnOnce(&mut Slot)) -> bool {
        let mut slots = lock(&self.slots);

        let Some(slot) = slots
            .get_mut(&task)
            .filter(|slot| slot.generation == generation)
        else {
            return false;
        };

        change(slot);
        self.clients.broadcast(&Event::ChecksChanged {
            run: slot.run.clone(),
        });

        true
    }
}

/// The checks after `index` will not run.
fn skip_rest(run: &mut ChecksRun, index: usize) {
    for later in &mut run.checks[index + 1..] {
        later.state = CheckState::Skipped;
    }
}

/// True when the worktree is at `commit` and has no uncommitted files. Ignored files do
/// not count. A git failure counts as touched.
fn untouched(task: &TaskInfo, commit: &str) -> bool {
    git::commit_of(&task.worktree, "HEAD").is_ok_and(|head| head == commit)
        && git::uncommitted_files(&task.worktree).is_ok_and(|count| count == 0)
}
