//! The GPUI view of one terminal.
//!
//! [`TerminalView`] keeps a client-side copy of the screen ([`swarm_term::ScreenModel`]),
//! paints it, and talks to the emulator only through [`swarm_term::TerminalChannels`].
//! It does not link libghostty, so app builds need no Zig.

mod box_drawing;
mod element;

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    Hsla, KeyDownEvent, Keystroke, Pixels, Point, Render, Rgba, Role, ScrollEvent, SharedString,
    Size, Subscription, Task, TextRun, UTF16Selection, UnderlineStyle, Window, div, prelude::*, px,
};
use swarm_term::{
    CursorStyle, ExitStatus, Key, Modifiers, Palette, Rgb, ScreenModel, TerminalChannels,
    TerminalInput, TerminalOutput,
};
use swarm_ui::{ActiveTheme as _, Theme};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::element::{
    CursorGlyph, CursorLayout, Frame, LayoutCache, MarkedLayout, Metrics, TerminalElement, hsla,
};

/// What the app may want to react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    TitleChanged(String),
    WorkingDirectoryChanged(String),
    Bell,
    Exited(ExitStatus),
}

pub struct TerminalView {
    input: flume::Sender<TerminalInput>,
    focus_handle: FocusHandle,
    accessible_name: SharedString,
    metrics: Metrics,
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
        });

        let mut view = Self {
            input,
            focus_handle,
            accessible_name,
            metrics: Metrics::measure(window),
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

    /// The grid size that fits `bounds` with the current font. Use it to spawn a
    /// terminal at the right size.
    pub fn size_for(bounds: Size<Pixels>, window: &mut Window, _cx: &App) -> swarm_term::Size {
        Metrics::measure(window).grid_for(bounds)
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

        if current == Some((size.cols, size.rows)) {
            return;
        }

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

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;

        if keystroke.modifiers.platform {
            let plain_paste = keystroke.key == "v"
                && !keystroke.modifiers.shift
                && !keystroke.modifiers.alt
                && !keystroke.modifiers.control;

            if plain_paste {
                self.paste(cx);
                cx.stop_propagation();
            }

            return;
        }

        self.send(TerminalInput::Key(key_from(keystroke, event.is_held)));
        self.scroll_to_bottom(cx);
        cx.stop_propagation();
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item: ClipboardItem| item.text());

        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.send(TerminalInput::Paste(text));
            self.scroll_to_bottom(cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.model.as_mut() else {
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
        model.scroll_by(whole_lines(lines));

        self.requested_history = None;
        self.request_history();
        cx.notify();
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a wheel event is a few lines at most"
)]
fn whole_lines(lines: f32) -> i64 {
    lines as i64
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
        // Building the text costs a pass over the rows, so only do it for a screen reader.
        let value = window.is_a11y_active().then(|| self.visible_text());

        div()
            .id("terminal")
            .role(Role::Terminal)
            .aria_label(self.accessible_name.clone())
            .when_some(value, StatefulInteractiveElement::aria_value)
            .size_full()
            .bg(background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(TerminalElement::new(cx.entity()))
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

        if !text.is_empty() {
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
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_owned());
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
    use gpui::Modifiers as GpuiModifiers;

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
}
