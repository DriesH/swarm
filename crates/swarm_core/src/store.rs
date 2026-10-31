//! The daemon's SQLite database (ADR 0005). Only the daemon opens it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use swarm_protocol::agent::InboxMessage;
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CreateTerminal,
    GridSize, MessageId, MessageInfo, MessageKind, ProjectId, TaskId, TaskInfo, TerminalId,
    TerminalKind,
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
    "ALTER TABLE tasks ADD COLUMN briefed INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE tasks ADD COLUMN created_by INTEGER",
    // SQLite cannot make a column nullable in place, so the table is rebuilt.
    "CREATE TABLE agents_rebuilt (
        id INTEGER PRIMARY KEY,
        project INTEGER NOT NULL REFERENCES projects(id),
        task INTEGER REFERENCES tasks(id),
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
        created_at INTEGER NOT NULL,
        name TEXT NOT NULL,
        worktree TEXT NOT NULL,
        branch TEXT NOT NULL,
        ports TEXT NOT NULL DEFAULT ''
    );
    INSERT INTO agents_rebuilt
        (id, project, task, role, kind, model, session_id, terminal, status, message,
         lifecycle, revision, session_confirmed, created_at, name, worktree, branch)
    SELECT agents.id, tasks.project, agents.task, agents.role, agents.kind, agents.model,
        agents.session_id, agents.terminal, agents.status, agents.message, agents.lifecycle,
        agents.revision, agents.session_confirmed, agents.created_at, 'orchestrator',
        tasks.worktree, tasks.branch
    FROM agents JOIN tasks ON tasks.id = agents.task;
    DROP TABLE agents;
    ALTER TABLE agents_rebuilt RENAME TO agents",
    "CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        project INTEGER NOT NULL REFERENCES projects(id),
        task INTEGER REFERENCES tasks(id),
        from_agent INTEGER,
        to_agent INTEGER NOT NULL,
        kind TEXT NOT NULL,
        text TEXT NOT NULL,
        sent_at INTEGER NOT NULL,
        delivered_at INTEGER
    );
    CREATE INDEX messages_unread ON messages (to_agent) WHERE delivered_at IS NULL",
    "ALTER TABLE terminals ADD COLUMN owner INTEGER;
    ALTER TABLE terminals ADD COLUMN name TEXT;
    ALTER TABLE terminals ADD COLUMN fixed_cols INTEGER;
    ALTER TABLE terminals ADD COLUMN fixed_rows INTEGER",
    "ALTER TABLE agents ADD COLUMN ended_at INTEGER;
    ALTER TABLE agents ADD COLUMN base_commit TEXT;
    UPDATE agents SET created_at = created_at * 1000",
    "ALTER TABLE agents ADD COLUMN started_at INTEGER",
    "ALTER TABLE tasks ADD COLUMN approved INTEGER;
    ALTER TABLE tasks ADD COLUMN closed_at INTEGER",
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

/// What an agent gave a process terminal it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeta {
    pub owner: AgentId,
    pub name: Option<String>,
    pub size: GridSize,
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
    pub project: ProjectId,
    /// `None` for a Start orchestrator.
    pub task: Option<TaskId>,
    pub role: AgentRole,
    pub kind: AgentKind,
    pub model: &'a str,
    pub session_id: Option<&'a str>,
    pub name: &'a str,
    pub worktree: &'a Path,
    pub branch: &'a str,
    /// The ports a worker reserved; empty for the others.
    pub ports: &'a [u16],
}

/// A message to record.
#[derive(Debug, Clone, Copy)]
pub struct NewMessage<'a> {
    pub project: ProjectId,
    pub task: Option<TaskId>,
    /// `None` when Swarm sends it.
    pub from: Option<AgentId>,
    pub to: AgentId,
    pub kind: MessageKind,
    pub text: &'a str,
    /// The recipient got it another way, for example as its first message.
    pub delivered: bool,
}

