//! The libghostty wrapper: bytes in, plain rows out. No threads and no PTY, so it
//! can be tested by feeding bytes.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions, focus, paste,
    render::{
        CellIterator, Colors as RenderColors, CursorVisualStyle, Dirty, RowIteration, RowIterator,
    },
    screen::{CellContentTag, CellWide, Screen as GhosttyScreen, TrackedGridRef},
    style::{RgbColor, Style, Underline},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
        PointCoordinate, PointSpace, PrimaryDeviceAttributes, ScrollViewport,
        SecondaryDeviceAttributes, TertiaryDeviceAttributes,
    },
};
use swarm_term::{
    BgSpan, Colors, Cursor, CursorStyle, History, Key, Palette, ProgramStatus, Rgb, Row, Screen,
    Size, Snapshot, TerminalOutput, TextFlags, TextSpan, Update,
};

use crate::keys::KeyEncoder;
use crate::osc7501::{Parsed, Parser, QUERY_REPLY};

/// Longest a synchronized update (mode 2026) holds back the screen, as in Ghostty.
const SYNC_OUTPUT_CAP: Duration = Duration::from_secs(1);

/// Where the terminal sends what it produces on its own.
pub(crate) struct Sinks {
    /// Bytes for the program: answers to queries like DA1.
    pub(crate) replies: flume::Sender<Vec<u8>>,
    /// Title, working directory and bell events.
    pub(crate) events: flume::Sender<TerminalOutput>,
}

pub(crate) struct Emulator {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    key_encoder: KeyEncoder,
    status_parser: Parser,
    parsed: Vec<Parsed>,
    replies: flume::Sender<Vec<u8>>,
    size: Size,
    /// The viewport as last sent; used to send only rows that really changed.
    rows: Vec<Row>,
    screen: Option<Screen>,
    history: History,
    /// Tracks the first active row at the last sync. Its new position tells
    /// how many lines scrolled into history since then.
    anchor: TrackedGridRef,
    history_reset: bool,
    /// Send every row with the next update, even unchanged ones.
    resend_all: bool,
    /// When the current synchronized update started.
    sync_since: Option<Instant>,
    text: String,
}

