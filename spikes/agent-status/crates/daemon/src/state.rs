//! Agent records on disk, so a restarted daemon can find processes it lost.
//! A JSON file for the spike; the real daemon uses SQLite (ADR 0005).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use proto::{AgentKind, TermId};
use serde::{Deserialize, Serialize};

const STOP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Running,
    Exited,
    /// Stopped by recovery after a daemon crash; resumable in the real product.
    Stopped,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Record {
    pub cmd: String,
    #[serde(default)]
    pub kind: Option<AgentKind>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Claude Code session ID or Codex thread ID, for resume.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript: Option<String>,
    pub status: Status,
    pub pid: Option<i32>,
    /// Process start time in microseconds since the epoch; guards against PID reuse.
    pub started_us: Option<u64>,
}

pub struct Store {
    path: PathBuf,
    records: BTreeMap<TermId, Record>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let records = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("parse agent records")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).context("read agent records"),
        };
        Ok(Self { path, records })
    }

    pub fn next_id(&self) -> TermId {
        self.records.keys().next_back().map_or(1, |id| id + 1)
    }

    /// Written before the process starts (ADR 0005), then again with its PID.
    pub fn starting(
        &mut self,
        id: TermId,
        cmd: &str,
        kind: Option<AgentKind>,
        cwd: Option<String>,
    ) -> Result<()> {
        self.records.insert(
            id,
            Record {
                cmd: cmd.to_owned(),
                kind,
                cwd,
                session_id: None,
                transcript: None,
                status: Status::Starting,
                pid: None,
                started_us: None,
            },
        );
        self.save()
    }

    pub fn running(&mut self, id: TermId, pid: Option<u32>) -> Result<()> {
        if let Some(r) = self.records.get_mut(&id) {
            r.pid = pid.and_then(|p| i32::try_from(p).ok());
            r.started_us = r.pid.and_then(|p| process_info(p).map(|i| i.started_us));
            r.status = Status::Running;
        }
        self.save()
    }

    pub fn get(&self, id: TermId) -> Option<&Record> {
        self.records.get(&id)
    }

    pub fn session(&mut self, id: TermId, session_id: String, transcript: Option<String>) -> Result<()> {
        let Some(r) = self.records.get_mut(&id) else {
            return Ok(());
        };
        if r.session_id.as_deref() == Some(&session_id) && transcript.is_none() {
            return Ok(());
        }
        r.session_id = Some(session_id);
        if transcript.is_some() {
            r.transcript = transcript;
        }
        self.save()
    }

    pub fn exited(&mut self, id: TermId) -> Result<()> {
        if let Some(r) = self.records.get_mut(&id) {
            r.status = Status::Exited;
        }
        self.save()
    }

    /// Stops every recorded agent that is still alive. Runs at startup, before
    /// the daemon manages anything, so every live record is a leftover.
    pub fn recover(&mut self) -> Result<()> {
        for (id, record) in &mut self.records {
            if !matches!(record.status, Status::Starting | Status::Running) {
                continue;
            }
            let (Some(pid), Some(started_us)) = (record.pid, record.started_us) else {
                record.status = Status::Stopped;
                continue;
            };
            let procs = session_members(pid, started_us);
            eprintln!(
                "[recover] agent {id} ({}): leader pid {pid}, {} live process(es) in its session",
                record.cmd,
                procs.len()
            );
            stop(&procs);
            record.status = Status::Stopped;
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.records)?)?;
        std::fs::rename(&tmp, &self.path).context("save agent records")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub started_us: u64,
    pub name: String,
}

pub fn process_info(pid: i32) -> Option<ProcessInfo> {
    // SAFETY: proc_bsdinfo is plain old data; proc_pidinfo fills at most `size` bytes.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let name: Vec<u8> = info
        .pbi_name
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Some(ProcessInfo {
        started_us: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        name: String::from_utf8_lossy(&name).into_owned(),
    })
}

/// Live processes in the session the agent's leader created.
///
/// portable-pty runs each child with `setsid`, so its PID is also the session
/// ID of everything the agent started on that terminal. A process only counts
/// if it started at or after the recorded leader start time; if the leader PID
/// itself is alive with another start time, the PID was reused and nothing is
/// returned.
pub fn session_members(leader: i32, leader_started_us: u64) -> Vec<(i32, ProcessInfo)> {
    if let Some(info) = process_info(leader)
        && info.started_us != leader_started_us
    {
        eprintln!(
            "[recover] pid {leader} now belongs to {:?}, started at another time: skipped",
            info.name
        );
        return Vec::new();
    }
    all_pids()
        .into_iter()
        // SAFETY: getsid has no memory effects; it fails with ESRCH/EPERM for bad PIDs.
        .filter(|&pid| pid > 0 && unsafe { libc::getsid(pid) } == leader)
        .filter_map(|pid| process_info(pid).map(|info| (pid, info)))
        .filter(|(_, info)| info.started_us >= leader_started_us)
        .collect()
}

fn all_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the count; the second call fills at most `cap` PIDs.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let cap = usize::try_from(count).unwrap_or(0) + 64;
    let mut pids = vec![0i32; cap];
    let bytes = libc::c_int::try_from(cap * 4).unwrap_or(libc::c_int::MAX);
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(n).unwrap_or(0));
    pids
}

/// SIGTERM, then SIGKILL for anything still alive (same PID and start time) after the timeout.
fn stop(procs: &[(i32, ProcessInfo)]) {
    for (pid, info) in procs {
        eprintln!("[recover] SIGTERM {pid} {}", info.name);
        // SAFETY: plain syscall.
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    let alive = |(pid, info): &&(i32, ProcessInfo)| process_info(*pid).as_ref() == Some(info);
    while Instant::now() < deadline && procs.iter().any(|p| alive(&p)) {
        thread::sleep(Duration::from_millis(50));
    }
    for (pid, info) in procs.iter().filter(alive) {
        eprintln!("[recover] SIGKILL {pid} {}", info.name);
        // SAFETY: plain syscall.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
}
