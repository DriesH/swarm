use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ContentMask, ElementId, Entity, Focusable, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Style, TextAlign, Window, fill, outline, point, prelude::*, px,
    relative, size,
};
use libghostty_vt::render::CursorVisualStyle;

use crate::view::{Frame, Metrics, TerminalView};

const PADDING: Pixels = px(8.);

/// Paints the terminal grid: background spans, then text batches, then the cursor.
pub struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

pub struct Prepaint {
    frame: Option<Frame>,
    metrics: Metrics,
    layout_time: Duration,
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
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let start = Instant::now();
        let focused = self.view.read(cx).focus_handle(cx).is_focused(window);
        let (frame, metrics) = self.view.update(cx, |view, _| {
            let m = view.metrics().clone();
            let width = bounds.size.width - PADDING * 2.;
            let height = bounds.size.height - PADDING * 2.;
            let cols = (width / m.cell_width).floor().max(2.) as u16;
            let rows = (height / m.line_height).floor().max(1.) as u16;
            let frame = view
                .resize(cols, rows)
                .and_then(|()| view.frame(focused))
                .inspect_err(|e| eprintln!("terminal frame failed: {e:#}"))
                .ok();
            (frame, m)
        });
        Prepaint {
            frame,
            metrics,
            layout_time: start.elapsed(),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let start = Instant::now();
        let Some(frame) = prepaint.frame.take() else {
            return;
        };
        let m = &prepaint.metrics;
        let origin = bounds.origin + point(PADDING, PADDING);
        let cell = |col: u16, row: usize| {
            point(
                origin.x + m.cell_width * f32::from(col),
                origin.y + m.line_height * row as f32,
            )
        };

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, frame.background));

            for (y, row) in frame.rows.iter().enumerate() {
                for bg in &row.bgs {
                    let bounds = Bounds::new(
                        cell(bg.col, y),
                        size(m.cell_width * f32::from(bg.len), m.line_height),
                    );
                    window.paint_quad(fill(bounds, bg.color));
                }
            }

            for (y, row) in frame.rows.iter().enumerate() {
                for batch in &row.batches {
                    let line = window.text_system().shape_line(
                        batch.text.clone().into(),
                        m.font_size,
                        std::slice::from_ref(&batch.run),
                        Some(m.cell_width),
                    );
                    let _ = line.paint(
                        cell(batch.col, y),
                        m.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }

            if let Some(cursor) = &frame.cursor {
                let at = cell(cursor.col, usize::from(cursor.row));
                let width = m.cell_width * if cursor.wide { 2. } else { 1. };
                let full = Bounds::new(at, size(width, m.line_height));
                match cursor.style {
                    CursorVisualStyle::Bar => window.paint_quad(fill(
                        Bounds::new(at, size(px(2.), m.line_height)),
                        cursor.color,
                    )),
                    CursorVisualStyle::Underline => window.paint_quad(fill(
                        Bounds::new(
                            point(at.x, at.y + m.line_height - px(2.)),
                            size(width, px(2.)),
                        ),
                        cursor.color,
                    )),
                    CursorVisualStyle::BlockHollow => {
                        window.paint_quad(outline(full, cursor.color, gpui::BorderStyle::Solid))
                    }
                    _ => {
                        window.paint_quad(fill(full, cursor.color));
                        if let Some((text, run)) = &cursor.glyph {
                            let line = window.text_system().shape_line(
                                text.clone().into(),
                                m.font_size,
                                std::slice::from_ref(run),
                                None,
                            );
                            let _ =
                                line.paint(at, m.line_height, TextAlign::Left, None, window, cx);
                        }
                    }
                }
            }
        });

        let cost = prepaint.layout_time + start.elapsed();
        self.view
            .update(cx, |view, _| view.stats.record_frame(cost, frame.rebuilt));
    }
}
