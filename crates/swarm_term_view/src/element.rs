//! Row layout cache and the element that paints the grid: background spans, then text
//! batches, then box and block characters, then the cursor and marked text.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, Font,
    FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    Pixels, Rgba, ShapedLine, SharedString, Size, StrikethroughStyle, Style, TextAlign, TextRun,
    UnderlineStyle, Window, fill, font, outline, point, px, relative, size,
};
use swarm_term::{CursorStyle, Rgb, Row, TextFlags, TextSpan};
use swarm_ui::fonts::MONO_FONT;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::TerminalView;
use crate::box_drawing::{self, CellRect};

const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT_RATIO: f32 = 1.3;
const CURSOR_THICKNESS: Pixels = px(2.);

/// Space between the view's edge and the grid.
pub(crate) const PADDING: Pixels = px(8.);

/// Rows kept in the history layout cache before it is emptied.
const MAX_HISTORY_LAYOUTS: usize = 2_000;

pub(crate) fn hsla(color: Rgb) -> Hsla {
    Rgba {
        r: f32::from(color.0) / 255.,
        g: f32::from(color.1) / 255.,
        b: f32::from(color.2) / 255.,
        a: 1.,
    }
    .into()
}

#[derive(Clone)]
pub(crate) struct Metrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell_width: Pixels,
    pub line_height: Pixels,
}

impl Metrics {
    pub(crate) fn measure(window: &Window) -> Self {
        let font = font(MONO_FONT);
        let font_size = px(FONT_SIZE);

        let run = TextRun {
            len: 1,
            font: font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let measured = window
            .text_system()
            .shape_line(SharedString::new_static("M"), font_size, &[run], None)
            .width;
        let cell_width = if measured > px(0.) {
            measured
        } else {
            font_size * 0.6
        };

        Self {
            font,
            font_size,
            cell_width,
            line_height: (font_size * LINE_HEIGHT_RATIO).round(),
        }
    }

    /// The grid that fits `bounds` after padding.
    pub(crate) fn grid_for(&self, bounds: Size<Pixels>) -> swarm_term::Size {
        let width = bounds.width - PADDING * 2.;
        let height = bounds.height - PADDING * 2.;

        let cols = whole_cells(width / self.cell_width).max(2.);
        let rows = whole_cells(height / self.line_height).max(1.);

        swarm_term::Size {
            cols: to_u16(cols),
            rows: to_u16(rows),
            cell_width: to_u16(f32::from(self.cell_width)),
            cell_height: to_u16(f32::from(self.line_height)),
        }
    }

    /// The column and row under `offset`, counted from the grid's top left corner.
    pub(crate) fn cell_at(&self, offset: gpui::Point<Pixels>) -> (u16, u16) {
        (
            to_u16((offset.x / self.cell_width).floor()),
            to_u16((offset.y / self.line_height).floor()),
        )
    }
}

/// Rounds down, but forgives the float error of a bounds that is exactly n cells wide.
fn whole_cells(cells: f32) -> f32 {
    (cells + 1e-3).floor()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the u16 range first"
)]
fn to_u16(value: f32) -> u16 {
    value.clamp(0., f32::from(u16::MAX)) as u16
}

pub(crate) struct BgRun {
    pub col: u16,
    pub len: u16,
    pub color: Hsla,
}

/// Adjacent cells with one style, shaped once and painted as one line.
pub(crate) struct TextBatch {
    pub col: u16,
    pub cells: u16,
    pub text: SharedString,
    pub run: TextRun,
    pub shaped: ShapedLine,
}

/// A box or block character painted from rectangles.
pub(crate) struct DrawnCell {
    pub col: u16,
    pub character: char,
    pub color: Hsla,
}

#[derive(Default)]
pub(crate) struct RowLayout {
    pub bgs: Vec<BgRun>,
    pub batches: Vec<TextBatch>,
    pub drawn: Vec<DrawnCell>,
}

