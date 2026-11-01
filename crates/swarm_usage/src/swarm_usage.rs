//! Subscription usage for Swarm: Claude windows from the status line of Swarm's own
//! Claude agents, and Codex windows from `codex app-server`.
//!
//! The crate has no GPUI and no network of its own, and it never touches a sign-in
//! token. `codex` is passed in as a path, so tests run a fake script.

mod claude;
mod codex;
mod process;

use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use swarm_protocol::{UsageProblem, UsageWindow};

pub use claude::status_line_windows;
pub use codex::{codex_usage, window_label};
pub use process::{Finished, RunError, run};

/// One good read of the usage of a subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// Shortest window first.
    pub windows: Vec<UsageWindow>,
    /// The plan name of the subscription, like `max` or `plus`.
    pub plan: Option<String>,
}

pub type Outcome = Result<Reading, UsageProblem>;

pub(crate) const FIVE_HOURS: u64 = 5 * 60 * 60;
pub(crate) const SEVEN_DAYS: u64 = 7 * 24 * 60 * 60;

/// The length of a window by its label, for windows saved before lengths were kept. Only
/// `5h` and `7d` are known.
pub fn length_of_label(label: &str) -> Option<u64> {
    match label {
        "5h" => Some(FIVE_HOURS),
        "7d" => Some(SEVEN_DAYS),
        _ => None,
    }
}

/// Writes the file whole or not at all, readable by the owner only.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;

    std::fs::rename(&temporary, path)
}

/// Rounds a percent and keeps it between 0 and 100.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=100 first, and NaN casts to 0"
)]
pub(crate) fn percent(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

/// Unix seconds from a JSON number. Negative and non-finite values are not times.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is finite and not negative; a huge value saturates"
)]
pub(crate) fn unix_seconds(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0).then_some(value as u64)
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_write_replaces_the_file_and_keeps_it_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let scripts = fixtures::Scripts::new();
        let path = scripts.dir().join("state.json");

        write_private(&path, b"one").expect("first write");
        write_private(&path, b"two").expect("second write");

        assert_eq!(scripts.read("state.json"), "two");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!scripts.dir().join("state.tmp").exists());
    }

    #[test]
    fn only_the_five_hour_and_seven_day_labels_have_a_known_length() {
        assert_eq!(length_of_label("5h"), Some(18_000));
        assert_eq!(length_of_label("7d"), Some(604_800));
        assert_eq!(length_of_label("30d"), None);
        assert_eq!(length_of_label("limit"), None);
    }

    #[test]
    fn a_percent_is_rounded_and_clamped() {
        assert_eq!(percent(41.5), 42);
        assert_eq!(percent(41.4), 41);
        assert_eq!(percent(-3.0), 0);
        assert_eq!(percent(250.0), 100);
        assert_eq!(percent(f64::NAN), 0);
    }

    #[test]
    fn a_time_must_be_a_finite_number_that_is_not_negative() {
        assert_eq!(unix_seconds(1_793_383_200.0), Some(1_793_383_200));
        assert_eq!(unix_seconds(-1.0), None);
        assert_eq!(unix_seconds(f64::NAN), None);
        assert_eq!(unix_seconds(f64::INFINITY), None);
    }
}
