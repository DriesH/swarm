//! Claude usage from the status line input of a running Claude Code. It is the only
//! source: Swarm never reads Claude Code's sign-in token and never calls Anthropic's
//! usage endpoint.

use serde_json::Value;
use swarm_protocol::UsageWindow;

use crate::{percent, unix_seconds};

/// Windows of the status line input: the JSON key, the label and the length in seconds,
/// shortest first. The only place that knows these lengths.
const STATUS_LINE_WINDOWS: [(&str, &str, u64); 2] = [
    ("five_hour", "5h", 5 * 60 * 60),
    ("seven_day", "7d", 7 * 24 * 60 * 60),
];

/// The length of a window by its label, for windows saved before lengths were kept. Only
/// the status line windows are known.
pub fn length_of_label(label: &str) -> Option<u64> {
    STATUS_LINE_WINDOWS
        .iter()
        .find(|(_, known, _)| *known == label)
        .map(|(_, _, length_secs)| *length_secs)
}

/// The `rate_limits` of Claude Code's status line input, shortest window first. A
/// window without a percentage is left out.
pub fn status_line_windows(input: &Value) -> Vec<UsageWindow> {
    let Some(limits) = input.get("rate_limits") else {
        return Vec::new();
    };

    STATUS_LINE_WINDOWS
        .iter()
        .filter_map(|(key, label, length_secs)| {
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
                length_secs: Some(*length_secs),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn windows(input: &Value) -> Vec<(String, u8, Option<u64>)> {
        status_line_windows(input)
            .into_iter()
            .map(|window| (window.label, window.used_percent, window.resets_at))
            .collect()
    }

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
    fn the_status_line_windows_know_their_length() {
        let input = json!({"rate_limits": {
            "five_hour": {"used_percentage": 1, "resets_at": 1},
            "seven_day": {"used_percentage": 2},
        }});

        let lengths: Vec<Option<u64>> = status_line_windows(&input)
            .into_iter()
            .map(|window| window.length_secs)
            .collect();

        assert_eq!(lengths, [Some(18_000), Some(604_800)]);
    }

    #[test]
    fn only_the_five_hour_and_seven_day_labels_have_a_known_length() {
        assert_eq!(length_of_label("5h"), Some(18_000));
        assert_eq!(length_of_label("7d"), Some(604_800));
        assert_eq!(length_of_label("30d"), None);
        assert_eq!(length_of_label("limit"), None);
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
}
