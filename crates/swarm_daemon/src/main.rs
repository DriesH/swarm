//! `swarm-daemon`: owns the terminals and serves them over a unix socket (ADR 0001).

mod agent_socket;
mod agents;
mod calls;
mod connection;
mod hub;
mod messages;
mod recovery;
mod tasks;

use std::fs::{DirBuilder, File, Permissions};
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result};
use swarm_core::Projects;
use swarm_core::store::Store;
use swarm_protocol::{Paths, check_private_dir};

use crate::agents::{AgentConfig, Agents};
use crate::calls::Calls;
use crate::hub::{Clients, Terminals};
use crate::messages::Messages;
use crate::recovery::System;

/// What every client connection works with.
pub(crate) struct Services {
    pub(crate) terminals: Arc<Terminals>,
    pub(crate) projects: Arc<Projects>,
    pub(crate) agents: Arc<Agents>,
    pub(crate) clients: Arc<Clients>,
    pub(crate) messages: Arc<Messages>,
    pub(crate) store: Arc<Mutex<Store>>,
}

/// Where the daemon is told to keep its files and read its settings.
struct Arguments {
    data_dir: Option<PathBuf>,
    settings: Option<PathBuf>,
}

/// How long terminals get to exit on SIGTERM before the daemon leaves anyway.
/// Pause after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Write end of the pipe the signal handler wakes the shutdown thread with.
static SHUTDOWN_PIPE: AtomicI32 = AtomicI32::new(-1);

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if let Err(error) = unblock_signals() {
        log::error!("{error:#}");
        return ExitCode::FAILURE;
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let arguments = arguments()?;
    let paths = match arguments.data_dir {
        Some(dir) => Paths::in_dir(dir),
        None => Paths::new().context("cannot find the data folder")?,
    };

    private_directory(&paths.data)?;
    let socket_dir = paths.socket.parent().context("the socket has no folder")?;
    private_directory(socket_dir)?;
    check_private_dir(socket_dir)
        .with_context(|| format!("cannot use {}", socket_dir.display()))?;

    // Held until the process ends; only the holder may touch the socket or recover.
    let Some(_lock) = take_lock(&paths.data.join("daemon.lock"))? else {
        log::info!("another daemon already runs for {}", paths.data.display());
        return Ok(());
    };

    ignore_hangup()?;

    match std::fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot remove the stale socket"),
    }
    let listener = UnixListener::bind(&paths.socket).context("cannot bind the socket")?;
    std::fs::set_permissions(&paths.socket, Permissions::from_mode(0o600))?;

    let agent_socket_path = socket_dir.join("agent.sock");
    let agent_listener = agent_socket::bind(&agent_socket_path)?;
    log::info!(
        "pid {} listening on {}",
        std::process::id(),
        paths.socket.display()
    );

    let store = Store::open(&paths.database)?;
    let leftovers = store.leftovers()?;
    store.clear_setup_terminals()?;
    store.recover_agents()?;
    let store = Arc::new(Mutex::new(store));

    let recovery_store = Arc::clone(&store);
    thread::Builder::new()
        .name("recovery".into())
        .spawn(move || recovery::recover(&recovery_store, &leftovers, &System))?;

    let swarm = std::env::current_exe()
        .context("cannot find the daemon's own path")?
        .with_file_name("swarm");
    if !swarm.is_file() {
        log::warn!("{} is missing: agent hooks will not work", swarm.display());
    }

    let projects = Arc::new(Projects::new(Arc::clone(&store)));
    let terminals = Terminals::new(Arc::clone(&store));
    let clients = Arc::new(Clients::default());
    let messages = Messages::new(Arc::clone(&store), Arc::clone(&clients));
    let agents = Agents::new(
        AgentConfig {
            data_dir: paths.data.clone(),
            socket: agent_socket_path.clone(),
            settings: arguments.settings.or_else(swarm_settings::default_path),
            swarm,
        },
        Arc::clone(&store),
        Arc::clone(&terminals),
        Arc::clone(&clients),
        Arc::clone(&messages),
    );

    watch_signals(
        Arc::clone(&terminals),
        vec![paths.socket.clone(), agent_socket_path],
    )?;

    let services = Arc::new(Services {
        terminals,
        projects,
        agents,
        clients,
        messages,
        store,
    });

    let calls = Arc::new(Calls::new(Arc::clone(&services)));
    thread::Builder::new()
        .name("agent-socket".into())
        .spawn(move || agent_socket::serve(&agent_listener, &calls))?;

    serve(&listener, &services);

    Ok(())
}

