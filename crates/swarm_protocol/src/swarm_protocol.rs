//! The socket protocol between `swarm-daemon` and the app, plus the app's client.
//!
//! A frame is a little-endian `u32` length followed by one postcard-encoded
//! [`Request`] (client → daemon) or [`Event`] (daemon → client). Terminal input and
//! output reuse the `swarm_term` types, tagged with a [`TerminalId`].
//!
//! No libghostty here: the app links this crate (ADR 0007).

use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use swarm_term::{Palette, Size, TerminalInput, TerminalOutput};

mod client;

pub use client::{Client, ClientError};

/// Bumped on every incompatible change; the daemon refuses other versions.
pub const VERSION: u32 = 4;

/// Guards against a corrupt length prefix allocating gigabytes.
const MAX_FRAME: usize = 256 << 20;

/// macOS limit for a unix socket path (`sun_path`), including the trailing NUL.
const MAX_SOCKET_PATH: usize = 104;

/// Stable for the daemon's lifetime and stored in its database.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    /// Opened by the user. Agents can never see it (ADR 0006).
    User,
    Process,
    Agent,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTerminal {
    pub kind: TerminalKind,
    /// `None` runs the user's login shell.
    pub command: Option<Vec<String>>,
    pub cwd: Option<PathBuf>,
    pub size: Size,
    pub palette: Palette,
    /// Opaque key the app uses to find the terminal's task again after a restart.
    pub task: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

impl TaskId {
    /// The `task` key of the terminals that belong to this task.
    pub fn terminal_key(self) -> String {
        format!("task:{}", self.0)
    }
}

/// A git repository the user added to Swarm.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub id: ProjectId,
    /// The repository folder's name.
    pub name: String,
    /// The repository root.
    pub path: PathBuf,
    pub base_branch: String,
    /// Why `.swarm/config.toml` could not be read; defaults apply meanwhile.
    pub config_error: Option<String>,
    pub tasks: Vec<TaskInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(pub u64);

/// Which CLI an agent runs.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRole {
    Orchestrator,
    Worker,
}

/// What an agent is doing, merged from OSC 7501, hooks and the process exit.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Unknown,
    Idle,
    Working,
    Done,
    Blocked(swarm_term::BlockedKind),
    Error,
}

/// Whether the agent's process runs. Separate from [`AgentStatus`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLifecycle {
    Starting,
    Running,
    /// Ended; it can start again from its session.
    Resumable,
    Stopped,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: AgentId,
    pub task: TaskId,
    pub role: AgentRole,
    pub kind: AgentKind,
    pub model: String,
    /// The agent's terminal while it runs.
    pub terminal: Option<TerminalId>,
    pub status: AgentStatus,
    /// The program's own status text, or an error reason.
    pub message: Option<String>,
    pub lifecycle: AgentLifecycle,
    /// Goes up with every change, so a client can drop a copy older than the one it has.
    pub revision: u64,
}

/// One task: its own branch and task worktree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub id: TaskId,
    pub project: ProjectId,
    pub name: String,
    pub branch: String,
    /// The branch it started from.
    pub base: String,
    pub worktree: PathBuf,
    /// Ports reserved for this task worktree, from `[worktrees] ports`.
    pub ports: Vec<u16>,
    /// The process terminal running the setup script, while it runs.
    pub setup_terminal: Option<TerminalId>,
    /// How the setup script ended, so a fast or failed setup is never missed.
    pub setup_exit: Option<swarm_term::ExitStatus>,
    pub agents: Vec<AgentInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTask {
    pub project: ProjectId,
    pub name: String,
    /// Branch to start from; `None` uses the project's base branch.
    pub from: Option<String>,
    pub run_setup: bool,
    /// Starts an orchestrator of this kind once setup is done; `None` starts none.
    pub orchestrator: Option<AgentKind>,
    /// Typed into the orchestrator once it reports that it is ready.
    pub first_message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub kind: TerminalKind,
    pub task: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    /// `None` while the process runs.
    pub exit: Option<swarm_term::ExitStatus>,
}

/// Matches a reply to its request, so a late reply never answers a newer call.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallId(pub u32);

/// Client → daemon. `Hello` must stay the first variant with the same fields in every
/// version: it is the only message both sides can read after a version change.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello {
        version: u32,
    },
    List {
        call: CallId,
    },
    /// Answered with [`Event::Created`]; the client is attached to the new terminal.
    Create {
        call: CallId,
        terminal: CreateTerminal,
    },
    /// Answered with a snapshot holding up to `history` scrollback lines, then the
    /// output stream until [`Request::Detach`] or disconnect.
    Attach {
        id: TerminalId,
        history: u32,
    },
    Detach {
        id: TerminalId,
    },
    Input {
        id: TerminalId,
        input: TerminalInput,
    },
    /// Answered with [`Event::Projects`].
    ListProjects {
        call: CallId,
    },
    /// `path` may be any folder inside a git repository. Answered with
    /// [`Event::ProjectAdded`]; adding a project twice returns the existing one.
    AddProject {
        call: CallId,
        path: PathBuf,
    },
    /// Creates the task branch and task worktree, copies files, reserves ports and
    /// starts the setup script in a process terminal. Answered with [`Event::TaskCreated`].
    CreateTask {
        call: CallId,
        task: CreateTask,
    },
}

