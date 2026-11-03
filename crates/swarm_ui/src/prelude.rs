//! `use swarm_ui::prelude::*;` in every UI file.

pub use gpui::prelude::*;
pub use gpui::{App, IntoElement, Window};

pub use crate::a11y::{A11yFlags, Live, WithA11yFlags};
pub use crate::components::{
    AgentStatus, BrandMark, Button, ButtonSize, ButtonVariant, CheckRow, CheckStatus, Checkbox,
    ConfirmRow, ContextMenu, Dialog, Dropdown, FileIcon, Highlighter, HoneyFill, Honeycomb, Icon,
    IconButton, IconButtonSize, IconName, IconSize, Kbd, Label, LabelSize, ListRow, LiveRegion,
    Menu, MenuItem, OutputBlock, Panel, PanelRow, PixelBee, PopoverMenu, ProjectTile, RailButton,
    ResizeHandle, SegmentedControl, StatusDot, StatusPill, StatusRing, Stepper, Switch,
    SystemCheckRow, SystemCheckState, Tab, TabBar, TabStrip, Tag, TagTone, TextArea, TextInput,
    TextInputEvent, TileFrame, Toast, TrayChip, TrayMore, TreeRow, UsageDetailMeter, UsageMeter,
    UsageSurface, UsageTone,
};
pub use crate::file_icons::FileIconName;
pub use crate::markdown_highlight::markdown_highlights;
pub use crate::styles::{Color, h_flex, v_flex};
pub use crate::theme::ActiveTheme;
pub use crate::traits::{Clickable, Disableable, Toggleable};
