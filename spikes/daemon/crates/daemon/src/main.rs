//! `swarm-daemon`: owns agent terminals and serves them over a unix socket.

mod encode;
mod keys;
mod pty;
mod state;
mod terminal;

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use anyhow::{Context as _, Result};
use proto::{Event, Request, TermId};

use crate::state::Store;
use crate::terminal::{ClientId, Cmd, Frame, Handle, Spawn};

struct Daemon {
    terminals: Mutex<HashMap<TermId, Handle>>,
    store: Mutex<Store>,
    next_client: AtomicU64,
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

    fn create(self: &Arc<Self>, req: Request) -> Result<TermId> {
        let Request::Create { cmd, cwd, size } = req else {
            anyhow::bail!("not a create request");
        };
        let id = {
            let mut store = self.store();
            let id = store.next_id();
            let label = cmd.as_ref().map_or("login shell".into(), |c| c.join(" "));
            store.starting(id, &label)?;
            id
        };
        let on_exit = {
            let daemon = self.clone();
            move || {
                daemon.terminals().remove(&id);
                let _ = daemon.store().exited(id);
                eprintln!("[daemon] terminal {id} exited");
            }
        };
        let handle = terminal::spawn(
            Spawn { id, cmd, cwd, size },
            |pid| {
                let _ = self.store().running(id, pid);
                eprintln!("[daemon] terminal {id} started, pid {pid:?}");
            },
            on_exit,
        )?;
        self.terminals().insert(id, handle);
        Ok(id)
    }

    fn list(&self) -> Vec<proto::TerminalInfo> {
        let mut list: Vec<_> = self
            .terminals()
            .values()
            .map(|h| h.info.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .collect();
        list.sort_by_key(|t| t.id);
        list
    }
}

pub fn spike_dir() -> PathBuf {
    std::env::var_os("SWARM_SPIKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("swarm-spike"))
}

fn main() -> Result<()> {
    let dir = spike_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let socket = dir.join("daemon.sock");
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
        next_client: AtomicU64::new(1),
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
