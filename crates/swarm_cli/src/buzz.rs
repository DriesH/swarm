//! `swarm buzz`: a hidden command that prints the pixel bee in color on a terminal. Inside
//! an agent it also prints the swarm's counts. It never fails and always exits 0.

use std::ffi::OsStr;
use std::io::{IsTerminal as _, Write as _};
use std::time::Duration;

use swarm_protocol::SwarmStats;
use swarm_protocol::agent::Call;
use swarm_sprite::{Frame, Rgb, SIZE, Wings};

use crate::session::Session;

/// The counts are decoration: do not keep the caller waiting for them.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

const RESET: &str = "\x1b[0m";

pub(crate) fn run() {
    let mut stdout = std::io::stdout();

    let color = wants_color(
        stdout.is_terminal(),
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
    );
    if color {
        print(&mut stdout, &bee());
    }

    if let Some(stats) = swarm_stats() {
        print(&mut stdout, &counts_text(&stats));
    }
}

/// Color only goes to a terminal that can show it. An empty `NO_COLOR` counts as unset
/// (no-color.org).
fn wants_color(is_terminal: bool, no_color: Option<&OsStr>, term: Option<&OsStr>) -> bool {
    is_terminal && no_color.is_none_or(OsStr::is_empty) && term != Some(OsStr::new("dumb"))
}

/// A closed stdout is the caller's business, not a failure of ours.
fn print(stdout: &mut std::io::Stdout, text: &str) {
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .ok();
}

/// `None` outside an agent, and when the daemon does not answer.
fn swarm_stats() -> Option<SwarmStats> {
    let session = Session::from_env().ok()?;
    let value = session.call_within(Call::Buzz, SOCKET_TIMEOUT).ok()?;

    serde_json::from_value(value).ok()
}

/// The bee with its wings down: 24 columns by 12 lines.
fn bee() -> String {
    let frame = Frame::new(Wings::Down, false);

    half_blocks(SIZE, SIZE, |column, row| frame.at(column, row))
}

/// Two pixel rows per line, as half blocks in 24-bit color. A pixel row past `height`
/// counts as empty.
fn half_blocks(width: usize, height: usize, pixel: impl Fn(usize, usize) -> Option<Rgb>) -> String {
    let mut text = String::new();

    for top_row in (0..height).step_by(2) {
        let mut shown = Paint::default();

        for column in 0..width {
            let top = pixel(column, top_row);
            let bottom = pixel(column, top_row + 1);
            let (glyph, wanted) = cell(top, bottom);

            switch(&mut text, shown, wanted);
            text.push(glyph);
            shown = wanted;
        }

        text.push_str(RESET);
        text.push('\n');
    }

    text
}

/// The colors a terminal shows from now on.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Paint {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
}

/// The glyph for a pair of pixels, and the colors it needs.
fn cell(top: Option<Rgb>, bottom: Option<Rgb>) -> (char, Paint) {
    match (top, bottom) {
        (None, None) => (' ', Paint::default()),
        (Some(top), None) => (
            '\u{2580}',
            Paint {
                foreground: Some(top),
                background: None,
            },
        ),
        (None, Some(bottom)) => (
            '\u{2584}',
            Paint {
                foreground: Some(bottom),
                background: None,
            },
        ),
        (Some(top), Some(bottom)) => (
            '\u{2580}',
            Paint {
                foreground: Some(top),
                background: Some(bottom),
            },
        ),
    }
}

/// Writes the escapes that turn `from` into `to`.
fn switch(text: &mut String, from: Paint, to: Paint) {
    let mut now = from;

    // A color only goes away with a reset, so a background never bleeds into a space.
    if (now.foreground.is_some() && to.foreground.is_none())
        || (now.background.is_some() && to.background.is_none())
    {
        text.push_str(RESET);
        now = Paint::default();
    }

    if to.foreground != now.foreground
        && let Some(color) = to.foreground
    {
        text.push_str(&color_code(38, color));
    }
    if to.background != now.background
        && let Some(color) = to.background
    {
        text.push_str(&color_code(48, color));
    }
}

/// 38 sets the foreground and 48 the background.
fn color_code(layer: u8, [red, green, blue]: Rgb) -> String {
    format!("\x1b[{layer};2;{red};{green};{blue}m")
}

/// Two plain lines under the bee.
fn counts_text(stats: &SwarmStats) -> String {
    format!(
        "{} \u{b7} {} running\n{} merged\n",
        count(u64::from(stats.open_tasks), "task", "tasks"),
        count(u64::from(stats.running_agents), "agent", "agents"),
        count(stats.merged_prs, "PR", "PRs"),
    )
}

