//! Claude subscription usage from the undocumented `/api/oauth/usage` endpoint.
//!
//! The token is read from Claude Code's Keychain item on every poll and never
//! stored, logged or refreshed here: Claude Code owns refresh, and refreshing
//! from a second client would rotate the refresh token under it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::usage::{self, Problem, Usage, Window};

/// Claude Code reads it with `security find-generic-password -a $USER -w -s "Claude Code-credentials"`.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// `security` exits with this when the item does not exist.
const ERR_SEC_ITEM_NOT_FOUND: i32 = 44;

/// Never printed: `Debug` is redacted and there is no `Display`.
struct AccessToken(String);

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl AccessToken {
    fn fingerprint(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.0.hash(&mut h);
        h.finish()
    }
}

#[derive(Deserialize)]
struct StoredCredentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<StoredOAuth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredOAuth {
    access_token: String,
    /// Unix milliseconds. Stored as a float (e.g. `1793390000000.0`).
    expires_at: Option<f64>,
    subscription_type: Option<String>,
}

struct Credential {
    token: AccessToken,
    expires_at: Option<DateTime<Utc>>,
    plan: Option<String>,
}

pub struct ClaudeSource {
    agent: ureq::Agent,
    /// Normal schedule: at most one request per `POLL_INTERVAL`.
    next_due: DateTime<Utc>,
    /// Set by a 429 or 403. Saved to disk so a relaunch never re-hits it.
    backoff_until: DateTime<Utc>,
    backoff_file: Option<PathBuf>,
    /// Fingerprint of a token the server answered 401 to; skip until it changes.
    rejected: Option<u64>,
    logged_shape: bool,
}

impl ClaudeSource {
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        let backoff_file = state_dir.map(|d| d.join("claude-usage-backoff"));
        let backoff_until = backoff_file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| s.trim().parse::<i64>().ok())
            .and_then(|secs| DateTime::from_timestamp(secs, 0))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .https_only(true)
            // The token must only ever go to api.anthropic.com.
            .max_redirects(0)
            .user_agent(concat!("swarm-menubar-spike/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: config.into(),
            next_due: DateTime::<Utc>::MIN_UTC,
            backoff_until,
            backoff_file,
            rejected: None,
            logged_shape: false,
        }
    }

    /// Blocking: may run `security` and one HTTPS request.
    /// `None` means nothing to report yet (not due).
    pub fn poll(&mut self) -> Option<Result<Usage, Problem>> {
        let now = Utc::now();
        if now < self.backoff_until {
            return Some(Err(Problem::RateLimited {
                retry_at: self.backoff_until,
            }));
        }
        if now < self.next_due {
            return None;
        }
        // Cheap local checks run every tick; only a real request moves `next_due`.
        let cred = match read_keychain() {
            Ok(Some(cred)) => cred,
            Ok(None) => return Some(Err(Problem::NotLoggedIn)),
            Err(problem) => return Some(Err(problem)),
        };
        if cred.expires_at.is_some_and(|t| t <= now) {
            return Some(Err(Problem::TokenExpired));
        }
        let fingerprint = cred.token.fingerprint();
        if self.rejected == Some(fingerprint) {
            return Some(Err(Problem::Unauthorized));
        }
        self.next_due = now + usage::POLL_INTERVAL;
        Some(self.fetch(&cred, fingerprint))
    }

    fn fetch(&mut self, cred: &Credential, fingerprint: u64) -> Result<Usage, Problem> {
        let mut resp = self
            .agent
            .get(USAGE_URL)
            .header("Authorization", &format!("Bearer {}", cred.token.0))
            .header("anthropic-beta", OAUTH_BETA)
            .call()
            .map_err(|e| Problem::Other(format!("Network error: {e}")))?;
        let status = resp.status().as_u16();
        eprintln!("[claude] GET /api/oauth/usage -> {status}");
        match status {
            200 => {
                let body = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| Problem::Other(format!("Read error: {e}")))?;
                if !self.logged_shape {
                    self.logged_shape = true;
                    log_keys(&body);
                }
                parse_usage(&body, cred.plan.clone())
                    .map_err(|e| Problem::Other(format!("Unexpected response: {e}")))
            }
            401 => {
                self.rejected = Some(fingerprint);
                Err(Problem::Unauthorized)
            }
            // Claude Code treats 403 like 429 for this endpoint.
            403 | 429 => {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok());
                let wait = usage::backoff_from_retry_after(retry_after);
                eprintln!("[claude] backing off {}s", wait.as_secs());
                self.set_backoff(Utc::now() + wait);
                Err(Problem::RateLimited {
                    retry_at: self.backoff_until,
                })
            }
            other => Err(Problem::Other(format!("HTTP {other}"))),
        }
    }

    fn set_backoff(&mut self, until: DateTime<Utc>) {
        self.backoff_until = until;
        if let Some(file) = &self.backoff_file
            && let Err(e) = std::fs::write(file, until.timestamp().to_string())
        {
            eprintln!("[claude] could not save backoff: {e}");
        }
    }
}

