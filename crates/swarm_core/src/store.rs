//! The daemon's SQLite database (ADR 0005). Only the daemon opens it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CreateTerminal,
    ProjectId, TaskId, TaskInfo, TerminalId, TerminalKind,
};
use swarm_term::ExitStatus;

use crate::Error;

/// How long a locked database makes a statement wait.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One entry per schema version; `PRAGMA user_version` counts how many ran. Never edit
/// an entry that shipped: add a new one.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE terminals (
        id INTEGER PRIMARY KEY,
        kind TEXT NOT NULL,
        task TEXT,
        command TEXT NOT NULL,
        cwd TEXT,
        pid INTEGER,
        started_us INTEGER,
        status TEXT NOT NULL,
        exit TEXT,
        created_at INTEGER NOT NULL
    )",
    "CREATE TABLE projects (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,
        added_at INTEGER NOT NULL
    );
    CREATE TABLE tasks (
        id INTEGER PRIMARY KEY,
        project INTEGER NOT NULL REFERENCES projects(id),
        name TEXT NOT NULL,
        branch TEXT NOT NULL,
        base TEXT NOT NULL,
        worktree TEXT NOT NULL,
        ports TEXT NOT NULL,
        setup_terminal INTEGER,
        created_at INTEGER NOT NULL
    )",
    "ALTER TABLE tasks ADD COLUMN setup_exit TEXT",
    "CREATE TABLE agents (
        id INTEGER PRIMARY KEY,
        task INTEGER NOT NULL REFERENCES tasks(id),
        role TEXT NOT NULL,
        kind TEXT NOT NULL,
        model TEXT NOT NULL,
        session_id TEXT,
        terminal INTEGER,
        status TEXT NOT NULL,
        message TEXT,
        lifecycle TEXT NOT NULL,
        revision INTEGER NOT NULL DEFAULT 0,
        session_confirmed INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    )",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Starting,
    Running,
    Exited,
    /// Stopped by recovery after a daemon crash.
    Stopped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Stopped => "stopped",
        }
    }
}

/// A terminal that an earlier daemon left unfinished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub id: TerminalId,
    pub command: String,
    pub pid: Option<i32>,
    /// Process start time in microseconds since the epoch; guards against PID reuse.
    pub started_us: Option<u64>,
}

/// A project row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProject {
    pub id: ProjectId,
    pub path: PathBuf,
    pub name: String,
}

/// A task to record.
#[derive(Debug, Clone, Copy)]
pub struct NewTask<'a> {
    pub project: ProjectId,
    pub name: &'a str,
    pub branch: &'a str,
    pub base: &'a str,
    pub worktree: &'a Path,
    pub ports: &'a [u16],
}

/// An agent to record. It starts `Unknown` and `Starting`.
#[derive(Debug, Clone, Copy)]
pub struct NewAgent<'a> {
    pub task: TaskId,
    pub role: AgentRole,
    pub kind: AgentKind,
    pub model: &'a str,
    pub session_id: Option<&'a str>,
}