fn count(number: u64, one: &str, many: &str) -> String {
    format!("{number} {}", if number == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: Rgb = [1, 2, 3];
    const SECOND: Rgb = [4, 5, 6];

    /// A grid of two pixel rows, one column per entry of `columns` (top, bottom).
    fn two_rows(columns: &[(Option<Rgb>, Option<Rgb>)]) -> String {
        half_blocks(columns.len(), 2, |column, row| match row {
            0 => columns[column].0,
            _ => columns[column].1,
        })
    }

    fn without_escapes(line: &str) -> String {
        let mut shown = String::new();
        let mut characters = line.chars();

        while let Some(character) = characters.next() {
            if character == '\x1b' {
                characters.by_ref().find(|end| *end == 'm');
            } else {
                shown.push(character);
            }
        }

        shown
    }

    fn os(text: &str) -> &OsStr {
        OsStr::new(text)
    }

    #[test]
    fn a_terminal_that_shows_color_gets_the_bee() {
        assert!(wants_color(true, None, Some(os("xterm-256color"))));
        assert!(wants_color(true, None, None));
    }

    #[test]
    fn output_that_is_not_a_terminal_gets_no_color() {
        assert!(!wants_color(false, None, Some(os("xterm-256color"))));
        assert!(!wants_color(false, None, None));
    }

    #[test]
    fn no_color_turns_the_bee_off() {
        assert!(!wants_color(
            true,
            Some(os("1")),
            Some(os("xterm-256color"))
        ));
        assert!(!wants_color(
            true,
            Some(os("0")),
            Some(os("xterm-256color"))
        ));
    }

    #[test]
    fn an_empty_no_color_counts_as_unset() {
        assert!(wants_color(true, Some(os("")), Some(os("xterm-256color"))));
    }

    #[test]
    fn a_dumb_terminal_gets_no_color() {
        assert!(!wants_color(true, None, Some(os("dumb"))));
        assert!(wants_color(true, None, Some(os("dumber"))));
    }

    #[test]
    fn the_bee_is_twelve_lines_of_24_columns() {
        let text = bee();

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 12);
        for line in lines {
            assert_eq!(without_escapes(line).chars().count(), 24, "{line:?}");
            assert!(line.ends_with(RESET), "{line:?}");
        }
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_known_pixel_shows_in_truecolor() {
        let text = bee();
        let lines: Vec<&str> = text.lines().collect();

        // Pixels (12, 8) and (12, 9) are both the yellow of the body: line 4.
        let yellow = "\x1b[38;2;252;201;53m\x1b[48;2;252;201;53m\u{2580}";
        assert!(lines[4].contains(yellow), "{:?}", lines[4]);
    }

    #[test]
    fn the_bee_uses_only_24_bit_colors_and_resets() {
        let text = bee();

        let codes: Vec<&str> = text
            .split("\x1b[")
            .skip(1)
            .filter_map(|escape| escape.split('m').next())
            .collect();
        assert!(!codes.is_empty());
        for code in codes {
            assert!(
                code == "0" || code.starts_with("38;2;") || code.starts_with("48;2;"),
                "{code}"
            );
        }
        assert!(without_escapes(&text).contains(' '));
    }

    #[test]
    fn a_pixel_on_top_is_an_upper_half_block_in_its_color() {
        assert_eq!(
            two_rows(&[(Some(FIRST), None)]),
            "\x1b[38;2;1;2;3m\u{2580}\x1b[0m\n"
        );
    }

    #[test]
    fn a_pixel_below_is_a_lower_half_block_in_its_color() {
        assert_eq!(
            two_rows(&[(None, Some(SECOND))]),
            "\x1b[38;2;4;5;6m\u{2584}\x1b[0m\n"
        );
    }

    #[test]
    fn two_pixels_use_the_top_as_foreground_and_the_bottom_as_background() {
        assert_eq!(
            two_rows(&[(Some(FIRST), Some(SECOND))]),
            "\x1b[38;2;1;2;3m\x1b[48;2;4;5;6m\u{2580}\x1b[0m\n"
        );
    }

    #[test]
    fn two_empty_pixels_are_a_space() {
        assert_eq!(two_rows(&[(None, None)]), " \x1b[0m\n");
    }

    #[test]
    fn a_background_never_bleeds_into_a_space() {
        let text = two_rows(&[
            (Some(FIRST), Some(SECOND)),
            (None, None),
            (Some(FIRST), None),
        ]);

        assert_eq!(
            text,
            "\x1b[38;2;1;2;3m\x1b[48;2;4;5;6m\u{2580}\x1b[0m \x1b[38;2;1;2;3m\u{2580}\x1b[0m\n"
        );
    }

    #[test]
    fn dropping_only_the_background_resets_and_sets_the_foreground_again() {
        let text = two_rows(&[(Some(FIRST), Some(SECOND)), (Some(FIRST), None)]);

        assert_eq!(
            text,
            "\x1b[38;2;1;2;3m\x1b[48;2;4;5;6m\u{2580}\x1b[0m\x1b[38;2;1;2;3m\u{2580}\x1b[0m\n"
        );
    }

    #[test]
    fn a_color_that_stays_is_not_written_again() {
        let text = two_rows(&[(Some(FIRST), None), (Some(FIRST), Some(SECOND))]);

        assert_eq!(
            text,
            "\x1b[38;2;1;2;3m\u{2580}\x1b[48;2;4;5;6m\u{2580}\x1b[0m\n"
        );
    }

    #[test]
    fn every_line_starts_without_a_color() {
        let text = half_blocks(1, 4, |_, _| Some(FIRST));

        assert_eq!(
            text,
            "\x1b[38;2;1;2;3m\x1b[48;2;1;2;3m\u{2580}\x1b[0m\n\x1b[38;2;1;2;3m\x1b[48;2;1;2;3m\u{2580}\x1b[0m\n"
        );
    }

    #[test]
    fn the_counts_are_two_plain_lines() {
        let stats = SwarmStats {
            running_agents: 7,
            open_tasks: 3,
            merged_prs: 14,
            hundredth_merge_at: None,
        };

        assert_eq!(
            counts_text(&stats),
            "3 tasks \u{b7} 7 agents running\n14 PRs merged\n"
        );
    }

    #[test]
    fn one_of_each_is_singular() {
        let stats = SwarmStats {
            running_agents: 1,
            open_tasks: 1,
            merged_prs: 1,
            hundredth_merge_at: Some(5),
        };

        assert_eq!(
            counts_text(&stats),
            "1 task \u{b7} 1 agent running\n1 PR merged\n"
        );
    }

    #[test]
    fn zero_is_plural() {
        assert_eq!(
            counts_text(&SwarmStats::default()),
            "0 tasks \u{b7} 0 agents running\n0 PRs merged\n"
        );
    }
}