/// Messages that just counted as delivered, with the time they did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub ids: Vec<MessageId>,
    /// Milliseconds since the Unix epoch.
    pub delivered_at: u64,
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

    /// Records who started a process terminal, what they called it and its fixed size.
    pub fn set_process(&self, id: TerminalId, meta: &ProcessMeta) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE terminals SET owner = ?1, name = ?2, fixed_cols = ?3, fixed_rows = ?4
             WHERE id = ?5",
            params![
                to_row(meta.owner.0)?,
                meta.name,
                meta.size.cols,
                meta.size.rows,
                to_row(id.0)?
            ],
        )?;

        Ok(())
    }

    pub fn set_process_size(&self, id: TerminalId, size: GridSize) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE terminals SET fixed_cols = ?1, fixed_rows = ?2 WHERE id = ?3",
            params![size.cols, size.rows, to_row(id.0)?],
        )?;

        Ok(())
    }

    /// `None` for a terminal no agent started.
    pub fn process(&self, id: TerminalId) -> Result<Option<ProcessMeta>, Error> {
        let row = self
            .connection
            .query_row(
                "SELECT owner, name, fixed_cols, fixed_rows FROM terminals WHERE id = ?1",
                params![to_row(id.0)?],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<u16>>(2)?,
                        row.get::<_, Option<u16>>(3)?,
                    ))
                },
            )
            .optional()?;

        Ok(match row {
            Some((Some(owner), name, Some(cols), Some(rows))) => Some(ProcessMeta {
                owner: AgentId(owner.unsigned_abs()),
                name,
                size: GridSize { cols, rows },
            }),
            _ => None,
        })
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
        self.add_task_by(task, None)
    }

    /// Records a task made by `created_by`, the Start orchestrator that asked for it.
    pub fn add_task_by(
        &self,
        task: &NewTask<'_>,
        created_by: Option<AgentId>,
    ) -> Result<TaskInfo, Error> {
        let ports = join_ports(task.ports);
        let creator = created_by.map(|agent| to_row(agent.0)).transpose()?;

        self.connection.execute(
            "INSERT INTO tasks (project, name, branch, base, worktree, ports, created_at,
                 created_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                to_row(task.project.0)?,
                task.name,
                task.branch,
                task.base,
                task.worktree.to_string_lossy(),
                ports,
                now_seconds(),
                creator
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
            created_by,
            briefed: false,
            approved: None,
            closed: false,
        })
    }

    /// The open tasks of the project; closed ones are never listed.
    pub fn list_tasks(&self, project: ProjectId) -> Result<Vec<TaskInfo>, Error> {
        let mut statement = self.connection.prepare(&format!(
            "{TASK_COLUMNS} WHERE project = ?1 AND closed_at IS NULL ORDER BY id"
        ))?;
        let rows = statement.query_map(params![to_row(project.0)?], task_from_row)?;

        let mut tasks: Vec<TaskInfo> = rows.collect::<rusqlite::Result<_>>()?;
        for task in &mut tasks {
            task.agents = self.list_agents(task.id)?;
        }

        Ok(tasks)
    }

    /// An open task. A closed task looks like one that never existed.
    pub fn task(&self, id: TaskId) -> Result<Option<TaskInfo>, Error> {
        Ok(self.task_including_closed(id)?.filter(|task| !task.closed))
    }

    pub fn task_including_closed(&self, id: TaskId) -> Result<Option<TaskInfo>, Error> {
        let task = self
            .connection
            .query_row(
                &format!("{TASK_COLUMNS} WHERE id = ?1"),
                params![to_row(id.0)?],
                task_from_row,
            )
            .optional()?;

        task.map(|mut task| {
            task.agents = self.list_agents(task.id)?;

            Ok(task)
        })
        .transpose()
    }

    /// Records that the task's orchestrator understood its brief.
    pub fn confirm_task(&self, id: TaskId) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE tasks SET briefed = 1 WHERE id = ?1",
            params![to_row(id.0)?],
        )?;

        Ok(())
    }

    /// Records the fingerprint of the Merged diff the user approved.
    pub fn set_approved(&self, task: TaskId, fingerprint: u64) -> Result<(), Error> {
        let stored = i64::from_ne_bytes(fingerprint.to_ne_bytes());

        self.connection.execute(
            "UPDATE tasks SET approved = ?1 WHERE id = ?2",
            params![stored, to_row(task.0)?],
        )?;

        Ok(())
    }

    /// Closes the task and ends every agent of it that is not `Stopped` yet. Returns
    /// the agents it changed.
    pub fn close_task(&self, task: TaskId) -> Result<Vec<AgentInfo>, Error> {
        self.connection.execute(
            "UPDATE tasks SET closed_at = ?1 WHERE id = ?2 AND closed_at IS NULL",
            params![now_millis(), to_row(task.0)?],
        )?;

        let mut changed = Vec::new();
        for mut agent in self.list_agents(task)? {
            if agent.lifecycle == AgentLifecycle::Stopped {
                continue;
            }

            agent.terminal = None;
            agent.lifecycle = AgentLifecycle::Stopped;
            agent.revision += 1;
            agent.ended_at.get_or_insert(from_row(now_millis())?);

            self.save_agent(&agent)?;
            changed.push(agent);
        }

        Ok(changed)
    }

    /// Records the Start orchestrator that made the task.
    pub fn set_task_created_by(&self, task: TaskId, agent: AgentId) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE tasks SET created_by = ?1 WHERE id = ?2",
            params![to_row(agent.0)?, to_row(task.0)?],
        )?;

        Ok(())
    }

    /// Removes a task that never started.
    pub fn delete_task(&self, id: TaskId) -> Result<(), Error> {
        self.connection
            .execute("DELETE FROM tasks WHERE id = ?1", params![to_row(id.0)?])?;

        Ok(())
    }

    /// Removes an agent that never started, with its messages.
    pub fn delete_agent(&self, id: AgentId) -> Result<(), Error> {
        let id = to_row(id.0)?;

        self.connection.execute(
            "DELETE FROM messages WHERE to_agent = ?1 OR from_agent = ?1",
            params![id],
        )?;
        self.connection
            .execute("DELETE FROM agents WHERE id = ?1", params![id])?;

        Ok(())
    }

    /// Written before the agent's process starts (ADR 0005).
    pub fn add_agent(&self, agent: &NewAgent<'_>) -> Result<AgentInfo, Error> {
        let status = AgentStatus::Unknown;
        let lifecycle = AgentLifecycle::Starting;

        let task = agent.task.map(|task| to_row(task.0)).transpose()?;
        let created_at = now_millis();

        self.connection.execute(
            "INSERT INTO agents (project, task, role, kind, model, session_id, status, lifecycle,
                 created_at, name, worktree, branch, ports)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                to_row(agent.project.0)?,
                task,
                to_json(&agent.role)?,
                to_json(&agent.kind)?,
                agent.model,
                agent.session_id,
                to_json(&status)?,
                to_json(&lifecycle)?,
                created_at,
                agent.name,
                agent.worktree.to_string_lossy(),
                agent.branch,
                join_ports(agent.ports),
            ],
        )?;

        Ok(AgentInfo {
            id: AgentId(from_row(self.connection.last_insert_rowid())?),
            project: agent.project,
            task: agent.task,
            role: agent.role,
            kind: agent.kind,
            model: agent.model.to_owned(),
            name: agent.name.to_owned(),
            worktree: agent.worktree.to_owned(),
            branch: agent.branch.to_owned(),
            terminal: None,
            status,
            message: None,
            lifecycle,
            revision: 0,
            created_at: from_row(created_at)?,
            started_at: None,
            ended_at: None,
        })
    }

    /// The commit a worker branched from, to measure its changes against.
    pub fn base_commit(&self, id: AgentId) -> Result<Option<String>, Error> {
        let commit: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT base_commit FROM agents WHERE id = ?1",
                params![to_row(id.0)?],
                |row| row.get(0),
            )
            .optional()?;

        Ok(commit.flatten())
    }

    pub fn set_base_commit(&self, id: AgentId, commit: &str) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE agents SET base_commit = ?1 WHERE id = ?2",
            params![commit, to_row(id.0)?],
        )?;

        Ok(())
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

    /// The project's Start orchestrator while its process starts or runs.
    pub fn start_agent(&self, project: ProjectId) -> Result<Option<AgentInfo>, Error> {
        let alive = [
            to_json(&AgentLifecycle::Starting)?,
            to_json(&AgentLifecycle::Running)?,
        ];

        Ok(self
            .connection
            .query_row(
                &format!(
                    "{AGENT_COLUMNS} WHERE project = ?1 AND role = ?2 AND lifecycle IN (?3, ?4)
                     ORDER BY id DESC LIMIT 1"
                ),
                params![
                    to_row(project.0)?,
                    to_json(&AgentRole::StartOrchestrator)?,
                    alive[0],
                    alive[1]
                ],
                agent_from_row,
            )
            .optional()?
            .map(|stored| stored.info))
    }

    /// The ports the agent reserved; empty for an agent that reserved none.
    pub fn agent_ports(&self, id: AgentId) -> Result<Vec<u16>, Error> {
        let ports: Option<String> = self
            .connection
            .query_row(
                "SELECT ports FROM agents WHERE id = ?1",
                params![to_row(id.0)?],
                |row| row.get(0),
            )
            .optional()?;

        Ok(ports
            .iter()
            .flat_map(|ports| ports.split(','))
            .filter_map(|port| port.parse().ok())
            .collect())
    }

    pub fn list_agents(&self, task: TaskId) -> Result<Vec<AgentInfo>, Error> {
        let mut statement = self
            .connection
            .prepare(&format!("{AGENT_COLUMNS} WHERE task = ?1 ORDER BY id"))?;
        let rows = statement.query_map(params![to_row(task.0)?], agent_from_row)?;

        rows.map(|row| row.map(|stored| stored.info).map_err(Error::from))
            .collect()
    }

    /// Saves what changes while an agent runs: terminal, status, message, lifecycle,
    /// end time and revision. The caller raises the revision first.
    pub fn save_agent(&self, agent: &AgentInfo) -> Result<(), Error> {
        let terminal = agent
            .terminal
            .map(|terminal| to_row(terminal.0))
            .transpose()?;

        self.connection.execute(
            "UPDATE agents SET terminal = ?1, status = ?2, message = ?3, lifecycle = ?4,
                 revision = ?5, ended_at = ?6, started_at = ?7
             WHERE id = ?8",
            params![
                terminal,
                to_json(&agent.status)?,
                agent.message,
                to_json(&agent.lifecycle)?,
                to_row(agent.revision)?,
                agent.ended_at.map(to_row).transpose()?,
                agent.started_at.map(to_row).transpose()?,
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
            let task_closed = match agent.info.task {
                Some(task) => self.task(task)?.is_none(),
                None => false,
            };

            agent.info.terminal = None;
            agent.info.lifecycle =
                if !task_closed && agent.session_id.is_some() && agent.session_confirmed {
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
            agent.info.ended_at = Some(from_row(now_millis())?);

            self.save_agent(&agent.info)?;
        }

        Ok(())
    }

    /// Records a message. One that `delivered` already got another way is stored as read.
    pub fn add_message(&self, message: &NewMessage<'_>) -> Result<MessageInfo, Error> {
        let sent_at = now_millis();
        let task = message.task.map(|task| to_row(task.0)).transpose()?;
        let from = message.from.map(|agent| to_row(agent.0)).transpose()?;
        let delivered_at = message.delivered.then_some(sent_at);

        self.connection.execute(
            "INSERT INTO messages (project, task, from_agent, to_agent, kind, text, sent_at,
                 delivered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                to_row(message.project.0)?,
                task,
                from,
                to_row(message.to.0)?,
                to_json(&message.kind)?,
                message.text,
                sent_at,
                delivered_at,
            ],
        )?;

        Ok(MessageInfo {
            id: MessageId(from_row(self.connection.last_insert_rowid())?),
            project: message.project,
            task: message.task,
            from: message.from,
            to: message.to,
            kind: message.kind,
            text: message.text.to_owned(),
            sent_at: from_row(sent_at)?,
            delivered_at: delivered_at.map(from_row).transpose()?,
        })
    }

    /// The newest `limit` messages of a task, or of the project's Start orchestrator
    /// when `task` is `None`. Oldest first.
    pub fn list_messages(
        &self,
        project: ProjectId,
        task: Option<TaskId>,
        limit: usize,
    ) -> Result<Vec<MessageInfo>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, project, task, from_agent, to_agent, kind, text, sent_at, delivered_at
             FROM messages WHERE project = ?1 AND task IS ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let task = task.map(|task| to_row(task.0)).transpose()?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![to_row(project.0)?, task, limit], |row| {
            let id: i64 = row.get(0)?;
            let project: i64 = row.get(1)?;
            let task: Option<i64> = row.get(2)?;
            let from: Option<i64> = row.get(3)?;
            let to: i64 = row.get(4)?;
            let sent_at: i64 = row.get(7)?;
            let delivered_at: Option<i64> = row.get(8)?;

            Ok(MessageInfo {
                id: MessageId(id.unsigned_abs()),
                project: ProjectId(project.unsigned_abs()),
                task: task.map(|task| TaskId(task.unsigned_abs())),
                from: from.map(|agent| AgentId(agent.unsigned_abs())),
                to: AgentId(to.unsigned_abs()),
                kind: json_column(row, 5)?,
                text: row.get(6)?,
                sent_at: sent_at.unsigned_abs(),
                delivered_at: delivered_at.map(i64::unsigned_abs),
            })
        })?;

        let mut messages: Vec<MessageInfo> = rows.collect::<rusqlite::Result<_>>()?;
        messages.reverse();

        Ok(messages)
    }

    pub fn has_unread(&self, agent: AgentId) -> Result<bool, Error> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE to_agent = ?1 AND delivered_at IS NULL)",
            params![to_row(agent.0)?],
            |row| row.get(0),
        )?)
    }

    /// The agent's unread messages, oldest first. Nothing is marked.
    pub fn unread(&self, agent: AgentId) -> Result<Vec<InboxMessage>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT messages.id, messages.from_agent, sender.name, messages.kind, messages.text,
                 messages.sent_at
             FROM messages LEFT JOIN agents AS sender ON sender.id = messages.from_agent
             WHERE messages.to_agent = ?1 AND messages.delivered_at IS NULL
             ORDER BY messages.id",
        )?;
        let rows = statement.query_map(params![to_row(agent.0)?], |row| {
            let id: i64 = row.get(0)?;
            let from: Option<i64> = row.get(1)?;
            let from_name: Option<String> = row.get(2)?;
            let sent_at: i64 = row.get(5)?;

            Ok(InboxMessage {
                id: MessageId(id.unsigned_abs()),
                from: from.map(|agent| AgentId(agent.unsigned_abs())),
                from_name: from_name.unwrap_or_else(|| "swarm".to_owned()),
                kind: json_column(row, 3)?,
                text: row.get(4)?,
                sent_at: sent_at.unsigned_abs(),
            })
        })?;

        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks messages as delivered, all with one time. Messages that already count as
    /// delivered stay as they are and are left out of the result.
    pub fn mark_delivered(&self, messages: &[MessageId]) -> Result<Delivery, Error> {
        let now = now_millis();
        let mut ids = Vec::new();

        for message in messages {
            let changed = self.connection.execute(
                "UPDATE messages SET delivered_at = ?1 WHERE id = ?2 AND delivered_at IS NULL",
                params![now, to_row(message.0)?],
            )?;
            if changed > 0 {
                ids.push(*message);
            }
        }

        Ok(Delivery {
            ids,
            delivered_at: from_row(now)?,
        })
    }

    /// Takes the agent's unread messages, oldest first, and marks them delivered.
    pub fn take_unread(&self, agent: AgentId) -> Result<(Vec<InboxMessage>, Delivery), Error> {
        let messages = self.unread(agent)?;
        let ids: Vec<MessageId> = messages.iter().map(|message| message.id).collect();
        let delivery = self.mark_delivered(&ids)?;

        Ok((messages, delivery))
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

    /// The ports of every task and every worker that has not stopped, in every project.
    pub fn all_task_ports(&self) -> Result<Vec<u16>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT ports FROM tasks WHERE closed_at IS NULL
             UNION ALL
             SELECT ports FROM agents WHERE lifecycle != ?1
                 AND (task IS NULL OR task NOT IN (SELECT id FROM tasks WHERE closed_at IS NOT NULL))",
        )?;
        let stopped = to_json(&AgentLifecycle::Stopped)?;
        let rows = statement.query_map(params![stopped], |row| row.get::<_, String>(0))?;

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