/// Daemon → client. `Hello` stays first, as in [`Request`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Hello {
        version: u32,
        pid: u32,
    },
    Terminals {
        call: CallId,
        terminals: Vec<TerminalInfo>,
    },
    Created {
        call: CallId,
        id: TerminalId,
    },
    Output {
        id: TerminalId,
        output: TerminalOutput,
    },
    /// The terminal ended and the daemon forgot it; no more output follows.
    Closed {
        id: TerminalId,
    },
    /// `call` is set when the error answers a request, `id` when it is about a terminal.
    Error {
        call: Option<CallId>,
        id: Option<TerminalId>,
        message: String,
    },
    Projects {
        call: CallId,
        projects: Vec<ProjectInfo>,
    },
    ProjectAdded {
        call: CallId,
        project: ProjectInfo,
    },
    TaskCreated {
        call: CallId,
        task: TaskInfo,
    },
    /// Sent to every client whenever an agent starts, ends or changes status.
    AgentChanged {
        agent: AgentInfo,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("socket error: {0}")]
    Io(#[from] io::Error),
    #[error("cannot encode or decode a frame: {0}")]
    Codec(#[from] postcard::Error),
    #[error("frame of {0} bytes is over the limit")]
    TooLarge(usize),
}

/// Encodes one length-prefixed frame, ready to write to any number of clients.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut frame = postcard::to_extend(message, vec![0; 4])?;
    let length = frame.len() - 4;
    let prefix = u32::try_from(length).map_err(|_| FrameError::TooLarge(length))?;

    frame[..4].copy_from_slice(&prefix.to_le_bytes());

    Ok(frame)
}

pub fn write<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<(), FrameError> {
    let frame = encode(message)?;
    writer.write_all(&frame)?;

    Ok(())
}

/// Reads one frame into `buffer` and decodes it. `buffer` is reused between calls.
pub fn read<T: DeserializeOwned>(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> Result<T, FrameError> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;

    let length = u32::from_le_bytes(prefix) as usize;
    if length > MAX_FRAME {
        return Err(FrameError::TooLarge(length));
    }

    buffer.resize(length, 0);
    reader.read_exact(buffer)?;

    Ok(postcard::from_bytes(buffer)?)
}

/// Where the daemon keeps its socket, database and log.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub socket: PathBuf,
    pub database: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    /// `~/Library/Application Support/Swarm`.
    pub fn new() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;

        Ok(Self::in_dir(home.join("Library/Application Support/Swarm")))
    }

    /// Everything inside `data`. The socket moves to the user's private temp folder
    /// when `data` makes it too long for macOS.
    pub fn in_dir(data: PathBuf) -> Self {
        let socket = data.join("daemon.sock");
        let socket = if socket.as_os_str().len() < MAX_SOCKET_PATH {
            socket
        } else {
            user_temp_dir().join("swarm").join("daemon.sock")
        };

        Self {
            database: data.join("swarm.db"),
            log: data.join("daemon.log"),
            socket,
            data,
        }
    }
}

/// The per-user temp folder macOS creates with mode 0700, unlike `/tmp`.
fn user_temp_dir() -> PathBuf {
    let mut buffer = vec![0_u8; 1024];

    // SAFETY: the buffer is valid for its full length, which is passed as the size.
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };

    if length == 0 || length > buffer.len() {
        return std::env::temp_dir();
    }

    buffer.truncate(length - 1);
    PathBuf::from(std::ffi::OsString::from_vec(buffer))
}

/// Refuses a folder that another user owns or others can open, so nobody else can
/// plant a socket there. Both the daemon and the client check the socket's folder.
pub fn check_private_dir(path: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(path)?;

    // SAFETY: getuid has no preconditions and cannot fail.
    let user = unsafe { libc::getuid() };

    if metadata.uid() != user || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be owned by you with mode 0700", path.display()),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let message = Request::Attach {
            id: TerminalId(7),
            history: 1_000,
        };

        let frame = encode(&message).unwrap();
        let back: Request = read(&mut frame.as_slice(), &mut Vec::new()).unwrap();

        assert_eq!(back, message);
    }

    #[test]
    fn a_huge_length_prefix_is_refused() {
        let frame = u32::MAX.to_le_bytes();

        let result: Result<Request, _> = read(&mut frame.as_slice(), &mut Vec::new());

        assert!(matches!(result, Err(FrameError::TooLarge(_))));
    }
}
