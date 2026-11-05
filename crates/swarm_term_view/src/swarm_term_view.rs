//! The GPUI view of one terminal.
//!
//! [`TerminalView`] keeps a client-side copy of the screen ([`swarm_term::ScreenModel`]),
//! paints it, and talks to the emulator only through [`swarm_term::TerminalChannels`].
//! It does not link libghostty, so app builds need no Zig.

mod box_drawing;
mod element;

use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardEntry, Context, EntityInputHandler, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, Global, Hsla, KeyBinding, KeyDownEvent, Keystroke, NoAction, Pixels,
    Point, Render, Rgba, Role, ScrollEvent, SharedString, Size, Subscription, Task, TextRun,
    UTF16Selection, UnderlineStyle, Window, div, prelude::*, px,
};
use swarm_term::{
    CursorStyle, ExitStatus, Key, Modifiers, Palette, Rgb, ScreenModel, TerminalChannels,
    TerminalInput, TerminalOutput,
};
use swarm_ui::{ActiveTheme as _, FocusNext, FocusPrevious, Theme, edit};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::element::{
    CursorGlyph, CursorLayout, Frame, LayoutCache, MarkedLayout, Metrics, TerminalElement, hsla,
};

/// The font size of a view until [`TerminalView::set_font_size`] says otherwise, in points.
/// It is the settings' default, so the two never differ.
pub const DEFAULT_FONT_SIZE: f32 = swarm_settings::FONT_SIZE_DEFAULT as f32;

/// How soon after a ⌃C that reached an agent a second one is held back.
pub const DOUBLE_INTERRUPT: Duration = Duration::from_secs(2);

/// Key context of a focused terminal.
pub const TERMINAL_CONTEXT: &str = "Terminal";

