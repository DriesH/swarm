//! The focus and attention borders of a tile sit on the tile's own border.

use gpui::{Bounds, Context, Pixels, Render, TestAppContext, Window, div, px};
use swarm_ui::prelude::*;
use swarm_ui::{ResizeAxis, ResizeHandle, TILE_RADIUS, TileFrame};

struct Harness {
    focused: bool,
    attention: bool,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size(px(300.)).p(px(20.)).child(
            TileFrame::new("tile", IconName::Terminal, "zsh")
                .debug_selector(|| "tile".to_owned())
                .focused(self.focused)
                .attention(self.attention),
        )
    }
}

type Boxes = (Option<Bounds<Pixels>>, Option<Bounds<Pixels>>);

/// The bounds of the tile and of its ring.
fn draw(focused: bool, attention: bool, cx: &mut TestAppContext) -> Boxes {
    cx.update(swarm_ui::theme::init);

    let (_, cx) = cx.add_window_view(|_, _| Harness { focused, attention });
    cx.run_until_parked();

    (cx.debug_bounds("tile"), cx.debug_bounds("tile-ring"))
}

#[gpui::test]
fn the_focus_border_covers_the_tile_exactly(cx: &mut TestAppContext) {
    let (tile, ring) = draw(true, false, cx);

    assert!(tile.is_some());
    assert_eq!(ring, tile);
}

#[gpui::test]
fn the_attention_border_covers_the_tile_exactly(cx: &mut TestAppContext) {
    let (tile, ring) = draw(false, true, cx);

    assert!(tile.is_some());
    assert_eq!(ring, tile);
}

#[gpui::test]
fn a_tile_without_a_ring_has_no_border_overlay(cx: &mut TestAppContext) {
    let (_, ring) = draw(false, false, cx);

    assert!(ring.is_none());
}

#[gpui::test]
fn the_ring_does_not_move_the_tile(cx: &mut TestAppContext) {
    let (plain, _) = draw(false, false, cx);
    let (ringed, _) = draw(true, false, cx);

    assert_eq!(plain, ringed);
}

struct Edge;

impl Render for Edge {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = cx.focus_handle();

        div()
            .debug_selector(|| "panel".to_owned())
            .relative()
            .w(px(200.))
            .h(px(100.))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(4.))
                    .child(
                        ResizeHandle::new("resize", "Resize", &focus)
                            .axis(ResizeAxis::Vertical)
                            .corner_radius(TILE_RADIUS)
                            .value(100., 40., 300.)
                            .debug_selector(|| "edge".to_owned()),
                    ),
            )
    }
}

#[gpui::test]
fn a_vertical_handle_paints_the_border_of_its_panel_and_no_more(cx: &mut TestAppContext) {
    cx.update(swarm_ui::theme::init);
    let (_, cx) = cx.add_window_view(|_, _| Edge);
    cx.run_until_parked();

    assert_eq!(cx.debug_bounds("edge"), cx.debug_bounds("panel"));
}