/// An agent row with the session that [`AgentInfo`] does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAgent {
    pub info: AgentInfo,
    pub session_id: Option<String>,
    /// The agent itself reported the session, so a real conversation exists.
    pub session_confirmed: bool,
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;

        Self::migrated(connection)
    }

    pub fn open_in_memory() -> Result<Self, Error> {
        Self::migrated(Connection::open_in_memory()?)
    }

    fn migrated(mut connection: Connection) -> Result<Self, Error> {
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let applied = usize::try_from(version)
            .map_err(|_| Error::Invalid(format!("the database version {version} is negative")))?;

        for (index, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(migration)?;
            let version = i64::try_from(index + 1)
                .map_err(|_| Error::Invalid("too many migrations".to_owned()))?;
            transaction.pragma_update(None, "user_version", version)?;
            transaction.commit()?;
        }

        Ok(Self { connection })
    }

    /// Written before the process starts (ADR 0005). The row id is the terminal id,
    /// so ids never repeat across restarts.
    pub fn insert_starting(&self, request: &CreateTerminal) -> Result<TerminalId, Error> {
        let command = serde_json::to_string(&request.command)?;
        let cwd = request
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned());

        self.connection.execute(
            "INSERT INTO terminals (kind, task, command, cwd, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                kind_name(request.kind),
                request.task,
                command,
                cwd,
                Status::Starting.as_str(),
                now_seconds()
            ],
        )?;

        Ok(TerminalId(from_row(self.connection.last_insert_rowid())?))
    }

    pub fn mark_running(
        &self,
        id: TerminalId,
        pid: Option<u32>,
        started_us: Option<u64>,
    ) -> Result<(), Error> {
        let started_us = started_us.map(to_row).transpose()?;

        self.connection.execute(
            "UPDATE terminals SET status = ?1, pid = ?2, started_us = ?3 WHERE id = ?4",
            params![Status::Running.as_str(), pid, started_us, to_row(id.0)?],
        )?;

        Ok(())
    }

    pub fn mark_exited(&self, id: TerminalId, exit: Option<&ExitStatus>) -> Result<(), Error> {
        let exit = exit.map(serde_json::to_string).transpose()?;

        self.connection.execute(
            "UPDATE terminals SET status = ?1, exit = ?2 WHERE id = ?3",
            params![Status::Exited.as_str(), exit, to_row(id.0)?],
        )?;

        Ok(())
    }

    pub fn mark_stopped(&self, id: TerminalId) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE terminals SET status = ?1 WHERE id = ?2",
            params![Status::Stopped.as_str(), to_row(id.0)?],
        )?;

        Ok(())
    }

    pub fn status(&self, id: TerminalId) -> Result<Status, Error> {
        let status: String = self.connection.query_row(
            "SELECT status FROM terminals WHERE id = ?1",
            params![to_row(id.0)?],
            |row| row.get(0),
        )?;

        [
            Status::Starting,
            Status::Running,
            Status::Exited,
            Status::Stopped,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == status)
        .ok_or_else(|| Error::Invalid(format!("unknown status {status:?}")))
    }

    /// Rows that no daemon finished. Call it once at startup, before this run creates
    /// any row, so the result holds only leftovers.
    pub fn leftovers(&self) -> Result<Vec<Leftover>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, command, pid, started_us FROM terminals
             WHERE status IN (?1, ?2) ORDER BY id",
        )?;

        let rows = statement.query_map(
            params![Status::Starting.as_str(), Status::Running.as_str()],
            |row| {
                let id: i64 = row.get(0)?;
                let command: String = row.get(1)?;
                let pid: Option<i64> = row.get(2)?;
                let started_us: Option<i64> = row.get(3)?;

                Ok(Leftover {
                    id: TerminalId(id.unsigned_abs()),
                    command: command_label(&command),
                    pid: pid.and_then(|pid| i32::try_from(pid).ok()),
                    started_us: started_us.and_then(|started| u64::try_from(started).ok()),
                })
            },
        )?;

        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds the project, or returns the existing one with the same path.
    pub fn add_project(&self, path: &Path, name: &str) -> Result<StoredProject, Error> {
        let path_text = path.to_string_lossy();

        self.connection.execute(
            "INSERT OR IGNORE INTO projects (path, name, added_at) VALUES (?1, ?2, ?3)",
            params![path_text, name, now_seconds()],
        )?;

        self.connection
            .query_row(
                "SELECT id, path, name FROM projects WHERE path = ?1",
                params![path_text],
                project_from_row,
            )
            .map_err(Error::from)
    }

    pub fn project(&self, id: ProjectId) -> Result<Option<StoredProject>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, path, name FROM projects WHERE id = ?1",
                params![to_row(id.0)?],
                project_from_row,
            )
            .optional()?)
    }

    pub fn list_projects(&self) -> Result<Vec<StoredProject>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT id, path, name FROM projects ORDER BY id")?;
        let rows = statement.query_map([], project_from_row)?;

        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_task(&self, task: &NewTask<'_>) -> Result<TaskInfo, Error> {
        let ports = task
            .ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");

        self.connection.execute(
            "INSERT INTO tasks (project, name, branch, base, worktree, ports, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_row(task.project.0)?,
                task.name,
                task.branch,
                task.base,
                task.worktree.to_string_lossy(),
                ports,
                now_seconds()
            ],
        )?;

        Ok(TaskInfo {
            id: TaskId(from_row(self.connection.last_insert_rowid())?),
            project: task.project,
            name: task.name.to_owned(),
            branch: task.branch.to_owned(),
            base: task.base.to_owned(),
            worktree: task.worktree.to_owned(),
            ports: task.ports.to_vec(),
            setup_terminal: None,
            setup_exit: None,
            agents: Vec::new(),
        })
    }

    pub fn list_tasks(&self, project: ProjectId) -> Result<Vec<TaskInfo>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, project, name, branch, base, worktree, ports, setup_terminal, setup_exit
             FROM tasks WHERE project = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![to_row(project.0)?], |row| {
            let id: i64 = row.get(0)?;
            let project: i64 = row.get(1)?;
            let worktree: String = row.get(5)?;
            let ports: String = row.get(6)?;
            let setup_terminal: Option<i64> = row.get(7)?;
            let setup_exit: Option<String> = row.get(8)?;

            Ok(TaskInfo {
                id: TaskId(id.unsigned_abs()),
                project: ProjectId(project.unsigned_abs()),
                name: row.get(2)?,
                branch: row.get(3)?,
                base: row.get(4)?,
                worktree: PathBuf::from(worktree),
                ports: ports
                    .split(',')
                    .filter_map(|port| port.parse().ok())
                    .collect(),
                setup_terminal: setup_terminal.map(|id| TerminalId(id.unsigned_abs())),
                setup_exit: setup_exit.and_then(|json| serde_json::from_str(&json).ok()),
                agents: Vec::new(),
            })
        })?;

        let mut tasks: Vec<TaskInfo> = rows.collect::<rusqlite::Result<_>>()?;
        for task in &mut tasks {
            task.agents = self.list_agents(task.id)?;
        }

        Ok(tasks)
    }

    /// Written before the agent's process starts (ADR 0005).
    pub fn add_agent(&self, agent: &NewAgent<'_>) -> Result<AgentInfo, Error> {
        let status = AgentStatus::Unknown;
        let lifecycle = AgentLifecycle::Starting;

        self.connection.execute(
            "INSERT INTO agents (task, role, kind, model, session_id, status, lifecycle, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                to_row(agent.task.0)?,
                to_json(&agent.role)?,
                to_json(&agent.kind)?,
                agent.model,
                agent.session_id,
                to_json(&status)?,
                to_json(&lifecycle)?,
                now_seconds()
            ],
        )?;

        Ok(AgentInfo {
            id: AgentId(from_row(self.connection.last_insert_rowid())?),
            task: agent.task,
            role: agent.role,
            kind: agent.kind,
            model: agent.model.to_owned(),
            terminal: None,
            status,
            message: None,
            lifecycle,
            revision: 0,
        })
    }

    pub fn agent(&self, id: AgentId) -> Result<Option<StoredAgent>, Error> {
        Ok(self
            .connection
            .query_row(
                &format!("{AGENT_COLUMNS} WHERE id = ?1"),
                params![to_row(id.0)?],
                agent_from_row,
            )
            .optional()?)
    }

    pub fn list_agents(&self, task: TaskId) -> Result<Vec<AgentInfo>, Error> {
        let mut statement = self
            .connection
            .prepare(&format!("{AGENT_COLUMNS} WHERE task = ?1 ORDER BY id"))?;
        let rows = statement.query_map(params![to_row(task.0)?], agent_from_row)?;

        rows.map(|row| row.map(|stored| stored.info).map_err(Error::from))
            .collect()
    }

    /// Saves what changes while an agent runs: terminal, status, message, lifecycle and
    /// revision. The caller raises the revision first.
    pub fn save_agent(&self, agent: &AgentInfo) -> Result<(), Error> {
        let terminal = agent
            .terminal
            .map(|terminal| to_row(terminal.0))
            .transpose()?;

        self.connection.execute(
            "UPDATE agents SET terminal = ?1, status = ?2, message = ?3, lifecycle = ?4,
                 revision = ?5
             WHERE id = ?6",
            params![
                terminal,
                to_json(&agent.status)?,
                agent.message,
                to_json(&agent.lifecycle)?,
                to_row(agent.revision)?,
                to_row(agent.id.0)?
            ],
        )?;

        Ok(())
    }

    pub fn set_agent_session(&self, id: AgentId, session_id: &str) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE agents SET session_id = ?1 WHERE id = ?2",
            params![session_id, to_row(id.0)?],
        )?;

        Ok(())
    }

    /// Marks the session as real: the agent reported it in a hook.
    pub fn confirm_agent_session(&self, id: AgentId) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE agents SET session_confirmed = 1 WHERE id = ?1",
            params![to_row(id.0)?],
        )?;

        Ok(())
    }

    /// Terminals never outlive a daemon run, so an agent that still looks alive lost
    /// its process in a crash. It can resume when its session is confirmed.
    pub fn recover_agents(&self) -> Result<(), Error> {
        let mut statement = self
            .connection
            .prepare(&format!("{AGENT_COLUMNS} WHERE lifecycle IN (?1, ?2)"))?;
        let alive = [
            to_json(&AgentLifecycle::Starting)?,
            to_json(&AgentLifecycle::Running)?,
        ];
        let rows = statement.query_map(params![alive[0], alive[1]], agent_from_row)?;
        let stored: Vec<StoredAgent> = rows.collect::<rusqlite::Result<_>>()?;

        for mut agent in stored {
            agent.info.terminal = None;
            agent.info.lifecycle = if agent.session_id.is_some() && agent.session_confirmed {
                AgentLifecycle::Resumable
            } else {
                AgentLifecycle::Stopped
            };
            if matches!(
                agent.info.status,
                AgentStatus::Working | AgentStatus::Blocked(_)
            ) {
                agent.info.status = AgentStatus::Unknown;
                agent.info.message = None;
            }
            agent.info.revision += 1;

            self.save_agent(&agent.info)?;
        }

        Ok(())
    }

    pub fn set_setup_terminal(
        &self,
        task: TaskId,
        terminal: Option<TerminalId>,
    ) -> Result<(), Error> {
        let terminal = terminal.map(|terminal| to_row(terminal.0)).transpose()?;

        self.connection.execute(
            "UPDATE tasks SET setup_terminal = ?1 WHERE id = ?2",
            params![terminal, to_row(task.0)?],
        )?;

        Ok(())
    }

    /// Records how the setup script ended and clears its terminal.
    pub fn finish_setup(&self, task: TaskId, exit: Option<&ExitStatus>) -> Result<(), Error> {
        let exit = exit.map(serde_json::to_string).transpose()?;

        self.connection.execute(
            "UPDATE tasks SET setup_terminal = NULL, setup_exit = ?1 WHERE id = ?2",
            params![exit, to_row(task.0)?],
        )?;

        Ok(())
    }

    /// The ports of every task in every project.
    pub fn all_task_ports(&self) -> Result<Vec<u16>, Error> {
        let mut statement = self.connection.prepare("SELECT ports FROM tasks")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;

        let mut ports = Vec::new();
        for row in rows {
            ports.extend(row?.split(',').filter_map(|port| port.parse::<u16>().ok()));
        }

        Ok(ports)
    }

    /// Terminals never outlive a daemon run, so no setup script can still be running
    /// when the daemon starts.
    pub fn clear_setup_terminals(&self) -> Result<(), Error> {
        self.connection
            .execute("UPDATE tasks SET setup_terminal = NULL", [])?;

        Ok(())
    }
}