const AGENT_COLUMNS: &str = "SELECT id, project, task, role, kind, model, session_id, terminal,
    status, message, lifecycle, revision, session_confirmed, name, worktree, branch,
    created_at, ended_at, started_at FROM agents";

fn agent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredAgent> {
    let id: i64 = row.get(0)?;
    let project: i64 = row.get(1)?;
    let task: Option<i64> = row.get(2)?;
    let terminal: Option<i64> = row.get(7)?;
    let revision: i64 = row.get(11)?;
    let worktree: String = row.get(14)?;
    let created_at: i64 = row.get(16)?;
    let ended_at: Option<i64> = row.get(17)?;
    let started_at: Option<i64> = row.get(18)?;

    Ok(StoredAgent {
        info: AgentInfo {
            id: AgentId(id.unsigned_abs()),
            project: ProjectId(project.unsigned_abs()),
            task: task.map(|task| TaskId(task.unsigned_abs())),
            role: json_column(row, 3)?,
            kind: json_column(row, 4)?,
            model: row.get(5)?,
            name: row.get(13)?,
            worktree: PathBuf::from(worktree),
            branch: row.get(15)?,
            terminal: terminal.map(|terminal| TerminalId(terminal.unsigned_abs())),
            status: json_column(row, 8)?,
            message: row.get(9)?,
            lifecycle: json_column(row, 10)?,
            revision: revision.unsigned_abs(),
            created_at: created_at.unsigned_abs(),
            started_at: started_at.map(i64::unsigned_abs),
            ended_at: ended_at.map(i64::unsigned_abs),
        },
        session_id: row.get(6)?,
        session_confirmed: row.get(12)?,
    })
}