impl Emulator {
    pub(crate) fn new(
        size: Size,
        palette: &Palette,
        scrollback_bytes: usize,
        sinks: Sinks,
    ) -> Result<Self> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: size.cols,
            rows: size.rows,
            max_scrollback: scrollback_bytes,
        })?;

        let Sinks { replies, events } = sinks;
        let query_replies = replies.clone();
        let title_events = events.clone();
        let pwd_events = events.clone();
        terminal
            .on_pty_write(move |_, data| {
                if replies.send(data.to_vec()).is_err() {
                    log::debug!("dropping a query reply, the pty is gone");
                }
            })?
            // TUIs probe DA1 at startup and may stall without an answer.
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[DeviceAttributeFeature::ANSI_COLOR],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: TertiaryDeviceAttributes::default(),
                })
            })?
            .on_xtversion(|_| Some("swarm"))?
            .on_title_changed(move |terminal| match terminal.title() {
                Ok(title) => emit(&title_events, TerminalOutput::Title(title.to_owned())),
                Err(error) => log::debug!("reading the title failed: {error}"),
            })?
            .on_pwd_changed(move |terminal| match terminal.pwd() {
                Ok(pwd) => emit(
                    &pwd_events,
                    TerminalOutput::WorkingDirectory(pwd.to_owned()),
                ),
                Err(error) => log::debug!("reading the working directory failed: {error}"),
            })?
            .on_bell(move |_| emit(&events, TerminalOutput::Bell))?;

        let anchor = terminal.track_grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))?;

        let mut emulator = Self {
            terminal,
            render: RenderState::new()?,
            row_iterator: RowIterator::new()?,
            cell_iterator: CellIterator::new()?,
            key_encoder: KeyEncoder::new()?,
            status_parser: Parser::default(),
            parsed: Vec::new(),
            replies: query_replies,
            size,
            rows: Vec::new(),
            screen: None,
            history: History::default(),
            anchor,
            history_reset: false,
            resend_all: false,
            sync_since: None,
            text: String::new(),
        };
        emulator.set_palette(palette)?;

        Ok(emulator)
    }

    /// Finds OSC 7501 in a chunk and answers its support query. Call it before
    /// `write` with the same bytes, so the answer goes out before libghostty's
    /// replies to the same chunk.
    pub(crate) fn scan_status(&mut self, bytes: &[u8]) -> Vec<ProgramStatus> {
        self.parsed.clear();
        self.status_parser.feed(bytes, &mut self.parsed);

        let mut statuses = Vec::new();
        for parsed in self.parsed.drain(..) {
            match parsed {
                Parsed::Report(status) => statuses.push(status),
                Parsed::Query => {
                    if self.replies.send(QUERY_REPLY.to_vec()).is_err() {
                        log::debug!("dropping the OSC 7501 reply, the pty is gone");
                    }
                }
            }
        }

        statuses
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    pub(crate) fn size(&self) -> Size {
        self.size
    }

    pub(crate) fn encode_key(&mut self, key: &Key) -> Vec<u8> {
        self.key_encoder.encode(&self.terminal, key)
    }

    /// Wrapped in bracketed paste when the program asked for it; unsafe control
    /// bytes are replaced.
    pub(crate) fn encode_paste(&self, text: &str) -> Result<Vec<u8>> {
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        // Encoding replaces bytes one for one and adds at most the 12 wrapper bytes.
        let mut buffer = vec![0u8; data.len() + 32];

        let written = paste::encode(&mut data, bracketed, &mut buffer)
            .map_err(|error| anyhow!("encoding paste failed: {error}"))?;
        buffer.truncate(written);

        Ok(buffer)
    }

    /// `None` unless the program enabled focus reporting (mode 1004).
    pub(crate) fn focus_report(&self, focused: bool) -> Result<Option<Vec<u8>>> {
        if !self.terminal.mode(Mode::FOCUS_EVENT)? {
            return Ok(None);
        }

        let event = if focused {
            focus::Event::Gained
        } else {
            focus::Event::Lost
        };
        let mut buffer = [0u8; 8];
        let written = event.encode(&mut buffer)?;

        Ok(Some(buffer[..written].to_vec()))
    }

    pub(crate) fn resize(&mut self, size: Size) -> Result<()> {
        if size == self.size {
            return Ok(());
        }

        self.terminal.resize(
            size.cols,
            size.rows,
            size.cell_width.into(),
            size.cell_height.into(),
        )?;
        self.size = size;
        // Reflow rewraps scrollback, so cached lines are no longer valid.
        self.history_reset = true;
        self.resend_all = true;

        Ok(())
    }

    /// Sets the default colors and the 16 ANSI colors. Colors are resolved per row,
    /// so the next update carries every row.
    pub(crate) fn set_palette(&mut self, palette: &Palette) -> Result<()> {
        let mut colors = self.terminal.default_color_palette()?;
        for (slot, color) in colors.0.iter_mut().zip(palette.ansi) {
            *slot = rgb_color(color);
        }

        self.terminal
            .set_default_fg_color(Some(rgb_color(palette.fg)))?
            .set_default_bg_color(Some(rgb_color(palette.bg)))?
            .set_default_cursor_color(Some(rgb_color(palette.cursor)))?
            .set_default_color_palette(Some(colors))?;
        self.resend_all = true;
        // Cached scrollback was resolved with the old colors.
        self.history_reset = true;

        Ok(())
    }

    /// Whether a flush should wait for the end of a synchronized update. Returns
    /// the time the hold ends at the latest, or `None` when it is fine to flush.
    pub(crate) fn sync_hold(&mut self, now: Instant) -> Result<Option<Instant>> {
        if !self.terminal.mode(Mode::SYNC_OUTPUT)? {
            self.sync_since = None;
            return Ok(None);
        }

        let until = *self.sync_since.get_or_insert(now) + SYNC_OUTPUT_CAP;
        if now >= until {
            self.sync_since = None;
            return Ok(None);
        }

        Ok(Some(until))
    }

    /// The whole screen plus up to `history_limit` lines of scrollback. The update
    /// that was pending is returned too, so the caller can send it first.
    pub(crate) fn snapshot(&mut self, history_limit: u32) -> Result<(Option<Update>, Snapshot)> {
        let update = self.flush()?;

        let screen = self.screen.clone().context("no screen after flush")?;
        let count = u64::from(history_limit).min(self.history.len);
        let from = self.history.end() - count;
        let (_, history_rows) = self.history_rows(from, history_limit)?;

        let snapshot = Snapshot {
            screen,
            rows: self.rows.clone(),
            history_rows,
            title: non_empty(self.terminal.title()?),
            working_directory: non_empty(self.terminal.pwd()?),
        };

        Ok((update, snapshot))
    }

    /// The rows that changed since the last call, or `None` when nothing did.
    pub(crate) fn flush(&mut self) -> Result<Option<Update>> {
        self.sync_history()?;

        let snapshot = self.render.update(&self.terminal)?;
        let colors = snapshot.colors()?;
        let screen_colors = Colors {
            fg: rgb(colors.foreground),
            bg: rgb(colors.background),
            cursor: colors.cursor.map(rgb),
        };

        let (cursor_col, cursor_row, in_viewport) = match snapshot.cursor_viewport()? {
            Some(viewport) => (viewport.x, viewport.y, true),
            None => (0, 0, false),
        };
        let cursor = Cursor {
            col: cursor_col,
            row: cursor_row,
            style: match snapshot
                .cursor_visual_style()
                .unwrap_or(CursorVisualStyle::Block)
            {
                CursorVisualStyle::Bar => CursorStyle::Bar,
                CursorVisualStyle::Underline => CursorStyle::Underline,
                CursorVisualStyle::BlockHollow => CursorStyle::BlockHollow,
                _ => CursorStyle::Block,
            },
            visible: in_viewport && snapshot.cursor_visible()?,
        };

        let row_count = snapshot.rows()?;
        let resend_all = std::mem::take(&mut self.resend_all);
        let full = snapshot.dirty()? == Dirty::Full
            || resend_all
            || self.rows.len() != usize::from(row_count);
        self.rows.resize_with(usize::from(row_count), Row::default);

        let mut changed = Vec::new();
        let mut rows = self.row_iterator.update(&snapshot)?;
        let mut index: u16 = 0;
        while let Some(row) = rows.next() {
            if full || row.dirty()? {
                let encoded = encode_row(row, &mut self.cell_iterator, &colors, &mut self.text)?;
                let slot = &mut self.rows[usize::from(index)];
                if resend_all || *slot != encoded {
                    changed.push((index, encoded.clone()));
                    *slot = encoded;
                }
                row.set_dirty(false)?;
            }
            index += 1;
        }
        snapshot.set_dirty(Dirty::Clean)?;

        let screen = Screen {
            cols: self.size.cols,
            rows: row_count,
            colors: screen_colors,
            cursor,
            alternate: self.terminal.active_screen()? == GhosttyScreen::Alternate,
            history: self.history,
        };
        let history_reset = std::mem::take(&mut self.history_reset);
        if changed.is_empty() && !history_reset && self.screen.as_ref() == Some(&screen) {
            return Ok(None);
        }

        self.screen = Some(screen.clone());

        Ok(Some(Update {
            screen,
            rows: changed,
            history_reset,
        }))
    }

    /// Scrollback lines `from..from + count` that still exist, with the (clamped)
    /// first line number. The viewport moves while reading, so the next flush sees
    /// a full redraw; the row diff absorbs it.
    pub(crate) fn history_rows(&mut self, from: u64, count: u32) -> Result<(u64, Vec<Row>)> {
        self.sync_history()?;

        let from = from.max(self.history.start);
        let end = from
            .saturating_add(u64::from(count))
            .min(self.history.end());
        if from >= end || self.terminal.active_screen()? != GhosttyScreen::Primary {
            return Ok((from, Vec::new()));
        }

        let first = usize::try_from(from - self.history.start)?;
        let last = usize::try_from(end - self.history.start)?;
        let mut out = Vec::with_capacity(last - first);
        let mut line = first;
        while line < last {
            self.terminal.scroll_viewport(ScrollViewport::Row(line));

            let snapshot = self.render.update(&self.terminal)?;
            let colors = snapshot.colors()?;
            let mut rows = self.row_iterator.update(&snapshot)?;
            let mut taken = 0;
            while taken < last - line
                && let Some(row) = rows.next()
            {
                out.push(encode_row(
                    row,
                    &mut self.cell_iterator,
                    &colors,
                    &mut self.text,
                )?);
                taken += 1;
            }
            if taken == 0 {
                break;
            }
            line += taken;
        }
        self.terminal.scroll_viewport(ScrollViewport::Bottom);

        Ok((from, out))
    }

    /// Updates `history` from the anchor. Skipped on the alternate screen, which
    /// has no scrollback; the primary history is frozen meanwhile.
    fn sync_history(&mut self) -> Result<()> {
        if self.terminal.active_screen()? != GhosttyScreen::Primary {
            return Ok(());
        }

        let len = self.terminal.scrollback_rows()? as u64;
        if let Some(point) = self.anchor.point(PointSpace::Screen)? {
            // The anchor was line `history.end()`; it now sits at screen row `point.y`.
            self.history.start = self.history.end().saturating_sub(u64::from(point.y));
        } else {
            // Pruned or cleared: start a fresh numbering past everything sent.
            self.history.start = self.history.end() + u64::from(self.size.rows);
            self.history_reset = true;
        }
        self.history.len = len;
        self.anchor.set(
            &mut self.terminal,
            Point::Active(PointCoordinate { x: 0, y: 0 }),
        )?;

        Ok(())
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn emit(events: &flume::Sender<TerminalOutput>, output: TerminalOutput) {
    if events.send(output).is_err() {
        log::debug!("dropping a terminal event, the view is gone");
    }
}

fn rgb(color: RgbColor) -> Rgb {
    Rgb(color.r, color.g, color.b)
}

fn rgb_color(color: Rgb) -> RgbColor {
    RgbColor {
        r: color.0,
        g: color.1,
        b: color.2,
    }
}

/// Encodes one row. `text` is a scratch buffer reused between calls.
fn encode_row(
    row: &RowIteration<'static, '_>,
    cells: &mut CellIterator<'static>,
    colors: &RenderColors,
    text: &mut String,
) -> libghostty_vt::error::Result<Row> {
    let mut out = Row::default();
    let mut cells = cells.update(row)?;
    let mut column: u16 = 0;
    let mut last_own_span = false;
    while let Some(cell) = cells.next() {
        let col = column;
        column += 1;

        let raw = cell.raw_cell()?;
        let wide = raw.wide()?;
        if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
            continue;
        }
        let width = if wide == CellWide::Wide { 2 } else { 1 };
        let tag = raw.content_tag()?;

        // Each query is an FFI call, so skip style and color lookups for plain cells.
        let style = if raw.has_styling()? {
            Some(cell.style()?)
        } else {
            None
        };

        let mut fg = colors.foreground;
        let mut bg = None;
        if style.is_some() {
            fg = cell.fg_color()?.unwrap_or(colors.foreground);
            bg = cell.bg_color()?;
        } else if matches!(
            tag,
            CellContentTag::BgColorPalette | CellContentTag::BgColorRgb
        ) {
            bg = cell.bg_color()?;
        }
        if style.is_some_and(|style| style.inverse) {
            let old_fg = fg;
            fg = bg.unwrap_or(colors.background);
            bg = Some(old_fg);
        }
        if let Some(bg) = bg.filter(|bg| *bg != colors.background) {
            push_bg(&mut out.bgs, col, width, rgb(bg));
        }

        text.clear();
        match tag {
            CellContentTag::Codepoint => {
                if let Some(character) = char::from_u32(raw.codepoint()?).filter(|c| *c != '\0') {
                    text.push(character);
                }
            }
            CellContentTag::CodepointGrapheme => cell.graphemes_utf8(text)?,
            _ => {}
        }
        if style.is_some_and(|style| style.invisible) {
            continue;
        }

        let flags = style.map_or(TextFlags::default(), |style| text_flags(&style));
        let decorated = flags.contains(TextFlags::UNDERLINE) || flags.contains(TextFlags::STRIKE);
        let blank = text.is_empty() || text == " ";
        let fg = rgb(fg);

        // Wide glyphs and grapheme clusters get their own span: the renderer
        // spaces glyphs one cell apart.
        let own_span = width == 2 || text.chars().nth(1).is_some();
        let merge = !own_span && !last_own_span;

        if blank {
            // A blank cell is only kept when it shows a line, or when it
            // joins the words around it into one span.
            let joins = text == " " && merge && can_merge(&out.texts, col, fg, flags);
            if !decorated && !joins {
                continue;
            }
            text.clear();
            text.push(' ');
        }

        last_own_span = own_span;
        push_text(&mut out.texts, col, width, text, fg, flags, merge);
    }

    Ok(out)
}

fn text_flags(style: &Style) -> TextFlags {
    let mut flags = TextFlags::default();
    if style.bold {
        flags |= TextFlags::BOLD;
    }
    if style.italic {
        flags |= TextFlags::ITALIC;
    }
    if style.faint {
        flags |= TextFlags::FAINT;
    }
    if style.underline != Underline::None {
        flags |= TextFlags::UNDERLINE;
    }
    if style.underline == Underline::Curly {
        flags |= TextFlags::CURLY;
    }
    if style.strikethrough {
        flags |= TextFlags::STRIKE;
    }

    flags
}

fn push_bg(bgs: &mut Vec<BgSpan>, col: u16, width: u16, color: Rgb) {
    if let Some(last) = bgs.last_mut()
        && last.color == color
        && last.col + last.len == col
    {
        last.len += width;
    } else {
        bgs.push(BgSpan {
            col,
            len: width,
            color,
        });
    }
}

/// The last span ends at `col` and has the same style.
fn can_merge(spans: &[TextSpan], col: u16, fg: Rgb, flags: TextFlags) -> bool {
    spans
        .last()
        .is_some_and(|last| last.col + last.cells == col && last.fg == fg && last.flags == flags)
}

fn push_text(
    spans: &mut Vec<TextSpan>,
    col: u16,
    width: u16,
    text: &str,
    fg: Rgb,
    flags: TextFlags,
    merge: bool,
) {
    if merge && can_merge(spans, col, fg, flags) {
        if let Some(last) = spans.last_mut() {
            last.text.push_str(text);
            last.cells += 1;
        }
        return;
    }

    spans.push(TextSpan {
        col,
        cells: width,
        text: text.to_owned(),
        fg,
        flags,
    });
}

#[cfg(test)]
mod tests {
    use swarm_term::ProgramState;

    use super::*;

    const RED: Rgb = Rgb(200, 10, 10);
    const FG: Rgb = Rgb(230, 230, 230);
    const BG: Rgb = Rgb(10, 10, 12);

    struct Harness {
        emulator: Emulator,
        replies: flume::Receiver<Vec<u8>>,
    }

    fn palette() -> Palette {
        let mut ansi = [Rgb(100, 100, 100); 16];
        ansi[1] = RED;
        Palette {
            fg: FG,
            bg: BG,
            cursor: Rgb(255, 255, 255),
            ansi,
        }
    }

    fn harness_with(rows: u16, scrollback_bytes: usize) -> Harness {
        let (replies_sender, replies) = flume::unbounded();
        let (events, _) = flume::unbounded();
        let emulator = Emulator::new(
            Size {
                cols: 20,
                rows,
                cell_width: 8,
                cell_height: 16,
            },
            &palette(),
            scrollback_bytes,
            Sinks {
                replies: replies_sender,
                events,
            },
        )
        .expect("emulator");

        Harness { emulator, replies }
    }

    fn harness() -> Harness {
        harness_with(10, 1 << 20)
    }

    fn row_text(row: &Row) -> String {
        let mut line = String::new();
        for span in &row.texts {
            while line.chars().count() < usize::from(span.col) {
                line.push(' ');
            }
            line.push_str(&span.text);
        }
        line
    }

    fn first_row(emulator: &mut Emulator) -> Row {
        let update = emulator.flush().expect("flush").expect("update");
        update.rows.into_iter().next().expect("a row").1
    }

    #[test]
    fn plain_text_becomes_one_span() {
        let mut harness = harness();
        harness.emulator.write(b"hello");

        let row = first_row(&mut harness.emulator);

        assert_eq!(row_text(&row), "hello");
        assert_eq!(row.texts.len(), 1);
    }

    #[test]
    fn sixteen_color_foreground_uses_the_palette() {
        let mut harness = harness();
        harness.emulator.write(b"\x1b[31mred");

        let row = first_row(&mut harness.emulator);

        assert_eq!(row.texts[0].fg, RED);
    }

    #[test]
    fn indexed_256_foreground_resolves() {
        let mut harness = harness();
        harness.emulator.write(b"\x1b[38;5;196mred");

        let row = first_row(&mut harness.emulator);

        assert_eq!(row.texts[0].fg, Rgb(255, 0, 0));
    }

    #[test]
    fn truecolor_foreground_resolves() {
        let mut harness = harness();
        harness.emulator.write(b"\x1b[38;2;10;20;30mx");

        let row = first_row(&mut harness.emulator);

        assert_eq!(row.texts[0].fg, Rgb(10, 20, 30));
    }

    #[test]
    fn backgrounds_become_spans() {
        let mut harness = harness();
        harness
            .emulator
            .write(b"\x1b[41ma\x1b[48;5;21mb\x1b[48;2;1;2;3mc");

        let row = first_row(&mut harness.emulator);

        let colors: Vec<Rgb> = row.bgs.iter().map(|span| span.color).collect();
        assert_eq!(colors, [RED, Rgb(0, 0, 255), Rgb(1, 2, 3)]);
    }

    #[test]
    fn text_flags_are_reported() {
        let mut harness = harness();
        harness
            .emulator
            .write(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU\x1b[0m\x1b[9mS");

        let row = first_row(&mut harness.emulator);

        let flags: Vec<TextFlags> = row.texts.iter().map(|span| span.flags).collect();
        assert_eq!(
            flags,
            [
                TextFlags::BOLD,
                TextFlags::ITALIC,
                TextFlags::UNDERLINE,
                TextFlags::STRIKE
            ]
        );
    }

    #[test]
    fn wide_character_is_its_own_span() {
        let mut harness = harness();
        harness.emulator.write("a世b".as_bytes());

        let row = first_row(&mut harness.emulator);

        let spans: Vec<(&str, u16, u16)> = row
            .texts
            .iter()
            .map(|span| (span.text.as_str(), span.col, span.cells))
            .collect();
        assert_eq!(spans, [("a", 0, 1), ("世", 1, 2), ("b", 3, 1)]);
    }

    #[test]
    fn update_only_carries_changed_rows() {
        let mut harness = harness();
        harness.emulator.write(b"one\r\ntwo");
        harness.emulator.flush().expect("flush");

        harness.emulator.write(b"\x1b[2;1Hxyz");
        let update = harness.emulator.flush().expect("flush").expect("update");

        let indexes: Vec<u16> = update.rows.iter().map(|(index, _)| *index).collect();
        assert_eq!(indexes, [1]);
    }

    #[test]
    fn flush_without_changes_sends_nothing() {
        let mut harness = harness();
        harness.emulator.write(b"one");
        harness.emulator.flush().expect("flush");

        let update = harness.emulator.flush().expect("flush");

        assert!(update.is_none());
    }

    #[test]
    fn scrollback_numbering_survives_three_screens() {
        let mut harness = harness();
        for chunk in 0..3 {
            for line in (chunk * 10 + 1)..=(chunk * 10 + 10) {
                harness
                    .emulator
                    .write(format!("line {line}\r\n").as_bytes());
            }
            harness.emulator.flush().expect("flush");
        }

        let update = harness.emulator.flush().expect("flush");
        let (from, rows) = harness.emulator.history_rows(0, 100).expect("history");

        assert!(update.is_none());
        assert_eq!(from, 0);
        assert_eq!(rows.len(), 21);
        assert_eq!(row_text(&rows[0]), "line 1");
        assert_eq!(row_text(&rows[20]), "line 21");
    }

    #[test]
    fn history_fetch_returns_the_requested_lines() {
        let mut harness = harness();
        for line in 1..=40 {
            harness
                .emulator
                .write(format!("line {line}\r\n").as_bytes());
        }
        harness.emulator.flush().expect("flush");

        let (from, rows) = harness.emulator.history_rows(5, 3).expect("history");

        let texts: Vec<String> = rows.iter().map(row_text).collect();
        assert_eq!(from, 5);
        assert_eq!(texts, ["line 6", "line 7", "line 8"]);
    }

    #[test]
    fn history_fetch_clamps_to_existing_lines() {
        let mut harness = harness();
        for line in 1..=15 {
            harness
                .emulator
                .write(format!("line {line}\r\n").as_bytes());
        }
        harness.emulator.flush().expect("flush");

        let (_, rows) = harness.emulator.history_rows(2, 1000).expect("history");

        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn snapshot_includes_screen_and_history() {
        let mut harness = harness();
        for line in 1..=15 {
            harness
                .emulator
                .write(format!("line {line}\r\n").as_bytes());
        }

        let (_, snapshot) = harness.emulator.snapshot(1000).expect("snapshot");

        assert_eq!(snapshot.rows.len(), 10);
        assert_eq!(snapshot.history_rows.len(), 6);
        assert_eq!(snapshot.screen.history.len, 6);
    }

    #[test]
    fn snapshot_returns_the_pending_update_with_the_text() {
        let mut harness = harness();
        harness.emulator.write(b"pending text");

        let (update, snapshot) = harness.emulator.snapshot(10).expect("snapshot");

        let update = update.expect("update");
        assert_eq!(row_text(&update.rows[0].1), "pending text");
        assert_eq!(row_text(&snapshot.rows[0]), "pending text");
    }

    #[test]
    fn palette_changes_the_resolved_default_colors() {
        let mut harness = harness();
        harness.emulator.write(b"hi");
        harness.emulator.flush().expect("flush");

        let new_palette = Palette {
            fg: Rgb(1, 2, 3),
            bg: Rgb(4, 5, 6),
            ..palette()
        };
        harness.emulator.set_palette(&new_palette).expect("palette");
        let update = harness.emulator.flush().expect("flush").expect("update");

        assert_eq!(update.screen.colors.fg, Rgb(1, 2, 3));
        assert_eq!(update.screen.colors.bg, Rgb(4, 5, 6));
        assert_eq!(update.rows.len(), 10);
        assert_eq!(update.rows[0].1.texts[0].fg, Rgb(1, 2, 3));
    }

    #[test]
    fn resize_resets_history_and_resends_rows() {
        let mut harness = harness();
        harness.emulator.write(b"hi");
        harness.emulator.flush().expect("flush");

        let mut size = harness.emulator.size();
        size.cols = 30;
        size.rows = 12;
        harness.emulator.resize(size).expect("resize");
        let update = harness.emulator.flush().expect("flush").expect("update");

        assert!(update.history_reset);
        assert_eq!(update.rows.len(), 12);
    }

    #[test]
    fn device_attributes_query_gets_a_reply() {
        let mut harness = harness();

        harness.emulator.write(b"\x1b[c");

        let reply = harness.replies.try_recv().expect("reply");
        assert!(reply.starts_with(b"\x1b[?"));
    }

    #[test]
    fn support_query_is_answered_before_device_attributes() {
        let mut harness = harness();
        let chunk = b"\x1b]7501;?\x07\x1b[c";

        harness.emulator.scan_status(chunk);
        harness.emulator.write(chunk);

        let first = harness.replies.try_recv().expect("first reply");
        let second = harness.replies.try_recv().expect("second reply");
        assert_eq!(first, b"\x1b]7501;?\x1b\\");
        assert!(second.starts_with(b"\x1b[?"));
    }

    #[test]
    fn scan_returns_reports_in_order() {
        let mut harness = harness();

        let statuses = harness
            .emulator
            .scan_status(b"\x1b]7501;state=working\x07\x1b]7501;state=done\x07");

        let states: Vec<_> = statuses.iter().map(|status| status.state).collect();
        assert_eq!(states, vec![ProgramState::Working, ProgramState::Done]);
    }

    #[test]
    fn xtversion_query_reports_swarm() {
        let mut harness = harness();

        harness.emulator.write(b"\x1b[>q");

        let reply = harness.replies.try_recv().expect("reply");
        assert!(String::from_utf8_lossy(&reply).contains("swarm"));
    }

    #[test]
    fn paste_is_bracketed_only_when_the_program_asked() {
        let mut harness = harness();
        let plain = harness.emulator.encode_paste("a\nb").expect("paste");
        harness.emulator.write(b"\x1b[?2004h");

        let bracketed = harness.emulator.encode_paste("a\nb").expect("paste");

        assert_eq!(plain, b"a\rb");
        assert_eq!(bracketed, b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn focus_is_reported_only_when_the_program_asked() {
        let mut harness = harness();
        let silent = harness.emulator.focus_report(true).expect("focus");
        harness.emulator.write(b"\x1b[?1004h");

        let gained = harness.emulator.focus_report(true).expect("focus");
        let lost = harness.emulator.focus_report(false).expect("focus");

        assert_eq!(silent, None);
        assert_eq!(gained.as_deref(), Some(&b"\x1b[I"[..]));
        assert_eq!(lost.as_deref(), Some(&b"\x1b[O"[..]));
    }

    #[test]
    fn title_and_bell_reach_the_event_sink() {
        let (replies, _) = flume::unbounded();
        let (events, received) = flume::unbounded();
        let mut emulator = Emulator::new(
            Size {
                cols: 20,
                rows: 5,
                cell_width: 8,
                cell_height: 16,
            },
            &palette(),
            1 << 20,
            Sinks { replies, events },
        )
        .expect("emulator");

        emulator.write(b"\x1b]2;my title\x07");
        emulator.write(b"\x07");

        let outputs: Vec<TerminalOutput> = received.try_iter().collect();
        assert!(outputs.contains(&TerminalOutput::Title("my title".to_owned())));
        assert!(outputs.contains(&TerminalOutput::Bell));
    }

    #[test]
    fn palette_change_resets_history() {
        let mut harness = harness();
        harness.emulator.write(b"hi");
        harness.emulator.flush().expect("flush");

        harness.emulator.set_palette(&palette()).expect("palette");
        let update = harness.emulator.flush().expect("flush").expect("update");

        assert!(update.history_reset);
    }

    #[test]
    fn synchronized_output_holds_the_flush_until_it_ends() {
        let mut harness = harness();
        let start = Instant::now();
        harness.emulator.write(b"\x1b[?2026hhello");

        let during = harness.emulator.sync_hold(start).expect("hold");
        harness.emulator.write(b"\x1b[?2026l");
        let after = harness.emulator.sync_hold(start).expect("hold");
        let update = harness.emulator.flush().expect("flush");

        assert_eq!(during, Some(start + SYNC_OUTPUT_CAP));
        assert_eq!(after, None);
        assert!(update.is_some());
    }

    #[test]
    fn synchronized_output_is_capped() {
        let mut harness = harness();
        let start = Instant::now();
        harness.emulator.write(b"\x1b[?2026hhello");
        harness.emulator.sync_hold(start).expect("hold");

        let late = harness
            .emulator
            .sync_hold(start + SYNC_OUTPUT_CAP)
            .expect("hold");

        assert_eq!(late, None);
    }

    #[test]
    fn hidden_cursor_keeps_its_position() {
        let mut harness = harness();
        harness.emulator.write(b"\x1b[3;5H\x1b[?25l");

        let update = harness.emulator.flush().expect("flush").expect("update");

        let cursor = update.screen.cursor;
        assert_eq!((cursor.col, cursor.row, cursor.visible), (4, 2, false));
    }

    #[test]
    fn snapshot_carries_title_and_working_directory() {
        let mut harness = harness();
        harness
            .emulator
            .write(b"\x1b]2;my title\x07\x1b]7;file://host/tmp/dir\x07");

        let (_, snapshot) = harness.emulator.snapshot(10).expect("snapshot");

        assert_eq!(snapshot.title.as_deref(), Some("my title"));
        assert!(
            snapshot
                .working_directory
                .is_some_and(|directory| directory.contains("/tmp/dir"))
        );
    }

    #[test]
    fn words_on_a_line_share_one_span() {
        let mut harness = harness();
        harness.emulator.write(b"hello big world");

        let row = first_row(&mut harness.emulator);

        assert_eq!(row.texts.len(), 1);
        assert_eq!(row.texts[0].text, "hello big world");
    }

    #[test]
    fn underlined_blanks_are_kept() {
        let mut harness = harness();
        harness.emulator.write(b"a\x1b[4m   \x1b[0m");

        let row = first_row(&mut harness.emulator);

        let underlined = row
            .texts
            .iter()
            .find(|span| span.flags.contains(TextFlags::UNDERLINE))
            .expect("underlined span");
        assert_eq!((underlined.col, underlined.cells), (1, 3));
    }

    #[test]
    fn combining_mark_cluster_is_its_own_span() {
        let mut harness = harness();
        harness.emulator.write("ae\u{301}b".as_bytes());

        let row = first_row(&mut harness.emulator);

        let spans: Vec<(&str, u16)> = row
            .texts
            .iter()
            .map(|span| (span.text.as_str(), span.col))
            .collect();
        assert_eq!(spans, [("a", 0), ("e\u{301}", 1), ("b", 2)]);
    }

    #[test]
    fn emoji_with_variation_selector_is_its_own_span() {
        let mut harness = harness();
        harness.emulator.write("a\u{2764}\u{fe0f}b".as_bytes());

        let row = first_row(&mut harness.emulator);

        let texts: Vec<&str> = row.texts.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(texts, ["a", "\u{2764}\u{fe0f}", "b"]);
    }
}
