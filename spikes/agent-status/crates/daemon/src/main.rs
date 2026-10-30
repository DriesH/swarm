//! `swarm-daemon`: owns agent terminals and serves them over a unix socket.

mod agent;
mod encode;
mod keys;
mod osc7501;
mod pty;
mod state;
mod terminal;

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use anyhow::{Context as _, Result};
use proto::{AgentKind, Event, Request, Size, TermId};

use crate::agent::Agents;
use crate::state::Store;
use crate::terminal::{ClientId, Cmd, Frame, Handle, Spawn};

struct Daemon {
    terminals: Mutex<HashMap<TermId, Handle>>,
    store: Mutex<Store>,
    agents: Arc<Mutex<Agents>>,
    next_client: AtomicU64,
    dir: PathBuf,
    socket: PathBuf,
}

/// What to start, after the daemon added its own flags.
struct Launch {
    cmd: Option<Vec<String>>,
    cwd: Option<String>,
    size: Size,
    kind: Option<AgentKind>,
    env: Vec<(String, String)>,
}

impl Daemon {
    fn terminals(&self) -> MutexGuard<'_, HashMap<TermId, Handle>> {
        self.terminals.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn handle(&self, id: TermId) -> Option<Handle> {
        self.terminals().get(&id).cloned()
    }

    fn agents(&self) -> MutexGuard<'_, Agents> {
        self.agents.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn create(self: &Arc<Self>, req: Request) -> Result<TermId> {
        let Request::Create {
            cmd,
            cwd,
            size,
            agent,
            env,
        } = req
        else {
            anyhow::bail!("not a create request");
        };
        let kind = agent.or_else(|| cmd.as_deref().and_then(guess_kind));
        self.launch(Launch {
            cmd,
            cwd,
            size,
            kind,
            env,
        })
    }

    /// Starts a stopped agent again in a new terminal, from its saved session.
    fn resume(self: &Arc<Self>, old: TermId, size: Size) -> Result<TermId> {
        let record = self
            .store()
            .get(old)
            .cloned()
            .with_context(|| format!("no agent {old}"))?;
        let sid = record
            .session_id
            .with_context(|| format!("agent {old} has no session ID"))?;
        let cmd = match record.kind {
            Some(AgentKind::Claude) => vec!["claude".into(), "--resume".into(), sid],
            Some(AgentKind::Codex) => vec!["codex".into(), "resume".into(), sid],
            None => anyhow::bail!("agent {old} is not a Claude Code or Codex agent"),
        };
        self.launch(Launch {
            cmd: Some(cmd),
            cwd: record.cwd,
            size,
            kind: record.kind,
            env: Vec::new(),
        })
    }

    fn launch(self: &Arc<Self>, mut l: Launch) -> Result<TermId> {
        let id = {
            let mut store = self.store();
            let id = store.next_id();
            let label = l.cmd.as_ref().map_or("login shell".into(), |c| c.join(" "));
            store.starting(id, &label, l.kind, l.cwd.clone())?;
            id
        };
        if let (Some(kind), Some(cmd)) = (l.kind, l.cmd.as_mut()) {
            self.add_status_flags(kind, cmd, l.cwd.as_deref())?;
        }
        l.env.push(("SWARM_SOCKET".into(), self.socket.display().to_string()));
        l.env.push(("SWARM_AGENT_ID".into(), id.to_string()));
        self.agents().add(id, l.kind);
        let on_exit = {
            let daemon = self.clone();
            move || {
                daemon.terminals().remove(&id);
                let _ = daemon.store().exited(id);
                eprintln!("[daemon] terminal {id} exited");
            }
        };
        let raw_log = std::env::var_os("SWARM_RAW_LOG").map(|_| self.dir.join(format!("term-{id}.raw")));
        let handle = terminal::spawn(
            Spawn {
                id,
                cmd: l.cmd,
                cwd: l.cwd,
                env: l.env,
                size: l.size,
                agents: self.agents.clone(),
                raw_log,
            },
            |pid| {
                let _ = self.store().running(id, pid);
                eprintln!("[daemon] terminal {id} started, pid {pid:?}");
            },
            on_exit,
        )?;
        self.terminals().insert(id, handle);
        Ok(id)
    }

