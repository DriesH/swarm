//! Codex usage through `codex app-server` and its `account/rateLimits/read` call.
//!
//! Codex reads and refreshes its own token, so Swarm never handles it.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use swarm_protocol::{UsageProblem, UsageWindow};

use crate::{Outcome, Reading, percent, process, unix_seconds};

/// The ID of the `account/rateLimits/read` request.
const RATE_LIMITS_ID: u64 = 2;

/// The longest `Failed` text taken from Codex.
const MAX_MESSAGE_CHARS: usize = 80;

/// How long the app server gets to end by itself once its input is closed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// How often the app server is checked while it ends.
const POLL: Duration = Duration::from_millis(10);

/// Kills and reaps the app server when it goes out of scope, also on a timeout.
struct Server(Child);

impl Server {
    /// Waits up to `grace` for the server to end by itself.
    fn wait_for_exit(&mut self, grace: Duration) {
        let deadline = Instant::now() + grace;

        loop {
            match self.0.try_wait() {
                Ok(None) if Instant::now() < deadline => thread::park_timeout(POLL),
                Ok(_) => return,
                Err(error) => {
                    log::debug!("cannot check codex: {error}");

                    return;
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Err(error) = self.0.kill() {
            log::debug!("cannot kill codex: {error}");
        }
        if let Err(error) = self.0.wait() {
            log::debug!("cannot reap codex: {error}");
        }
    }
}

/// The label of a window that lasts `minutes`: `30m`, `5h`, `7d`.
pub fn window_label(minutes: u64) -> String {
    match minutes {
        0..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// Asks Codex for its rate limits, or says it is not installed when `codex` is `None`.
/// Blocks for at most `timeout` and, after an answer, up to [`EXIT_GRACE`] more.
pub fn codex_usage(codex: Option<&Path>, timeout: Duration) -> Outcome {
    let Some(codex) = codex else {
        return Err(UsageProblem::NotInstalled);
    };

    let answer = ask(codex, timeout)?;

    parse_answer(&answer)
}

/// Starts `codex app-server`, sends the three messages and waits for the answer to the
/// last one. After the answer the server gets a moment to end by itself; on every other
/// way out it is killed at once.
fn ask(codex: &Path, timeout: Duration) -> Result<Value, UsageProblem> {
    let deadline = Instant::now() + timeout;

    let mut server = Server(
        process::spawn(
            Command::new(codex)
                .arg("app-server")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .map_err(|error| {
            log::warn!("cannot start codex: {error}");
            failed("Codex did not start")
        })?,
    );

    let (Some(mut stdin), Some(stdout)) = (server.0.stdin.take(), server.0.stdout.take()) else {
        return Err(failed("Codex did not start"));
    };

    let messages = [
        json!({
            "id": 1,
            "method": "initialize",
            "params": {"clientInfo": {"name": "swarm", "version": env!("CARGO_PKG_VERSION")}},
        }),
        json!({"method": "initialized"}),
        json!({"id": RATE_LIMITS_ID, "method": "account/rateLimits/read"}),
    ];
    for message in messages {
        if let Err(error) = writeln!(stdin, "{message}").and_then(|()| stdin.flush()) {
            log::warn!("cannot write to codex: {error}");

            return Err(failed("Codex did not answer"));
        }
    }

    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("codex-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };

                if message.get("id").and_then(Value::as_u64) == Some(RATE_LIMITS_ID) {
                    sender.send(message).ok();
                    break;
                }
            }
        })
        .map_err(|error| {
            log::warn!("cannot read from codex: {error}");
            failed("Codex did not answer")
        })?;

    // `stdin` stays open until the answer is here: Codex ends when it reads end of file.
    let answer = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    drop(stdin);
    if answer.is_ok() {
        server.wait_for_exit(EXIT_GRACE);
    }

    answer.map_err(|error| {
        failed(match error {
            mpsc::RecvTimeoutError::Timeout => "Codex did not answer",
            mpsc::RecvTimeoutError::Disconnected => "Codex ended without an answer",
        })
    })
}

fn failed(text: impl Into<String>) -> UsageProblem {
    UsageProblem::Failed(text.into())
}

fn parse_answer(answer: &Value) -> Outcome {
    if let Some(error) = answer.get("error") {
        return Err(problem_of(error));
    }

    let limits = answer
        .pointer("/result/rateLimits")
        .filter(|limits| limits.is_object())
        .ok_or_else(|| failed("Unexpected answer from Codex"))?;

    let mut windows: Vec<UsageWindow> = ["primary", "secondary"]
        .into_iter()
        .filter_map(|key| window(limits.get(key)?))
        .collect();
    // The shortest first; a window of unknown length goes last.
    windows.sort_by_key(|window| window.length_secs.unwrap_or(u64::MAX));

    Ok(Reading {
        windows,
        plan: limits
            .get("planType")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// `None` when the window has no percentage.
fn window(value: &Value) -> Option<UsageWindow> {
    let used = value.get("usedPercent")?.as_f64()?;
    let minutes = value
        .get("windowDurationMins")
        .and_then(Value::as_f64)
        .and_then(unix_seconds)
        .filter(|minutes| *minutes > 0);
    let resets_at = value
        .get("resetsAt")
        .and_then(Value::as_f64)
        .and_then(unix_seconds);

    Some(UsageWindow {
        label: minutes.map_or_else(|| "limit".to_owned(), window_label),
        used_percent: percent(used),
        resets_at,
        length_secs: minutes.and_then(|minutes| minutes.checked_mul(60)),
    })
}

/// A JSON-RPC error: no login is `NotLoggedIn`, the rest is a short `Failed`.
fn problem_of(error: &Value) -> UsageProblem {
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    let lower = message.to_lowercase();

    let logged_out = ["auth", "log in", "login", "logged in", "sign in"]
        .iter()
        .any(|word| lower.contains(word));
    if logged_out {
        return UsageProblem::NotLoggedIn;
    }

    let first_line = message.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return failed("Codex error");
    }

    failed(
        first_line
            .chars()
            .take(MAX_MESSAGE_CHARS)
            .collect::<String>(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::fixtures::Scripts;

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn labels_come_from_the_minutes() {
        let labels: Vec<String> = [1, 30, 59, 60, 300, 1439, 1440, 10_080, 43_200]
            .map(window_label)
            .into();

        assert_eq!(
            labels,
            ["1m", "30m", "59m", "1h", "5h", "23h", "1d", "7d", "30d"]
        );
    }

    fn answer(limits: &Value) -> Value {
        json!({"id": 2, "result": {"rateLimits": limits}})
    }

    fn windows(outcome: Outcome) -> Vec<(String, u8, Option<u64>)> {
        outcome
            .expect("reading")
            .windows
            .into_iter()
            .map(|window| (window.label, window.used_percent, window.resets_at))
            .collect()
    }

    fn lengths(outcome: Outcome) -> Vec<Option<u64>> {
        outcome
            .expect("reading")
            .windows
            .into_iter()
            .map(|window| window.length_secs)
            .collect()
    }

    #[test]
    fn primary_and_secondary_become_windows_with_the_plan() {
        let reading = parse_answer(&answer(&json!({
            "limitId": "codex",
            "planType": "plus",
            "primary": {"usedPercent": 12.4, "windowDurationMins": 300, "resetsAt": 1_793_383_200_u64},
            "secondary": {"usedPercent": 40, "windowDurationMins": 10_080, "resetsAt": null},
        })))
        .expect("reading");

        assert_eq!(reading.plan.as_deref(), Some("plus"));
        assert_eq!(
            windows(Ok(reading)),
            [
                ("5h".to_owned(), 12, Some(1_793_383_200)),
                ("7d".to_owned(), 40, None),
            ]
        );
    }

    #[test]
    fn a_window_length_is_the_duration_in_seconds() {
        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 1, "windowDurationMins": 300},
            "secondary": {"usedPercent": 2, "windowDurationMins": 10_080},
        })));
        assert_eq!(lengths(outcome), [Some(18_000), Some(604_800)]);

        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 3, "windowDurationMins": 43_200},
            "secondary": {"usedPercent": 4, "windowDurationMins": 45},
        })));
        assert_eq!(lengths(outcome), [Some(2_700), Some(2_592_000)]);
    }

    #[test]
    fn a_window_without_a_duration_has_no_length() {
        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 1},
            "secondary": {"usedPercent": 2, "windowDurationMins": 0},
        })));
        assert_eq!(lengths(outcome), [None, None]);

        let huge = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 1, "windowDurationMins": 1e19},
        })));
        assert_eq!(lengths(huge), [None]);
    }

    #[test]
    fn a_single_monthly_window_is_labelled_30d() {
        let outcome = parse_answer(&answer(&json!({
            "planType": "go",
            "primary": {"usedPercent": 3, "windowDurationMins": 43_200, "resetsAt": 5},
            "secondary": null,
        })));

        assert_eq!(windows(outcome), [("30d".to_owned(), 3, Some(5))]);
    }

    #[test]
    fn the_shortest_window_comes_first() {
        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 1, "windowDurationMins": 10_080},
            "secondary": {"usedPercent": 2, "windowDurationMins": 300},
        })));

        let labels: Vec<String> = windows(outcome)
            .into_iter()
            .map(|window| window.0)
            .collect();

        assert_eq!(labels, ["5h", "7d"]);
    }

    #[test]
    fn a_percent_out_of_range_is_clamped() {
        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 120.0, "windowDurationMins": 300},
            "secondary": {"usedPercent": -1, "windowDurationMins": 10_080},
        })));

        let percents: Vec<u8> = windows(outcome)
            .into_iter()
            .map(|window| window.1)
            .collect();

        assert_eq!(percents, [100, 0]);
    }

    #[test]
    fn a_window_without_a_length_is_kept_last_with_a_plain_label() {
        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 1},
            "secondary": {"usedPercent": 2, "windowDurationMins": 0},
        })));
        let labels: Vec<String> = windows(outcome)
            .into_iter()
            .map(|window| window.0)
            .collect();

        assert_eq!(labels, ["limit", "limit"]);

        let outcome = parse_answer(&answer(&json!({
            "primary": {"usedPercent": 1},
            "secondary": {"usedPercent": 2, "windowDurationMins": 300},
        })));
        let labels: Vec<String> = windows(outcome)
            .into_iter()
            .map(|window| window.0)
            .collect();

        assert_eq!(labels, ["5h", "limit"]);
    }

    #[test]
    fn a_window_without_a_percent_is_left_out() {
        let outcome = parse_answer(&answer(&json!({
            "primary": {"windowDurationMins": 300},
            "secondary": {"usedPercent": null, "windowDurationMins": 10_080},
        })));

        assert!(windows(outcome).is_empty());
    }

    #[test]
    fn answers_without_rate_limits_are_unexpected() {
        for body in [
            json!({"id": 2, "result": {}}),
            json!({"id": 2, "result": {"rateLimits": null}}),
            json!({"id": 2, "result": null}),
            json!({"id": 2}),
        ] {
            let outcome = parse_answer(&body);

            assert_eq!(
                outcome,
                Err(UsageProblem::Failed(
                    "Unexpected answer from Codex".to_owned()
                )),
                "{body}"
            );
        }
    }

    #[test]
    fn a_login_error_is_not_logged_in() {
        for message in [
            "codex account authentication required to read rate limits",
            "Please log in",
            "not logged in",
        ] {
            let outcome =
                parse_answer(&json!({"id": 2, "error": {"code": -32600, "message": message}}));

            assert_eq!(outcome, Err(UsageProblem::NotLoggedIn), "{message}");
        }
    }

    #[test]
    fn another_error_is_a_short_failure() {
        let long = format!("boom\nsecond line {}", "x".repeat(300));

        let outcome = parse_answer(&json!({"id": 2, "error": {"message": long}}));
        let empty = parse_answer(&json!({"id": 2, "error": {}}));

        assert_eq!(outcome, Err(UsageProblem::Failed("boom".to_owned())));
        assert_eq!(empty, Err(UsageProblem::Failed("Codex error".to_owned())));

        let long_line = "y".repeat(300);
        let Err(UsageProblem::Failed(text)) =
            parse_answer(&json!({"id": 2, "error": {"message": long_line}}))
        else {
            panic!("expected a failure");
        };
        assert_eq!(text.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn a_missing_codex_is_not_installed() {
        assert_eq!(codex_usage(None, TIMEOUT), Err(UsageProblem::NotInstalled));
    }

    // Fake `codex app-server` scripts.

    /// A fake that checks the handshake: `initialize`, `initialized`, then the read. It
    /// answers `initialize` and the read, as the real server does.
    fn fake_codex(scripts: &Scripts, read_answer: &str) -> PathBuf {
        scripts.write("answer", &format!("{read_answer}\n"));

        scripts.script(
            "codex",
            r#"[ "$1" = app-server ] || exit 2
echo started >> "$DIR/starts"
step=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      case "$line" in *'"clientInfo"'*) ;; *) echo "no clientInfo" >> "$DIR/errors" ;; esac
      [ "$step" = 0 ] || echo "initialize out of order" >> "$DIR/errors"
      step=1
      printf '{"id":1,"result":{"userAgent":"fake"}}\n'
      ;;
    *'"method":"initialized"'*)
      [ "$step" = 1 ] || echo "initialized out of order" >> "$DIR/errors"
      step=2
      ;;
    *'account/rateLimits/read'*)
      [ "$step" = 2 ] || echo "read out of order" >> "$DIR/errors"
      printf '{"method":"account/rateLimits/updated","params":{}}\n'
      cat "$DIR/answer"
      ;;
  esac
