//! `swarm hook`: forwards a Claude Code or Codex hook event to the daemon. It never
//! prints, never fails and always exits 0, because the agent must not see an error.

use std::io::Read as _;
use std::time::Duration;

use swarm_protocol::agent::Call;

use crate::session::Session;

/// Longest hook JSON we forward; a bigger event is dropped.
const MAX_HOOK_BYTES: u64 = 2 << 20;

/// A hook must never keep an agent waiting.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn run() {
    let mut input = Vec::new();
    if std::io::stdin()
        .take(MAX_HOOK_BYTES + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() as u64 > MAX_HOOK_BYTES
    {
        return;
    }

    forward(&input).unwrap_or_default();
}

fn forward(input: &[u8]) -> Option<()> {
    let session = Session::from_env().ok()?;
    let event = serde_json::from_slice(input).ok()?;

    // Waiting for the answer means the daemon has read the event before this ends.
    session
        .call_within(Call::Hook { event }, SOCKET_TIMEOUT)
        .ok()
        .map(|_| ())
}
