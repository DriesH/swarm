use gpui::{Div, div};
use swarm_ui::prelude::*;

/// Puts a live region in the tree without taking room in the layout. The region is always
/// drawn, because a screen reader speaks a live node when its value changes. `selector` lets
/// a test see that it is drawn.
pub(crate) fn announce(selector: &'static str, region: LiveRegion) -> Div {
    div()
        .absolute()
        .debug_selector(|| selector.to_owned())
        .child(region)
}