const TASK_COLUMNS: &str = "SELECT id, project, name, branch, base, worktree, ports,
    setup_terminal, setup_exit, created_by, briefed, approved, closed_at FROM tasks";

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskInfo> {
    let id: i64 = row.get(0)?;
    let project: i64 = row.get(1)?;
    let worktree: String = row.get(5)?;
    let ports: String = row.get(6)?;
    let setup_terminal: Option<i64> = row.get(7)?;
    let setup_exit: Option<String> = row.get(8)?;
    let created_by: Option<i64> = row.get(9)?;
    let approved: Option<i64> = row.get(11)?;
    let closed_at: Option<i64> = row.get(12)?;

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
        created_by: created_by.map(|id| AgentId(id.unsigned_abs())),
        briefed: row.get(10)?,
        approved: approved.map(|stored| u64::from_ne_bytes(stored.to_ne_bytes())),
        closed: closed_at.is_some(),
    })
}

fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
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

/// Milliseconds since the Unix epoch.
pub fn unix_millis() -> u64 {
    u64::try_from(now_millis()).unwrap_or(0)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
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
            project: ProjectId(1),
            task: Some(task),
            role: AgentRole::Orchestrator,
            kind: AgentKind::ClaudeCode,
            model: "opus",
            session_id: Some("session-1"),
            name: "orchestrator",
            worktree: Path::new("/repo/.worktrees/fix-it"),
            branch: "swarm/fix-it",
            ports: &[],
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
    #[test]
    fn a_database_from_the_current_schema_keeps_its_rows() {
        let connection = Connection::open_in_memory().expect("connection");
        for migration in &MIGRATIONS[..4] {
            connection.execute_batch(migration).expect("old schema");
        }
        connection
            .pragma_update(None, "user_version", 4)
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
        connection
            .execute(
                "INSERT INTO agents (task, role, kind, model, session_id, terminal, status,
                     message, lifecycle, revision, session_confirmed, created_at)
                 VALUES (1, '\"Orchestrator\"', '\"ClaudeCode\"', 'opus', 'abc', NULL,
                     '\"Idle\"', 'hi', '\"Resumable\"', 7, 1, 2)",
                [],
            )
            .expect("agent");

        let store = Store::migrated(connection).expect("migrated");

        let tasks = store.list_tasks(ProjectId(1)).expect("tasks");
        assert_eq!(tasks[0].name, "old");
        assert_eq!(tasks[0].ports, [5170]);
        assert!(!tasks[0].briefed);
        assert_eq!(tasks[0].created_by, None);
        let agent = &tasks[0].agents[0];
        assert_eq!(agent.id, AgentId(1));
        assert_eq!(agent.project, ProjectId(1));
        assert_eq!(agent.task, Some(TaskId(1)));
        assert_eq!(agent.name, "orchestrator");
        assert_eq!(agent.worktree, Path::new("/repo/.worktrees/old"));
        assert_eq!(agent.branch, "swarm/old");
        assert_eq!(agent.revision, 7);
        assert_eq!(agent.message.as_deref(), Some("hi"));
        let stored = store.agent(AgentId(1)).expect("agent").expect("row");
        assert_eq!(stored.session_id.as_deref(), Some("abc"));
        assert!(stored.session_confirmed);
        assert_eq!(store.all_task_ports().expect("ports"), [5170]);
    }

    #[test]
    fn an_agent_without_a_task_is_kept() {
        let (store, _) = store_with_task();
        let mut new = new_agent(TaskId(1));
        new.task = None;
        new.role = AgentRole::StartOrchestrator;
        new.name = "start";

        let agent = store.add_agent(&new).expect("agent");

        let stored = store.agent(agent.id).expect("agent").expect("row").info;
        assert_eq!(stored.task, None);
        assert_eq!(stored.name, "start");
    }

    #[test]
    fn worker_ports_count_as_used() {
        let (store, task) = store_with_task();
        let mut new = new_agent(task);
        new.ports = &[5180, 5181];
        store.add_agent(&new).expect("agent");

        assert_eq!(store.all_task_ports().expect("ports"), [5180, 5181]);
    }

    #[test]
    fn the_ports_of_a_stopped_worker_are_free_again() {
        let (store, task) = store_with_task();
        let mut new = new_agent(task);
        new.ports = &[5180, 5181];
        let mut worker = store.add_agent(&new).expect("agent");

        worker.lifecycle = AgentLifecycle::Stopped;
        store.save_agent(&worker).expect("save");

        assert!(store.all_task_ports().expect("ports").is_empty());

        worker.lifecycle = AgentLifecycle::Resumable;
        store.save_agent(&worker).expect("save");

        assert_eq!(store.all_task_ports().expect("ports"), [5180, 5181]);
    }

    #[test]
    fn a_database_from_the_sixth_schema_keeps_its_terminals() {
        let connection = Connection::open_in_memory().expect("connection");
        for migration in &MIGRATIONS[..6] {
            connection.execute_batch(migration).expect("old schema");
        }
        connection
            .pragma_update(None, "user_version", 6)
            .expect("version");
        connection
            .execute(
                "INSERT INTO terminals (kind, task, command, status, created_at)
                 VALUES ('process', 'task:1', '[\"sleep\",\"1\"]', 'exited', 5)",
                [],
            )
            .expect("old row");

        let store = Store::migrated(connection).expect("migrated");

        assert_eq!(store.status(TerminalId(1)).expect("status"), Status::Exited);
        assert_eq!(store.process(TerminalId(1)).expect("process"), None);
    }

    #[test]
    fn a_process_terminal_keeps_its_owner_name_and_size() {
        let store = Store::open_in_memory().expect("store");
        let id = store.insert_starting(&request(None)).expect("terminal");
        let meta = ProcessMeta {
            owner: AgentId(4),
            name: Some("dev server".to_owned()),
            size: GridSize {
                cols: 100,
                rows: 30,
            },
        };

        store.set_process(id, &meta).expect("set");
        assert_eq!(store.process(id).expect("get"), Some(meta));

        store
            .set_process_size(
                id,
                GridSize {
                    cols: 120,
                    rows: 40,
                },
            )
            .expect("resize");
        let resized = store.process(id).expect("get").expect("row");
        assert_eq!((resized.size.cols, resized.size.rows), (120, 40));
        assert_eq!(resized.owner, AgentId(4));
    }

    #[test]
    fn the_start_agent_is_the_one_that_starts_or_runs() {
        let (store, task) = store_with_task();
        let project = ProjectId(1);
        assert_eq!(store.start_agent(project).expect("none"), None);

        let mut new = new_agent(task);
        new.task = None;
        new.role = AgentRole::StartOrchestrator;
        new.name = "start";
        let mut first = store.add_agent(&new).expect("agent");
        assert_eq!(
            store
                .start_agent(project)
                .expect("starting")
                .map(|agent| agent.id),
            Some(first.id)
        );

        first.lifecycle = AgentLifecycle::Stopped;
        store.save_agent(&first).expect("save");
        assert_eq!(store.start_agent(project).expect("stopped"), None);

        let second = store.add_agent(&new).expect("second");
        assert_eq!(
            store
                .start_agent(project)
                .expect("running")
                .map(|agent| agent.id),
            Some(second.id)
        );
        assert_eq!(
            store.start_agent(ProjectId(2)).expect("other project"),
            None
        );
    }

    #[test]
    fn a_deleted_agent_takes_its_messages_along() {
        let (store, task) = store_with_task();
        let worker = store.add_agent(&new_agent(task)).expect("agent");
        store
            .add_message(&message(None, worker.id, "brief"))
            .expect("message");

        store.delete_agent(worker.id).expect("delete");

        assert!(store.agent(worker.id).expect("read").is_none());
        assert!(!store.has_unread(worker.id).expect("unread"));
    }

    #[test]
    fn a_deleted_task_is_gone() {
        let (store, task) = store_with_task();

        store.delete_task(task).expect("delete");

        assert!(store.task(task).expect("read").is_none());
    }

    #[test]
    fn a_task_can_be_recorded_with_its_creator() {
        let (store, _) = store_with_task();

        let task = store
            .add_task_by(&new_task(ProjectId(1), &[]), Some(AgentId(9)))
            .expect("task");

        assert_eq!(task.created_by, Some(AgentId(9)));
        assert_eq!(
            store.task(task.id).expect("read").expect("task").created_by,
            Some(AgentId(9))
        );
    }

    #[test]
    fn a_task_remembers_who_created_it() {
        let (store, task) = store_with_task();

        store.set_task_created_by(task, AgentId(9)).expect("set");

        let stored = store.task(task).expect("task").expect("row");
        assert_eq!(stored.created_by, Some(AgentId(9)));
    }

    #[test]
    fn an_agent_has_its_own_ports() {
        let (store, task) = store_with_task();
        let mut new = new_agent(task);
        new.ports = &[5180, 5181];
        let agent = store.add_agent(&new).expect("agent");

        assert_eq!(store.agent_ports(agent.id).expect("ports"), [5180, 5181]);
        assert_eq!(
            store.agent_ports(AgentId(99)).expect("none"),
            Vec::<u16>::new()
        );
    }

    #[test]
    fn confirming_a_task_marks_it_briefed() {
        let (store, task) = store_with_task();
        assert!(!store.task(task).expect("task").expect("row").briefed);

        store.confirm_task(task).expect("confirm");

        assert!(store.task(task).expect("task").expect("row").briefed);
    }

    fn message(from: Option<AgentId>, to: AgentId, text: &str) -> NewMessage<'_> {
        NewMessage {
            project: ProjectId(1),
            task: Some(TaskId(1)),
            from,
            to,
            kind: MessageKind::Report,
            text,
            delivered: false,
        }
    }

    #[test]
    fn unread_messages_are_taken_once_in_order() {
        let (store, task) = store_with_task();
        let sender = store.add_agent(&new_agent(task)).expect("sender");
        let mut worker = new_agent(task);
        worker.name = "api";
        worker.role = AgentRole::Worker;
        let worker = store.add_agent(&worker).expect("worker");
        store
            .add_message(&message(Some(worker.id), sender.id, "first"))
            .expect("first");
        store
            .add_message(&message(None, sender.id, "second"))
            .expect("second");
        store
            .add_message(&message(None, worker.id, "other"))
            .expect("other");
        assert!(store.has_unread(sender.id).expect("unread"));

        let (taken, delivery) = store.take_unread(sender.id).expect("take");

        assert_eq!(taken.len(), 2);
        assert_eq!(delivery.ids.len(), 2);
        assert_eq!(taken[0].text, "first");
        assert_eq!(taken[0].from_name, "api");
        assert_eq!(taken[0].from, Some(worker.id));
        assert_eq!(taken[1].from_name, "swarm");
        assert_eq!(taken[1].from, None);
        assert!(store.take_unread(sender.id).expect("again").0.is_empty());
        assert!(!store.has_unread(sender.id).expect("unread"));
        assert_eq!(store.take_unread(worker.id).expect("other").0.len(), 1);
    }

    #[test]
    fn reading_does_not_mark_until_told() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");
        store
            .add_message(&message(None, agent.id, "one"))
            .expect("message");

        let first = store.unread(agent.id).expect("unread");
        let again = store.unread(agent.id).expect("unread");
        store.mark_delivered(&[first[0].id]).expect("mark");

        assert_eq!(first, again);
        assert!(store.unread(agent.id).expect("unread").is_empty());
    }

    #[test]
    fn a_message_delivered_on_arrival_is_not_unread() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");
        let mut brief = message(None, agent.id, "brief");
        brief.delivered = true;

        store.add_message(&brief).expect("brief");

        assert!(store.take_unread(agent.id).expect("take").0.is_empty());
    }

    /// The migration that adds `ended_at` is the ninth.
    const END_MIGRATION: usize = 8;

    #[test]
    fn the_end_migration_turns_agent_seconds_into_milliseconds() {
        let connection = Connection::open_in_memory().expect("memory");
        for migration in &MIGRATIONS[..END_MIGRATION] {
            connection.execute_batch(migration).expect("schema");
        }
        connection
            .execute(
                "INSERT INTO projects (path, name, added_at) VALUES ('/p', 'p', 1)",
                [],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO tasks (project, name, branch, base, worktree, ports, created_at)
                 VALUES (1, 't', 'b', 'main', '/w', '', 1)",
                [],
            )
            .expect("task");
        connection
            .execute(
                "INSERT INTO agents (project, role, kind, model, status, lifecycle, created_at,
                     name, worktree, branch, task)
                 VALUES (1, '\"Worker\"', '\"ClaudeCode\"', 'm', '\"Unknown\"', '\"Stopped\"',
                     1700000000, 'w', '/w', 'b', 1)",
                [],
            )
            .expect("agent");
        connection
            .pragma_update(
                None,
                "user_version",
                i64::try_from(END_MIGRATION).expect("count"),
            )
            .expect("version");

        let store = Store::migrated(connection).expect("migrate");
        let agent = store.agent(AgentId(1)).expect("query").expect("agent");

        assert_eq!(agent.info.created_at, 1_700_000_000_000);
        assert_eq!(agent.info.ended_at, None);
        assert_eq!(store.base_commit(AgentId(1)).expect("base"), None);
    }

    #[test]
    fn a_new_agent_has_a_creation_time_in_milliseconds() {
        let (store, task) = store_with_task();
        let before = unix_millis();

        let agent = store.add_agent(&new_agent(task)).expect("agent");
        let stored = store.agent(agent.id).expect("query").expect("agent");

        assert!(agent.created_at >= before);
        assert_eq!(stored.info.created_at, agent.created_at);
        assert_eq!(stored.info.ended_at, None);
    }

    #[test]
    fn the_start_time_is_saved() {
        let (store, task) = store_with_task();
        let mut agent = store.add_agent(&new_agent(task)).expect("agent");
        assert_eq!(agent.started_at, None);

        agent.started_at = Some(4321);
        store.save_agent(&agent).expect("save");
        let stored = store.agent(agent.id).expect("query").expect("agent");

        assert_eq!(stored.info.started_at, Some(4321));
    }

    #[test]
    fn the_end_time_is_saved_and_cleared() {
        let (store, task) = store_with_task();
        let mut agent = store.add_agent(&new_agent(task)).expect("agent");

        agent.lifecycle = AgentLifecycle::Resumable;
        agent.ended_at = Some(1234);
        store.save_agent(&agent).expect("save");
        let ended = store.agent(agent.id).expect("query").expect("agent");

        agent.lifecycle = AgentLifecycle::Running;
        agent.ended_at = None;
        store.save_agent(&agent).expect("save");
        let again = store.agent(agent.id).expect("query").expect("agent");

        assert_eq!(ended.info.ended_at, Some(1234));
        assert_eq!(again.info.ended_at, None);
    }

    #[test]
    fn recovery_sets_the_end_time() {
        let (store, task) = store_with_task();
        let agent = running_agent(&store, &new_agent(task));

        store.recover_agents().expect("recover");
        let stored = store.agent(agent.id).expect("query").expect("agent");

        assert!(stored.info.ended_at.is_some());
    }

    #[test]
    fn the_base_commit_is_kept() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");

        store.set_base_commit(agent.id, "abc123").expect("set");

        assert_eq!(
            store.base_commit(agent.id).expect("base").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn delivery_time_is_read_back_and_shared_by_a_batch() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");
        let first = store
            .add_message(&message(None, agent.id, "one"))
            .expect("one");
        let second = store
            .add_message(&message(None, agent.id, "two"))
            .expect("two");
        assert_eq!(first.delivered_at, None);

        let delivery = store.mark_delivered(&[first.id, second.id]).expect("mark");
        let again = store.mark_delivered(&[first.id]).expect("again");
        let listed = store
            .list_messages(first.project, first.task, 10)
            .expect("list");

        assert_eq!(delivery.ids, vec![first.id, second.id]);
        assert!(again.ids.is_empty());
        assert_eq!(listed[0].delivered_at, Some(delivery.delivered_at));
        assert_eq!(listed[1].delivered_at, Some(delivery.delivered_at));
    }

    #[test]
    fn a_message_delivered_on_arrival_carries_its_delivery_time() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");
        let mut brief = message(None, agent.id, "brief");
        brief.delivered = true;

        let info = store.add_message(&brief).expect("brief");

        assert_eq!(info.delivered_at, Some(info.sent_at));
    }

    #[test]
    fn listed_messages_are_the_newest_ones_oldest_first() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");
        for number in 0..5 {
            store
                .add_message(&message(None, agent.id, &format!("m{number}")))
                .expect("message");
        }

        let listed = store
            .list_messages(agent.project, Some(task), 3)
            .expect("list");
        let texts: Vec<&str> = listed.iter().map(|message| message.text.as_str()).collect();

        assert_eq!(texts, ["m2", "m3", "m4"]);
    }

    #[test]
    fn messages_without_a_task_belong_to_the_start_orchestrator() {
        let (store, task) = store_with_task();
        let agent = store.add_agent(&new_agent(task)).expect("agent");
        let project = agent.project;
        let mut start = message(None, agent.id, "for start");
        start.task = None;
        store.add_message(&start).expect("start");
        store
            .add_message(&message(None, agent.id, "for task"))
            .expect("task");

        let start_messages = store.list_messages(project, None, 10).expect("start");
        let task_messages = store.list_messages(project, Some(task), 10).expect("task");
        let other = store
            .list_messages(ProjectId(project.0 + 1), None, 10)
            .expect("other");

        assert_eq!(start_messages.len(), 1);
        assert_eq!(start_messages[0].text, "for start");
        assert_eq!(task_messages.len(), 1);
        assert_eq!(task_messages[0].text, "for task");
        assert!(other.is_empty());
    }
}