/// Takes Tab and Shift-Tab away from the focus keys of the root, so they reach the shell.
pub fn shell_key_bindings() -> [KeyBinding; 2] {
    [
        KeyBinding::new("tab", NoAction, Some(TERMINAL_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(TERMINAL_CONTEXT)),
    ]
}

/// Moves focus out of a terminal to the next and the previous Tab stop.
pub fn focus_key_bindings() -> [KeyBinding; 2] {
    [
        KeyBinding::new("ctrl-tab", FocusNext, Some(TERMINAL_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", FocusPrevious, Some(TERMINAL_CONTEXT)),
    ]
}

/// Holds PTY resizes until `deadline`, a time on the GPUI clock
/// (`cx.background_executor().now()`). The view keeps drawing its grid. A refresh of all
/// windows at the deadline makes every view send its size, also a view that GPUI would
/// otherwise reuse from its cache.
pub struct ResizeHold {
    deadline: Instant,
    _release: Task<()>,
}

impl Global for ResizeHold {}

/// Keeps every view from resizing its PTY until `deadline`, for example while the layout
/// animates. A later call replaces the hold and its deadline.
pub fn hold_resizes_until(deadline: Instant, cx: &mut App) {
    let wait = deadline.saturating_duration_since(cx.background_executor().now());
    let release = cx.spawn(async move |cx| {
        cx.background_executor().timer(wait).await;
        cx.update(App::refresh_windows);
    });

    cx.set_global(ResizeHold {
        deadline,
        _release: release,
    });
}

/// True while a [`ResizeHold`] has not reached its deadline.
fn resizes_held(cx: &App) -> bool {
    let now = cx.background_executor().now();

    cx.try_global::<ResizeHold>()
        .is_some_and(|hold| hold.deadline > now)
}

/// What the app may want to react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    TitleChanged(String),
    WorkingDirectoryChanged(String),
    Bell,
    Exited(ExitStatus),
    /// The output ended without an exit, for example when the daemon went away.
    Disconnected,
}

/// Tells whether the user may type into a terminal right now.
type TypingGate = Rc<dyn Fn(&App) -> bool>;

pub struct TerminalView {
    input: flume::Sender<TerminalInput>,
    typing_gate: Option<TypingGate>,
    /// Set for an agent terminal: ⌃D and a quick second ⌃C stay in the view.
    guards_quit: bool,
    /// When the last ⌃C reached the program, on the GPUI clock.
    last_interrupt: Option<Instant>,
    focus_handle: FocusHandle,
    accessible_name: SharedString,
    metrics: Metrics,
    /// True after a font change: the emulator learns the new cell size even when the
    /// columns and rows stay the same.
    resend_size: bool,
    /// `None` until the first snapshot arrives.
    model: Option<ScreenModel>,
    layouts: LayoutCache,
    title: Option<String>,
    working_directory: Option<String>,
    exit_status: Option<ExitStatus>,
    palette: Option<Palette>,
    /// The grid size last sent to the emulator.
    requested_grid: Option<(u16, u16)>,
    /// The scrollback range last asked for, so a short answer does not loop.
    requested_history: Option<(u64, u32)>,
    scroll_remainder: f32,
    marked_text: Option<String>,
    paint_info: PaintInfo,
    _output_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// What the last paint learned, for the input method.
#[derive(Default)]
struct PaintInfo {
    cursor_bounds: Option<Bounds<Pixels>>,
    /// Top left corner of the grid, in window coordinates.
    grid_origin: Point<Pixels>,
    paints: u64,
}

impl TerminalView {
    pub fn new(
        channels: TerminalChannels,
        accessible_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let TerminalChannels { input, output } = channels;

        let focus_handle = cx.focus_handle().tab_stop(true);
        let subscriptions = vec![
            cx.on_focus_in(&focus_handle, window, |view, _, cx| {
                view.send(TerminalInput::Focus(true));
                cx.notify();
            }),
            cx.on_focus_out(&focus_handle, window, |view, _, _, cx| {
                view.send(TerminalInput::Focus(false));
                cx.notify();
            }),
        ];

        let output_task = cx.spawn(async move |this, cx| {
            while let Ok(first) = output.recv_async().await {
                let batch: Vec<TerminalOutput> =
                    std::iter::once(first).chain(output.try_iter()).collect();

                let exited = this.update(cx, |view, cx| {
                    let mut exited = false;

                    for output in batch {
                        exited |= view.apply(output, cx);
                    }

                    view.request_history();
                    cx.notify();

                    exited
                });

                if exited.unwrap_or(true) {
                    return;
                }
            }

            log::debug!("terminal output channel closed");

            if let Err(error) = this.update(cx, |_, cx| cx.emit(TerminalEvent::Disconnected)) {
                log::debug!("the terminal view is gone: {error}");
            }
        });

        let mut view = Self {
            input,
            typing_gate: None,
            guards_quit: false,
            last_interrupt: None,
            focus_handle,
            accessible_name,
            metrics: Metrics::measure(window, DEFAULT_FONT_SIZE),
            resend_size: false,
            model: None,
            layouts: LayoutCache::default(),
            title: None,
            working_directory: None,
            exit_status: None,
            palette: None,
            requested_grid: None,
            requested_history: None,
            scroll_remainder: 0.,
            marked_text: None,
            paint_info: PaintInfo::default(),
            _output_task: output_task,
            _subscriptions: subscriptions,
        };
        view.sync_palette(cx);

        view
    }

    /// The grid size that fits `bounds` with the font at `font_size` points. Use it to
    /// spawn a terminal at the right size.
    pub fn size_for(
        bounds: Size<Pixels>,
        font_size: f32,
        window: &mut Window,
        _cx: &App,
    ) -> swarm_term::Size {
        Metrics::measure(window, font_size).grid_for(bounds)
    }

    /// The grid that fits `bounds` for cells of the size `cell`, worked out as the view does
    /// when it paints: padding included. `cell` comes from
    /// [`cell_size`](Self::cell_size).
    pub fn grid_in(bounds: Size<Pixels>, cell: Size<Pixels>) -> swarm_term::Size {
        element::grid_in(bounds, cell.width, cell.height)
    }

    /// The exact size of one cell with the font at `font_size` points, as the view paints
    /// it. The cell sizes in [`size_for`](Self::size_for) are whole pixels, which is too
    /// narrow for a tile that must hold a number of columns.
    pub fn cell_size(font_size: f32, window: &Window, _cx: &App) -> Size<Pixels> {
        let metrics = Metrics::measure(window, font_size);

        Size {
            width: metrics.cell_width,
            height: metrics.line_height,
        }
    }

    /// The font size in points.
    pub fn font_size(&self) -> f32 {
        f32::from(self.metrics.font_size)
    }

    /// Measures the cell again at `points` and draws the view again. The grid then gets
    /// more or fewer columns and rows in the same space, and the emulator is told at the
    /// next paint.
    pub fn set_font_size(&mut self, points: f32, window: &Window, cx: &mut Context<Self>) {
        if (self.font_size() - points).abs() < f32::EPSILON {
            return;
        }

        self.metrics = Metrics::measure(window, points);
        self.resend_size = true;
        self.layouts.invalidate(true, &[]);
        cx.notify();
    }

    /// Lets `allowed` decide, for every key, paste, text input and composing text, whether it
    /// reaches the program. Without it all typing is allowed. Scrolling and focus work either way, and
    /// a key that is not allowed bubbles up like a key the view does not handle.
    pub fn set_typing_gate(&mut self, allowed: impl Fn(&App) -> bool + 'static) {
        self.typing_gate = Some(Rc::new(allowed));
    }

    fn typing_allowed(&self, cx: &App) -> bool {
        self.exit_status.is_none() && self.typing_gate.as_ref().is_none_or(|allowed| allowed(cx))
    }

    /// Makes this the terminal of an agent. The first ⌃C reaches the program, a second
    /// one within [`DOUBLE_INTERRUPT`] of it does not, and ⌃D never does, so the agent
    /// cannot quit from the keyboard. Esc and all other keys pass.
    pub fn hold_back_quit_keys(&mut self) {
        self.guards_quit = true;
    }

    /// Whether a quit key of an agent terminal stays in the view. A ⌃C that passes starts
    /// the window of the next one.
    fn holds_back(&mut self, keystroke: &Keystroke, cx: &App) -> bool {
        let modifiers = keystroke.modifiers;
        let plain_control = modifiers.control
            && !modifiers.shift
            && !modifiers.alt
            && !modifiers.platform
            && !modifiers.function;

        if !self.guards_quit || !plain_control {
            return false;
        }

        match keystroke.key.as_str() {
            "d" => true,
            "c" => {
                let now = cx.background_executor().now();
                let repeated = self
                    .last_interrupt
                    .is_some_and(|last| now.saturating_duration_since(last) < DOUBLE_INTERRUPT);

                if !repeated {
                    self.last_interrupt = Some(now);
                }

                repeated
            }
            _ => false,
        }
    }

    /// How many times the view was painted. Tests use it to see that a view is drawn once
    /// per frame, and only where it is shown.
    pub fn paint_count(&self) -> u64 {
        self.paint_info.paints
    }

    /// How many lines the view is scrolled back from the bottom.
    pub fn scrolled_back(&self) -> u64 {
        self.model.as_ref().map_or(0, ScreenModel::scroll_offset)
    }

    /// The last title the program set.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The last working directory the program reported.
    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// How the program ended, or `None` while it runs.
    pub fn exit_status(&self) -> Option<&ExitStatus> {
        self.exit_status.as_ref()
    }

    /// Columns and rows of the screen, once the first snapshot arrived.
    pub fn grid_size(&self) -> Option<(u16, u16)> {
        let model = self.model.as_ref()?;

        Some((model.screen().cols, model.screen().rows))
    }

    /// The text on screen, one line per row, for assistive technology and tests.
    pub fn visible_text(&self) -> String {
        let Some(model) = &self.model else {
            return String::new();
        };

        let mut text = String::new();

        for row in model.visible_rows() {
            let mut column = 0;

            for span in row.iter().flat_map(|row| &row.texts) {
                for _ in column..span.col {
                    text.push(' ');
                }

                text.push_str(&span.text);
                column = span.col + span.cells;
            }

            text.push('\n');
        }

        text
    }

    fn send(&self, input: TerminalInput) {
        if let Err(error) = self.input.send(input) {
            log::debug!("terminal input dropped, the emulator is gone: {error}");
        }
    }

    /// Applies one output. Returns whether the program exited.
    fn apply(&mut self, output: TerminalOutput, cx: &mut Context<Self>) -> bool {
        match output {
            TerminalOutput::Title(title) => {
                self.title = Some(title.clone());
                cx.emit(TerminalEvent::TitleChanged(title));
            }
            TerminalOutput::WorkingDirectory(directory) => {
                self.working_directory = Some(directory.clone());
                cx.emit(TerminalEvent::WorkingDirectoryChanged(directory));
            }
            TerminalOutput::Bell => cx.emit(TerminalEvent::Bell),
            TerminalOutput::ProgramStatus(_) => {}
            TerminalOutput::Exited(status) => {
                log::info!("terminal exited: {status:?}");

                self.exit_status = Some(status.clone());
                cx.emit(TerminalEvent::Exited(status));

                return true;
            }
            output @ (TerminalOutput::Snapshot(_)
            | TerminalOutput::Update(_)
            | TerminalOutput::History { .. }) => {
                if let TerminalOutput::Snapshot(snapshot) = &output {
                    self.title.clone_from(&snapshot.title);
                    self.working_directory
                        .clone_from(&snapshot.working_directory);
                }

                self.apply_screen(output);
            }
        }

        false
    }

    fn apply_screen(&mut self, output: TerminalOutput) {
        let (all, rows) = if let Some(model) = self.model.as_mut() {
            let changed = model.apply(&output);

            (changed.is_all(), changed.rows().to_vec())
        } else if let TerminalOutput::Snapshot(snapshot) = output {
            self.model = Some(ScreenModel::new(snapshot));

            (true, Vec::new())
        } else {
            log::debug!("screen output before the first snapshot ignored");

            return;
        };

        if all {
            self.requested_history = None;
        }

        self.layouts.invalidate(all, &rows);
    }

    /// Asks the emulator for the scrollback lines the view shows but does not have.
    fn request_history(&mut self) {
        let Some((from, count)) = self.model.as_ref().and_then(ScreenModel::missing_history) else {
            return;
        };

        if self.requested_history == Some((from, count)) {
            return;
        }

        self.requested_history = Some((from, count));
        self.send(TerminalInput::FetchHistory { from, count });
    }

    fn sync_palette(&mut self, cx: &App) {
        let palette = palette_for(cx.theme());

        if self.palette != Some(palette) {
            self.palette = Some(palette);
            self.send(TerminalInput::Palette(palette));
        }
    }

    fn resize_to(&mut self, size: swarm_term::Size) {
        let current = self.requested_grid.or_else(|| {
            self.model
                .as_ref()
                .map(|model| (model.screen().cols, model.screen().rows))
        });

        if current == Some((size.cols, size.rows)) && !self.resend_size {
            return;
        }

        self.resend_size = false;
        self.requested_grid = Some((size.cols, size.rows));
        self.send(TerminalInput::Resize(size));
    }

    fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.model.as_mut() else {
            return;
        };

        if model.scroll_offset() != 0 {
            model.scroll_to_bottom();
            cx.notify();
        }
    }

    /// What to paint, or `None` before the first snapshot.
    fn frame(&mut self, focused: bool, window: &Window) -> Option<Frame> {
        let model = self.model.as_ref()?;
        let screen = model.screen();
        let history_end = screen.history.end();

        let rows = model
            .visible_lines()
            .map(|line| {
                if line >= history_end {
                    let index = u16::try_from(line - history_end).unwrap_or(u16::MAX);

                    self.layouts
                        .live(index, model.row(line), &self.metrics, window)
                } else {
                    self.layouts
                        .history(line, model.row(line), &self.metrics, window)
                }
            })
            .collect();

        let background = hsla(screen.colors.bg);
        let cursor_cell = {
            let cursor = screen.cursor;
            let row = u64::from(cursor.row) + model.scroll_offset();

            (row < u64::from(screen.rows)).then(|| (cursor, u16::try_from(row).unwrap_or(u16::MAX)))
        };

        let cursor = cursor_cell
            .filter(|(cursor, _)| cursor.visible)
            .map(|(cursor, row)| {
                let color = hsla(screen.colors.cursor.unwrap_or(screen.colors.fg));
                let style = if focused {
                    cursor.style
                } else {
                    CursorStyle::BlockHollow
                };

                let mut layout = CursorLayout {
                    col: cursor.col,
                    row,
                    wide: false,
                    style,
                    color,
                    glyph_color: background,
                    glyph: None,
                };

                if let Some(under) = self.layouts.live_layout(cursor.row) {
                    let batch = under.batches.iter().find(|batch| {
                        batch.col <= cursor.col && cursor.col < batch.col + batch.cells
                    });

                    if let Some(batch) = batch {
                        let narrow = batch.text.graphemes(true).count() == usize::from(batch.cells);
                        let glyph: String = if narrow {
                            batch
                                .text
                                .graphemes(true)
                                .nth(usize::from(cursor.col - batch.col))
                                .unwrap_or_default()
                                .to_owned()
                        } else {
                            batch.text.to_string()
                        };

                        let mut run = batch.run.clone();
                        run.len = glyph.len();
                        run.color = background;

                        layout.wide = !narrow && batch.cells == 2;
                        layout.glyph = Some(CursorGlyph::Text(glyph.into(), run));
                    } else if let Some(cell) =
                        under.drawn.iter().find(|cell| cell.col == cursor.col)
                    {
                        layout.glyph = Some(CursorGlyph::Drawn(cell.character));
                    }
                }

                layout
            });

        let marked = self
            .marked_text
            .as_ref()
            .zip(cursor_cell)
            .map(|(text, (cursor, row))| {
                let run = TextRun {
                    len: text.len(),
                    font: self.metrics.font.clone(),
                    color: hsla(screen.colors.fg),
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        thickness: px(1.),
                        color: None,
                        wavy: false,
                    }),
                    strikethrough: None,
                };

                MarkedLayout {
                    col: cursor.col,
                    row,
                    shaped: window.text_system().shape_line(
                        text.clone().into(),
                        self.metrics.font_size,
                        &[run],
                        None,
                    ),
                }
            });

        Some(Frame {
            rows,
            cursor,
            ime_cell: cursor_cell.map(|(cursor, row)| (cursor.col, row)),
            marked,
            background,
            marked_background: background,
        })
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if !self.typing_allowed(cx) {
            return;
        }

        if keystroke.modifiers.platform {
            let plain_paste = keystroke.key == "v"
                && !keystroke.modifiers.shift
                && !keystroke.modifiers.alt
                && !keystroke.modifiers.control;

            if plain_paste {
                self.paste(window, cx);
                cx.stop_propagation();
            } else if let Some(text) = line_edit_text(keystroke) {
                self.send(TerminalInput::Text(text.to_owned()));
                self.scroll_to_bottom(cx);
                cx.stop_propagation();
            }

            return;
        }

        if self.holds_back(keystroke, cx) {
            cx.stop_propagation();
            return;
        }

        if let Some(text) = line_edit_text(keystroke) {
            self.send(TerminalInput::Text(text.to_owned()));
        } else {
            self.send(TerminalInput::Key(key_from(keystroke, event.is_held)));
        }
        self.scroll_to_bottom(cx);
        cx.stop_propagation();
    }

    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.typing_allowed(cx) {
            return;
        }

        let Some(item) = cx.read_from_clipboard() else {
            return;
        };

        match item.entries().first() {
            // The program reads the clipboard image itself when it gets Ctrl-V.
            Some(ClipboardEntry::Image(image)) if !image.bytes.is_empty() => {
                self.send(TerminalInput::Key(Key {
                    name: "v".to_owned(),
                    modifiers: Modifiers {
                        control: true,
                        ..Modifiers::default()
                    },
                    text: None,
                    repeat: false,
                }));
                self.scroll_to_bottom(cx);
            }
            Some(ClipboardEntry::ExternalPaths(paths)) => {
                self.paste_paths(paths.paths(), window, cx);
            }
            _ => {
                if let Some(text) = item.text().filter(|text| !text.is_empty()) {
                    self.send(TerminalInput::Paste(text));
                    self.scroll_to_bottom(cx);
                }
            }
        }
    }

    /// Types the quoted `paths` into the program, as a paste. A program like Claude Code
    /// attaches an image path it sees in its input.
    fn paste_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        if !self.typing_allowed(cx) {
            return;
        }

        let text = paths_text(paths);
        if text.is_empty() {
            return;
        }

        window.focus(&self.focus_handle, cx);
        self.send(TerminalInput::Paste(text));
        self.scroll_to_bottom(cx);
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(alternate) = self.model.as_ref().map(|model| model.screen().alternate) else {
            return;
        };

        let line_height = self.metrics.line_height;
        let delta = event.delta.pixel_delta(line_height).y;

        self.scroll_remainder += f32::from(delta) / f32::from(line_height);
        let lines = self.scroll_remainder.trunc();

        if lines == 0. {
            return;
        }

        self.scroll_remainder -= lines;
        // A positive wheel delta moves the content down, back into history.
        let lines = whole_lines(lines);

        if alternate {
            // Mouse wheel events are not typing, so the gate does not apply to them. The
            // arrow-key fallback is typing, so it needs the gate open.
            let (column, row) = self
                .metrics
                .cell_at(event.position - self.paint_info.grid_origin);
            let keys = self.typing_allowed(cx);

            self.send(TerminalInput::Scroll {
                lines,
                column,
                row,
                keys,
            });
            return;
        }

        if let Some(model) = self.model.as_mut() {
            model.scroll_by(i64::from(lines));
        }

        self.requested_history = None;
        self.request_history();
        cx.notify();
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a wheel event is a few lines at most"
)]
fn whole_lines(lines: f32) -> i32 {
    lines as i32
}