impl RowLayout {
    pub(crate) fn build(row: &Row, metrics: &Metrics, window: &Window) -> Self {
        let mut layout = Self::default();

        for span in &row.bgs {
            let color = hsla(span.color);

            match layout.bgs.last_mut() {
                Some(last) if last.color == color && last.col + last.len == span.col => {
                    last.len += span.len;
                }
                _ => layout.bgs.push(BgRun {
                    col: span.col,
                    len: span.len,
                    color,
                }),
            }
        }

        for span in &row.texts {
            layout.push_span(span, metrics, window);
        }

        layout
    }

    fn push_span(&mut self, span: &TextSpan, metrics: &Metrics, window: &Window) {
        let narrow = span.text.graphemes(true).count() == usize::from(span.cells);

        if !narrow {
            self.push_batch(span, span.col, span.cells, &span.text, metrics, window);

            return;
        }

        // Box and block characters leave the text and are drawn on their own.
        let mut pending = String::new();
        let mut pending_col = span.col;

        for (col, grapheme) in (span.col..).zip(span.text.graphemes(true)) {
            let mut characters = grapheme.chars();
            let drawn = match (characters.next(), characters.next()) {
                (Some(character), None) if box_drawing::is_drawn(character) => Some(character),
                _ => None,
            };

            if let Some(character) = drawn {
                self.flush(span, pending_col, &mut pending, metrics, window);
                self.drawn.push(DrawnCell {
                    col,
                    character,
                    color: text_run(&metrics.font, span, 0).color,
                });
                pending_col = col + 1;
            } else {
                pending.push_str(grapheme);
            }
        }

        self.flush(span, pending_col, &mut pending, metrics, window);
    }

    fn flush(
        &mut self,
        span: &TextSpan,
        col: u16,
        pending: &mut String,
        metrics: &Metrics,
        window: &Window,
    ) {
        if pending.is_empty() {
            return;
        }

        let cells = u16::try_from(pending.graphemes(true).count()).unwrap_or(u16::MAX);
        self.push_batch(span, col, cells, pending, metrics, window);
        pending.clear();
    }

    fn push_batch(
        &mut self,
        span: &TextSpan,
        col: u16,
        cells: u16,
        text: &str,
        metrics: &Metrics,
        window: &Window,
    ) {
        let decorated = span.flags.contains(TextFlags::UNDERLINE)
            || span.flags.contains(TextFlags::STRIKE)
            || span.flags.contains(TextFlags::CURLY);

        if text.trim().is_empty() && !decorated {
            return;
        }

        let text: SharedString = text.to_owned().into();
        let run = text_run(&metrics.font, span, text.len());
        let shaped = window.text_system().shape_line(
            text.clone(),
            metrics.font_size,
            std::slice::from_ref(&run),
            Some(metrics.cell_width),
        );

        self.batches.push(TextBatch {
            col,
            cells,
            text,
            run,
            shaped,
        });
    }
}

fn text_run(base: &Font, span: &TextSpan, len: usize) -> TextRun {
    let flags = span.flags;
    let mut font = base.clone();

    if flags.contains(TextFlags::BOLD) {
        font.weight = FontWeight::BOLD;
    }

    if flags.contains(TextFlags::ITALIC) {
        font.style = FontStyle::Italic;
    }

    let mut color = hsla(span.fg);
    if flags.contains(TextFlags::FAINT) {
        color.a = 0.6;
    }

    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: flags
            .contains(TextFlags::UNDERLINE)
            .then_some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: flags.contains(TextFlags::CURLY),
            }),
        strikethrough: flags
            .contains(TextFlags::STRIKE)
            .then_some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
    }
}

/// Row layouts by source, so only rows an output changed are rebuilt.
#[derive(Default)]
pub(crate) struct LayoutCache {
    live: Vec<Option<Rc<RowLayout>>>,
    history: HashMap<u64, Rc<RowLayout>>,
}

