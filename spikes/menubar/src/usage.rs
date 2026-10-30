//! Shared usage types and the poll schedule.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// One rate-limit window, e.g. "5-hour" or "Weekly".
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    /// 0–100.
    pub used_percent: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    pub fetched_at: DateTime<Utc>,
}

/// Why a poll did not give fresh data. Shown in the popover as a short status.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    NotLoggedIn,
    /// The stored token is past `expiresAt`; its owner (Claude Code / Codex) refreshes it.
    TokenExpired,
    /// The server rejected the token. Not retried until the stored token changes.
    Unauthorized,
    RateLimited {
        retry_at: DateTime<Utc>,
    },
    Other(String),
}

impl Problem {
    pub fn label(&self, now: DateTime<Utc>) -> String {
        match self {
            Problem::NotLoggedIn => "Not logged in".into(),
            Problem::TokenExpired => "Waiting for token refresh".into(),
            Problem::Unauthorized => "Token rejected".into(),
            Problem::RateLimited { retry_at } => {
                format!("Rate limited, retry in {}", short_duration(*retry_at - now))
            }
            Problem::Other(msg) => msg.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SourceState {
    pub usage: Option<Usage>,
    pub problem: Option<Problem>,
}

impl SourceState {
    pub fn apply(&mut self, result: Result<Usage, Problem>) {
        match result {
            Ok(usage) => {
                self.usage = Some(usage);
                self.problem = None;
            }
            // Keep the last good data; it is still the best estimate.
            Err(problem) => self.problem = Some(problem),
        }
    }

    pub fn max_percent(&self) -> Option<f32> {
        self.usage
            .as_ref()?
            .windows
            .iter()
            .map(|w| w.used_percent)
            .reduce(f32::max)
    }
}

/// Polling at most this often keeps us well under the Claude endpoint's limit.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Fallback when a 429 has no usable `Retry-After`.
pub const DEFAULT_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// Claude Code itself never holds off longer than this after a 429.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);

pub fn backoff_from_retry_after(header: Option<&str>) -> Duration {
    header
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BACKOFF)
        .min(MAX_BACKOFF)
}

pub fn short_duration(d: chrono::TimeDelta) -> String {
    let mins = d.num_minutes().max(0);
    match mins {
        0 => "<1m".into(),
        1..60 => format!("{mins}m"),
        60..1440 => format!("{}h {}m", mins / 60, mins % 60),
        _ => format!("{}d {}h", mins / 1440, (mins % 1440) / 60),
    }
}

/// Label for a window length in minutes (Codex reports lengths, not names).
pub fn window_label(minutes: i64) -> String {
    match minutes {
        300 => "5-hour".into(),
        10080 => "Weekly".into(),
        43200 => "Monthly".into(),
        m if m % 1440 == 0 => format!("{}-day", m / 1440),
        m if m % 60 == 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_should_use_retry_after_seconds() {
        assert_eq!(
            backoff_from_retry_after(Some("2467")),
            Duration::from_secs(2467)
        );
    }

    #[test]
    fn backoff_should_cap_at_one_hour() {
        assert_eq!(backoff_from_retry_after(Some("99999")), MAX_BACKOFF);
    }

    #[test]
    fn backoff_should_default_when_header_is_a_date() {
        let header = Some("Wed, 21 Oct 2026 07:28:00 GMT");
        assert_eq!(backoff_from_retry_after(header), DEFAULT_BACKOFF);
    }

    #[test]
    fn window_label_should_name_codex_windows() {
        let labels: Vec<_> = [300, 10080, 43200, 120].map(window_label).into();
        assert_eq!(labels, ["5-hour", "Weekly", "Monthly", "2-hour"]);
    }

    #[test]
    fn short_duration_should_show_hours_and_minutes() {
        assert_eq!(short_duration(chrono::TimeDelta::minutes(125)), "2h 5m");
    }
}