/// The key press as the emulator wants it. GPUI reports Enter and Tab as `\n` and `\t`,
/// so control characters are dropped from the text; the emulator derives them from the
/// key name.
fn key_from(keystroke: &Keystroke, repeat: bool) -> Key {
    let modifiers = keystroke.modifiers;

    Key {
        name: keystroke.key.clone(),
        modifiers: Modifiers {
            shift: modifiers.shift,
            alt: modifiers.alt,
            control: modifiers.control,
            command: modifiers.platform,
        },
        text: keystroke
            .key_char
            .clone()
            .filter(|text| !text.chars().any(char::is_control)),
        repeat,
    }
}

/// What macOS terminals send for the line-editing keys: ⌘← and ⌘→ jump to the start and
/// the end of the line (⌃A, ⌃E), ⌘⌫ deletes to the start (⌃U), ⌥← and ⌥→ jump a word
/// (Esc b, Esc f). Ghostty sends these by default; the key encoder would send CSI codes.
fn line_edit_text(keystroke: &Keystroke) -> Option<&'static str> {
    let modifiers = keystroke.modifiers;
    if modifiers.shift || modifiers.function {
        return None;
    }

    let only_command = modifiers.platform && !modifiers.alt && !modifiers.control;
    let only_option = modifiers.alt && !modifiers.platform && !modifiers.control;

    match keystroke.key.as_str() {
        "left" if only_command => Some("\x01"),
        "right" if only_command => Some("\x05"),
        "backspace" if only_command => Some("\x15"),
        "left" if only_option => Some("\x1bb"),
        "right" if only_option => Some("\x1bf"),
        _ => None,
    }
}