impl LayoutCache {
    pub(crate) fn invalidate(&mut self, all: bool, rows: &[u16]) {
        if all {
            self.live.clear();
            self.history.clear();

            return;
        }

        for index in rows {
            if let Some(slot) = self.live.get_mut(usize::from(*index)) {
                *slot = None;
            }
        }
    }

    pub(crate) fn live(
        &mut self,
        index: u16,
        row: Option<&Row>,
        metrics: &Metrics,
        window: &Window,
    ) -> Option<Rc<RowLayout>> {
        let slot = usize::from(index);

        if self.live.len() <= slot {
            self.live.resize(slot + 1, None);
        }

        if self.live[slot].is_none() {
            self.live[slot] = row.map(|row| Rc::new(RowLayout::build(row, metrics, window)));
        }

        self.live[slot].clone()
    }

    pub(crate) fn history(
        &mut self,
        line: u64,
        row: Option<&Row>,
        metrics: &Metrics,
        window: &Window,
    ) -> Option<Rc<RowLayout>> {
        if let Some(layout) = self.history.get(&line) {
            return Some(Rc::clone(layout));
        }

        let layout = Rc::new(RowLayout::build(row?, metrics, window));

        if self.history.len() >= MAX_HISTORY_LAYOUTS {
            self.history.clear();
        }
        self.history.insert(line, Rc::clone(&layout));

        Some(layout)
    }

    pub(crate) fn live_layout(&self, index: u16) -> Option<&Rc<RowLayout>> {
        self.live.get(usize::from(index))?.as_ref()
    }
}

pub(crate) enum CursorGlyph {
    Text(SharedString, TextRun),
    Drawn(char),
}

pub(crate) struct CursorLayout {
    pub col: u16,
    pub row: u16,
    pub wide: bool,
    pub style: CursorStyle,
    pub color: Hsla,
    /// The screen background, for the character under a block cursor.
    pub glyph_color: Hsla,
    /// The character under a block cursor, redrawn in `glyph_color`.
    pub glyph: Option<CursorGlyph>,
}

/// Composition text drawn at the cursor.
pub(crate) struct MarkedLayout {
    pub col: u16,
    pub row: u16,
    pub shaped: ShapedLine,
}

pub(crate) struct Frame {
    pub rows: Vec<Option<Rc<RowLayout>>>,
    /// `None` when the cursor is hidden or scrolled out of view.
    pub cursor: Option<CursorLayout>,
    /// Where the cursor is even when hidden: composition text and the input method
    /// window go here.
    pub ime_cell: Option<(u16, u16)>,
    pub marked: Option<MarkedLayout>,
    pub background: Hsla,
    pub marked_background: Hsla,
}

/// Paints the terminal grid of one [`TerminalView`].
pub(crate) struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub(crate) fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

pub(crate) struct Prepaint {
    frame: Option<Frame>,
    metrics: Metrics,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let focused = self.view.read(cx).focus_handle.is_focused(window);

        let (frame, metrics) = self.view.update(cx, |view, _| {
            let metrics = view.metrics.clone();
            view.resize_to(metrics.grid_for(bounds.size));

            (view.frame(focused, window), metrics)
        });

        Prepaint { frame, metrics }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let metrics = &prepaint.metrics;
        let origin = bounds.origin + point(PADDING, PADDING);
        let cell_origin = |col: u16, row: usize| {
            point(
                origin.x + metrics.cell_width * f32::from(col),
                origin.y + metrics.line_height * f32::from(u16::try_from(row).unwrap_or(u16::MAX)),
            )
        };

