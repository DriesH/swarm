use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, Font, FontStyle, FontWeight, Hsla,
    KeyDownEvent, Pixels, Render, Rgba, ScrollEvent, StrikethroughStyle, TextRun, UnderlineStyle,
    Window, div, prelude::*, px,
};
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions, key,
    render::{CellIterator, CursorVisualStyle, Dirty, RowIterator},
    screen::CellWide,
    style::{RgbColor, Underline},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode,
        PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes,
    },
};
use portable_pty::PtySize;

use crate::element::TerminalElement;
use crate::keys;
use crate::pty::Pty;

pub const BACKGROUND: RgbColor = RgbColor {
    r: 0x0d,
    g: 0x0d,
    b: 0x0f,
};
pub const FOREGROUND: RgbColor = RgbColor {
    r: 0xed,
    g: 0xed,
    b: 0xef,
};
const SCROLLBACK_LINES: usize = 10_000;

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
    pub text: String,
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
    pub style: CursorVisualStyle,
    pub color: Hsla,
    /// The glyph under a block cursor, redrawn in the background color.
    pub glyph: Option<(String, TextRun)>,
}

pub struct Frame {
    pub rows: Vec<Rc<RowLayout>>,
    pub cursor: Option<CursorLayout>,
    pub background: Hsla,
    pub rebuilt: u32,
}

/// Per-second counters, printed by a timer so floods show up even without frames.
#[derive(Default)]
pub struct Stats {
    frames: u32,
    frame_total: Duration,
    frame_max: Duration,
    rows_rebuilt: u32,
    bytes: usize,
    parse: Duration,
}

impl Stats {
    pub fn record_frame(&mut self, cost: Duration, rows_rebuilt: u32) {
        self.frames += 1;
        self.frame_total += cost;
        self.frame_max = self.frame_max.max(cost);
        self.rows_rebuilt += rows_rebuilt;
    }

    fn report(&mut self) {
        if self.frames == 0 && self.bytes == 0 {
            return;
        }
        let n = f64::from(self.frames.max(1));
        let parse_ms = self.parse.as_secs_f64() * 1e3;
        eprintln!(
            "[stats] {} fps | frame cpu avg {:.2}ms max {:.2}ms | rows rebuilt/frame {:.1} | parsed {:.1} MB in {:.0}ms ({:.0} MB/s)",
            self.frames,
            self.frame_total.as_secs_f64() * 1e3 / n,
            self.frame_max.as_secs_f64() * 1e3,
            f64::from(self.rows_rebuilt) / n,
            self.bytes as f64 / 1e6,
            parse_ms,
            if parse_ms > 0. {
                self.bytes as f64 / 1e3 / parse_ms
            } else {
                0.
            },
        );
        *self = Self::default();
    }
}

pub struct TerminalView {
    term: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    pty: Pty,
    focus: FocusHandle,
    metrics: Metrics,
    grid: (u16, u16),
    rows: Vec<Rc<RowLayout>>,
    scroll_remainder: f32,
    pub stats: Stats,
}

