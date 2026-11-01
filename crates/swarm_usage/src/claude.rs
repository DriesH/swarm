//! Claude usage from two sources.
//!
//! 1. The status line input of a running Claude Code ([`status_line_windows`]). It is
//!    documented, free and has no rate limit.
//! 2. The `/api/oauth/usage` endpoint ([`ClaudeEndpoint`]), for when no agent runs. It
//!    shares a budget with the user's own sessions and answers 429 often.
//!
//! The endpoint token comes from Claude Code's Keychain item through `/usr/bin/security`.
//! It is never logged, never stored and never refreshed here: Claude Code owns the
//! refresh, and a second client refreshing would rotate its refresh token.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use swarm_protocol::{UsageProblem, UsageWindow};

use crate::process::{self, RunError};
use crate::{Outcome, Reading, percent, unix_seconds, write_private};

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "anthropic-beta: oauth-2025-04-20";

/// `security` exits with this when the item does not exist.
const ITEM_NOT_FOUND: i32 = 44;
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// `curl` stops itself after this long. The process limit is a little longer.
const CURL_MAX_SECS: u64 = 5;
const CURL_TIMEOUT: Duration = Duration::from_secs(CURL_MAX_SECS + 2);
const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// At most one request in this many seconds, whatever the answer was.
const REQUEST_INTERVAL: u64 = 5 * 60;

/// The wait after a 429 or 403 without a usable `Retry-After`.
const DEFAULT_RETRY: u64 = 5 * 60;

/// The longest wait after a 429 or 403. Claude Code holds off no longer either.
const MAX_RETRY: u64 = 60 * 60;

/// How soon the Keychain is read again, so a stuck prompt or an expired token does not
/// start `security` on every tick.
const KEYCHAIN_RECHECK: u64 = 60;

/// Windows of the status line input: the JSON key and the label, shortest first.
const STATUS_LINE_WINDOWS: [(&str, &str); 2] = [("five_hour", "5h"), ("seven_day", "7d")];

/// The `rate_limits` of Claude Code's status line input, shortest window first. A
/// window without a percentage is left out.
pub fn status_line_windows(input: &Value) -> Vec<UsageWindow> {
    let Some(limits) = input.get("rate_limits") else {
        return Vec::new();
    };

    STATUS_LINE_WINDOWS
        .iter()
        .filter_map(|(key, label)| {
            let window = limits.get(key)?;
            let used = window.get("used_percentage")?.as_f64()?;
            let resets_at = window
                .get("resets_at")
                .and_then(Value::as_f64)
                .and_then(unix_seconds);

            Some(UsageWindow {
                label: (*label).to_owned(),
                used_percent: percent(used),
                resets_at,
            })
        })
        .collect()
}

/// The programs the endpoint source runs.
#[derive(Debug, Clone)]
pub struct Commands {
    pub security: PathBuf,
    pub curl: PathBuf,
}

impl Commands {
    pub fn system() -> Self {
        Self {
            security: PathBuf::from("/usr/bin/security"),
            curl: PathBuf::from("/usr/bin/curl"),
        }
    }
}

/// The OAuth access token. `Debug` hides it and there is no `Display`.
struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Token(<redacted>)")
    }
}

impl Token {
    /// What is remembered of a token the server refused.
    fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());

        digest.iter().fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            write!(hex, "{byte:02x}").ok();
            hex
        })
    }
}

/// Claude Code's Keychain item.
#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<StoredOauth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredOauth {
    access_token: String,
    /// Unix milliseconds, stored as a float.
    expires_at: Option<f64>,
    subscription_type: Option<String>,
}

struct Credential {
    token: Token,
    expires_at_ms: Option<f64>,
    plan: Option<String>,
}

impl Credential {
    fn expired(&self, now: u64) -> bool {
        #[expect(
            clippy::cast_precision_loss,
            reason = "Unix seconds are far below 2^53"
        )]
        let now_ms = now as f64 * 1000.0;

        self.expires_at_ms.is_some_and(|expires| expires <= now_ms)
    }
}

/// What is kept on disk so a restart does not ask the endpoint again.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
struct State {
    /// No request before this time (Unix seconds).
    next_request_at: u64,
    /// After a 429 or 403: no request before this time.
    backoff_until: u64,
    /// The fingerprint of a token the server answered 401 to.
    rejected: Option<String>,
}

/// The `/api/oauth/usage` source with its backoff.
pub struct ClaudeEndpoint {
    commands: Commands,
    /// The account name of the Keychain item.
    user: Option<String>,
    state_file: PathBuf,
    state: State,
    /// In memory only.
    next_keychain_at: u64,
}

impl ClaudeEndpoint {
    /// Loads the backoff from `state_file`, if there is one.
    pub fn new(commands: Commands, user: Option<String>, state_file: PathBuf) -> Self {
        let state = load_state(&state_file);

        Self {
            commands,
            user,
            state_file,
            state,
            next_keychain_at: 0,
        }
    }