done"#,
        )
    }

    #[test]
    fn the_fake_server_speaks_json_rpc_in_order() {
        let scripts = Scripts::new();
        let codex = fake_codex(
            &scripts,
            r#"{"id":2,"result":{"rateLimits":{"planType":"pro","primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":1793383200},"secondary":{"usedPercent":55,"windowDurationMins":10080,"resetsAt":1793900000}}}}"#,
        );

        let reading = codex_usage(Some(&codex), TIMEOUT).expect("reading");

        assert_eq!(reading.plan.as_deref(), Some("pro"));
        assert_eq!(
            windows(Ok(reading)),
            [
                ("5h".to_owned(), 7, Some(1_793_383_200)),
                ("7d".to_owned(), 55, Some(1_793_900_000)),
            ]
        );
        assert_eq!(scripts.read("errors"), "");
    }

    #[test]
    fn a_not_logged_in_server_gives_not_logged_in() {
        let scripts = Scripts::new();
        let codex = fake_codex(
            &scripts,
            r#"{"id":2,"error":{"code":-32600,"message":"codex account authentication required to read rate limits"}}"#,
        );

        let outcome = codex_usage(Some(&codex), TIMEOUT);

        assert_eq!(outcome, Err(UsageProblem::NotLoggedIn));
    }

    #[test]
    fn a_server_that_never_answers_times_out_and_is_stopped() {
        let scripts = Scripts::new();
        let codex = scripts.script(
            "codex",
            r#"[ "$1" = warm-up ] && exit 0
echo $$ > "$DIR/pid"
exec sleep 30"#,
        );
        Scripts::warm_up(&codex);

        let started = Instant::now();
        let outcome = codex_usage(Some(&codex), Duration::from_millis(400));

        assert_eq!(
            outcome,
            Err(UsageProblem::Failed("Codex did not answer".to_owned()))
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = scripts.read("pid");
        assert!(!pid.is_empty());
        assert!(
            !Scripts::process_exists(pid.trim()),
            "the server still runs"
        );
    }

    #[test]
    fn a_server_that_stays_after_a_good_answer_is_stopped_once_the_grace_is_over() {
        let scripts = Scripts::new();
        scripts.write(
            "answer",
            "{\"id\":2,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":1,\"windowDurationMins\":300}}}}\n",
        );
        let codex = scripts.script(
            "codex",
            r#"echo $$ > "$DIR/pid"
while IFS= read -r line; do
  case "$line" in *account/rateLimits/read*) cat "$DIR/answer"; exec sleep 30;; esac
done"#,
        );

        let started = Instant::now();
        let outcome = codex_usage(Some(&codex), TIMEOUT);

        assert!(outcome.is_ok());
        assert!(started.elapsed() >= EXIT_GRACE, "{:?}", started.elapsed());
        assert!(started.elapsed() < Duration::from_secs(10));
        let pid = scripts.read("pid");
        assert!(
            !Scripts::process_exists(pid.trim()),
            "the server still runs"
        );
    }

    #[test]
    fn the_server_gets_time_to_end_by_itself_after_its_input_is_closed() {
        let scripts = Scripts::new();
        scripts.write(
            "answer",
            "{\"id\":2,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":1,\"windowDurationMins\":300}}}}\n",
        );
        let codex = scripts.script(
            "codex",
            r#"while IFS= read -r line; do
  case "$line" in *account/rateLimits/read*) cat "$DIR/answer";; esac
done
sleep 0.3
echo ended > "$DIR/clean-exit""#,
        );

        let outcome = codex_usage(Some(&codex), TIMEOUT);

        assert!(outcome.is_ok());
        assert_eq!(scripts.read("clean-exit"), "ended\n");
    }

    #[test]
    fn a_server_that_ends_without_an_answer_is_a_short_failure() {
        let scripts = Scripts::new();
        let codex = scripts.script("codex", "exit 1");

        let outcome = codex_usage(Some(&codex), TIMEOUT);

        assert!(
            matches!(outcome, Err(UsageProblem::Failed(_))),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_program_that_cannot_start_is_a_short_failure() {
        let outcome = codex_usage(Some(Path::new("/nonexistent/codex")), TIMEOUT);

        assert_eq!(
            outcome,
            Err(UsageProblem::Failed("Codex did not start".to_owned()))
        );
    }

    #[test]
    fn output_that_is_not_json_is_skipped() {
        let scripts = Scripts::new();
        let codex = scripts.script(
            "codex",
            r#"printf 'warming up\n'
while IFS= read -r line; do
  case "$line" in *account/rateLimits/read*)
    printf 'not json\n'
    printf '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":9,"windowDurationMins":60}}}}\n';;
  esac
done"#,
        );

        let outcome = codex_usage(Some(&codex), TIMEOUT);

        assert_eq!(windows(outcome), [("1h".to_owned(), 9, None)]);
    }
}
