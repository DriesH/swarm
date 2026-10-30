//! `use swarm_ui::prelude::*;` in every UI file.

pub use gpui::prelude::*;
pub use gpui::{App, IntoElement, Window};

pub use crate::components::{
    AgentStatus, Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize, IconName,
    IconSize, Label, LabelSize, ListRow, Panel, PanelRow, SegmentedControl, StatusDot, StatusPill,
    StatusRing, Switch, Tab, TileFrame, TrayChip,
};
pub use crate::styles::{Color, h_flex, v_flex};
pub use crate::theme::ActiveTheme;
pub use crate::traits::{Clickable, Disableable, Toggleable};