    /// Adds the flags that make the CLI report to us, after the program name
    /// and its subcommand (`codex resume`, `codex exec`).
    fn add_status_flags(
        &self,
        kind: AgentKind,
        cmd: &mut Vec<String>,
        cwd: Option<&str>,
    ) -> Result<()> {
        let mut at = 1.min(cmd.len());
        let flags = match kind {
            AgentKind::Claude => {
                let path = self.dir.join("claude-settings.json");
                agent::write_claude_settings(&path, &ctl_path()?)?;
                vec!["--settings".to_owned(), path.display().to_string()]
            }
            AgentKind::Codex => {
                if cmd.get(1).is_some_and(|c| matches!(c.as_str(), "resume" | "exec" | "fork")) {
                    at = 2;
                }
                let mut flags = agent::codex_hook_flags(&ctl_path()?);
                // The startup update prompt defaults to "Update now", which
                // runs `brew upgrade`; the first Enter we send would accept it.
                flags.push("-c".to_owned());
                flags.push("check_for_update_on_startup=false".to_owned());
                // Swarm made this worktree, so trust it for this run. Without
                // this Codex asks and saves the answer in ~/.codex/config.toml,
                // and skips the project's AGENTS.md while untrusted.
                if let Some(dir) = cwd {
                    flags.push("-c".to_owned());
                    flags.push(format!("projects={{{dir:?}={{trust_level=\"trusted\"}}}}"));
                }
                flags
            }
        };
        cmd.splice(at..at, flags);
        Ok(())
    }

    fn hook(&self, agent: TermId, sent_us: u64, payload: &str) {
        let session = self.agents().hook(agent, sent_us, payload);
        if let Some(s) = session {
            let _ = self.store().session(agent, s.id, s.transcript);
        }
    }

    fn list(&self) -> Vec<proto::TerminalInfo> {
        let mut list: Vec<_> = self
            .terminals()
            .values()
            .map(|h| {
                let mut info = h.info.lock().unwrap_or_else(|e| e.into_inner()).clone();
                info.agent = self.agents().view(info.id);
                info
            })
            .collect();
        list.sort_by_key(|t| t.id);
        list
    }
}

fn guess_kind(cmd: &[String]) -> Option<AgentKind> {
    let program = Path::new(cmd.first()?).file_name()?.to_str()?;
    match program {
        "claude" => Some(AgentKind::Claude),
        "codex" => Some(AgentKind::Codex),
        _ => None,
    }
}

/// `swarm-ctl` lives next to the daemon binary.
fn ctl_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.with_file_name("swarm-ctl"))
}

pub fn spike_dir() -> PathBuf {
    std::env::var_os("SWARM_SPIKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("swarm-agent-status"))
}

fn main() -> Result<()> {
    let dir = spike_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    // macOS limits socket paths to 104 bytes, so long dirs need an override.
    let socket = std::env::var_os("SWARM_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("daemon.sock"));
    if UnixStream::connect(&socket).is_ok() {
        eprintln!("[daemon] already running at {}", socket.display());
        return Ok(());
    }
    // Nobody answers, so the file is left over from a dead daemon.
    let _ = std::fs::remove_file(&socket);

    let mut store = Store::open(dir.join("agents.json"))?;
    store.recover()?;

    let listener = UnixListener::bind(&socket).context("bind socket")?;
    eprintln!(
        "[daemon] pid {} listening on {}",
        std::process::id(),
        socket.display()
    );
    let daemon = Arc::new(Daemon {
        terminals: Mutex::default(),
        store: Mutex::new(store),
        agents: Arc::default(),
        next_client: AtomicU64::new(1),
        dir: dir.clone(),
        socket: socket.clone(),
    });
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[daemon] accept failed: {e}");
                continue;
            }
        };
        let daemon = daemon.clone();
        let client = daemon.next_client.fetch_add(1, Ordering::Relaxed);
        thread::Builder::new()
            .name(format!("client-{client}"))
            .spawn(move || {
                if let Err(e) = serve(&daemon, client, stream) {
                    eprintln!("[daemon] client {client}: {e:#}");
                }
            })?;
    }
    Ok(())
}

