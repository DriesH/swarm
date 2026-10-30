//! Client-side copy of one terminal's screen: the viewport rows, the scrollback cache
//! and the scroll offset.

use std::collections::HashMap;

use crate::{Row, Screen, Snapshot, TerminalOutput, Update};

/// Scrollback lines kept in memory.
const MAX_CACHED_LINES: usize = 20_000;

/// Lines fetched around a missing range, so scrolling rarely waits.
const FETCH_MARGIN: u64 = 200;

/// Which viewport rows an output changed, so a renderer can rebuild only those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changed {
    Nothing,
    Rows(Vec<u16>),
    /// Every row, and any layout cached for scrollback, is stale.
    All,
}

impl Changed {
    /// Every row, and any layout cached for scrollback, is stale.
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// The changed viewport rows; empty for [`Changed::All`], which needs no list.
    pub fn rows(&self) -> &[u16] {
        match self {
            Self::Rows(rows) => rows,
            Self::Nothing | Self::All => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScreenModel {
    screen: Screen,
    rows: Vec<Row>,
    history: HashMap<u64, Row>,
    /// Lines scrolled up from the bottom; 0 follows the live screen.
    scroll: u64,
    cap: usize,
}

impl ScreenModel {
    pub fn new(snapshot: Snapshot) -> Self {
        let mut model = Self {
            screen: snapshot.screen.clone(),
            rows: Vec::new(),
            history: HashMap::new(),
            scroll: 0,
            cap: MAX_CACHED_LINES,
        };
        model.load_snapshot(snapshot);

        model
    }

    /// Applies one output. Returns which viewport rows changed.
    pub fn apply(&mut self, output: &TerminalOutput) -> Changed {
        match output {
            TerminalOutput::Snapshot(snapshot) => {
                self.history.clear();
                self.scroll = 0;
                self.load_snapshot(snapshot.clone());

                Changed::All
            }
            TerminalOutput::Update(update) => self.apply_update(update),
            TerminalOutput::History { from, rows } => {
                self.insert_history(*from, rows);

                Changed::Nothing
            }
            TerminalOutput::Title(_)
            | TerminalOutput::WorkingDirectory(_)
            | TerminalOutput::Bell
            | TerminalOutput::Exited(_) => Changed::Nothing,
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The numbers of the lines on screen, top to bottom. Numbers below
    /// `screen().history.end()` are scrollback lines; the live viewport continues
    /// after them, so the viewport row `i` is line `history.end() + i`.
    pub fn visible_lines(&self) -> impl Iterator<Item = u64> + use<> {
        let top = self.screen.history.end() - self.scroll;

        (0..u64::from(self.screen.rows)).map(move |offset| top + offset)
    }

    /// The row behind a line number; `None` for a scrollback line not cached yet.
    pub fn row(&self, line: u64) -> Option<&Row> {
        let end = self.screen.history.end();

        if line >= end {
            let index = usize::try_from(line - end).ok()?;

            self.rows.get(index)
        } else {
            self.history.get(&line)
        }
    }

    /// Rows to paint top to bottom, honoring the scroll offset; `None` for a scrollback
    /// line not cached yet.
    pub fn visible_rows(&self) -> impl Iterator<Item = Option<&Row>> {
        self.visible_lines().map(|line| self.row(line))
    }

    /// Scrolls up into history for positive `lines`, down for negative. Does nothing on
    /// the alternate screen.
    pub fn scroll_by(&mut self, lines: i64) {
        if self.screen.alternate {
            return;
        }

        let target = i128::from(self.scroll) + i128::from(lines);
        let clamped = target.clamp(0, i128::from(self.screen.history.len));
        self.scroll = u64::try_from(clamped).unwrap_or(0);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_offset(&self) -> u64 {
        self.scroll
    }

    /// The history lines to fetch for the current view, with a margin, or `None`.
    pub fn missing_history(&self) -> Option<(u64, u32)> {
        let mut missing: Option<(u64, u64)> = None;

        let end = self.screen.history.end();

        for number in self.visible_lines() {
            if number >= end || self.history.contains_key(&number) {
                continue;
            }

            let (low, high) = missing.get_or_insert((number, number));
            *low = (*low).min(number);
            *high = (*high).max(number);
        }

        let (low, high) = missing?;
        let history = self.screen.history;
        let from = low.saturating_sub(FETCH_MARGIN).max(history.start);
        let to = (high + 1 + FETCH_MARGIN).min(history.end());
        let count = u32::try_from(to - from).unwrap_or(u32::MAX);

        Some((from, count))
    }

    fn load_snapshot(&mut self, snapshot: Snapshot) {
        let Snapshot {
            screen,
            rows,
            history_rows,
            ..
        } = snapshot;

        let first = screen
            .history
            .end()
            .saturating_sub(history_rows.len() as u64);
        self.screen = screen;
        self.rows = rows;
        self.fit_rows();
        self.insert_history(first, &history_rows);
        self.settle_scroll();
    }

    fn apply_update(&mut self, update: &Update) -> Changed {
        let old_history = self.screen.history;
        let resized =
            update.screen.cols != self.screen.cols || update.screen.rows != self.screen.rows;

        if update.history_reset {
            self.history.clear();
        }

        // Keep the view on the same lines while new output scrolls in.
        if self.scroll > 0 {
            let new_history = update.screen.history;
            self.scroll += new_history.end().saturating_sub(old_history.end());
        }

        self.screen = update.screen.clone();
        self.fit_rows();

        for (index, row) in &update.rows {
            if let Some(slot) = self.rows.get_mut(usize::from(*index)) {
                slot.clone_from(row);
            }
        }

        self.settle_scroll();

        if resized || update.history_reset {
            Changed::All
        } else {
            Changed::Rows(update.rows.iter().map(|(index, _)| *index).collect())
        }
    }

    fn fit_rows(&mut self) {
        self.rows
            .resize_with(usize::from(self.screen.rows), Row::default);
    }

    fn insert_history(&mut self, from: u64, rows: &[Row]) {
        let history = self.screen.history;

        for (offset, row) in rows.iter().enumerate() {
            let number = from + offset as u64;

            if number >= history.start && number < history.end() {
                self.history.insert(number, row.clone());
            }
        }

        self.prune();
    }

    /// Keeps the offset valid and drops lines that left the scrollback.
    fn settle_scroll(&mut self) {
        if self.screen.alternate {
            self.scroll = 0;
        } else {
            self.scroll = self.scroll.min(self.screen.history.len);
        }

        self.prune();
    }

    fn prune(&mut self) {
        let history = self.screen.history;
        self.history
            .retain(|number, _| *number >= history.start && *number < history.end());

        let excess = self.history.len().saturating_sub(self.cap);
        if excess == 0 {
            return;
        }

        // Drop the lines farthest from the ones on screen.
        let center = history.end() - self.scroll;
        let mut numbers: Vec<u64> = self.history.keys().copied().collect();
        numbers.sort_unstable_by_key(|number| std::cmp::Reverse(number.abs_diff(center)));

        for number in numbers.into_iter().take(excess) {
            self.history.remove(&number);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BgSpan, Colors, Cursor, CursorStyle, History, Rgb, TextFlags, TextSpan};

    fn screen(rows: u16, history: History) -> Screen {
        Screen {
            cols: 10,
            rows,
            colors: Colors {
                fg: Rgb(255, 255, 255),
                bg: Rgb(0, 0, 0),
                cursor: None,
            },
            cursor: Cursor {
                col: 0,
                row: 0,
                style: CursorStyle::Block,
                visible: true,
            },
            alternate: false,
            history,
        }
    }

    fn row(text: &str) -> Row {
        Row {
            bgs: vec![BgSpan {
                col: 0,
                len: 1,
                color: Rgb(1, 2, 3),
            }],
            texts: vec![TextSpan {
                col: 0,
                cells: u16::try_from(text.chars().count()).unwrap_or(u16::MAX),
                text: text.to_owned(),
                fg: Rgb(255, 255, 255),
                flags: TextFlags::default(),
            }],
        }
    }

    fn text(row: Option<&Row>) -> Option<&str> {
        row.and_then(|row| row.texts.first())
            .map(|span| span.text.as_str())
    }

    /// Three viewport rows `a b c` and scrollback lines 10..16 named `h10`..`h15`.
    fn model() -> ScreenModel {
        let history = History { start: 10, len: 6 };

        ScreenModel::new(Snapshot {
            screen: screen(3, history),
            rows: vec![row("a"), row("b"), row("c")],
            history_rows: (10..16).map(|number| row(&format!("h{number}"))).collect(),
            title: None,
            working_directory: None,
        })
    }

    fn update(history: History, rows: Vec<(u16, Row)>) -> TerminalOutput {
        TerminalOutput::Update(Update {
            screen: screen(3, history),
            rows,
            history_reset: false,
        })
    }

    #[test]
    fn snapshot_fills_viewport_and_history() {
        let model = model();

        let visible: Vec<_> = model.visible_rows().map(text).collect();

        assert_eq!(visible, [Some("a"), Some("b"), Some("c")]);
        assert_eq!(model.history.len(), 6);
    }

    #[test]
    fn snapshot_replaces_the_previous_state() {
        let mut model = model();
        model.scroll_by(2);

        let changed = model.apply(&TerminalOutput::Snapshot(Snapshot {
            screen: screen(3, History::default()),
            rows: vec![row("x"), row("y"), row("z")],
            history_rows: Vec::new(),
            title: None,
            working_directory: None,
        }));

        assert_eq!(changed, Changed::All);
        assert_eq!(model.scroll_offset(), 0);
        assert!(model.history.is_empty());
    }

    #[test]
    fn update_replaces_only_the_given_rows() {
        let mut model = model();

        let changed = model.apply(&update(History { start: 10, len: 6 }, vec![(1, row("B"))]));

        let visible: Vec<_> = model.visible_rows().map(text).collect();
        assert_eq!(changed, Changed::Rows(vec![1]));
        assert_eq!(visible, [Some("a"), Some("B"), Some("c")]);
    }

    #[test]
    fn update_with_a_new_grid_size_changes_all_rows() {
        let mut model = model();
        let mut resized = screen(4, History { start: 10, len: 6 });
        resized.cols = 20;

        let changed = model.apply(&TerminalOutput::Update(Update {
            screen: resized,
            rows: vec![(3, row("d"))],
            history_reset: false,
        }));

        assert_eq!(changed, Changed::All);
        assert_eq!(model.visible_rows().count(), 4);
    }

    #[test]
    fn history_output_fills_missing_lines_inside_the_range() {
        let mut model = ScreenModel::new(Snapshot {
            screen: screen(3, History { start: 0, len: 100 }),
            rows: vec![row("a"), row("b"), row("c")],
            history_rows: Vec::new(),
            title: None,
            working_directory: None,
        });

        model.apply(&TerminalOutput::History {
            from: 98,
            rows: vec![row("h98"), row("h99"), row("h100")],
        });

        assert_eq!(text(model.row(99)), Some("h99"));
        // Line 100 is the first viewport row, not scrollback.
        assert_eq!(text(model.row(100)), Some("a"));
    }

    #[test]
    fn history_reset_drops_cached_lines() {
        let mut model = model();

        let changed = model.apply(&TerminalOutput::Update(Update {
            screen: screen(3, History { start: 10, len: 6 }),
            rows: Vec::new(),
            history_reset: true,
        }));

        assert_eq!(changed, Changed::All);
        assert!(model.history.is_empty());
    }

    #[test]
    fn lines_below_history_start_are_dropped() {
        let mut model = model();

        model.apply(&update(History { start: 13, len: 3 }, Vec::new()));

        let mut cached: Vec<_> = model.history.keys().copied().collect();
        cached.sort_unstable();
        assert_eq!(cached, [13, 14, 15]);
    }

    #[test]
    fn scrolling_shows_history_above_the_viewport() {
        let mut model = model();

        model.scroll_by(2);

        let visible: Vec<_> = model.visible_rows().map(text).collect();
        assert_eq!(visible, [Some("h14"), Some("h15"), Some("a")]);
    }

    #[test]
    fn scroll_is_clamped_to_the_history() {
        let mut model = model();

        model.scroll_by(1_000);
        assert_eq!(model.scroll_offset(), 6);

        model.scroll_by(-1_000);
        assert_eq!(model.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_bottom_follows_the_live_screen() {
        let mut model = model();
        model.scroll_by(3);

        model.scroll_to_bottom();

        assert_eq!(model.scroll_offset(), 0);
    }

    #[test]
    fn scrolled_view_stays_on_the_same_lines_when_output_arrives() {
        let mut model = model();
        model.scroll_by(2);

        model.apply(&update(History { start: 10, len: 8 }, Vec::new()));

        assert_eq!(model.scroll_offset(), 4);
    }

    #[test]
    fn alternate_screen_disables_scrolling() {
        let mut model = model();
        let mut alternate = screen(3, History { start: 10, len: 6 });
        alternate.alternate = true;
        model.scroll_by(2);

        model.apply(&TerminalOutput::Update(Update {
            screen: alternate,
            rows: Vec::new(),
            history_reset: false,
        }));
        model.scroll_by(2);

        assert_eq!(model.scroll_offset(), 0);
    }

    #[test]
    fn missing_history_is_none_when_everything_is_cached() {
        let mut model = model();
        model.scroll_by(3);

        assert_eq!(model.missing_history(), None);
    }

    #[test]
    fn missing_history_adds_a_margin_and_stays_in_range() {
        let mut model = ScreenModel::new(Snapshot {
            screen: screen(
                3,
                History {
                    start: 100,
                    len: 1_000,
                },
            ),
            rows: vec![row("a"), row("b"), row("c")],
            history_rows: Vec::new(),
            title: None,
            working_directory: None,
        });
        model.scroll_by(500);

        // The view shows lines 600..603.
        assert_eq!(model.missing_history(), Some((400, 403)));
    }

    #[test]
    fn missing_history_is_clamped_to_the_start_and_end() {
        let mut model = ScreenModel::new(Snapshot {
            screen: screen(
                3,
                History {
                    start: 50,
                    len: 100,
                },
            ),
            rows: vec![row("a"), row("b"), row("c")],
            history_rows: Vec::new(),
            title: None,
            working_directory: None,
        });
        model.scroll_by(100);

        assert_eq!(model.missing_history(), Some((50, 100)));
    }

    #[test]
    fn cache_is_capped_by_dropping_the_farthest_lines() {
        let mut model = model();
        model.cap = 3;
        model.scroll_by(6);

        model.apply(&TerminalOutput::History {
            from: 10,
            rows: (10..16).map(|number| row(&format!("h{number}"))).collect(),
        });

        let mut cached: Vec<_> = model.history.keys().copied().collect();
        cached.sort_unstable();
        assert_eq!(cached, [10, 11, 12]);
    }
}