const AGENT_COLUMNS: &str = "SELECT id, task, role, kind, model, session_id, terminal, status,
    message, lifecycle, revision, session_confirmed FROM agents";

fn agent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredAgent> {
    let id: i64 = row.get(0)?;
    let task: i64 = row.get(1)?;
    let terminal: Option<i64> = row.get(6)?;
    let revision: i64 = row.get(10)?;

    Ok(StoredAgent {
        info: AgentInfo {
            id: AgentId(id.unsigned_abs()),
            task: TaskId(task.unsigned_abs()),
            role: json_column(row, 2)?,
            kind: json_column(row, 3)?,
            model: row.get(4)?,
            terminal: terminal.map(|terminal| TerminalId(terminal.unsigned_abs())),
            status: json_column(row, 7)?,
            message: row.get(8)?,
            lifecycle: json_column(row, 9)?,
            revision: revision.unsigned_abs(),
        },
        session_id: row.get(5)?,
        session_confirmed: row.get(11)?,
    })
}

fn json_column<T: DeserializeOwned>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;

    serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn to_json<T: Serialize>(value: &T) -> Result<String, Error> {
    Ok(serde_json::to_string(value)?)
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredProject> {
    let id: i64 = row.get(0)?;
    let path: String = row.get(1)?;

    Ok(StoredProject {
        id: ProjectId(id.unsigned_abs()),
        path: PathBuf::from(path),
        name: row.get(2)?,
    })
}

fn to_row(id: u64) -> Result<i64, Error> {
    i64::try_from(id).map_err(|_| Error::Invalid(format!("id {id} is too large")))
}

fn from_row(id: i64) -> Result<u64, Error> {
    u64::try_from(id).map_err(|_| Error::Invalid(format!("id {id} is negative")))
}

fn kind_name(kind: TerminalKind) -> &'static str {
    match kind {
        TerminalKind::User => "user",
        TerminalKind::Process => "process",
        TerminalKind::Agent => "agent",
    }
}