/// Reads requests on this thread; a second thread writes frames, so a slow
/// client never blocks a terminal thread.
fn serve(daemon: &Arc<Daemon>, client: ClientId, stream: UnixStream) -> Result<()> {
    let (tx, rx) = flume::unbounded::<Frame>();
    let mut writer = BufWriter::new(stream.try_clone()?);
    thread::Builder::new()
        .name(format!("client-{client}-writer"))
        .spawn(move || {
            while let Ok(frame) = rx.recv() {
                if writer.write_all(&frame).is_err() {
                    return;
                }
                // Batch whatever else is queued, then flush once.
                for frame in rx.try_iter() {
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                }
                if writer.flush().is_err() {
                    return;
                }
            }
        })?;
    let send = |event: &Event| -> Result<()> {
        tx.send(Arc::new(proto::encode(event)?))
            .context("client writer gone")
    };

    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    let mut attached = HashSet::new();
    let result = loop {
        let req: Request = match proto::read(&mut reader, &mut buf) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break Ok(()),
            Err(e) => break Err(e.into()),
        };
        let to_term = |id: TermId, cmd: Cmd| -> Result<()> {
            match daemon.handle(id) {
                Some(h) => {
                    let _ = h.tx.send(cmd);
                    Ok(())
                }
                None => send(&Event::Error {
                    message: format!("no terminal {id}"),
                }),
            }
        };
        let r = match req {
            Request::Hello { version } => {
                if version != proto::VERSION {
                    eprintln!("[daemon] client {client} speaks version {version}");
                }
                send(&Event::Hello {
                    version: proto::VERSION,
                    pid: std::process::id(),
                })
            }
            Request::List => send(&Event::Terminals(daemon.list())),
            req @ Request::Create { .. } => match daemon.create(req) {
                Ok(id) => send(&Event::Created { id }),
                Err(e) => send(&Event::Error {
                    message: format!("{e:#}"),
                }),
            },
            Request::Attach { id, history } => {
                attached.insert(id);
                to_term(
                    id,
                    Cmd::Attach {
                        client,
                        tx: tx.clone(),
                        history,
                    },
                )
            }
            Request::Detach { id } => {
                attached.remove(&id);
                to_term(id, Cmd::Detach { client })
            }
            Request::Key { id, key } => to_term(id, Cmd::Key(key)),
            Request::Paste { id, text } => to_term(id, Cmd::Paste(text)),
            Request::Input { id, bytes } => to_term(id, Cmd::Input(bytes)),
            Request::Resize { id, size } => to_term(id, Cmd::Resize(size)),
            Request::FetchHistory { id, from, count } => to_term(
                id,
                Cmd::FetchHistory {
                    client,
                    from,
                    count,
                },
            ),
            Request::Kill { id } => to_term(id, Cmd::Kill),
            Request::Resume { id, size } => match daemon.resume(id, size) {
                Ok(id) => send(&Event::Created { id }),
                Err(e) => send(&Event::Error {
                    message: format!("{e:#}"),
                }),
            },
            Request::Watch => daemon.agents().watch(tx.clone()),
            Request::Hook {
                agent,
                sent_us,
                payload,
            } => {
                daemon.hook(agent, sent_us, &payload);
                Ok(())
            }
        };
        if let Err(e) = r {
            break Err(e);
        }
    };
    for id in attached {
        if let Some(h) = daemon.handle(id) {
            let _ = h.tx.send(Cmd::Detach { client });
        }
    }
    result
}
