mod button;
mod icon;
mod icon_button;
mod label;
mod list_row;
mod panel;
mod segmented_control;
mod status;
mod switch;
mod tab;
mod tile_frame;
mod tray_chip;

pub use button::*;
pub use icon::*;
pub use icon_button::*;
pub use label::*;
pub use list_row::*;
pub use panel::*;
pub use segmented_control::*;
pub use status::*;
pub use switch::*;
pub use tab::*;
pub use tile_frame::*;
pub use tray_chip::*;

use crate::component::ComponentEntry;

/// Every component with a preview, in gallery order. An explicit list keeps
/// the order stable and needs no link-time registration.
pub fn all_components() -> Vec<ComponentEntry> {
    vec![
        ComponentEntry::of::<Icon>(),
        ComponentEntry::of::<Label>(),
        ComponentEntry::of::<Button>(),
        ComponentEntry::of::<IconButton>(),
        ComponentEntry::of::<Switch>(),
        ComponentEntry::of::<SegmentedControl>(),
        ComponentEntry::of::<Tab>(),
        ComponentEntry::of::<StatusPill>(),
        ComponentEntry::of::<ListRow>(),
        ComponentEntry::of::<TrayChip>(),
        ComponentEntry::of::<Panel>(),
        ComponentEntry::of::<TileFrame>(),
    ]
}
