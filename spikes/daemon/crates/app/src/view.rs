use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, Font, FontStyle, FontWeight, Hsla,
    KeyDownEvent, Pixels, Render, Rgba, ScrollEvent, SharedString, StrikethroughStyle, TextRun,
    UnderlineStyle, Window, div, prelude::*, px,
};
use proto::{Event, History, Request, Rgb, Row, Screen, Size, Snapshot, TermId, TextSpan};

use crate::client::Client;
use crate::element::TerminalElement;

/// Scrollback lines fetched around a missing range, so scrolling rarely waits.
const FETCH_MARGIN: u64 = 200;
const MAX_CACHED_HISTORY: usize = 20_000;

#[derive(Clone)]
pub struct Metrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell_width: Pixels,
    pub line_height: Pixels,
}

pub struct BgSpan {
    pub col: u16,
    pub len: u16,
    pub color: Hsla,
}

/// Adjacent cells with one style, shaped and painted as one line.
pub struct TextBatch {
    pub col: u16,
    pub cells: u16,
    pub text: SharedString,
    pub run: TextRun,
}

#[derive(Default)]
pub struct RowLayout {
    pub bgs: Vec<BgSpan>,
    pub batches: Vec<TextBatch>,
}

pub struct CursorLayout {
    pub col: u16,
    pub row: u16,
    pub wide: bool,
    pub style: proto::CursorStyle,
    pub color: Hsla,
    /// The glyph under a block cursor, redrawn in the background color.
    pub glyph: Option<(SharedString, TextRun)>,
}

pub struct Frame {
    pub rows: Vec<Rc<RowLayout>>,
    pub cursor: Option<CursorLayout>,
    pub background: Hsla,
}

#[derive(Default)]
pub struct Stats {
    frames: u32,
    frame_total: Duration,
    frame_max: Duration,
    updates: u32,
    rows_received: u32,
    /// Key sent → update with changed rows applied.
    pub key_to_update_us: Vec<u32>,
    /// Key sent → first paint after that update.
    pub key_to_paint_us: Vec<u32>,
}

impl Stats {
    pub fn record_frame(&mut self, cost: Duration) {
        self.frames += 1;
        self.frame_total += cost;
        self.frame_max = self.frame_max.max(cost);
    }

    fn report(&mut self) {
        if self.frames == 0 && self.updates == 0 {
            return;
        }
        eprintln!(
            "[stats] {} fps | frame cpu avg {:.2}ms max {:.2}ms | {} updates, {} rows",
            self.frames,
            self.frame_total.as_secs_f64() * 1e3 / f64::from(self.frames.max(1)),
            self.frame_max.as_secs_f64() * 1e3,
            self.updates,
            self.rows_received,
        );
        self.frames = 0;
        self.frame_total = Duration::ZERO;
        self.frame_max = Duration::ZERO;
        self.updates = 0;
        self.rows_received = 0;
    }
}

pub struct TerminalView {
    id: TermId,
    client: Client,
    focus: FocusHandle,
    metrics: Metrics,
    screen: Screen,
    rows: Vec<Rc<RowLayout>>,
    /// Scrollback by absolute line number; filled by the snapshot and fetches.
    history: HashMap<u64, Rc<RowLayout>>,
    fetching: bool,
    /// Lines scrolled up from the bottom.
    scroll: u64,
    scroll_remainder: f32,
    grid: (u16, u16),
    pending_key: Option<Instant>,
    pending_paint: Option<Instant>,
    first_paint: Option<Instant>,
    pub stats: Stats,
}

