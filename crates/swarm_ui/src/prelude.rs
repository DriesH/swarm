//! `use swarm_ui::prelude::*;` in every UI file.

pub use gpui::prelude::*;
pub use gpui::{App, IntoElement, Window};

pub use crate::components::{
    AgentStatus, Button, ButtonSize, ButtonVariant, CheckRow, CheckStatus, Checkbox, Dialog,
    Dropdown, Icon, IconButton, IconButtonSize, IconName, IconSize, Kbd, Label, LabelSize, ListRow,
    Menu, MenuItem, OutputBlock, Panel, PanelRow, PopoverMenu, ProjectTile, ResizeHandle,
    SegmentedControl, StatusDot, StatusPill, StatusRing, Switch, Tab, TabBar, TabStrip, Tag,
    TagTone, TextArea, TextInput, TextInputEvent, TileFrame, TrayChip, TrayMore,
};
pub use crate::styles::{Color, h_flex, v_flex};
pub use crate::theme::ActiveTheme;
pub use crate::traits::{Clickable, Disableable, Toggleable};
