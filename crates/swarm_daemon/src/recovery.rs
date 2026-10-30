//! Stops processes that an earlier daemon lost when it crashed.
//!
//! portable-pty runs each terminal with `setsid`, so the recorded PID is also the
//! session id of everything the terminal started. A process that ignores SIGHUP
//! outlives the daemon, but is still found through its session.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::store::{Leftover, Store};

const STOP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessInfo {
    /// Microseconds since the epoch.
    pub(crate) started_us: u64,
    pub(crate) name: String,
}

/// The process table, so the matching logic can run on fake data.
pub(crate) trait Processes {
    fn info(&self, pid: i32) -> Option<ProcessInfo>;
    fn all_pids(&self) -> Vec<i32>;
    fn session_id(&self, pid: i32) -> Option<i32>;
    fn signal(&self, pid: i32, signal: i32);
}

pub(crate) struct System;

impl Processes for System {
    fn info(&self, pid: i32) -> Option<ProcessInfo> {
        process_info(pid)
    }

    fn all_pids(&self) -> Vec<i32> {
        // SAFETY: a null buffer only asks for the number of processes.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let capacity = usize::try_from(count).unwrap_or(0) + 64;

        let mut pids: Vec<i32> = vec![0; capacity];
        let bytes = libc::c_int::try_from(capacity * size_of::<i32>()).unwrap_or(libc::c_int::MAX);
        // SAFETY: the buffer holds `bytes` bytes and lives for the whole call.
        let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        pids.truncate(usize::try_from(filled).unwrap_or(0));

        pids
    }

    fn session_id(&self, pid: i32) -> Option<i32> {
        // SAFETY: getsid takes a plain integer and fails with an error for bad PIDs.
        let session = unsafe { libc::getsid(pid) };

        (session >= 0).then_some(session)
    }

