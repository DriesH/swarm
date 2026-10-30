//! The daemon's SQLite database (ADR 0005). Only the daemon opens it.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};
use swarm_protocol::{CreateTerminal, TerminalId, TerminalKind};
use swarm_term::ExitStatus;

/// How long a locked database makes a statement wait.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One entry per schema version; `PRAGMA user_version` counts how many ran.
const MIGRATIONS: &[&str] = &["CREATE TABLE terminals (
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
    )"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
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
pub(crate) struct Leftover {
    pub(crate) id: TerminalId,
    pub(crate) command: String,
    pub(crate) pid: Option<i32>,
    /// Process start time in microseconds since the epoch; guards against PID reuse.
    pub(crate) started_us: Option<u64>,
}

pub(crate) struct Store {
    connection: Connection,
}

impl Store {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("cannot open the database {}", path.display()))?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .context("cannot set the busy timeout")?;
        connection
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
            .context("cannot turn on WAL")?;

        Self::migrated(connection)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        Self::migrated(Connection::open_in_memory()?)
    }

    fn migrated(mut connection: Connection) -> Result<Self> {
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let applied = usize::try_from(version)?;

        for (index, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(migration)?;
            transaction.pragma_update(None, "user_version", i64::try_from(index + 1)?)?;
            transaction.commit()?;
        }

        Ok(Self { connection })
    }

    /// Written before the process starts (ADR 0005). The row id is the terminal id,
    /// so ids never repeat across restarts.
    pub(crate) fn insert_starting(&self, request: &CreateTerminal) -> Result<TerminalId> {
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

        let id = u64::try_from(self.connection.last_insert_rowid())?;

        Ok(TerminalId(id))
    }

    pub(crate) fn mark_running(
        &self,
        id: TerminalId,
        pid: Option<u32>,
        started_us: Option<u64>,
    ) -> Result<()> {
        let started_us = started_us.map(i64::try_from).transpose()?;

        self.connection.execute(
            "UPDATE terminals SET status = ?1, pid = ?2, started_us = ?3 WHERE id = ?4",
            params![Status::Running.as_str(), pid, started_us, row_id(id)?],
        )?;

        Ok(())
    }

    pub(crate) fn mark_exited(&self, id: TerminalId, exit: Option<&ExitStatus>) -> Result<()> {
        let exit = exit.map(serde_json::to_string).transpose()?;

        self.connection.execute(
            "UPDATE terminals SET status = ?1, exit = ?2 WHERE id = ?3",
            params![Status::Exited.as_str(), exit, row_id(id)?],
        )?;

        Ok(())
    }

    pub(crate) fn mark_stopped(&self, id: TerminalId) -> Result<()> {
        self.connection.execute(
            "UPDATE terminals SET status = ?1 WHERE id = ?2",
            params![Status::Stopped.as_str(), row_id(id)?],
        )?;

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn status(&self, id: TerminalId) -> Result<Status> {
        let status: String = self.connection.query_row(
            "SELECT status FROM terminals WHERE id = ?1",
            params![row_id(id)?],
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
        .with_context(|| format!("unknown status {status:?}"))
    }

    /// Rows that no daemon finished. Call it once at startup, before this run creates
    /// any row, so the result holds only leftovers.
    pub(crate) fn leftovers(&self) -> Result<Vec<Leftover>> {
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
}

fn row_id(id: TerminalId) -> Result<i64> {
    i64::try_from(id.0).context("terminal id is too large")
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
}