    /// Reads the endpoint when it is due. `None` means there is nothing new to report:
    /// not due yet, or the token has expired and Claude Code will refresh it.
    ///
    /// Blocks for at most the Keychain timeout and the request time.
    pub fn poll(&mut self, now: u64, claude_installed: bool) -> Option<Outcome> {
        if !claude_installed {
            return Some(Err(UsageProblem::NotInstalled));
        }
        if now < self.state.backoff_until {
            return Some(Err(UsageProblem::RateLimited {
                retry_at: self.state.backoff_until,
            }));
        }
        if now < self.state.next_request_at || now < self.next_keychain_at {
            return None;
        }

        self.next_keychain_at = now + KEYCHAIN_RECHECK;
        let credential =
            match read_credential(&self.commands, self.user.as_deref(), KEYCHAIN_TIMEOUT) {
                Ok(credential) => credential,
                Err(problem) => return Some(Err(problem)),
            };

        if credential.expired(now) {
            return None;
        }

        let fingerprint = credential.token.fingerprint();
        if self.state.rejected.as_deref() == Some(fingerprint.as_str()) {
            return Some(Err(UsageProblem::Unauthorized));
        }

        self.state.next_request_at = now + REQUEST_INTERVAL;
        self.save();

        let response = match request(&self.commands, &credential.token) {
            Ok(response) => response,
            Err(problem) => return Some(Err(problem)),
        };

        Some(self.answer(&response, fingerprint, credential.plan, now))
    }

    fn answer(
        &mut self,
        response: &Response,
        fingerprint: String,
        plan: Option<String>,
        now: u64,
    ) -> Outcome {
        match response.status {
            200 => endpoint_windows(&response.body)
                .map(|windows| Reading { windows, plan })
                .ok_or_else(|| failed("Unexpected response")),
            401 => {
                self.state.rejected = Some(fingerprint);
                self.save();

                Err(UsageProblem::Unauthorized)
            }
            // Claude Code treats 403 like 429 for this endpoint.
            403 | 429 => {
                let retry_at = now + retry_after(response.retry_after.as_deref());
                self.state.backoff_until = retry_at;
                self.save();

                Err(UsageProblem::RateLimited { retry_at })
            }
            other => Err(failed(format!("HTTP {other}"))),
        }
    }

    fn save(&self) {
        if let Err(error) = save_state(&self.state_file, &self.state) {
            log::warn!(
                "cannot save the Claude usage backoff to {}: {error}",
                self.state_file.display()
            );
        }
    }
}

fn failed(text: impl Into<String>) -> UsageProblem {
    UsageProblem::Failed(text.into())
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    write_private(path, &serde_json::to_vec(state)?)
}

fn load_state(path: &Path) -> State {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            log::warn!("ignoring {}: {error}", path.display());
            State::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(error) => {
            log::warn!("cannot read {}: {error}", path.display());
            State::default()
        }
    }
}

/// Seconds to wait after a 429 or 403, from the `Retry-After` header.
fn retry_after(header: Option<&str>) -> u64 {
    header
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_RETRY)
        .min(MAX_RETRY)
}

/// Runs `security`, whose Keychain entry Claude Code's item already trusts, so no
/// prompt should appear.
fn read_credential(
    commands: &Commands,
    user: Option<&str>,
    timeout: Duration,
) -> Result<Credential, UsageProblem> {
    let mut command = Command::new(&commands.security);
    command.arg("find-generic-password");
    if let Some(user) = user {
        command.args(["-a", user]);
    }
    command.args(["-w", "-s", KEYCHAIN_SERVICE]);

    let finished = match process::run(&mut command, b"", timeout, MAX_RESPONSE_BYTES) {
        Ok(finished) => finished,
        Err(RunError::Timeout) => return Err(failed("Keychain prompt open")),
        Err(error) => {
            log::warn!("cannot run security: {error}");

            return Err(failed("Keychain read failed"));
        }
    };

    if finished.status.code() == Some(ITEM_NOT_FOUND) {
        return Err(UsageProblem::NotLoggedIn);
    }
    if !finished.status.success() {
        return Err(failed("Keychain read failed"));
    }

    // The serde message can quote part of the input, so it is not logged.
    let stored: Stored = std::str::from_utf8(&finished.stdout)
        .ok()
        .and_then(|text| serde_json::from_str(text.trim()).ok())
        .ok_or_else(|| failed("Keychain item is not the expected JSON"))?;
    let oauth = stored.oauth.ok_or(UsageProblem::NotLoggedIn)?;

    Ok(Credential {
        token: Token(oauth.access_token),
        expires_at_ms: oauth.expires_at,
        plan: oauth.subscription_type,
    })
}

struct Response {
    status: u16,
    retry_after: Option<String>,
    body: Vec<u8>,
}