        self.view.update(cx, |view, _| {
            view.paint_info.paints += 1;
            view.paint_info.grid_origin = origin;
        });

        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let Some(frame) = prepaint.frame.take() else {
            return;
        };

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, frame.background));

            for (row_index, row) in frame.rows.iter().enumerate() {
                let Some(row) = row else {
                    continue;
                };

                for bg in &row.bgs {
                    let rect = Bounds::new(
                        cell_origin(bg.col, row_index),
                        size(metrics.cell_width * f32::from(bg.len), metrics.line_height),
                    );
                    window.paint_quad(fill(rect, bg.color));
                }
            }

            for (row_index, row) in frame.rows.iter().enumerate() {
                let Some(row) = row else {
                    continue;
                };

                for batch in &row.batches {
                    let result = batch.shaped.paint(
                        cell_origin(batch.col, row_index),
                        metrics.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );

                    if let Err(error) = result {
                        log::debug!("could not paint a text batch: {error:#}");
                    }
                }

                for cell in &row.drawn {
                    paint_drawn(
                        window,
                        cell.character,
                        cell_origin(cell.col, row_index),
                        metrics,
                        cell.color,
                    );
                }
            }

            if let Some(cursor) = &frame.cursor {
                paint_cursor(
                    window,
                    cx,
                    cursor,
                    cell_origin(cursor.col, usize::from(cursor.row)),
                    metrics,
                );
            }

            if let Some(marked) = &frame.marked {
                let at = cell_origin(marked.col, usize::from(marked.row));
                let rect = Bounds::new(at, size(marked.shaped.width, metrics.line_height));
                window.paint_quad(fill(rect, frame.marked_background));

                if let Err(error) =
                    marked
                        .shaped
                        .paint(at, metrics.line_height, TextAlign::Left, None, window, cx)
                {
                    log::debug!("could not paint composition text: {error:#}");
                }
            }
        });

        self.view.update(cx, |view, _| {
            view.paint_info.cursor_bounds = frame.ime_cell.map(|(col, row)| {
                Bounds::new(
                    cell_origin(col, usize::from(row)),
                    size(metrics.cell_width, metrics.line_height),
                )
            });
        });
    }
}

fn paint_drawn(
    window: &mut Window,
    character: char,
    origin: gpui::Point<Pixels>,
    metrics: &Metrics,
    color: Hsla,
) {
    let rects = box_drawing::cell_rects(
        character,
        f32::from(metrics.cell_width),
        f32::from(metrics.line_height),
    );

    for CellRect {
        x,
        y,
        width,
        height,
        alpha,
    } in rects.unwrap_or_default()
    {
        let rect = Bounds::new(origin + point(px(x), px(y)), size(px(width), px(height)));
        let mut shade = color;
        shade.a *= alpha;

        window.paint_quad(fill(rect, shade));
    }
}

fn paint_cursor(
    window: &mut Window,
    cx: &mut App,
    cursor: &CursorLayout,
    at: gpui::Point<Pixels>,
    metrics: &Metrics,
) {
    let width = metrics.cell_width * if cursor.wide { 2. } else { 1. };
    let full = Bounds::new(at, size(width, metrics.line_height));

    match cursor.style {
        CursorStyle::Bar => window.paint_quad(fill(
            Bounds::new(at, size(CURSOR_THICKNESS, metrics.line_height)),
            cursor.color,
        )),
        CursorStyle::Underline => window.paint_quad(fill(
            Bounds::new(
                point(at.x, at.y + metrics.line_height - CURSOR_THICKNESS),
                size(width, CURSOR_THICKNESS),
            ),
            cursor.color,
        )),
        CursorStyle::BlockHollow => {
            window.paint_quad(outline(full, cursor.color, BorderStyle::Solid));
        }
        CursorStyle::Block => {
            window.paint_quad(fill(full, cursor.color));

            match &cursor.glyph {
                Some(CursorGlyph::Text(text, run)) => {
                    let line = window.text_system().shape_line(
                        text.clone(),
                        metrics.font_size,
                        std::slice::from_ref(run),
                        None,
                    );

                    if let Err(error) =
                        line.paint(at, metrics.line_height, TextAlign::Left, None, window, cx)
                    {
                        log::debug!("could not paint the cursor glyph: {error:#}");
                    }
                }
                Some(CursorGlyph::Drawn(character)) => {
                    paint_drawn(window, *character, at, metrics, cursor.glyph_color);
                }
                None => {}
            }
        }
    }
}