impl TerminalView {
    pub fn new(metrics: Metrics, window: &mut Window, cx: &mut Context<Self>) -> Result<Self> {
        let grid = (80, 24);
        let (pty, output, wake) = Pty::spawn(pty_size(grid, &metrics))?;

        let mut term = Terminal::new(TerminalOptions {
            cols: grid.0,
            rows: grid.1,
            max_scrollback: SCROLLBACK_LINES,
        })?;
        term.set_default_fg_color(Some(FOREGROUND))?
            .set_default_bg_color(Some(BACKGROUND))?;
        let replies = pty.input.clone();
        term.on_pty_write(move |_, data| {
            let _ = replies.send(data.to_vec());
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
                tertiary: Default::default(),
            })
        })?
        .on_xtversion(|_| Some("swarm-spike"))?;

        // Bytes are read on a thread; the terminal is !Send so parsing happens here.
        cx.spawn(async move |this, cx| {
            while wake.recv_async().await.is_ok() {
                let alive = this.update(cx, |view, cx| {
                    view.feed(&output.take());
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            // Shell exited.
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

        Ok(Self {
            term,
            render_state: RenderState::new()?,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            pty,
            focus,
            metrics,
            grid,
            rows: Vec::new(),
            scroll_remainder: 0.,
            stats: Stats::default(),
        })
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    fn feed(&mut self, bytes: &[u8]) {
        let start = Instant::now();
        self.term.vt_write(bytes);
        self.stats.parse += start.elapsed();
        self.stats.bytes += bytes.len();
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if (cols, rows) == self.grid {
            return Ok(());
        }
        self.grid = (cols, rows);
        let size = pty_size(self.grid, &self.metrics);
        self.term.resize(
            cols,
            rows,
            size.pixel_width.into(),
            size.pixel_height.into(),
        )?;
        self.pty.resize(size)?;
        Ok(())
    }

    /// Pulls a render snapshot and rebuilds only the rows ghostty marks dirty.
    pub fn frame(&mut self, focused: bool) -> Result<Frame> {
        let snap = self.render_state.update(&self.term)?;
        let colors = snap.colors()?;
        let default_fg = colors.foreground;
        let default_bg = colors.background;
        let row_count = usize::from(snap.rows()?);
        let full = snap.dirty()? == Dirty::Full || self.rows.len() != row_count;
        if self.rows.len() != row_count {
            self.rows.resize_with(row_count, Default::default);
        }

        let cursor_pos = if snap.cursor_visible()? {
            snap.cursor_viewport()?
        } else {
            None
        };
        let mut cursor = cursor_pos.map(|vp| CursorLayout {
            col: vp.x,
            row: vp.y,
            wide: false,
            style: if focused {
                snap.cursor_visual_style()
                    .unwrap_or(CursorVisualStyle::Block)
            } else {
                CursorVisualStyle::BlockHollow
            },
            color: hsla(colors.cursor.unwrap_or(default_fg)),
            glyph: None,
        });

        let mut rows = self.row_it.update(&snap)?;
        let mut y: u16 = 0;
        let mut rebuilt = 0;
        while let Some(row) = rows.next() {
            let on_cursor_row = cursor.as_ref().is_some_and(|c| c.row == y);
            if full || on_cursor_row || row.dirty()? {
                let mut layout = RowLayout::default();
                let mut cells = self.cell_it.update(row)?;
                let mut x: u16 = 0;
                let mut text = String::new();
                while let Some(cell) = cells.next() {
                    let col = x;
                    x += 1;
                    let wide = cell.raw_cell()?.wide()?;
                    if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                        continue;
                    }
                    let style = if cell.has_styling()? {
                        Some(cell.style()?)
                    } else {
                        None
                    };
                    let mut fg = cell.fg_color()?.unwrap_or(default_fg);
                    let mut bg = cell.bg_color()?;
                    if style.is_some_and(|s| s.inverse) {
                        let old_fg = fg;
                        fg = bg.unwrap_or(default_bg);
                        bg = Some(old_fg);
                    }
                    let width = if wide == CellWide::Wide { 2 } else { 1 };
                    if let Some(bg) = bg.filter(|bg| *bg != default_bg) {
                        push_bg(&mut layout.bgs, col, width, hsla(bg));
                    }

                    text.clear();
                    if cell.graphemes_len()? > 0 {
                        cell.graphemes_utf8(&mut text)?;
                    }
                    let blank = text.is_empty() || text == " ";
                    if blank || style.is_some_and(|s| s.invisible) {
                        continue;
                    }

                    let run = text_run(&self.metrics.font, fg, style.as_ref(), text.len());
                    if let Some(c) = cursor.as_mut().filter(|c| c.row == y && c.col == col) {
                        c.wide = width == 2;
                        let mut inverse = run.clone();
                        inverse.color = hsla(default_bg);
                        c.glyph = Some((text.clone(), inverse));
                    }
                    push_text(&mut layout.batches, col, width, &text, run);
                }
                self.rows[usize::from(y)] = Rc::new(layout);
                rebuilt += 1;
                row.set_dirty(false)?;
            }
            y += 1;
        }
        snap.set_dirty(Dirty::Clean)?;

        Ok(Frame {
            rows: self.rows.clone(),
            cursor,
            background: hsla(default_bg),
            rebuilt,
        })
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

        let (gkey, unshifted) = keys::ghostty_key(&ks.key);
        let mods = keys::ghostty_mods(&ks.modifiers);
        let text = keys::typed_text(ks);
        let mut consumed = key::Mods::empty();
        if text.is_some() && ks.modifiers.shift {
            consumed |= key::Mods::SHIFT;
        }
        self.key_event
            .set_action(if event.is_held {
                key::Action::Repeat
            } else {
                key::Action::Press
            })
            .set_key(gkey)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text);

        let mut out = Vec::with_capacity(16);
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.term)
            .encode_to_vec(&self.key_event, &mut out);
        if (encoded.is_err() || out.is_empty())
            && let Some(text) = text
        {
            out.clear();
            out.extend_from_slice(text.as_bytes());
        }
        if !out.is_empty() {
            self.term.scroll_viewport(ScrollViewport::Bottom);
            self.pty.write(out);
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item: ClipboardItem| item.text())
        else {
            return;
        };
        let bracketed = self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut out = Vec::with_capacity(text.len() + 12);
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
        }
        out.extend_from_slice(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        if bracketed {
            out.extend_from_slice(b"\x1b[201~");
        }
        self.pty.write(out);
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(self.metrics.line_height).y;
        self.scroll_remainder += f32::from(delta) / f32::from(self.metrics.line_height);
        let lines = self.scroll_remainder.trunc();
        if lines != 0. {
            self.scroll_remainder -= lines;
            // Positive wheel delta means "content down", i.e. back into history.
            self.term
                .scroll_viewport(ScrollViewport::Delta(-(lines as isize)));
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
            .bg(hsla(BACKGROUND))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll))
            .child(TerminalElement::new(cx.entity()))
    }
}

