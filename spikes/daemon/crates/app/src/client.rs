//! Connection to `swarm-daemon`: startup handshake, then a reader and a writer thread.

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use proto::{Event, Request, Size, Snapshot, TermId};

/// Scrollback lines sent with the attach snapshot; older lines are fetched on scroll.
const ATTACH_HISTORY: u32 = 1_000;

#[derive(Clone)]
pub struct Client {
    tx: flume::Sender<Request>,
}

impl Client {
    pub fn send(&self, req: Request) {
        // Fails only after the connection closed; the reader reports that.
        let _ = self.tx.send(req);
    }
}

pub struct Attached {
    pub client: Client,
    pub events: flume::Receiver<Event>,
    pub snapshot: Snapshot,
    pub timings: String,
}

pub fn spike_dir() -> PathBuf {
    std::env::var_os("SWARM_SPIKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("swarm-spike"))
}

/// Connects (starting the daemon if needed), picks the first terminal or
/// creates one, and attaches. Blocks until the snapshot arrives, so the first
/// frame already shows the screen.
pub fn attach(size: Size) -> Result<Attached> {
    let start = Instant::now();
    let socket = spike_dir().join("daemon.sock");
    let (stream, spawned) = connect_or_spawn(&socket)?;
    let connected = start.elapsed();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    let mut buf = Vec::new();
    let mut call = |req: &Request| -> Result<Event> {
        proto::write(&mut writer, req)?;
        writer.flush()?;
        Ok(proto::read(&mut reader, &mut buf)?)
    };

    call(&Request::Hello {
        version: proto::VERSION,
    })?;
    let Event::Terminals(list) = call(&Request::List)? else {
        bail!("unexpected reply to List");
    };
    // SWARM_NEW=1 always creates a terminal, e.g. for a second agent.
    let existing = list
        .first()
        .filter(|_| std::env::var_os("SWARM_NEW").is_none());
    let id: TermId = match existing {
        Some(t) => t.id,
        None => {
            let cmd = std::env::var("SWARM_CMD")
                .ok()
                .map(|c| c.split_whitespace().map(str::to_owned).collect());
            match call(&Request::Create {
                cmd,
                cwd: None,
                size,
            })? {
                Event::Created { id } => id,
                Event::Error { message } => bail!("create failed: {message}"),
                other => bail!("unexpected reply to Create: {other:?}"),
            }
        }
    };
    let before_attach = Instant::now();
    let snapshot = match call(&Request::Attach {
        id,
        history: ATTACH_HISTORY,
    })? {
        Event::Snapshot(s) => s,
        Event::Error { message } => bail!("attach failed: {message}"),
        other => bail!("unexpected reply to Attach: {other:?}"),
    };
    let attach_time = before_attach.elapsed();
    let timings = format!(
        "connect {:.1} ms{} | attach {:.1} ms ({} B, {}x{} + {} history lines) | total {:.1} ms",
        connected.as_secs_f64() * 1e3,
        if spawned { " (started daemon)" } else { "" },
        attach_time.as_secs_f64() * 1e3,
        buf.len(),
        snapshot.screen.cols,
        snapshot.screen.rows,
        snapshot.history_rows.len(),
        start.elapsed().as_secs_f64() * 1e3,
    );

    let (events_tx, events) = flume::unbounded();
    thread::Builder::new()
        .name("daemon-reader".into())
        .spawn(move || {
            let mut buf = Vec::new();
            loop {
                match proto::read::<Event>(&mut reader, &mut buf) {
                    Ok(event) => {
                        if events_tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("[client] connection closed: {e}");
                        return;
                    }
                }
            }
        })?;
    let (tx, rx) = flume::unbounded::<Request>();
    thread::Builder::new()
        .name("daemon-writer".into())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                let mut ok = proto::write(&mut writer, &req).is_ok();
                for req in rx.try_iter() {
                    ok = ok && proto::write(&mut writer, &req).is_ok();
                }
                if !ok || writer.flush().is_err() {
                    return;
                }
            }
        })?;

    Ok(Attached {
        client: Client { tx },
        events,
        snapshot,
        timings,
    })
}

fn connect_or_spawn(socket: &Path) -> Result<(UnixStream, bool)> {
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok((stream, false));
    }
    spawn_daemon()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok((stream, true));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(anyhow!(
        "daemon did not start; see {}",
        socket.with_extension("log").display()
    ))
}

/// Starts the daemon in its own session, so quitting or killing the app (or
/// closing the terminal that started it) does not signal it.
fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?
        .parent()
        .context("app has no directory")?
        .join("swarm-daemon");
    let dir = spike_dir();
    std::fs::create_dir_all(&dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("daemon.log"))?;
    let mut cmd = Command::new(&exe);
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe and touches no memory.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("start {}", exe.display()))?;
    eprintln!("[client] started daemon pid {}", child.id());
    Ok(())
}