impl TerminalView {
    pub fn new(
        client: Client,
        snapshot: Snapshot,
        events: flume::Receiver<Event>,
        metrics: Metrics,
        started: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv_async().await {
                let alive = this.update(cx, |view, cx| {
                    view.apply(event, cx);
                    // Apply everything already queued before one re-render.
                    for event in events.try_iter() {
                        view.apply(event, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            eprintln!("[app] daemon connection closed");
            cx.update(|cx| cx.quit());
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |view, _| view.stats.report()).is_err() {
                    return;
                }
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let font = metrics.font.clone();
        let rows = snapshot
            .rows
            .iter()
            .map(|r| Rc::new(layout(r, &font)))
            .collect();
        let first = snapshot.screen.history.end() - snapshot.history_rows.len() as u64;
        let history = snapshot
            .history_rows
            .iter()
            .enumerate()
            .map(|(i, r)| (first + i as u64, Rc::new(layout(r, &font))))
            .collect();
        Self {
            id: snapshot.id,
            client,
            focus,
            metrics,
            grid: (snapshot.screen.cols, snapshot.screen.rows),
            screen: snapshot.screen,
            rows,
            history,
            fetching: false,
            scroll: 0,
            scroll_remainder: 0.,
            pending_key: None,
            pending_paint: None,
            first_paint: Some(started),
            stats: Stats::default(),
        }
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    fn apply(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Update(update) if update.id == self.id => {
                self.stats.updates += 1;
                self.stats.rows_received += update.rows.len() as u32;
                if update.history_reset {
                    self.history.clear();
                    self.fetching = false;
                }
                let rows = usize::from(update.screen.rows);
                self.rows.resize_with(rows, Default::default);
                for (y, row) in &update.rows {
                    if let Some(slot) = self.rows.get_mut(usize::from(*y)) {
                        *slot = Rc::new(layout(row, &self.metrics.font));
                    }
                }
                self.set_screen(update.screen);
                if !update.rows.is_empty()
                    && let Some(t) = self.pending_key.take()
                {
                    self.stats.key_to_update_us.push(micros(t.elapsed()));
                    self.pending_paint = Some(t);
                }
            }
            Event::History { id, from, rows } if id == self.id => {
                self.fetching = false;
                for (i, row) in rows.iter().enumerate() {
                    self.history
                        .insert(from + i as u64, Rc::new(layout(row, &self.metrics.font)));
                }
            }
            Event::Exited { id } if id == self.id => {
                eprintln!("[app] terminal {id} exited");
                cx.quit();
            }
            Event::Error { message } => eprintln!("[app] daemon error: {message}"),
            _ => {}
        }
    }

    fn set_screen(&mut self, screen: Screen) {
        let old = self.screen.history;
        let new = screen.history;
        // Keep the view on the same lines while new output scrolls in.
        if self.scroll > 0 {
            self.scroll += new.end().saturating_sub(old.end());
        }
        self.scroll = if screen.alt_screen {
            0
        } else {
            self.scroll.min(new.len)
        };
        if self.history.len() > MAX_CACHED_HISTORY || new.start > old.start {
            self.history.retain(|line, _| *line >= new.start);
        }
        self.screen = screen;
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.grid {
            return;
        }
        self.grid = (cols, rows);
        self.client.send(Request::Resize {
            id: self.id,
            size: Size {
                cols,
                rows,
                cell_width: f32::from(self.metrics.cell_width) as u16,
                cell_height: f32::from(self.metrics.line_height) as u16,
            },
        });
    }

    /// The lines on screen, from scrollback when scrolled up, plus the range
    /// of scrollback lines not cached yet.
    fn visible_lines(&self) -> (Vec<Rc<RowLayout>>, Option<(u64, u64)>) {
        let History { start, len } = self.screen.history;
        let top = start + len - self.scroll;
        let mut missing: Option<(u64, u64)> = None;
        let lines = (0..self.rows.len() as u64)
            .map(|i| {
                let line = top + i;
                if line >= start + len {
                    return self.rows[(line - start - len) as usize].clone();
                }
                self.history.get(&line).cloned().unwrap_or_else(|| {
                    let (lo, hi) = missing.get_or_insert((line, line));
                    *lo = (*lo).min(line);
                    *hi = (*hi).max(line);
                    Rc::default()
                })
            })
            .collect();
        (lines, missing)
    }

    /// What to paint. Requests missing scrollback, which shows blank until it arrives.
    pub fn frame(&mut self, focused: bool) -> Frame {
        let rows = self.rows.len();
        let (lines, missing) = self.visible_lines();
        if let Some((lo, hi)) = missing
            && !self.fetching
        {
            self.fetching = true;
            let from = lo
                .saturating_sub(FETCH_MARGIN)
                .max(self.screen.history.start);
            self.client.send(Request::FetchHistory {
                id: self.id,
                from,
                count: (hi + FETCH_MARGIN - from) as u32,
            });
        }

        let colors = self.screen.colors;
        let cursor = self.screen.cursor.and_then(|c| {
            let row = u64::from(c.row) + self.scroll;
            if row >= rows as u64 {
                return None;
            }
            let color = hsla(colors.cursor.unwrap_or(colors.fg));
            let mut layout = CursorLayout {
                col: c.col,
                row: row as u16,
                wide: false,
                style: if focused {
                    c.style
                } else {
                    proto::CursorStyle::BlockHollow
                },
                color,
                glyph: None,
            };
            let under = self.rows.get(usize::from(c.row)).and_then(|r| {
                r.batches
                    .iter()
                    .find(|b| b.col <= c.col && c.col < b.col + b.cells)
            });
            if let Some(batch) = under {
                let narrow = usize::from(batch.cells) == batch.text.chars().count();
                let glyph: String = if narrow {
                    batch
                        .text
                        .chars()
                        .nth(usize::from(c.col - batch.col))
                        .map(String::from)
                        .unwrap_or_default()
                } else {
                    batch.text.to_string()
                };
                layout.wide = !narrow;
                let mut run = batch.run.clone();
                run.len = glyph.len();
                run.color = hsla(colors.bg);
                layout.glyph = Some((glyph.into(), run));
            }
            Some(layout)
        });

        Frame {
            rows: lines,
            cursor,
            background: hsla(colors.bg),
        }
    }

    /// Called after each paint, for latency and startup numbers.
    pub fn painted(&mut self) {
        if let Some(t) = self.pending_paint.take() {
            self.stats.key_to_paint_us.push(micros(t.elapsed()));
        }
        if let Some(t) = self.first_paint.take() {
            eprintln!(
                "[app] first paint {:.1} ms after start",
                t.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    pub fn visible_text(&self) -> String {
        let mut out = String::new();
        for row in &self.visible_lines().0 {
            let mut line = String::new();
            for b in &row.batches {
                while line.chars().count() < usize::from(b.col) {
                    line.push(' ');
                }
                line.push_str(&b.text);
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform {
            match ks.key.as_str() {
                "q" => cx.quit(),
                "v" => self.paste(cx),
                #[cfg(feature = "snapshot")]
                "s" => save_snapshot(_window),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        let m = &ks.modifiers;
        let mut mods = 0;
        for (on, bit) in [
            (m.shift, proto::Key::SHIFT),
            (m.alt, proto::Key::ALT),
            (m.control, proto::Key::CTRL),
        ] {
            if on {
                mods |= bit;
            }
        }
        // GPUI reports Enter/Tab as "\n"/"\t"; the daemon's encoder derives those from the key.
        let text = ks
            .key_char
            .clone()
            .filter(|s| !s.chars().any(char::is_control));
        self.pending_key.get_or_insert_with(Instant::now);
        self.client.send(Request::Key {
            id: self.id,
            key: proto::Key {
                name: ks.key.clone(),
                mods,
                text,
                repeat: event.is_held,
            },
        });
        if self.scroll != 0 {
            self.scroll = 0;
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item: ClipboardItem| item.text())
        {
            self.client.send(Request::Paste { id: self.id, text });
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.screen.alt_screen {
            return;
        }
        let delta = event.delta.pixel_delta(self.metrics.line_height).y;
        self.scroll_remainder += f32::from(delta) / f32::from(self.metrics.line_height);
        let lines = self.scroll_remainder.trunc();
        if lines != 0. {
            self.scroll_remainder -= lines;
            // Positive wheel delta means "content down", i.e. back into history.
            self.scroll =
                (self.scroll as i64 + lines as i64).clamp(0, self.screen.history.len as i64) as u64;
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(hsla(self.screen.colors.bg))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll))
            .child(TerminalElement::new(cx.entity()))
    }
}

#[cfg(feature = "snapshot")]
pub fn save_snapshot(window: &Window) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::var("SPIKE_SNAPSHOT_DIR").unwrap_or_else(|_| ".".into());
    let path = format!(
        "{dir}/snapshot-{}-{}.png",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    );
    match window.render_to_image().map(|img| img.save(&path)) {
        Ok(Ok(())) => eprintln!("[snapshot] {path}"),
        Ok(Err(e)) => eprintln!("[snapshot] save failed: {e}"),
        Err(e) => eprintln!("[snapshot] render failed: {e:#}"),
    }
}

fn micros(d: Duration) -> u32 {
    d.as_micros().min(u128::from(u32::MAX)) as u32
}

pub fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.0) / 255.,
        g: f32::from(c.1) / 255.,
        b: f32::from(c.2) / 255.,
        a: 1.,
    }
    .into()
}

fn layout(row: &Row, base: &Font) -> RowLayout {
    RowLayout {
        bgs: row
            .bgs
            .iter()
            .map(|b| BgSpan {
                col: b.col,
                len: b.len,
                color: hsla(b.color),
            })
            .collect(),
        batches: row
            .texts
            .iter()
            .map(|t| TextBatch {
                col: t.col,
                cells: t.cells,
                run: text_run(base, t),
                text: t.text.clone().into(),
            })
            .collect(),
    }
}

fn text_run(base: &Font, span: &TextSpan) -> TextRun {
    let f = span.flags;
    let mut font = base.clone();
    if f & TextSpan::BOLD != 0 {
        font.weight = FontWeight::BOLD;
    }
    if f & TextSpan::ITALIC != 0 {
        font.style = FontStyle::Italic;
    }
    let mut color = hsla(span.fg);
    if f & TextSpan::FAINT != 0 {
        color.a = 0.6;
    }
    TextRun {
        len: span.text.len(),
        font,
        color,
        background_color: None,
        underline: (f & TextSpan::UNDERLINE != 0).then_some(UnderlineStyle {
            thickness: px(1.),
            color: None,
            wavy: f & TextSpan::CURLY != 0,
        }),
        strikethrough: (f & TextSpan::STRIKE != 0).then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    }
}