fn command_label(json: &str) -> String {
    match serde_json::from_str::<Option<Vec<String>>>(json) {
        Ok(Some(command)) => command.join(" "),
        _ => "login shell".to_owned(),
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use swarm_term::{Palette, Rgb, Size};

    fn request(command: Option<Vec<String>>) -> CreateTerminal {
        CreateTerminal {
            kind: TerminalKind::Agent,
            command,
            cwd: None,
            size: Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            palette: Palette {
                fg: Rgb(0, 0, 0),
                bg: Rgb(0, 0, 0),
                cursor: Rgb(0, 0, 0),
                ansi: [Rgb(0, 0, 0); 16],
            },
            task: Some("task-1".to_owned()),
        }
    }

    fn new_task(project: ProjectId, ports: &[u16]) -> NewTask<'_> {
        NewTask {
            project,
            name: "Fix it",
            branch: "swarm/fix-it",
            base: "main",
            worktree: Path::new("/repo/.worktrees/fix-it"),
            ports,
        }
    }

    #[test]
    fn migrations_set_the_schema_version() {
        let store = Store::open_in_memory().expect("store");

        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");

        assert_eq!(usize::try_from(version).expect("version"), MIGRATIONS.len());
    }

    #[test]
    fn migrations_run_once_on_a_second_open() {
        let store = Store::open_in_memory().expect("store");

        let store = Store::migrated(store.connection).expect("second open");

        assert_eq!(store.leftovers().expect("leftovers"), Vec::new());
    }

    #[test]
    fn a_database_from_the_first_schema_keeps_its_data() {
        let connection = Connection::open_in_memory().expect("connection");
        connection.execute_batch(MIGRATIONS[0]).expect("old schema");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("version");
        connection
            .execute(
                "INSERT INTO terminals (kind, task, command, status, created_at)
                 VALUES ('agent', 'task-1', 'null', 'running', 5)",
                [],
            )
            .expect("old row");

        let store = Store::migrated(connection).expect("migrated");

        let leftovers = store.leftovers().expect("leftovers");
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].id, TerminalId(1));
        assert_eq!(store.list_projects().expect("projects"), Vec::new());
    }

    #[test]
    fn a_database_from_the_second_schema_keeps_its_tasks() {
        let connection = Connection::open_in_memory().expect("connection");
        connection.execute_batch(MIGRATIONS[0]).expect("schema 1");
        connection.execute_batch(MIGRATIONS[1]).expect("schema 2");
        connection
            .pragma_update(None, "user_version", 2)
            .expect("version");
        connection
            .execute(
                "INSERT INTO projects (path, name, added_at) VALUES ('/repo', 'repo', 1)",
                [],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO tasks (project, name, branch, base, worktree, ports, created_at)
                 VALUES (1, 'old', 'swarm/old', 'main', '/repo/.worktrees/old', '5170', 1)",
                [],
            )
            .expect("task");

        let store = Store::migrated(connection).expect("migrated");

        let tasks = store.list_tasks(ProjectId(1)).expect("tasks");
        assert_eq!(tasks[0].name, "old");
        assert_eq!(tasks[0].setup_exit, None);
    }

    #[test]
    fn ids_come_from_the_row_id() {
        let store = Store::open_in_memory().expect("store");

        let first = store.insert_starting(&request(None)).expect("first");
        let second = store.insert_starting(&request(None)).expect("second");

        assert_eq!((first, second), (TerminalId(1), TerminalId(2)));
    }

    #[test]
    fn a_new_row_is_starting() {
        let store = Store::open_in_memory().expect("store");

        let id = store.insert_starting(&request(None)).expect("insert");

        assert_eq!(store.status(id).expect("status"), Status::Starting);
    }

    #[test]
    fn running_keeps_the_pid_and_start_time() {
        let store = Store::open_in_memory().expect("store");
        let id = store.insert_starting(&request(None)).expect("insert");

        store.mark_running(id, Some(42), Some(7)).expect("running");

        let leftovers = store.leftovers().expect("leftovers");
        assert_eq!(leftovers[0].pid, Some(42));
        assert_eq!(leftovers[0].started_us, Some(7));
    }

    #[test]
    fn exited_and_stopped_rows_are_not_leftovers() {
        let store = Store::open_in_memory().expect("store");
        let exited = store.insert_starting(&request(None)).expect("insert");
        let stopped = store.insert_starting(&request(None)).expect("insert");
        let running = store.insert_starting(&request(None)).expect("insert");

        store
            .mark_exited(exited, Some(&ExitStatus::Code(0)))
            .expect("exited");
        store.mark_stopped(stopped).expect("stopped");

        let ids: Vec<_> = store
            .leftovers()
            .expect("leftovers")
            .into_iter()
            .map(|leftover| leftover.id)
            .collect();
        assert_eq!(ids, [running]);
        assert_eq!(store.status(exited).expect("status"), Status::Exited);
        assert_eq!(store.status(stopped).expect("status"), Status::Stopped);
    }

    #[test]
    fn the_command_is_shown_as_a_label() {
        let store = Store::open_in_memory().expect("store");
        store
            .insert_starting(&request(Some(vec!["/bin/sh".into(), "-c".into()])))
            .expect("insert");

        let leftovers = store.leftovers().expect("leftovers");

        assert_eq!(leftovers[0].command, "/bin/sh -c");
    }

    #[test]
    fn adding_a_project_twice_returns_the_first() {
        let store = Store::open_in_memory().expect("store");

        let first = store
            .add_project(Path::new("/repo"), "repo")
            .expect("first");
        let second = store
            .add_project(Path::new("/repo"), "other")
            .expect("second");

        assert_eq!(first, second);
        assert_eq!(store.list_projects().expect("list").len(), 1);
    }

    #[test]
    fn a_task_keeps_its_ports_and_setup_terminal() {
        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(Path::new("/repo"), "repo")
            .expect("project");

        let task = store
            .add_task(&new_task(project.id, &[5170, 5171]))
            .expect("task");
        store
            .set_setup_terminal(task.id, Some(TerminalId(9)))
            .expect("setup terminal");

        let tasks = store.list_tasks(project.id).expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].ports, [5170, 5171]);
        assert_eq!(tasks[0].setup_terminal, Some(TerminalId(9)));
    }

    #[test]
    fn finishing_the_setup_keeps_the_exit_and_clears_the_terminal() {
        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(Path::new("/repo"), "repo")
            .expect("project");
        let task = store.add_task(&new_task(project.id, &[])).expect("task");
        store
            .set_setup_terminal(task.id, Some(TerminalId(9)))
            .expect("setup terminal");

        store
            .finish_setup(task.id, Some(&ExitStatus::Code(3)))
            .expect("finish");

        let tasks = store.list_tasks(project.id).expect("tasks");
        assert_eq!(tasks[0].setup_terminal, None);
        assert_eq!(tasks[0].setup_exit, Some(ExitStatus::Code(3)));
    }

    #[test]
    fn ports_are_listed_across_projects() {
        let store = Store::open_in_memory().expect("store");
        let first = store.add_project(Path::new("/a"), "a").expect("project");
        let second = store.add_project(Path::new("/b"), "b").expect("project");
        store
            .add_task(&new_task(first.id, &[5170, 5171]))
            .expect("task");
        store.add_task(&new_task(second.id, &[5172])).expect("task");

        let mut ports = store.all_task_ports().expect("ports");
        ports.sort_unstable();

        assert_eq!(ports, [5170, 5171, 5172]);
    }

    #[test]
    fn clearing_removes_every_setup_terminal() {
        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(Path::new("/repo"), "repo")
            .expect("project");
        let task = store.add_task(&new_task(project.id, &[])).expect("task");
        store
            .set_setup_terminal(task.id, Some(TerminalId(9)))
            .expect("setup terminal");

        store.clear_setup_terminals().expect("clear");

        let tasks = store.list_tasks(project.id).expect("tasks");
        assert_eq!(tasks[0].setup_terminal, None);
        assert!(tasks[0].ports.is_empty());
    }

    fn new_agent(task: TaskId) -> NewAgent<'static> {
        NewAgent {
            task,
            role: AgentRole::Orchestrator,
            kind: AgentKind::ClaudeCode,
            model: "opus",
            session_id: Some("session-1"),
        }
    }

    fn store_with_task() -> (Store, TaskId) {
        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(Path::new("/repo"), "repo")
            .expect("project");
        let task = store.add_task(&new_task(project.id, &[])).expect("task");

        (store, task.id)
    }

    #[test]
    fn a_new_agent_starts_unknown_and_starting() {
        let (store, task) = store_with_task();

        let agent = store.add_agent(&new_agent(task)).expect("agent");

        assert_eq!(agent.status, AgentStatus::Unknown);
        assert_eq!(agent.lifecycle, AgentLifecycle::Starting);
        assert_eq!(agent.terminal, None);
    }

    #[test]
    fn tasks_list_their_agents() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");

        let tasks = store.list_tasks(ProjectId(1)).expect("tasks");

        assert_eq!(tasks[0].agents, [agent]);
    }

    #[test]
    fn a_saved_agent_keeps_status_message_terminal_and_lifecycle() {
        let (store, task) = store_with_task();
        let mut agent = store.add_agent(&new_agent(task)).expect("agent");
        agent.terminal = Some(TerminalId(4));
        agent.status = AgentStatus::Blocked(swarm_term::BlockedKind::Question);
        agent.message = Some("Red or blue?".to_owned());
        agent.lifecycle = AgentLifecycle::Running;

        store.save_agent(&agent).expect("save");

        assert_eq!(
            store.agent(agent.id).expect("agent").expect("row").info,
            agent
        );
    }

    #[test]
    fn the_session_id_is_stored_apart_from_the_info() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");

        store
            .set_agent_session(agent.id, "session-2")
            .expect("session");

        let stored = store.agent(agent.id).expect("agent").expect("row");
        assert_eq!(stored.session_id.as_deref(), Some("session-2"));
    }

    fn running_agent(store: &Store, new: &NewAgent<'_>) -> AgentInfo {
        let mut agent = store.add_agent(new).expect("agent");
        agent.lifecycle = AgentLifecycle::Running;
        agent.terminal = Some(TerminalId(2));
        agent.status = AgentStatus::Working;
        agent.message = Some("Writing".to_owned());
        store.save_agent(&agent).expect("save");

        agent
    }

    #[test]
    fn recovery_makes_running_agents_resumable_when_their_session_is_confirmed() {
        let (store, task) = store_with_task();
        let agent = running_agent(&store, &new_agent(task));
        store.confirm_agent_session(agent.id).expect("confirm");

        store.recover_agents().expect("recover");

        let recovered = store.agent(agent.id).expect("agent").expect("row").info;
        assert_eq!(recovered.lifecycle, AgentLifecycle::Resumable);
        assert_eq!(recovered.status, AgentStatus::Unknown);
        assert_eq!(recovered.message, None);
        assert_eq!(recovered.terminal, None);
    }

    #[test]
    fn recovery_stops_agents_with_an_unconfirmed_session() {
        let (store, task) = store_with_task();
        let agent = running_agent(&store, &new_agent(task));

        store.recover_agents().expect("recover");

        let recovered = store.agent(agent.id).expect("agent").expect("row").info;
        assert_eq!(recovered.lifecycle, AgentLifecycle::Stopped);
    }

    #[test]
    fn recovery_stops_agents_without_a_session() {
        let (store, task) = store_with_task();
        let mut new = new_agent(task);
        new.session_id = None;
        let agent = store.add_agent(&new).expect("agent");

        store.recover_agents().expect("recover");

        let recovered = store.agent(agent.id).expect("agent").expect("row").info;
        assert_eq!(recovered.lifecycle, AgentLifecycle::Stopped);
    }

    #[test]
    fn the_revision_is_stored_and_recovery_raises_it() {
        let (store, task) = store_with_task();
        let mut agent = running_agent(&store, &new_agent(task));
        assert_eq!(agent.revision, 0);
        agent.revision = 3;
        store.save_agent(&agent).expect("save");

        store.recover_agents().expect("recover");

        let recovered = store.agent(agent.id).expect("agent").expect("row").info;
        assert_eq!(recovered.revision, 4);
        assert_eq!(store.list_agents(task).expect("list")[0].revision, 4);
    }

    #[test]
    fn recovery_leaves_finished_agents_alone() {
        let (store, task) = store_with_task();
        let mut agent = store.add_agent(&new_agent(task)).expect("agent");
        agent.lifecycle = AgentLifecycle::Resumable;
        agent.status = AgentStatus::Done;
        store.save_agent(&agent).expect("save");

        store.recover_agents().expect("recover");

        assert_eq!(
            store.agent(agent.id).expect("agent").expect("row").info,
            agent
        );
    }

    #[test]
    fn a_database_from_the_third_schema_gets_the_agents_table() {
        let connection = Connection::open_in_memory().expect("connection");
        for migration in &MIGRATIONS[..3] {
            connection.execute_batch(migration).expect("old schema");
        }
        connection
            .pragma_update(None, "user_version", 3)
            .expect("version");
        connection
            .execute(
                "INSERT INTO projects (path, name, added_at) VALUES ('/repo', 'repo', 1)",
                [],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO tasks (project, name, branch, base, worktree, ports, created_at)
                 VALUES (1, 'old', 'swarm/old', 'main', '/repo/.worktrees/old', '', 1)",
                [],
            )
            .expect("task");

        let store = Store::migrated(connection).expect("migrated");

        let tasks = store.list_tasks(ProjectId(1)).expect("tasks");
        assert_eq!(tasks[0].name, "old");
        assert_eq!(tasks[0].agents, Vec::new());
    }
}
