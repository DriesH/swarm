use gpui::{
    AnyElement, App, ElementId, FocusHandle, FontWeight, Hsla, Role, SharedString, Window, div, px,
    relative,
};

use crate::component::{Component, example, example_group};
use crate::fonts::MONO_FONT;
use crate::prelude::*;

/// The color of the ring on a focused tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TileRing {
    /// The accent color, for agent tiles.
    #[default]
    Accent,
    /// The text color, for the user's own terminal.
    Fg,
}

/// The frame around a terminal: a header with icon, name, meta, status and
/// actions, and a body slot for the terminal itself.
#[derive(IntoElement)]
#[must_use]
pub struct TileFrame {
    id: ElementId,
    icon: IconName,
    name: SharedString,
    accessible_label: Option<SharedString>,
    meta: Option<SharedString>,
    status: Option<AgentStatus>,
    spinner: bool,
    actions: Vec<AnyElement>,
    body: Vec<AnyElement>,
    footer: Option<AnyElement>,
    name_suffix: Option<AnyElement>,
    ring: TileRing,
    focused: bool,
    attention: bool,
    focus_handle: Option<FocusHandle>,
}

impl TileFrame {
    pub fn new(id: impl Into<ElementId>, icon: IconName, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            name: name.into(),
            accessible_label: None,
            meta: None,
            status: None,
            spinner: false,
            actions: Vec::new(),
            body: Vec::new(),
            footer: None,
            name_suffix: None,
            ring: TileRing::default(),
            focused: false,
            attention: false,
            focus_handle: None,
        }
    }

    /// Model, owner or size, shown faint after the name.
    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// Shows a status pill in the header.
    pub fn status(mut self, status: AgentStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Shows a status ring instead of a pill, like the orchestrator tile.
    pub fn status_ring(mut self, status: AgentStatus) -> Self {
        self.status = Some(status);
        self.spinner = true;
        self
    }

    /// What VoiceOver reads for the tile, for example "pty-io terminal".
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Adds a header action, usually an `IconButton`.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// Fixed content below the body, such as the orchestrator's input.
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// Shown right after the name, before the meta.
    pub fn name_suffix(mut self, suffix: impl IntoElement) -> Self {
        self.name_suffix = Some(suffix.into_any_element());
        self
    }

    /// The color of the focus ring.
    pub fn ring(mut self, ring: TileRing) -> Self {
        self.ring = ring;
        self
    }

    /// The tile the user is working in: 2px ring, accent unless set with `ring`.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The agent needs the user: 2px warn ring. The focus ring wins when both
    /// are set, so the user always sees where they are.
    pub fn attention(mut self, attention: bool) -> Self {
        self.attention = attention;
        self
    }

    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }
}

impl ParentElement for TileFrame {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.body.extend(elements);
    }
}

impl RenderOnce for TileFrame {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();

        let ring: Option<Hsla> = if self.focused {
            Some(match self.ring {
                TileRing::Accent => colors.accent,
                TileRing::Fg => colors.fg,
            })
        } else if self.attention {
            Some(colors.warn)
        } else {
            None
        };

        let (line, panel, term) = (colors.line, colors.panel, colors.term);
        let label = self
            .accessible_label
            .unwrap_or_else(|| SharedString::from(format!("{} terminal", self.name)));

        let icon_color = if self.icon == IconName::Terminal || self.icon == IconName::User {
            Color::Muted
        } else {
            Color::Default
        };

        let header = h_flex()
            .flex_none()
            .h(px(34.))
            .gap(px(8.))
            .pl(px(12.))
            .pr(px(8.))
            .bg(panel)
            .border_b_1()
            .border_color(line)
            .child(Icon::new(self.icon).size(IconSize::Large).color(icon_color))
            .child(Label::new(self.name).weight(FontWeight::MEDIUM))
            .children(self.name_suffix)
            .when_some(self.meta, |header, meta| {
                header.child(
                    div().min_w_0().flex().child(
                        Label::new(meta)
                            .mono()
                            .size(LabelSize::XSmall)
                            .color(Color::Faint)
                            .truncate(),
                    ),
                )
            })
            .child(div().flex_1())
            .when_some(self.status, |header, status| {
                if self.spinner {
                    header.child(StatusRing::new(status))
                } else {
                    header.child(StatusPill::new(status))
                }
            })
            .children(self.actions);