/// Sends the request through `curl`. The URL and the headers go in a curl config on
/// stdin, so the token is never in the arguments, which `ps` shows. HTTPS only, and no
/// redirects, so the token can only go to the one host.
fn request(commands: &Commands, token: &Token) -> Result<Response, UsageProblem> {
    let config = curl_config(token).ok_or_else(|| failed("Token cannot be sent"))?;

    let mut command = Command::new(&commands.curl);
    // `-q` first: no `.curlrc` can add a proxy or a redirect.
    command
        .args(["-q", "--silent", "--include", "-K", "-"])
        .args(["--proto", "=https", "--max-redirs", "0"])
        .args(["--max-time", &CURL_MAX_SECS.to_string()]);

    let finished = match process::run(
        &mut command,
        config.as_bytes(),
        CURL_TIMEOUT,
        MAX_RESPONSE_BYTES,
    ) {
        Ok(finished) => finished,
        Err(RunError::Timeout) => return Err(failed("Request timed out")),
        Err(error) => {
            log::warn!("cannot run curl: {error}");

            return Err(failed("Request failed"));
        }
    };

    if !finished.status.success() {
        // curl exit code 28 is its own timeout.
        return Err(failed(if finished.status.code() == Some(28) {
            "Request timed out"
        } else {
            "Request failed"
        }));
    }

    parse_response(&finished.stdout).ok_or_else(|| failed("Unexpected response"))
}

/// `None` when the token holds a character a config line cannot carry.
fn curl_config(token: &Token) -> Option<String> {
    if token.0.is_empty() || token.0.chars().any(char::is_control) {
        return None;
    }

    let authorization = format!("Authorization: Bearer {}", token.0);

    Some(format!(
        "url = {}\nheader = {}\nheader = {}\n",
        config_quote(USAGE_URL),
        config_quote(&authorization),
        config_quote(OAUTH_BETA),
    ))
}

/// A curl config value in double quotes.
fn config_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The status, `Retry-After` and body of `curl --include` output. Interim `1xx`
/// answers are skipped.
fn parse_response(raw: &[u8]) -> Option<Response> {
    let mut rest = raw;

    loop {
        let (head, body) = split_head(rest)?;
        let head = std::str::from_utf8(head).ok()?;
        let mut lines = head.lines();
        let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;

        if (100..200).contains(&status) {
            rest = body;
            continue;
        }

        let retry_after = lines.find_map(|line| {
            let (name, value) = line.split_once(':')?;

            name.trim()
                .eq_ignore_ascii_case("retry-after")
                .then(|| value.trim().to_owned())
        });

        return Some(Response {
            status,
            retry_after,
            body: body.to_vec(),
        });
    }
}

/// Splits at the first empty line.
fn split_head(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let crlf = raw.windows(4).position(|window| window == b"\r\n\r\n");
    let lf = raw.windows(2).position(|window| window == b"\n\n");

    let (at, length) = match (crlf, lf) {
        (Some(crlf), Some(lf)) if lf < crlf => (lf, 2),
        (Some(crlf), _) => (crlf, 4),
        (None, Some(lf)) => (lf, 2),
        (None, None) => return None,
    };

    Some((&raw[..at], &raw[at + length..]))
}

/// `five_hour` and `seven_day` of the endpoint's answer. A window with a `null`
/// utilization has not started and is left out. `None` when the body is not an object.
fn endpoint_windows(body: &[u8]) -> Option<Vec<UsageWindow>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value.as_object()?;

    let windows = STATUS_LINE_WINDOWS
        .iter()
        .filter_map(|(key, label)| {
            let window = value.get(key)?;
            let utilization = window.get("utilization")?.as_f64()?;
            let resets_at = window
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_iso8601);

            Some(UsageWindow {
                label: (*label).to_owned(),
                used_percent: percent(utilization),
                resets_at,
            })
        })
        .collect();

    Some(windows)
}

