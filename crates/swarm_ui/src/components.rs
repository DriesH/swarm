mod brand_mark;
mod button;
mod check_row;
mod checkbox;
mod confirm_row;
mod dialog;
mod dropdown;
mod icon;
mod icon_button;
mod kbd;
mod label;
mod list_row;
mod menu;
mod output_block;
mod panel;
mod popover_menu;
mod project_tile;
mod resize_handle;
mod segmented_control;
mod status;
mod stepper;
mod switch;
mod tab;
mod tab_bar;
mod tab_strip;
mod tag;
pub(crate) mod text_input;
mod tile_frame;
mod tooltip;
mod tray_chip;
mod tree_row;
mod usage_meter;

pub use brand_mark::*;
pub use button::*;
pub use check_row::*;
pub use checkbox::*;
pub use confirm_row::*;
pub use dialog::*;
pub use dropdown::*;
pub use icon::*;
pub use icon_button::*;
pub use kbd::*;
pub use label::*;
pub use list_row::*;
pub use menu::*;
pub use output_block::*;
pub use panel::*;
pub use popover_menu::*;
pub use project_tile::*;
pub use resize_handle::*;
pub use segmented_control::*;
pub use status::*;
pub use stepper::*;
pub use switch::*;
pub use tab::*;
pub use tab_bar::*;
pub use tab_strip::*;
pub use tag::*;
pub use text_input::*;
pub use tile_frame::*;
pub use tooltip::*;
pub use tray_chip::*;
pub use tree_row::*;
pub use usage_meter::*;

use crate::component::ComponentEntry;

/// Every component with a preview, in gallery order. An explicit list keeps
/// the order stable and needs no link-time registration.
pub fn all_components() -> Vec<ComponentEntry> {
    vec![
        ComponentEntry::of::<Icon>(),
        ComponentEntry::of::<Label>(),
        ComponentEntry::of::<Kbd>(),
        ComponentEntry::of::<Button>(),
        ComponentEntry::of::<TextInput>(),
        ComponentEntry::of::<IconButton>(),
        ComponentEntry::of::<Switch>(),
        ComponentEntry::of::<Stepper>(),
        ComponentEntry::of::<Checkbox>(),
        ComponentEntry::of::<SegmentedControl>(),
        ComponentEntry::of::<Tab>(),
        ComponentEntry::of::<TabBar>(),
        ComponentEntry::of::<TabStrip>(),
        ComponentEntry::of::<ResizeHandle>(),
        ComponentEntry::of::<Menu>(),
        ComponentEntry::of::<PopoverMenu>(),
        ComponentEntry::of::<Dropdown>(),
        ComponentEntry::of::<StatusPill>(),
        ComponentEntry::of::<Tag>(),
        ComponentEntry::of::<CheckRow>(),
        ComponentEntry::of::<OutputBlock>(),
        ComponentEntry::of::<ListRow>(),
        ComponentEntry::of::<TreeRow>(),
        ComponentEntry::of::<TrayChip>(),
        ComponentEntry::of::<TrayMore>(),
        ComponentEntry::of::<Panel>(),
        ComponentEntry::of::<UsageMeter>(),
        ComponentEntry::of::<BrandMark>(),
        ComponentEntry::of::<ConfirmRow>(),
        ComponentEntry::of::<ProjectTile>(),
        ComponentEntry::of::<TileFrame>(),
        ComponentEntry::of::<Dialog>(),
        ComponentEntry::of::<Tooltip>(),
    ]
}