        div()
            .id(self.id)
            .role(Role::Group)
            .aria_label(label)
            .map(|tile| match &self.focus_handle {
                Some(handle) => tile.track_focus(handle),
                None => tile,
            })
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .rounded(px(10.))
            .border_1()
            .border_color(line)
            .bg(term)
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(12.))
                    .py(px(10.))
                    .font_family(MONO_FONT)
                    .text_size(px(11.5))
                    .line_height(relative(1.65))
                    .overflow_hidden()
                    .children(self.body),
            )
            .children(self.footer.map(|footer| div().flex_none().child(footer)))
            // Drawn last, above the header, as an overlay so the ring never
            // moves the layout.
            .when_some(ring, |tile, ring| {
                tile.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(px(10.))
                        .border_2()
                        .border_color(ring),
                )
            })
    }
}

impl Component for TileFrame {
    fn name() -> &'static str {
        "TileFrame"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let line = |text: &'static str, color: Color| {
            Label::new(text).mono().size(LabelSize::XSmall).color(color)
        };

        let tile = |id: &'static str| div().id(id).w(px(340.)).h(px(190.));

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Focused",
                        tile("focused-wrap").child(
                            TileFrame::new("orchestrator", IconName::ClaudeCode, "orchestrator")
                                .meta("opus 5.5")
                                .status_ring(AgentStatus::Working)
                                .focused(true)
                                .child(line("› split the renderer work", Color::Default))
                                .child(line("● swarm spawn term-render", Color::Default))
                                .child(line("  └ worktree ready · setup ✓ 8.2s", Color::Muted)),
                        ),
                    ),
                    example(
                        "Attention",
                        tile("attention-wrap").child(
                            TileFrame::new("pty-io", IconName::Codex, "pty-io")
                                .meta("codex")
                                .status(AgentStatus::Waiting)
                                .attention(true)
                                .action(IconButton::new(
                                    "minimize",
                                    IconName::Minimize,
                                    "Move pty-io to tray",
                                ))
                                .child(line("• Ran cargo test -p swarm-term", Color::Default))
                                .child(line("  └ 14 passed · 0 failed", Color::Ok))
                                .child(line("◦ Waiting for orchestrator", Color::Warn)),
                        ),
                    ),
                    example(
                        "User terminal",
                        tile("user-wrap").child(
                            TileFrame::new("user-terminal", IconName::User, "zsh")
                                .name_suffix(
                                    Label::new("You")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Faint),
                                )
                                .meta("~/swarm")
                                .ring(TileRing::Fg)
                                .focused(true)
                                .child(line("$ cargo build", Color::Default)),
                        ),
                    ),
                    example(
                        "With footer",
                        tile("footer-wrap").child(
                            TileFrame::new(
                                "orchestrator-footer",
                                IconName::ClaudeCode,
                                "orchestrator",
                            )
                            .meta("opus 5.5")
                            .status_ring(AgentStatus::Working)
                            .footer(
                                div().px(px(12.)).py(px(8.)).child(
                                    Label::new("Message the orchestrator").color(Color::Faint),
                                ),
                            )
                            .child(line("› split the renderer work", Color::Default)),
                        ),
                    ),
                ],
            ))
            .child(example_group(
                "Process",
                [example(
                    "Working",
                    tile("process-wrap").child(
                        TileFrame::new("cargo-test", IconName::Terminal, "cargo test")
                            .meta("term-render · 90×14")
                            .accessible_label("cargo test process terminal")
                            .status(AgentStatus::Working)
                            .action(IconButton::new(
                                "minimize",
                                IconName::Minimize,
                                "Move cargo test to tray",
                            ))
                            .action(IconButton::new("stop", IconName::Close, "Stop cargo test"))
                            .child(line("running 22 tests", Color::Default))
                            .child(line("test pty::spawn ... ok", Color::Muted))
                            .child(line("test grid::shape_line ... ok", Color::Muted)),
                    ),
                )],
            ))
            .into_any_element()
    }
}