/// Runs `/usr/bin/security`, whose ACL entry Claude Code's item already trusts,
/// so no Keychain prompt appears. The secret goes straight from the pipe into memory.
fn read_keychain() -> Result<Option<Credential>, Problem> {
    let mut child = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Problem::Other(format!("security: {e}")))?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > KEYCHAIN_TIMEOUT => {
                let _ = child.kill();
                return Err(Problem::Other(
                    "Keychain did not answer (access prompt open?)".into(),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(Problem::Other(format!("security: {e}"))),
        }
    };
    if status.code() == Some(ERR_SEC_ITEM_NOT_FOUND) {
        return Ok(None);
    }
    if !status.success() {
        return Err(Problem::Other(format!("security exited with {status}")));
    }
    let mut raw = String::new();
    child
        .stdout
        .take()
        .ok_or_else(|| Problem::Other("security: no stdout".into()))?
        .read_to_string(&mut raw)
        .map_err(|e| Problem::Other(format!("security: {e}")))?;

    // Don't include serde's message: it can quote part of the input.
    let stored: StoredCredentials = serde_json::from_str(raw.trim()).map_err(|e| {
        eprintln!(
            "[claude] keychain JSON: {:?} at column {} of {} bytes",
            e.classify(),
            e.column(),
            raw.len()
        );
        Problem::Other("Keychain item is not the expected JSON".into())
    })?;
    Ok(stored.oauth.map(|o| Credential {
        token: AccessToken(o.access_token),
        expires_at: o
            .expires_at
            .and_then(|ms| DateTime::from_timestamp_millis(ms as i64)),
        plan: o.subscription_type,
    }))
}

/// Logs the response's top-level keys (no values) once, to track shape changes.
fn log_keys(body: &str) {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(body) {
        let keys: Vec<_> = map.keys().map(String::as_str).collect();
        eprintln!("[claude] usage response keys: {keys:?}");
    }
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<Limit>,
    seven_day: Option<Limit>,
    seven_day_opus: Option<Limit>,
    seven_day_sonnet: Option<Limit>,
    #[serde(default)]
    model_scoped: Vec<ModelScoped>,
}

#[derive(Deserialize)]
struct Limit {
    /// 0–100. `null` when the window has not started.
    utilization: Option<f64>,
    /// ISO 8601 string here; Claude Code's `new Date(...)` also accepts epoch numbers.
    resets_at: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ModelScoped {
    display_name: String,
    utilization: Option<f64>,
    resets_at: Option<serde_json::Value>,
}

fn parse_usage(body: &str, plan: Option<String>) -> anyhow::Result<Usage> {
    let r: UsageResponse = serde_json::from_str(body)?;
    let named = [
        ("5-hour", r.five_hour),
        ("Weekly", r.seven_day),
        ("Weekly Opus", r.seven_day_opus),
        ("Weekly Sonnet", r.seven_day_sonnet),
    ];
    let mut windows: Vec<Window> = named
        .into_iter()
        .filter_map(|(label, limit)| {
            let limit = limit?;
            Some(window(
                label.into(),
                limit.utilization?,
                limit.resets_at.as_ref(),
            ))
        })
        .collect();
    windows.extend(r.model_scoped.into_iter().filter_map(|m| {
        let label = format!("Weekly {}", m.display_name);
        Some(window(label, m.utilization?, m.resets_at.as_ref()))
    }));
    Ok(Usage {
        plan,
        windows,
        fetched_at: Utc::now(),
    })
}

fn window(label: String, utilization: f64, resets_at: Option<&serde_json::Value>) -> Window {
    Window {
        label,
        used_percent: utilization.clamp(0., 100.) as f32,
        resets_at: resets_at.and_then(parse_reset),
    }
}

fn parse_reset(v: &serde_json::Value) -> Option<DateTime<Utc>> {
    match v {
        serde_json::Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        serde_json::Value::Number(n) => DateTime::from_timestamp(n.as_i64()?, 0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape taken from Claude Code 2.1.314's `/usage` code, which reads these fields.
    const SAMPLE: &str = r#"{
        "five_hour": {"utilization": 42.0, "resets_at": "2026-10-30T18:00:00.123456+00:00"},
        "seven_day": {"utilization": 13.5, "resets_at": "2026-11-04T09:00:00+00:00"},
        "seven_day_opus": null,
        "seven_day_sonnet": {"utilization": null, "resets_at": null},
        "model_scoped": [{"display_name": "Fable", "utilization": 7, "resets_at": 1793000000}],
        "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null}
    }"#;

    fn labels(u: &Usage) -> Vec<&str> {
        u.windows.iter().map(|w| w.label.as_str()).collect()
    }

    #[test]
    fn parse_usage_should_skip_null_windows() {
        let u = parse_usage(SAMPLE, None).unwrap();
        assert_eq!(labels(&u), ["5-hour", "Weekly", "Weekly Fable"]);
    }

    #[test]
    fn parse_usage_should_read_iso_reset_times() {
        let u = parse_usage(SAMPLE, None).unwrap();
        assert_eq!(
            u.windows[0].resets_at.map(|t| t.timestamp()),
            Some(1_793_383_200)
        );
    }

    #[test]
    fn parse_usage_should_read_epoch_reset_times() {
        let u = parse_usage(SAMPLE, None).unwrap();
        assert_eq!(
            u.windows[2].resets_at.map(|t| t.timestamp()),
            Some(1_793_000_000)
        );
    }

    #[test]
    fn stored_credentials_should_accept_float_expiry() {
        let raw = r#"{"claudeAiOauth": {"accessToken": "x", "expiresAt": 1793390000000.0}}"#;
        let stored: StoredCredentials = serde_json::from_str(raw).unwrap();
        assert_eq!(
            stored.oauth.and_then(|o| o.expires_at),
            Some(1_793_390_000_000.0)
        );
    }

    #[test]
    fn access_token_debug_should_not_show_the_secret() {
        let token = AccessToken("sk-ant-oat01-secret".into());
        assert!(!format!("{token:?}").contains("secret"));
    }
}