    fn signal(&self, pid: i32, signal: i32) {
        // SAFETY: kill takes plain integers.
        let result = unsafe { libc::kill(pid, signal) };
        if result != 0 {
            log::debug!(
                "kill({pid}, {signal}) failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Start time and name of a live process.
pub(crate) fn process_info(pid: i32) -> Option<ProcessInfo> {
    // SAFETY: proc_bsdinfo is plain old data, so all zeroes is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;

    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if written != size {
        return None;
    }

    let name: Vec<u8> = info
        .pbi_name
        .iter()
        .take_while(|character| **character != 0)
        .map(|character| character.cast_unsigned())
        .collect();

    Some(ProcessInfo {
        started_us: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        name: String::from_utf8_lossy(&name).into_owned(),
    })
}

/// Live processes in the session that `leader` created. A process counts only if
/// it started after the leader. If `leader` is alive with another start time, the
/// PID was reused and nothing is returned.
pub(crate) fn session_members(
    processes: &impl Processes,
    leader: i32,
    leader_started_us: u64,
) -> Vec<(i32, ProcessInfo)> {
    if let Some(info) = processes.info(leader)
        && info.started_us != leader_started_us
    {
        log::info!(
            "pid {leader} now belongs to {:?} and started at another time: skipped",
            info.name
        );

        return Vec::new();
    }

    processes
        .all_pids()
        .into_iter()
        .filter(|pid| *pid > 0 && processes.session_id(*pid) == Some(leader))
        .filter_map(|pid| processes.info(pid).map(|info| (pid, info)))
        .filter(|(_, info)| info.started_us >= leader_started_us)
        .collect()
}

/// SIGTERM, then SIGKILL for whatever still has the same PID and start time
/// after `timeout`.
pub(crate) fn stop(processes: &impl Processes, members: &[(i32, ProcessInfo)], timeout: Duration) {
    for (pid, info) in members {
        log::info!("SIGTERM {pid} {}", info.name);
        processes.signal(*pid, libc::SIGTERM);
    }

    let alive = |(pid, info): &&(i32, ProcessInfo)| processes.info(*pid).as_ref() == Some(info);

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && members.iter().any(|member| alive(&member)) {
        thread::park_timeout(POLL_INTERVAL);
    }

    for (pid, info) in members.iter().filter(alive) {
        log::info!("SIGKILL {pid} {}", info.name);
        processes.signal(*pid, libc::SIGKILL);
    }
}

/// Stops the processes of every leftover row and marks the row `stopped`. The store
/// lock is held only for the update, never while waiting for processes.
pub(crate) fn recover(store: &Mutex<Store>, leftovers: &[Leftover], processes: &impl Processes) {
    for leftover in leftovers {
        if let (Some(pid), Some(started_us)) = (leftover.pid, leftover.started_us) {
            let members = session_members(processes, pid, started_us);
            log::info!(
                "terminal {} ({}): leader pid {pid}, {} live process(es) in its session",
                leftover.id.0,
                leftover.command,
                members.len()
            );

            stop(processes, &members, STOP_TIMEOUT);
        }

        let result = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mark_stopped(leftover.id);
        if let Err(error) = result {
            log::error!("cannot mark terminal {} stopped: {error:#}", leftover.id.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    struct Fake {
        table: Mutex<HashMap<i32, (i32, ProcessInfo)>>,
        ignores_term: Vec<i32>,
        signals: Mutex<Vec<(i32, i32)>>,
    }

    impl Fake {
        fn new(processes: &[(i32, i32, u64)]) -> Self {
            let table = processes
                .iter()
                .map(|(pid, session, started_us)| {
                    let info = ProcessInfo {
                        started_us: *started_us,
                        name: format!("proc{pid}"),
                    };
                    (*pid, (*session, info))
                })
                .collect();

            Self {
                table: Mutex::new(table),
                ignores_term: Vec::new(),
                signals: Mutex::default(),
            }
        }
    }

    impl Processes for Fake {
        fn info(&self, pid: i32) -> Option<ProcessInfo> {
            self.table
                .lock()
                .unwrap()
                .get(&pid)
                .map(|entry| entry.1.clone())
        }

        fn all_pids(&self) -> Vec<i32> {
            self.table.lock().unwrap().keys().copied().collect()
        }

        fn session_id(&self, pid: i32) -> Option<i32> {
            self.table.lock().unwrap().get(&pid).map(|entry| entry.0)
        }

        fn signal(&self, pid: i32, signal: i32) {
            self.signals.lock().unwrap().push((pid, signal));
            if signal == libc::SIGKILL || !self.ignores_term.contains(&pid) {
                self.table.lock().unwrap().remove(&pid);
            }
        }
    }

    fn pids(members: &[(i32, ProcessInfo)]) -> Vec<i32> {
        let mut pids: Vec<i32> = members.iter().map(|member| member.0).collect();
        pids.sort_unstable();

        pids
    }

    #[test]
    fn members_are_found_by_session_even_without_the_leader() {
        let fake = Fake::new(&[(11, 10, 150), (12, 10, 160), (20, 20, 100)]);

        let members = session_members(&fake, 10, 100);

        assert_eq!(pids(&members), [11, 12]);
    }

    #[test]
    fn a_process_that_started_before_the_leader_is_ignored() {
        let fake = Fake::new(&[(11, 10, 50), (12, 10, 160)]);

        let members = session_members(&fake, 10, 100);

        assert_eq!(pids(&members), [12]);
    }

    #[test]
    fn a_reused_leader_pid_is_skipped() {
        let fake = Fake::new(&[(10, 10, 999), (11, 10, 1000)]);

        let members = session_members(&fake, 10, 100);

        assert_eq!(members, Vec::new());
    }

    #[test]
    fn the_leader_with_the_same_start_time_is_included() {
        let fake = Fake::new(&[(10, 10, 100), (11, 10, 150)]);

        let members = session_members(&fake, 10, 100);

        assert_eq!(pids(&members), [10, 11]);
    }

    #[test]
    fn stop_sends_only_sigterm_when_processes_obey() {
        let fake = Fake::new(&[(11, 10, 150)]);
        let members = session_members(&fake, 10, 100);

        stop(&fake, &members, Duration::from_millis(200));

        assert_eq!(*fake.signals.lock().unwrap(), [(11, libc::SIGTERM)]);
    }

    #[test]
    fn stop_kills_a_process_that_ignores_sigterm() {
        let mut fake = Fake::new(&[(11, 10, 150)]);
        fake.ignores_term = vec![11];
        let members = session_members(&fake, 10, 100);

        stop(&fake, &members, Duration::from_millis(100));

        assert_eq!(
            *fake.signals.lock().unwrap(),
            [(11, libc::SIGTERM), (11, libc::SIGKILL)]
        );
    }

    #[test]
    fn stop_spares_a_reused_pid_at_kill_time() {
        let mut fake = Fake::new(&[(11, 10, 150)]);
        fake.ignores_term = vec![11];
        let members = session_members(&fake, 10, 100);
        fake.table
            .lock()
            .unwrap()
            .get_mut(&11)
            .unwrap()
            .1
            .started_us = 9_999;

        stop(&fake, &members, Duration::from_millis(100));

        assert_eq!(*fake.signals.lock().unwrap(), [(11, libc::SIGTERM)]);
    }
}
