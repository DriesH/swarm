//! Codex usage through `codex app-server` (`account/rateLimits/read`).
//!
//! Codex reads its own `~/.codex/auth.json`, refreshes the token and calls
//! `GET https://chatgpt.com/backend-api/wham/usage`. Swarm never touches the token.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::usage::{Problem, Usage, Window, window_label};

const TIMEOUT: Duration = Duration::from_secs(20);
const RATE_LIMITS_ID: u64 = 2;

/// Apps opened from Finder get a minimal PATH, so look in the usual install places too.
fn find_codex() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("SWARM_CODEX_BIN") {
        return Some(bin.into());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let fixed = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"].map(PathBuf::from);
    let in_home = [
        ".local/bin/codex",
        ".npm-global/bin/codex",
        ".bun/bin/codex",
        ".volta/bin/codex",
    ]
    .iter()
    .filter_map(|rel| home.as_ref().map(|h| h.join(rel)));
    let on_path = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("codex"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    fixed
        .into_iter()
        .chain(in_home)
        .chain(on_path)
        .find(|p| p.is_file())
}

/// Kills the app-server when dropped, also on timeout.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Blocking: starts `codex app-server`, asks once, stops it.
pub fn poll() -> Result<Usage, Problem> {
    let bin = find_codex().ok_or_else(|| Problem::Other("Codex not found".into()))?;
    let child = Command::new(&bin)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Problem::Other(format!("codex: {e}")))?;
    let mut server = Server(child);

    let mut stdin = server
        .0
        .stdin
        .take()
        .ok_or_else(|| Problem::Other("codex: no stdin".into()))?;
    let stdout = server
        .0
        .stdout
        .take()
        .ok_or_else(|| Problem::Other("codex: no stdout".into()))?;
    let requests = [
        json!({"id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "swarm_menubar_spike", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"method": "initialized"}),
        json!({"id": RATE_LIMITS_ID, "method": "account/rateLimits/read"}),
    ];
    for req in requests {
        writeln!(stdin, "{req}").map_err(|e| Problem::Other(format!("codex: {e}")))?;
    }

    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if msg.get("id").and_then(|v| v.as_u64()) == Some(RATE_LIMITS_ID) {
                let _ = tx.send(msg);
                break;
            }
        }
    });
    let msg = rx
        .recv_timeout(TIMEOUT)
        .map_err(|_| Problem::Other("Codex did not answer".into()))?;
    drop(server);

    if let Some(err) = msg.get("error") {
        let text = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("error");
        eprintln!("[codex] account/rateLimits/read error: {text}");
        return Err(
            if text.to_lowercase().contains("auth") || text.to_lowercase().contains("login") {
                Problem::NotLoggedIn
            } else {
                Problem::Other(format!("Codex: {text}"))
            },
        );
    }
    let result = msg.get("result").cloned().unwrap_or_default();
    parse(result).map_err(|e| Problem::Other(format!("Unexpected Codex answer: {e}")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse {
    rate_limits: Snapshot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    plan_type: Option<String>,
    primary: Option<RateLimitWindow>,
    secondary: Option<RateLimitWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitWindow {
    used_percent: f64,
    window_duration_mins: Option<i64>,
    /// Unix seconds.
    resets_at: Option<i64>,
}

fn parse(result: serde_json::Value) -> anyhow::Result<Usage> {
    let r: RateLimitsResponse = serde_json::from_value(result)?;
    let windows = [r.rate_limits.primary, r.rate_limits.secondary]
        .into_iter()
        .flatten()
        .map(|w| Window {
            label: w
                .window_duration_mins
                .map_or_else(|| "Current".into(), window_label),
            used_percent: w.used_percent.clamp(0., 100.) as f32,
            resets_at: w.resets_at.and_then(|s| DateTime::from_timestamp(s, 0)),
        })
        .collect();
    Ok(Usage {
        plan: r.rate_limits.plan_type,
        windows,
        fetched_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_should_label_windows_by_length() {
        let result = json!({"rateLimits": {
            "limitId": "codex", "planType": "plus",
            "primary": {"usedPercent": 12, "windowDurationMins": 300, "resetsAt": 1793383200},
            "secondary": {"usedPercent": 40, "windowDurationMins": 10080, "resetsAt": null}
        }});
        let labels: Vec<_> = parse(result)
            .unwrap()
            .windows
            .into_iter()
            .map(|w| w.label)
            .collect();
        assert_eq!(labels, ["5-hour", "Weekly"]);
    }
}