/// `path` as one POSIX shell word. A path of safe characters stays as it is.
fn quote_path(path: &str) -> String {
    let safe =
        |character: char| character.is_ascii_alphanumeric() || "/._-+,:@%".contains(character);

    if !path.is_empty() && path.chars().all(safe) {
        return path.to_owned();
    }

    format!("'{}'", path.replace('\'', r"'\''"))
}

/// The text to type for dropped `paths`: a space before each quoted path and one at the end.
/// A path that is not valid UTF-8 is skipped. It is empty when no path is left.
fn paths_text(paths: &[PathBuf]) -> String {
    let mut text = String::new();

    for path in paths.iter().filter_map(|path| path.to_str()) {
        text.push(' ');
        text.push_str(&quote_path(path));
    }

    if !text.is_empty() {
        text.push(' ');
    }

    text
}

/// The terminal colors for `theme`: `term` background, `fg` text and cursor, and the
/// theme's 16 ANSI colors.
pub fn palette_for(theme: &Theme) -> Palette {
    let colors = theme.colors();

    Palette {
        fg: rgb(colors.fg),
        bg: rgb(colors.term),
        cursor: rgb(colors.fg),
        ansi: colors.ansi.map(rgb),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 first"
)]
fn rgb(color: Hsla) -> Rgb {
    let color = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;

    Rgb(channel(color.r), channel(color.g), channel(color.b))
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_palette(cx);

        let background = self.model.as_ref().map_or_else(
            || cx.theme().colors().term,
            |model| hsla(model.screen().colors.bg),
        );
        let drop_ring = cx.theme().colors().accent;
        // Building the text costs a pass over the rows, so only do it for a screen reader.
        let value = window.is_a11y_active().then(|| self.visible_text());

        div()
            .id("terminal")
            .key_context(TERMINAL_CONTEXT)
            .role(Role::Terminal)
            .aria_label(self.accessible_name.clone())
            .when_some(value, StatefulInteractiveElement::aria_value)
            .size_full()
            .bg(background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|view, _: &edit::Paste, window, cx| view.paste(window, cx)))
            .on_drop(cx.listener(|view, dropped: &ExternalPaths, window, cx| {
                view.paste_paths(dropped.paths(), window, cx);
            }))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(TerminalElement::new(cx.entity()))
            .child(div().absolute().size_full().drag_over::<ExternalPaths>(
                move |style, _, _, _| style.border_2().border_color(drop_ring),
            ))
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self.marked_text.as_ref()?.encode_utf16().collect();
        let start = range.start.min(units.len());
        let end = range.end.clamp(start, units.len());

        *adjusted_range = Some(start..end);

        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked_text
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());

        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_text.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;

        if !text.is_empty() && self.typing_allowed(cx) {
            self.send(TerminalInput::Text(text.to_owned()));
            self.scroll_to_bottom(cx);
        }

        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown = !new_text.is_empty() && self.typing_allowed(cx);
        self.marked_text = shown.then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.paint_info.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ClipboardItem, Modifiers as GpuiModifiers};

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: GpuiModifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    #[test]
    fn key_keeps_the_gpui_name_and_text() {
        let key = key_from(&keystroke("a", Some("a"), GpuiModifiers::default()), false);

        assert_eq!(key.name, "a");
        assert_eq!(key.text.as_deref(), Some("a"));
    }

    #[test]
    fn key_drops_control_characters_from_the_text() {
        let key = key_from(
            &keystroke("enter", Some("\n"), GpuiModifiers::default()),
            false,
        );

        assert_eq!(key.text, None);
    }

    #[test]
    fn key_maps_modifiers_and_repeat() {
        let key = key_from(
            &keystroke(
                "up",
                None,
                GpuiModifiers {
                    shift: true,
                    platform: true,
                    ..GpuiModifiers::default()
                },
            ),
            true,
        );

        assert_eq!(
            key.modifiers,
            Modifiers {
                shift: true,
                alt: false,
                control: false,
                command: true,
            }
        );
        assert!(key.repeat);
    }

    #[test]
    fn palette_follows_the_theme() {
        let palette = palette_for(&Theme::dark());

        assert_eq!(palette.bg, Rgb(0x0d, 0x0d, 0x0f));
        assert_eq!(palette.fg, Rgb(0xed, 0xed, 0xef));
        assert_eq!(palette.cursor, palette.fg);
        assert_eq!(palette.ansi[1], Rgb(0xff, 0x63, 0x69));
        assert_eq!(palette.ansi[15], Rgb(0xf4, 0xf4, 0xf6));
    }

    #[test]
    fn light_and_dark_palettes_differ() {
        assert_ne!(palette_for(&Theme::light()), palette_for(&Theme::dark()));
    }

    #[gpui::test]
    fn paste_from_the_edit_menu_reaches_the_program(cx: &mut gpui::TestAppContext) {
        cx.update(swarm_ui::theme::init);
        let (channels, emulator) = swarm_term::channels();
        let (view, cx) = cx.add_window_view(|window, cx| {
            TerminalView::new(channels, "terminal".into(), window, cx)
        });
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.write_to_clipboard(ClipboardItem::new_string("ls -la".to_owned()));

        cx.dispatch_action(edit::Paste);

        let sent: Vec<TerminalInput> = emulator.input.try_iter().collect();
        assert!(sent.contains(&TerminalInput::Paste("ls -la".to_owned())));
    }

    #[gpui::test]
    fn paste_from_the_edit_menu_waits_for_the_typing_gate(cx: &mut gpui::TestAppContext) {
        cx.update(swarm_ui::theme::init);
        let (channels, emulator) = swarm_term::channels();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = TerminalView::new(channels, "terminal".into(), window, cx);
            view.set_typing_gate(|_| false);

            view
        });
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.write_to_clipboard(ClipboardItem::new_string("rm -rf".to_owned()));

        cx.dispatch_action(edit::Paste);

        let sent: Vec<TerminalInput> = emulator.input.try_iter().collect();
        assert!(
            !sent
                .iter()
                .any(|input| matches!(input, TerminalInput::Paste(_)))
        );
    }

    fn pasted_text(emulator: &swarm_term::EmulatorChannels) -> Vec<String> {
        emulator
            .input
            .try_iter()
            .filter_map(|input| match input {
                TerminalInput::Paste(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    fn is_control_v(input: &TerminalInput) -> bool {
        matches!(
            input,
            TerminalInput::Key(key)
                if key.name == "v" && key.modifiers.control && !key.modifiers.command
        )
    }

    fn paths(names: &[&str]) -> ExternalPaths {
        ExternalPaths(names.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn a_plain_path_is_not_quoted() {
        assert_eq!(
            quote_path("/tmp/shot-1_a.b+c,d:e@f%g.png"),
            "/tmp/shot-1_a.b+c,d:e@f%g.png"
        );
    }

    #[test]
    fn a_path_with_spaces_is_single_quoted() {
        assert_eq!(quote_path("/tmp/my shot.png"), "'/tmp/my shot.png'");
    }

    #[test]
    fn a_single_quote_in_a_path_is_escaped() {
        assert_eq!(quote_path("/tmp/it's.png"), r"'/tmp/it'\''s.png'");
    }

    #[test]
    fn dollar_and_backtick_in_a_path_stay_literal() {
        assert_eq!(quote_path("/tmp/$HOME`id`.png"), "'/tmp/$HOME`id`.png'");
    }

    #[test]
    fn an_empty_path_is_quoted() {
        assert_eq!(quote_path(""), "''");
    }

    #[test]
    fn dropped_paths_get_a_space_before_each_and_one_at_the_end() {
        let text = paths_text(&[PathBuf::from("/a/b.png"), PathBuf::from("/c d.png")]);

        assert_eq!(text, " /a/b.png '/c d.png' ");
    }

    #[test]
    fn no_paths_make_no_text() {
        assert_eq!(paths_text(&[]), "");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_skipped() {
        use std::os::unix::ffi::OsStringExt as _;

        let bad = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));

        assert_eq!(paths_text(&[bad, PathBuf::from("/ok")]), " /ok ");
    }

    fn drop_on_view(cx: &mut gpui::VisualTestContext, names: &[&str]) {
        let position = gpui::point(px(20.), px(20.));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.simulate_event(gpui::FileDropEvent::Entered {
            position,
            paths: paths(names),
        });
        cx.simulate_event(gpui::FileDropEvent::Submit { position });
    }

    #[gpui::test]
    fn dropping_files_pastes_the_quoted_paths_and_focuses_the_terminal(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(swarm_ui::theme::init);
        let (channels, emulator) = swarm_term::channels();
        let (view, cx) = cx.add_window_view(|window, cx| {
            TerminalView::new(channels, "terminal".into(), window, cx)
        });

        drop_on_view(cx, &["/tmp/a.png", "/tmp/my shot.png"]);

        assert_eq!(
            pasted_text(&emulator),
            vec![" /tmp/a.png '/tmp/my shot.png' ".to_owned()]
        );
        assert!(cx.update(|window, cx| view.focus_handle(cx).is_focused(window)));
    }

    #[gpui::test]
    fn dropping_files_waits_for_the_typing_gate(cx: &mut gpui::TestAppContext) {
        cx.update(swarm_ui::theme::init);
        let (channels, emulator) = swarm_term::channels();
        let (_view, cx) = cx.add_window_view(|window, cx| {
            let mut view = TerminalView::new(channels, "terminal".into(), window, cx);
            view.set_typing_gate(|_| false);

            view
        });

        drop_on_view(cx, &["/tmp/a.png"]);

        assert!(pasted_text(&emulator).is_empty());
    }

    fn view_with_clipboard(
        cx: &mut gpui::TestAppContext,
        item: ClipboardItem,
        gate_open: bool,
    ) -> Vec<TerminalInput> {
        cx.update(swarm_ui::theme::init);
        let (channels, emulator) = swarm_term::channels();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = TerminalView::new(channels, "terminal".into(), window, cx);
            view.set_typing_gate(move |_| gate_open);

            view
        });
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.write_to_clipboard(item);

        cx.dispatch_action(edit::Paste);

        emulator.input.try_iter().collect()
    }

    fn png(bytes: Vec<u8>) -> ClipboardItem {
        ClipboardItem::new_image(&gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))
    }

    #[gpui::test]
    fn pasting_an_image_sends_control_v_and_no_paste(cx: &mut gpui::TestAppContext) {
        let sent = view_with_clipboard(cx, png(vec![1, 2, 3]), true);

        assert!(sent.iter().any(is_control_v));
        assert!(
            !sent
                .iter()
                .any(|input| matches!(input, TerminalInput::Paste(_)))
        );
    }

    #[gpui::test]
    fn pasting_an_empty_image_sends_nothing(cx: &mut gpui::TestAppContext) {
        let sent = view_with_clipboard(cx, png(Vec::new()), true);

        assert!(!sent.iter().any(is_control_v));
    }

    #[gpui::test]
    fn pasting_an_image_waits_for_the_typing_gate(cx: &mut gpui::TestAppContext) {
        let sent = view_with_clipboard(cx, png(vec![1, 2, 3]), false);

        assert!(!sent.iter().any(is_control_v));
    }

    #[gpui::test]
    fn pasting_files_sends_the_quoted_paths(cx: &mut gpui::TestAppContext) {
        let item = ClipboardItem::from(ClipboardEntry::ExternalPaths(paths(&[
            "/tmp/a.png",
            "/tmp/b c.png",
        ])));

        let sent = view_with_clipboard(cx, item, true);

        assert!(sent.contains(&TerminalInput::Paste(
            " /tmp/a.png '/tmp/b c.png' ".to_owned()
        )));
    }

    #[gpui::test]
    fn pasting_files_waits_for_the_typing_gate(cx: &mut gpui::TestAppContext) {
        let item = ClipboardItem::from(ClipboardEntry::ExternalPaths(paths(&["/tmp/a.png"])));

        let sent = view_with_clipboard(cx, item, false);

        assert!(
            !sent
                .iter()
                .any(|input| matches!(input, TerminalInput::Paste(_)))
        );
    }

    /// A root with a stop before and a stop after the terminal. Tab moves focus through the
    /// root context, as in the app.
    struct TabWalk {
        before: FocusHandle,
        terminal: gpui::Entity<TerminalView>,
        after: FocusHandle,
    }

    impl Render for TabWalk {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            swarm_ui::tab_navigation(
                div()
                    .key_context(swarm_ui::ROOT_CONTEXT)
                    .child(div().id("before").track_focus(&self.before))
                    .child(self.terminal.clone())
                    .child(div().id("after").track_focus(&self.after)),
            )
        }
    }

    struct Walk<'a> {
        view: gpui::Entity<TabWalk>,
        emulator: swarm_term::EmulatorChannels,
        cx: &'a mut gpui::VisualTestContext,
    }

    impl Walk<'_> {
        fn handles(&mut self) -> (FocusHandle, FocusHandle, FocusHandle) {
            self.cx.update(|_, cx| {
                let walk = self.view.read(cx);

                (
                    walk.before.clone(),
                    walk.terminal.read(cx).focus_handle.clone(),
                    walk.after.clone(),
                )
            })
        }

        fn focus_terminal(&mut self) {
            let (_, terminal, _) = self.handles();
            self.cx.update(|window, cx| window.focus(&terminal, cx));
        }

        fn sent_keys(&self) -> Vec<Key> {
            self.emulator
                .input
                .try_iter()
                .filter_map(|input| match input {
                    TerminalInput::Key(key) => Some(key),
                    _ => None,
                })
                .collect()
        }
    }

    fn walk(cx: &mut gpui::TestAppContext) -> Walk<'_> {
        cx.update(swarm_ui::theme::init);
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("tab", FocusNext, Some(swarm_ui::ROOT_CONTEXT)),
                KeyBinding::new("shift-tab", FocusPrevious, Some(swarm_ui::ROOT_CONTEXT)),
            ]);
            cx.bind_keys(shell_key_bindings());
            cx.bind_keys(focus_key_bindings());
        });
        let (channels, emulator) = swarm_term::channels();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let terminal = cx.new(|cx| TerminalView::new(channels, "terminal".into(), window, cx));

            TabWalk {
                before: cx.focus_handle().tab_stop(true),
                terminal,
                after: cx.focus_handle().tab_stop(true),
            }
        });
        cx.run_until_parked();

        let mut walk = Walk { view, emulator, cx };
        walk.focus_terminal();

        walk
    }

    #[gpui::test]
    fn tab_in_a_terminal_reaches_the_shell(cx: &mut gpui::TestAppContext) {
        let walk = walk(cx);

        walk.cx.simulate_keystrokes("tab");

        let keys = walk.sent_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "tab");
        assert!(!keys[0].modifiers.shift);
    }

    #[gpui::test]
    fn shift_tab_in_a_terminal_reaches_the_shell(cx: &mut gpui::TestAppContext) {
        let walk = walk(cx);

        walk.cx.simulate_keystrokes("shift-tab");

        let keys = walk.sent_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "tab");
        assert!(keys[0].modifiers.shift);
    }

    #[gpui::test]
    fn tab_in_a_terminal_keeps_focus_there(cx: &mut gpui::TestAppContext) {
        let mut walk = walk(cx);
        let (_, terminal, _) = walk.handles();

        walk.cx.simulate_keystrokes("tab");

        assert!(walk.cx.update(|window, _| terminal.is_focused(window)));
    }

    #[gpui::test]
    fn control_tab_moves_focus_to_the_next_stop(cx: &mut gpui::TestAppContext) {
        let mut walk = walk(cx);
        let (_, _, after) = walk.handles();

        walk.cx.simulate_keystrokes("ctrl-tab");

        assert!(walk.cx.update(|window, _| after.is_focused(window)));
        assert!(walk.sent_keys().is_empty());
    }

    #[gpui::test]
    fn control_shift_tab_moves_focus_to_the_previous_stop(cx: &mut gpui::TestAppContext) {
        let mut walk = walk(cx);
        let (before, _, _) = walk.handles();

        walk.cx.simulate_keystrokes("ctrl-shift-tab");

        assert!(walk.cx.update(|window, _| before.is_focused(window)));
        assert!(walk.sent_keys().is_empty());
    }
}