/// Unix seconds from an ISO 8601 / RFC 3339 time such as `2026-10-30T18:00:00.123+00:00`.
/// Fractions of a second are dropped.
fn parse_iso8601(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let digits = |from: usize, length: usize| -> Option<i64> {
        let part = bytes.get(from..from + length)?;
        part.iter().all(u8::is_ascii_digit).then(|| {
            part.iter()
                .fold(0, |sum, digit| sum * 10 + i64::from(digit - b'0'))
        })
    };
    let at =
        |index: usize, wanted: &[u8]| bytes.get(index).is_some_and(|byte| wanted.contains(byte));

    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    if !(at(4, b"-") && at(7, b"-") && at(10, b"Tt ") && at(13, b":") && at(16, b":")) {
        return None;
    }

    let mut end = 19;
    if at(end, b".") {
        end += 1;
        let fraction = bytes[end..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if fraction == 0 {
            return None;
        }
        end += fraction;
    }

    let offset = match bytes.get(end)? {
        b'Z' | b'z' if end + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-') => {
            let hours = digits(end + 1, 2)?;
            let colon = usize::from(at(end + 3, b":"));
            let minutes = digits(end + 3 + colon, 2)?;
            if end + 5 + colon != bytes.len() || hours > 23 || minutes > 59 {
                return None;
            }

            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };

    let valid = (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour < 24
        && minute < 60
        && second <= 60;
    if !valid {
        return None;
    }

    let seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;

    u64::try_from(seconds).ok()
}

fn days_in_month(year: i64, month: i64) -> i64 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);

    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;

    use super::*;
    use crate::fixtures::Scripts;

    const NOW: u64 = 1_800_000_000;
    const SECRET: &str = "sk-ant-oat01-top-secret";

    fn windows(input: &Value) -> Vec<(String, u8, Option<u64>)> {
        status_line_windows(input)
            .into_iter()
            .map(|window| (window.label, window.used_percent, window.resets_at))
            .collect()
    }

    // The status line input.

    #[test]
    fn the_status_line_gives_a_five_hour_and_a_seven_day_window() {
        let input = json!({"rate_limits": {
            "five_hour": {"used_percentage": 23.5, "resets_at": 1_793_383_200_u64},
            "seven_day": {"used_percentage": 41.2, "resets_at": 1_793_900_000_u64},
        }});

        assert_eq!(
            windows(&input),
            [
                ("5h".to_owned(), 24, Some(1_793_383_200)),
                ("7d".to_owned(), 41, Some(1_793_900_000)),
            ]
        );
    }

    #[test]
    fn the_shortest_window_comes_first_whatever_the_key_order() {
        let input = json!({"rate_limits": {
            "seven_day": {"used_percentage": 5, "resets_at": 2},
            "five_hour": {"used_percentage": 9, "resets_at": 1},
        }});

        let labels: Vec<String> = windows(&input).into_iter().map(|window| window.0).collect();

        assert_eq!(labels, ["5h", "7d"]);
    }

    #[test]
    fn the_status_line_percent_is_clamped() {
        let input = json!({"rate_limits": {
            "five_hour": {"used_percentage": -4.0, "resets_at": 1},
            "seven_day": {"used_percentage": 180, "resets_at": 2},
        }});

        let percents: Vec<u8> = windows(&input).into_iter().map(|window| window.1).collect();

        assert_eq!(percents, [0, 100]);
    }

    #[test]
    fn a_status_line_window_without_a_percentage_is_left_out() {
        let input = json!({"rate_limits": {
            "five_hour": {"resets_at": 1},
            "seven_day": {"used_percentage": null, "resets_at": 2},
        }});

        assert!(windows(&input).is_empty());
    }

    #[test]
    fn a_status_line_window_without_a_reset_time_has_none() {
        let input = json!({"rate_limits": {
            "five_hour": {"used_percentage": 10},
            "seven_day": {"used_percentage": 20, "resets_at": "soon"},
        }});

        let resets: Vec<Option<u64>> = windows(&input).into_iter().map(|window| window.2).collect();

        assert_eq!(resets, [None, None]);
    }

    #[test]
    fn a_status_line_without_rate_limits_has_no_windows() {
        assert!(windows(&json!({"model": "opus"})).is_empty());
        assert!(windows(&json!({"rate_limits": null})).is_empty());
        assert!(windows(&json!({"rate_limits": {"five_hour": null}})).is_empty());
        assert!(windows(&json!("text")).is_empty());
    }

    #[test]
    fn a_negative_reset_time_is_dropped() {
        let input = json!({"rate_limits": {"five_hour": {"used_percentage": 1, "resets_at": -5}}});

        assert_eq!(windows(&input), [("5h".to_owned(), 1, None)]);
    }

    // Times.

    #[test]
    fn iso_times_become_unix_seconds() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(
            parse_iso8601("2026-10-30T18:00:00.123456+00:00"),
            Some(1_793_383_200)
        );
        assert_eq!(
            parse_iso8601("2026-10-30T20:00:00+02:00"),
            Some(1_793_383_200)
        );
        assert_eq!(
            parse_iso8601("2026-10-30T13:30:00-0430"),
            Some(1_793_383_200)
        );
        assert_eq!(parse_iso8601("2024-02-29T00:00:00Z"), Some(1_709_164_800));
    }

    #[test]
    fn bad_iso_times_are_none() {
        for text in [
            "",
            "soon",
            "2026-10-30",
            "2026-10-30T18:00:00",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-10-30T24:00:00Z",
            "2026-10-30T18:61:00Z",
            "2026-10-30T18:00:00.Z",
            "2026-10-30T18:00:00+25:00",
            "2026-10-30T18:00:00Zjunk",
            "1969-12-31T00:00:00Z",
        ] {
            assert_eq!(parse_iso8601(text), None, "{text:?}");
        }
    }

    // The endpoint answer.

    #[test]
    fn the_endpoint_answer_gives_two_windows() {
        let body = br#"{
            "five_hour": {"utilization": 42.0, "resets_at": "2026-10-30T18:00:00.123456+00:00"},
            "seven_day": {"utilization": 13.5, "resets_at": "2026-11-04T09:00:00+00:00"},
            "seven_day_opus": null,
            "extra_usage": {"is_enabled": false}
        }"#;

        let windows = endpoint_windows(body).expect("windows");

        assert_eq!(
            windows,
            [
                UsageWindow {
                    label: "5h".to_owned(),
                    used_percent: 42,
                    resets_at: Some(1_793_383_200),
                },
                UsageWindow {
                    label: "7d".to_owned(),
                    used_percent: 14,
                    resets_at: Some(1_793_383_200 + 4 * 86_400 + 15 * 3600),
                },
            ]
        );
    }

    #[test]
    fn a_null_or_missing_utilization_leaves_the_window_out() {
        let body = br#"{
            "five_hour": {"utilization": null, "resets_at": null},
            "seven_day": {"resets_at": "2026-11-04T09:00:00+00:00"}
        }"#;

        assert_eq!(endpoint_windows(body), Some(Vec::new()));
        assert_eq!(
            endpoint_windows(br#"{"five_hour": null}"#),
            Some(Vec::new())
        );
    }

    #[test]
    fn an_out_of_range_utilization_is_clamped() {
        let body = br#"{"five_hour": {"utilization": 340}, "seven_day": {"utilization": -2}}"#;

        let percents: Vec<u8> = endpoint_windows(body)
            .expect("windows")
            .into_iter()
            .map(|window| window.used_percent)
            .collect();

        assert_eq!(percents, [100, 0]);
    }

    #[test]
    fn an_answer_that_is_not_an_object_is_refused() {
        assert_eq!(endpoint_windows(b"[]"), None);
        assert_eq!(endpoint_windows(b"not json"), None);
        assert_eq!(endpoint_windows(b""), None);
    }

    // The curl output.

    #[test]
    fn curl_output_gives_the_status_the_retry_after_and_the_body() {
        let raw =
            b"HTTP/2 429 \r\ncontent-type: application/json\r\nRetry-After: 120\r\n\r\n{\"a\":1}";

        let response = parse_response(raw).expect("response");

        assert_eq!(response.status, 429);
        assert_eq!(response.retry_after.as_deref(), Some("120"));
        assert_eq!(response.body, b"{\"a\":1}");
    }

    #[test]
    fn an_interim_answer_is_skipped() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\n\r\n{}";

        let response = parse_response(raw).expect("response");

        assert_eq!(
            (response.status, response.body.as_slice()),
            (200, &b"{}"[..])
        );
        assert_eq!(response.retry_after, None);
    }

    #[test]
    fn curl_output_without_a_status_line_is_refused() {
        assert!(parse_response(b"").is_none());
        assert!(parse_response(b"no headers here").is_none());
        assert!(parse_response(b"garbage\r\n\r\n{}").is_none());
    }

    #[test]
    fn retry_after_defaults_and_is_capped() {
        assert_eq!(retry_after(Some("2467")), 2467);
        assert_eq!(retry_after(Some(" 90 ")), 90);
        assert_eq!(retry_after(None), DEFAULT_RETRY);
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            DEFAULT_RETRY
        );
        assert_eq!(retry_after(Some("-5")), DEFAULT_RETRY);
        assert_eq!(retry_after(Some("99999")), MAX_RETRY);
    }

    // The token.

    #[test]
    fn the_token_debug_hides_the_secret() {
        let token = Token(SECRET.to_owned());

        let shown = format!("{token:?}");

        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn the_fingerprint_is_a_sha256_that_differs_per_token() {
        let fingerprint = Token("abc".to_owned()).fingerprint();

        assert_eq!(
            fingerprint,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(fingerprint, Token("abd".to_owned()).fingerprint());
    }

    #[test]
    fn a_token_with_a_control_character_is_not_sent() {
        assert!(curl_config(&Token("a\nheader = x".to_owned())).is_none());
        assert!(curl_config(&Token(String::new())).is_none());
    }

    #[test]
    fn the_curl_config_quotes_what_it_carries() {
        let config = curl_config(&Token("a\"b\\c".to_owned())).expect("config");

        assert!(
            config.contains(r#"header = "Authorization: Bearer a\"b\\c""#),
            "{config}"
        );
        assert!(config.contains(r#"url = "https://api.anthropic.com/api/oauth/usage""#));
        assert!(config.contains(r#"header = "anthropic-beta: oauth-2025-04-20""#));
    }

    // The endpoint with fake commands.

    /// Fake `security` and `curl`, and the folder they log to.
    struct Fakes {
        scripts: Scripts,
        commands: Commands,
    }

    impl Fakes {
        fn new() -> Self {
            let scripts = Scripts::new();
            let security = scripts.script(
                "security",
                r#"echo "$@" >> "$DIR/security-calls"
[ "$1" = find-generic-password ] && [ "$2" = -a ] && [ "$3" = tester ] && [ "$4" = -w ] \
  && [ "$5" = -s ] && [ "$6" = "Claude Code-credentials" ] || exit 9
if [ -f "$DIR/security-hangs" ]; then exec sleep 30; fi
cat "$DIR/credentials"
exit "$(cat "$DIR/security-exit" 2>/dev/null || echo 0)""#,
            );
            let curl = scripts.script(
                "curl",
                &format!(
                    r#"echo "$@" >> "$DIR/curl-args"
echo "$@" | grep -q '{SECRET}' && echo leaked >> "$DIR/leak"
cat > "$DIR/curl-config"
echo called >> "$DIR/curl-calls"
cat "$DIR/response"
exit "$(cat "$DIR/curl-exit" 2>/dev/null || echo 0)""#
                ),
            );
            let fakes = Self {
                scripts,
                commands: Commands { security, curl },
            };
            fakes.set_credentials(SECRET, Some((NOW + 86_400) * 1000), Some("max"));
            fakes.respond(
                200,
                &[],
                r#"{"five_hour":{"utilization":42,"resets_at":"2026-10-30T18:00:00Z"}}"#,
            );

            fakes
        }

        fn endpoint(&self) -> ClaudeEndpoint {
            ClaudeEndpoint::new(
                self.commands.clone(),
                Some("tester".to_owned()),
                self.scripts.dir().join("backoff.json"),
            )
        }

        fn set_credentials(&self, token: &str, expires_ms: Option<u64>, plan: Option<&str>) {
            // `expiresAt` is a float in the real item.
            let expires =
                expires_ms.map_or_else(String::new, |ms| format!(r#","expiresAt":{ms}.0"#));
            let plan = plan.map_or_else(String::new, |plan| {
                format!(r#","subscriptionType":"{plan}""#)
            });
            self.scripts.write(
                "credentials",
                &format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"r"{expires}{plan}}},"mcpOAuth":{{}}}}"#
                ),
            );
        }

        fn respond(&self, status: u16, headers: &[(&str, &str)], body: &str) {
            let mut text = format!("HTTP/2 {status} \r\n");
            for (name, value) in headers {
                write!(text, "{name}: {value}\r\n").ok();
            }
            write!(text, "\r\n{body}").ok();
            self.scripts.write("response", &text);
        }

        fn requests(&self) -> usize {
            self.scripts.lines("curl-calls")
        }
    }

    #[test]
    fn a_good_answer_gives_windows_and_the_plan() {
        let fakes = Fakes::new();

        let outcome = fakes.endpoint().poll(NOW, true).expect("outcome");

        assert_eq!(
            outcome,
            Ok(Reading {
                windows: vec![UsageWindow {
                    label: "5h".to_owned(),
                    used_percent: 42,
                    resets_at: Some(1_793_383_200),
                }],
                plan: Some("max".to_owned()),
            })
        );
    }

    #[test]
    fn the_token_goes_through_stdin_and_never_through_the_arguments() {
        let fakes = Fakes::new();

        fakes.endpoint().poll(NOW, true);

        let args = fakes.scripts.read("curl-args");
        let config = fakes.scripts.read("curl-config");
        assert_eq!(
            fakes.scripts.read("leak"),
            "",
            "the token was in the arguments"
        );
        assert!(!args.contains("secret"), "{args}");
        assert!(config.contains(&format!("Bearer {SECRET}")), "{config}");
        assert!(config.contains("https://api.anthropic.com/api/oauth/usage"));
        assert!(config.contains("anthropic-beta: oauth-2025-04-20"));
        for wanted in ["-K -", "--proto =https", "--max-redirs 0", "--max-time 5"] {
            assert!(args.contains(wanted), "{wanted} is missing from {args}");
        }
        assert!(
            !args.contains("api.anthropic.com"),
            "the URL was in the arguments"
        );
    }

    #[test]
    fn at_most_one_request_in_five_minutes() {
        let fakes = Fakes::new();
        let mut endpoint = fakes.endpoint();

        assert!(endpoint.poll(NOW, true).is_some());
        assert!(endpoint.poll(NOW + 1, true).is_none());
        assert!(endpoint.poll(NOW + 299, true).is_none());
        assert_eq!(fakes.requests(), 1);

        assert!(endpoint.poll(NOW + 300, true).is_some());
        assert_eq!(fakes.requests(), 2);
    }

    #[test]
    fn a_429_without_retry_after_waits_five_minutes() {
        let fakes = Fakes::new();
        fakes.respond(429, &[], r#"{"error":{"type":"rate_limit_error"}}"#);
        let mut endpoint = fakes.endpoint();

        let first = endpoint.poll(NOW, true);

        assert_eq!(
            first,
            Some(Err(UsageProblem::RateLimited {
                retry_at: NOW + 300
            }))
        );
        assert_eq!(
            endpoint.poll(NOW + 100, true),
            Some(Err(UsageProblem::RateLimited {
                retry_at: NOW + 300
            }))
        );
        assert_eq!(fakes.requests(), 1);
    }

    #[test]
    fn a_429_waits_as_long_as_retry_after_says() {
        let fakes = Fakes::new();
        fakes.respond(429, &[("Retry-After", "1774")], "{}");
        let mut endpoint = fakes.endpoint();

        let first = endpoint.poll(NOW, true);
        let during = endpoint.poll(NOW + 1773, true);
        fakes.respond(200, &[], "{}");
        let after = endpoint.poll(NOW + 1774, true);

        assert_eq!(
            first,
            Some(Err(UsageProblem::RateLimited {
                retry_at: NOW + 1774
            }))
        );
        assert_eq!(
            during,
            Some(Err(UsageProblem::RateLimited {
                retry_at: NOW + 1774
            }))
        );
        assert!(matches!(after, Some(Ok(_))));
        assert_eq!(fakes.requests(), 2);
    }

    #[test]
    fn a_403_is_treated_like_a_429() {
        let fakes = Fakes::new();
        fakes.respond(403, &[("retry-after", "60")], "{}");

        let outcome = fakes.endpoint().poll(NOW, true);

        assert_eq!(
            outcome,
            Some(Err(UsageProblem::RateLimited { retry_at: NOW + 60 }))
        );
    }

    #[test]
    fn a_retry_after_over_one_hour_is_cut_to_one_hour() {
        let fakes = Fakes::new();
        fakes.respond(429, &[("Retry-After", "86400")], "{}");

        let outcome = fakes.endpoint().poll(NOW, true);

        assert_eq!(
            outcome,
            Some(Err(UsageProblem::RateLimited {
                retry_at: NOW + 3600
            }))
        );
    }

    #[test]
    fn a_401_waits_for_a_different_token() {
        let fakes = Fakes::new();
        fakes.respond(401, &[], "{}");
        let mut endpoint = fakes.endpoint();

        let first = endpoint.poll(NOW, true);
        let later = endpoint.poll(NOW + 600, true);
        let much_later = endpoint.poll(NOW + 7200, true);
        assert_eq!(fakes.requests(), 1, "the same token must not be sent again");

        fakes.set_credentials("sk-ant-oat01-fresh", Some((NOW + 90_000) * 1000), None);
        fakes.respond(200, &[], r#"{"five_hour":{"utilization":1}}"#);
        let fresh = endpoint.poll(NOW + 7260, true);

        assert_eq!(first, Some(Err(UsageProblem::Unauthorized)));
        assert_eq!(later, Some(Err(UsageProblem::Unauthorized)));
        assert_eq!(much_later, Some(Err(UsageProblem::Unauthorized)));
        assert!(matches!(fresh, Some(Ok(_))));
        assert_eq!(fakes.requests(), 2);
    }

    #[test]
    fn an_expired_token_makes_no_request_and_no_problem() {
        let fakes = Fakes::new();
        fakes.set_credentials(SECRET, Some((NOW - 1) * 1000), Some("max"));
        let mut endpoint = fakes.endpoint();

        let outcome = endpoint.poll(NOW, true);

        assert_eq!(outcome, None);
        assert_eq!(fakes.requests(), 0);
    }

    #[test]
    fn a_refreshed_token_is_used_at_the_next_look() {
        let fakes = Fakes::new();
        fakes.set_credentials(SECRET, Some((NOW - 1) * 1000), None);
        let mut endpoint = fakes.endpoint();
        assert_eq!(endpoint.poll(NOW, true), None);

        fakes.set_credentials("sk-ant-oat01-new", Some((NOW + 3600) * 1000), None);
        let outcome = endpoint.poll(NOW + KEYCHAIN_RECHECK, true);

        assert!(matches!(outcome, Some(Ok(_))));
        assert_eq!(fakes.requests(), 1);
    }

    #[test]
    fn the_keychain_is_not_read_on_every_tick() {
        let fakes = Fakes::new();
        fakes.set_credentials(SECRET, Some((NOW - 1) * 1000), None);
        let mut endpoint = fakes.endpoint();

        for second in 0..30 {
            endpoint.poll(NOW + second, true);
        }

        assert_eq!(fakes.scripts.lines("security-calls"), 1);
    }

    #[test]
    fn the_backoff_survives_a_restart() {
        let fakes = Fakes::new();
        fakes.respond(429, &[("Retry-After", "1000")], "{}");
        assert!(fakes.endpoint().poll(NOW, true).is_some());

        let mut restarted = fakes.endpoint();
        let during = restarted.poll(NOW + 10, true);

        assert_eq!(
            during,
            Some(Err(UsageProblem::RateLimited {
                retry_at: NOW + 1000
            }))
        );
        assert_eq!(fakes.requests(), 1);
    }

    #[test]
    fn the_five_minute_spacing_survives_a_restart() {
        let fakes = Fakes::new();
        assert!(fakes.endpoint().poll(NOW, true).is_some());

        let mut restarted = fakes.endpoint();

        assert_eq!(restarted.poll(NOW + 20, true), None);
        assert_eq!(fakes.requests(), 1);
        assert!(restarted.poll(NOW + 300, true).is_some());
    }

    #[test]
    fn a_rejected_token_survives_a_restart_and_the_file_has_no_token() {
        let fakes = Fakes::new();
        fakes.respond(401, &[], "{}");
        assert!(fakes.endpoint().poll(NOW, true).is_some());

        let mut restarted = fakes.endpoint();
        let outcome = restarted.poll(NOW + 4000, true);

        assert_eq!(outcome, Some(Err(UsageProblem::Unauthorized)));
        assert_eq!(fakes.requests(), 1);
        let saved = fakes.scripts.read("backoff.json");
        assert!(!saved.contains("secret"), "{saved}");
        assert!(saved.contains(&Token(SECRET.to_owned()).fingerprint()));
    }

    #[test]
    fn the_state_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let fakes = Fakes::new();
        fakes.endpoint().poll(NOW, true);

        let mode = std::fs::metadata(fakes.scripts.dir().join("backoff.json"))
            .expect("state file")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_broken_state_file_is_ignored() {
        let fakes = Fakes::new();
        fakes.scripts.write("backoff.json", "{ nope");

        let outcome = fakes.endpoint().poll(NOW, true);

        assert!(matches!(outcome, Some(Ok(_))));
    }

    #[test]
    fn a_missing_claude_is_not_installed() {
        let fakes = Fakes::new();

        let outcome = fakes.endpoint().poll(NOW, false);

        assert_eq!(outcome, Some(Err(UsageProblem::NotInstalled)));
        assert_eq!(fakes.requests(), 0);
    }

    #[test]
    fn a_missing_keychain_item_is_not_logged_in() {
        let fakes = Fakes::new();
        fakes.scripts.write("security-exit", "44");
        fakes.scripts.write("credentials", "");

        let outcome = fakes.endpoint().poll(NOW, true);

        assert_eq!(outcome, Some(Err(UsageProblem::NotLoggedIn)));
        assert_eq!(fakes.requests(), 0);
    }

    #[test]
    fn a_keychain_item_without_oauth_is_not_logged_in() {
        let fakes = Fakes::new();
        fakes.scripts.write("credentials", r#"{"mcpOAuth":{}}"#);

        let outcome = fakes.endpoint().poll(NOW, true);

        assert_eq!(outcome, Some(Err(UsageProblem::NotLoggedIn)));
    }

    #[test]
    fn a_keychain_item_that_is_not_json_is_a_short_failure_without_the_secret() {
        let fakes = Fakes::new();
        fakes.scripts.write("credentials", SECRET);

        let outcome = fakes.endpoint().poll(NOW, true);

        let Some(Err(UsageProblem::Failed(text))) = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert!(!text.contains("secret"), "{text}");
    }

    #[test]
    fn a_keychain_that_does_not_answer_says_a_prompt_is_open() {
        let fakes = Fakes::new();
        fakes.scripts.write("security-hangs", "");

        let started = std::time::Instant::now();
        let outcome = read_credential(&fakes.commands, Some("tester"), Duration::from_millis(300));

        assert_eq!(
            outcome.err(),
            Some(UsageProblem::Failed("Keychain prompt open".to_owned()))
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_failing_curl_is_a_short_failure_and_still_counts_as_a_request() {
        let fakes = Fakes::new();
        fakes.scripts.write("curl-exit", "7");
        let mut endpoint = fakes.endpoint();

        let first = endpoint.poll(NOW, true);
        let second = endpoint.poll(NOW + 10, true);

        assert_eq!(
            first,
            Some(Err(UsageProblem::Failed("Request failed".to_owned())))
        );
        assert_eq!(second, None);
        assert_eq!(fakes.requests(), 1);
    }

    #[test]
    fn a_curl_that_times_itself_out_says_so() {
        let fakes = Fakes::new();
        fakes.scripts.write("curl-exit", "28");

        let outcome = fakes.endpoint().poll(NOW, true);

        assert_eq!(
            outcome,
            Some(Err(UsageProblem::Failed("Request timed out".to_owned())))
        );
    }

    #[test]
    fn other_statuses_and_bad_bodies_are_short_failures() {
        let fakes = Fakes::new();
        fakes.respond(500, &[], "oops");
        let server_error = fakes.endpoint().poll(NOW, true);
        std::fs::remove_file(fakes.scripts.dir().join("backoff.json")).ok();
        fakes.respond(200, &[], "<html>");

        let bad_body = fakes.endpoint().poll(NOW, true);

        assert_eq!(
            server_error,
            Some(Err(UsageProblem::Failed("HTTP 500".to_owned())))
        );
        assert_eq!(
            bad_body,
            Some(Err(UsageProblem::Failed("Unexpected response".to_owned())))
        );
    }
}
