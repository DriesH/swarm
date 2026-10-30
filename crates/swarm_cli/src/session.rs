//! The connection to the daemon's agent socket: who the caller is and one call at a time.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use swarm_protocol::AgentId;
use swarm_protocol::agent::{AgentRequest, Call, Reply};

/// A call that is not a `wait` may take this long, for example a big merge.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Added to a `wait` timeout, so the daemon answers before the client gives up.
const WAIT_MARGIN: Duration = Duration::from_secs(30);

const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// The daemon replies to a `wait` after at most this many seconds.
pub(crate) const DEFAULT_WAIT_SECS: u32 = 120;

/// Who the caller is, as the agent terminal's environment says.
#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) socket: PathBuf,
    pub(crate) agent: AgentId,
    pub(crate) token: String,
}

impl Session {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let missing = |name: &str| format!("{name} is not set: run swarm inside a Swarm agent");

        let socket = var("SWARM_SOCKET").ok_or_else(|| missing("SWARM_SOCKET"))?;
        let agent = var("SWARM_AGENT_ID").ok_or_else(|| missing("SWARM_AGENT_ID"))?;
        let token = var("SWARM_TOKEN").ok_or_else(|| missing("SWARM_TOKEN"))?;
        let agent = agent
            .parse()
            .map_err(|_| format!("SWARM_AGENT_ID {agent:?} is not a number"))?;

        Ok(Self {
            socket: PathBuf::from(socket),
            agent: AgentId(agent),
            token,
        })
    }

    /// Sends the call and returns the result, or the daemon's error text.
    pub(crate) fn call(&self, call: Call) -> Result<Value, String> {
        let timeout = match &call {
            Call::Wait { timeout_secs } => {
                Duration::from_secs(u64::from(timeout_secs.unwrap_or(DEFAULT_WAIT_SECS)))
                    + WAIT_MARGIN
            }
            _ => CALL_TIMEOUT,
        };

        self.call_within(call, timeout)
    }

    pub(crate) fn call_within(&self, call: Call, read_timeout: Duration) -> Result<Value, String> {
        let request = AgentRequest {
            agent: self.agent,
            token: self.token.clone(),
            call,
        };
        let mut line = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        line.push('\n');

        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|error| format!("cannot reach Swarm at {}: {error}", self.socket.display()))?;
        stream
            .set_read_timeout(Some(read_timeout))
            .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
            .map_err(|error| error.to_string())?;
        stream
            .write_all(line.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|error| format!("cannot send the request: {error}"))?;

        let mut answer = String::new();
        let read = BufReader::new(stream)
            .read_line(&mut answer)
            .map_err(|error| format!("no answer from Swarm: {error}"))?;
        if read == 0 {
            return Err("Swarm closed the connection".to_owned());
        }

        let reply: Reply = serde_json::from_str(&answer)
            .map_err(|error| format!("Swarm sent an answer that cannot be read: {error}"))?;

        if reply.ok {
            Ok(reply.result)
        } else {
            Err(reply.error.unwrap_or_else(|| "the call failed".to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();

        move |name| {
            pairs
                .iter()
                .find(|(own, _)| own == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn the_session_comes_from_three_variables() {
        let session = Session::from_vars(vars(&[
            ("SWARM_SOCKET", "/tmp/a.sock"),
            ("SWARM_AGENT_ID", "7"),
            ("SWARM_TOKEN", "abc"),
        ]))
        .expect("session");

        assert_eq!(session.socket, PathBuf::from("/tmp/a.sock"));
        assert_eq!(session.agent, AgentId(7));
        assert_eq!(session.token, "abc");
    }

    #[test]
    fn a_missing_variable_is_named() {
        let error =
            Session::from_vars(vars(&[("SWARM_SOCKET", "/tmp/a.sock")])).expect_err("missing");

        assert!(error.contains("SWARM_AGENT_ID"), "{error}");
    }

    #[test]
    fn an_agent_id_that_is_not_a_number_is_refused() {
        let error = Session::from_vars(vars(&[
            ("SWARM_SOCKET", "/tmp/a.sock"),
            ("SWARM_AGENT_ID", "x"),
            ("SWARM_TOKEN", "abc"),
        ]))
        .expect_err("bad id");

        assert!(error.contains("not a number"), "{error}");
    }
}
