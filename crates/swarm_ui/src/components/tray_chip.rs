use gpui::{
    AnyElement, App, ClickEvent, ElementId, Font, FontWeight, Hsla, Pixels, Role, SharedString,
    TextRun, Window, div, font, px,
};

use crate::component::{Component, example, example_group};
use crate::fonts::UI_FONT;
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

const HEIGHT: f32 = 24.;
const BORDER: f32 = 1.;
const PADDING: f32 = 8.;
const GAP: f32 = 7.;
const DOT: f32 = 6.;
const MAX_NAME_WIDTH: f32 = 140.;
const LABEL_SIZE: LabelSize = LabelSize::Small;
const LABEL_WEIGHT: FontWeight = FontWeight::MEDIUM;

/// The width of `text` in the chip's label font.
fn label_width(text: &str, window: &Window) -> Pixels {
    let run = TextRun {
        len: text.len(),
        font: Font {
            weight: LABEL_WEIGHT,
            ..font(UI_FONT)
        },
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    window
        .text_system()
        .shape_line(
            SharedString::from(text.to_owned()),
            LABEL_SIZE.pixels(),
            &[run],
            None,
        )
        .width
}

/// A terminal in the tray. Clicking restores it to the layout. A long name is cut with an
/// ellipsis.
#[derive(IntoElement)]
#[must_use]
pub struct TrayChip {
    id: ElementId,
    icon: IconName,
    name: SharedString,
    status: AgentStatus,
    on_click: Option<ClickHandler>,
}

impl TrayChip {
    pub fn new(
        id: impl Into<ElementId>,
        icon: IconName,
        name: impl Into<SharedString>,
        status: AgentStatus,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            name: name.into(),
            status,
            on_click: None,
        }
    }
}

impl TrayChip {
    /// The width of the chip for `name`, so a row of chips can be fitted into a space.
    pub fn width(name: &str, window: &Window) -> Pixels {
        let icons = IconSize::Medium.pixels() + IconSize::XSmall.pixels();
        let name = label_width(name, window).min(px(MAX_NAME_WIDTH));

        icons + name + px(2. * BORDER + 2. * PADDING + 3. * GAP + DOT)
    }
}

impl Clickable for TrayChip {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for TrayChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (line, raised, active, accent) =
            (colors.line, colors.raised, colors.active, colors.accent);
        let label = SharedString::from(format!(
            "Show {}, {}",
            self.name,
            self.status.label().to_lowercase()
        ));

        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(label)
            .focusable()
            .tab_stop(true)
            .cursor_pointer()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(GAP))
            .h(px(HEIGHT))
            .px(px(PADDING))
            .rounded(px(6.))
            .border_1()
            .border_color(line)
            .bg(raised)
            .hover(|style| style.bg(active))
            .focus_visible(|style| focus_ring(style, accent))
            .child(Icon::new(self.icon).size(IconSize::Medium))
            .child(
                div().min_w_0().max_w(px(MAX_NAME_WIDTH)).child(
                    Label::new(self.name)
                        .size(LABEL_SIZE)
                        .weight(LABEL_WEIGHT)
                        .truncate(),
                ),
            )
            .child(StatusDot::new(self.status).size(px(DOT)))
            .child(
                Icon::new(IconName::Restore)
                    .size(IconSize::XSmall)
                    .color(Color::Faint),
            )
            .when_some(self.on_click, |chip, handler| chip.on_click(handler))
    }
}

/// The chip that ends a tray with more tiles than fit. It stands for them and is the
/// trigger of a menu that lists them.
#[derive(IntoElement)]
#[must_use]
pub struct TrayMore {
    id: ElementId,
    count: usize,
    debug_selector: Option<String>,
}

impl TrayMore {
    pub fn new(id: impl Into<ElementId>, count: usize) -> Self {
        Self {
            id: id.into(),
            count,
            debug_selector: None,
        }
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// The width of the chip for `count` tiles.
    pub fn width(count: usize, window: &Window) -> Pixels {
        label_width(&Self::text(count), window) + px(2. * BORDER + 2. * PADDING)
    }

    fn text(count: usize) -> String {
        format!("+{count}")
    }

    fn accessible_label(count: usize) -> String {
        let noun = if count == 1 { "tile" } else { "tiles" };

        format!("{count} more {noun}")
    }
}

impl RenderOnce for TrayMore {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (line, raised, active, accent) =
            (colors.line, colors.raised, colors.active, colors.accent);

        div()
            .id(self.id)
            .when_some(self.debug_selector, |element, selector| {
                element.debug_selector(move || selector)
            })
            .role(Role::Button)
            .aria_label(Self::accessible_label(self.count))
            .focusable()
            .tab_stop(true)
            .cursor_pointer()
            .flex()
            .flex_none()
            .items_center()
            .h(px(HEIGHT))
            .px(px(PADDING))
            .rounded(px(6.))
            .border_1()
            .border_color(line)
            .bg(raised)
            .hover(|style| style.bg(active))
            .focus_visible(|style| focus_ring(style, accent))
            .child(
                Label::new(Self::text(self.count))
                    .size(LABEL_SIZE)
                    .weight(LABEL_WEIGHT),
            )
    }
}

impl Component for TrayMore {
    fn name() -> &'static str {
        "TrayMore"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Tiles that do not fit",
                [
                    example("One", TrayMore::new("tray-more-one", 1)),
                    example("Four", TrayMore::new("tray-more-four", 4)),
                ],
            ))
            .into_any_element()
    }
}

impl Component for TrayChip {
    fn name() -> &'static str {
        "TrayChip"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Agents",
                [
                    example(
                        "Done",
                        TrayChip::new(
                            "ui-tokens",
                            IconName::ClaudeCode,
                            "ui-tokens",
                            AgentStatus::Done,
                        ),
                    ),
                    example(
                        "Idle",
                        TrayChip::new("bench", IconName::Codex, "bench", AgentStatus::Idle),
                    ),
                    example(
                        "Blocked",
                        TrayChip::new("pty-io", IconName::Codex, "pty-io", AgentStatus::Blocked),
                    ),
                ],
            ))
            .child(example_group(
                "Long name",
                [example(
                    "Cut with an ellipsis",
                    TrayChip::new(
                        "long-name",
                        IconName::Codex,
                        "a-very-long-worker-name-that-does-not-fit-in-a-chip",
                        AgentStatus::Working,
                    ),
                )],
            ))
            .child(example_group(
                "Process",
                [example(
                    "Working",
                    TrayChip::new(
                        "cargo-test",
                        IconName::Terminal,
                        "cargo test",
                        AgentStatus::Working,
                    ),
                )],
            ))
            .into_any_element()
    }
}