/// `--data-dir <folder>` and `--settings <file>`, both optional.
fn arguments() -> Result<Arguments> {
    let mut input = std::env::args_os().skip(1);
    let mut parsed = Arguments {
        data_dir: None,
        settings: None,
    };

    while let Some(argument) = input.next() {
        if argument == "--data-dir" {
            let dir = input.next().context("--data-dir needs a folder")?;
            parsed.data_dir = Some(PathBuf::from(dir));
        } else if argument == "--settings" {
            let file = input.next().context("--settings needs a file")?;
            parsed.settings = Some(PathBuf::from(file));
        } else {
            anyhow::bail!("unknown argument {}", argument.to_string_lossy());
        }
    }

    Ok(parsed)
}

/// `None` when another process holds the lock.
fn take_lock(path: &Path) -> Result<Option<File>> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;

    // SAFETY: the descriptor belongs to `file`, which outlives the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }

    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(None);
    }

    Err(error).context("cannot lock the daemon lock file")
}

/// The app's terminal may hang up; that must not end the daemon.
/// A process started from a GCD thread (the app's background work) inherits a mask
/// that blocks every signal, so SIGTERM would never arrive. Runs before any thread
/// starts, because threads copy the mask they are created with.
fn unblock_signals() -> Result<()> {
    // SAFETY: the set is initialised by sigemptyset before pthread_sigmask reads it.
    let result = unsafe {
        let mut none = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&raw mut none);
        libc::pthread_sigmask(libc::SIG_SETMASK, &raw const none, std::ptr::null_mut())
    };

    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result)).context("cannot unblock signals");
    }

    Ok(())
}

fn ignore_hangup() -> Result<()> {
    // SAFETY: ignoring a signal installs no handler code.
    if unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) } == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error()).context("cannot ignore SIGHUP");
    }

    Ok(())
}

fn serve(listener: &UnixListener, services: &Arc<Services>) {
    let next_client = AtomicU64::new(1);

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                log::warn!("accept failed: {error}");
                thread::park_timeout(ACCEPT_RETRY);
                continue;
            }
        };

        let client = next_client.fetch_add(1, Ordering::Relaxed);
        let services = Arc::clone(services);
        let spawned = thread::Builder::new()
            .name(format!("client-{client}"))
            .spawn(move || {
                if let Err(error) = connection::serve(&services, client, stream) {
                    log::warn!("client {client}: {error:#}");
                }
            });
        if let Err(error) = spawned {
            log::error!("cannot start a thread for client {client}: {error}");
        }
    }
}

fn private_directory(path: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    std::fs::set_permissions(path, Permissions::from_mode(0o700))?;

    Ok(())
}

extern "C" fn on_signal(_signal: libc::c_int) {
    let descriptor = SHUTDOWN_PIPE.load(Ordering::Relaxed);
    let byte = 1_u8;

    // SAFETY: write is async-signal-safe and reads one byte from a live stack value.
    unsafe {
        libc::write(descriptor, (&raw const byte).cast(), 1);
    }
}

/// SIGTERM and SIGINT kill every terminal, remove the sockets and end the process.
fn watch_signals(terminals: Arc<Terminals>, sockets: Vec<PathBuf>) -> Result<()> {
    let mut descriptors = [0; 2];
    // SAFETY: pipe fills the two integers of the array.
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot create the signal pipe");
    }
    SHUTDOWN_PIPE.store(descriptors[1], Ordering::Relaxed);

    for signal in [libc::SIGTERM, libc::SIGINT] {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler only calls write on a pipe descriptor.
        if unsafe { libc::signal(signal, handler) } == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error()).context("cannot install a handler");
        }
    }

    // SAFETY: the read end is a fresh descriptor that nothing else owns.
    let mut wake = unsafe { File::from_raw_fd(descriptors[0]) };
    thread::Builder::new()
        .name("shutdown".into())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            if let Err(error) = wake.read_exact(&mut byte) {
                log::error!("signal pipe failed: {error}");
                return;
            }

            log::info!("shutting down");
            terminals.kill_all();
            if !terminals.wait_until_empty(SHUTDOWN_GRACE) {
                log::warn!("some terminals did not exit in time");
            }

            for socket in &sockets {
                if let Err(error) = std::fs::remove_file(socket) {
                    log::warn!("cannot remove {}: {error}", socket.display());
                }
            }
            std::process::exit(0);
        })?;

    Ok(())
}