#[cfg(feature = "snapshot")]
fn save_snapshot(window: &Window) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::var("SPIKE_SNAPSHOT_DIR").unwrap_or_else(|_| ".".into());
    let path = format!(
        "{dir}/snapshot-{}.png",
        COUNT.fetch_add(1, Ordering::Relaxed)
    );
    match window.render_to_image().map(|img| img.save(&path)) {
        Ok(Ok(())) => eprintln!("[snapshot] {path}"),
        Ok(Err(e)) => eprintln!("[snapshot] save failed: {e}"),
        Err(e) => eprintln!("[snapshot] render failed: {e:#}"),
    }
}

fn pty_size((cols, rows): (u16, u16), m: &Metrics) -> PtySize {
    PtySize {
        cols,
        rows,
        pixel_width: f32::from(m.cell_width) as u16,
        pixel_height: f32::from(m.line_height) as u16,
    }
}

pub fn hsla(c: RgbColor) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: 1.,
    }
    .into()
}

fn text_run(
    base: &Font,
    fg: RgbColor,
    style: Option<&libghostty_vt::style::Style>,
    len: usize,
) -> TextRun {
    let mut font = base.clone();
    let mut color = hsla(fg);
    let mut underline = None;
    let mut strikethrough = None;
    if let Some(s) = style {
        if s.bold {
            font.weight = FontWeight::BOLD;
        }
        if s.italic {
            font.style = FontStyle::Italic;
        }
        if s.faint {
            color.a = 0.6;
        }
        if s.underline != Underline::None {
            underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: s.underline == Underline::Curly,
            });
        }
        if s.strikethrough {
            strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            });
        }
    }
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline,
        strikethrough,
    }
}

fn push_bg(bgs: &mut Vec<BgSpan>, col: u16, width: u16, color: Hsla) {
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

fn push_text(batches: &mut Vec<TextBatch>, col: u16, width: u16, text: &str, run: TextRun) {
    if let Some(last) = batches.last_mut()
        && last.col + last.cells == col
        // Wide glyphs end a batch: force_width spaces glyphs one cell apart.
        && width == 1
        && last.cells == last.text.chars().count() as u16
        && last.run.font == run.font
        && last.run.color == run.color
        && last.run.underline == run.underline
        && last.run.strikethrough == run.strikethrough
    {
        last.text.push_str(text);
        last.run.len += text.len();
        last.cells += 1;
        return;
    }
    batches.push(TextBatch {
        col,
        cells: width,
        text: text.to_owned(),
        run,
    });
}
