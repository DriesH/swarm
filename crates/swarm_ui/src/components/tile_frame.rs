use gpui::{
    AnyElement, App, CursorStyle, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla, Pixels,
    Point, Render, Role, SharedString, Stateful, Window, div, px, relative,
};

use crate::a11y::accessible;
use crate::component::{Component, example, example_group};
use crate::fonts::MONO_FONT;
use crate::prelude::*;

type DragHook = Box<dyn FnOnce(Stateful<Div>) -> Stateful<Div>>;

/// The corner radius of a tile, its border and its focus border.
pub const TILE_RADIUS: Pixels = px(10.);

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
#[expect(clippy::struct_excessive_bools, reason = "one flag per header state")]
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
    locked: bool,
    ring: TileRing,
    focused: bool,
    attention: bool,
    focus_handle: Option<FocusHandle>,
    debug_selector: Option<String>,
    header_drag: Option<DragHook>,
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
            locked: false,
            ring: TileRing::default(),
            focused: false,
            attention: false,
            focus_handle: None,
            debug_selector: None,
            header_drag: None,
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

    /// Draws the terminal icon with a lock badge instead of the icon, for the user's own
    /// terminal.
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
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

    /// Name for `VisualTestContext::debug_bounds` of the tile; the ring gets `<name>-ring`.
    /// Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }

    /// Makes the header a drag source with a grab cursor. The actions are not part of it, so
    /// a press on a button never starts a drag. `constructor` makes the view that follows the
    /// pointer.
    pub fn on_header_drag<T: 'static, W: Render>(
        mut self,
        value: T,
        constructor: impl Fn(&T, Point<Pixels>, &mut Window, &mut App) -> Entity<W> + 'static,
    ) -> Self {
        self.header_drag = Some(Box::new(move |area| {
            area.cursor(CursorStyle::OpenHand)
                .on_drag(value, constructor)
        }));
        self
    }
}

impl ParentElement for TileFrame {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.body.extend(elements);
    }
}

/// The icon at the start of the header. The user's own terminal shows a lock badge, cut out
/// of `cutout`.
fn header_icon(icon: IconName, locked: bool, cutout: Hsla, selector: Option<String>) -> AnyElement {
    if locked {
        return LockedTerminalIcon::new()
            .cutout(cutout)
            .when_some(selector, |icon, selector| {
                icon.debug_selector(move || format!("{selector}-lock"))
            })
            .into_any_element();
    }

    let color = if icon == IconName::Terminal || icon == IconName::User {
        Color::Muted
    } else {
        Color::Default
    };

    Icon::new(icon)
        .size(IconSize::Large)
        .color(color)
        .into_any_element()
}

/// Makes `lead` the drag source when the frame has one. It adds no role and no label: the
/// move commands are the keyboard way.
fn drag_area(lead: Div, drag: Option<DragHook>, selector: Option<String>) -> AnyElement {
    match drag {
        Some(apply) => apply(
            lead.id("tile-header")
                .when_some(selector, |area, selector| {
                    area.debug_selector(move || format!("{selector}-header"))
                }),
        )
        .into_any_element(),
        None => lead.into_any_element(),
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

        // Everything but the actions, so the drag area never holds a button.
        let lead = h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(px(8.))
            .pl(px(12.))
            .child(header_icon(
                self.icon,
                self.locked,
                panel,
                self.debug_selector.clone(),
            ))
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
            });

        let lead = drag_area(lead, self.header_drag, self.debug_selector.clone());

        let header = h_flex()
            .flex_none()
            .h(px(34.))
            .gap(px(8.))
            .pr(px(8.))
            .bg(panel)
            .border_b_1()
            .border_color(line)
            .child(lead)
            .children(self.actions);

        let tile = div()
            .when_some(self.debug_selector.clone(), |tile, selector| {
                tile.debug_selector(move || selector)
            })
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .rounded(TILE_RADIUS)
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
            .children(self.footer.map(|footer| div().flex_none().child(footer)));

        // The ring is a sibling of the tile, so it spans the tile's border box and paints
        // over the tile border: one border, and the layout never moves.
        accessible(div().id(self.id), Role::Group, label)
            .map(|wrapper| match &self.focus_handle {
                Some(handle) => wrapper.track_focus(handle),
                None => wrapper,
            })
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(tile)
            .when_some(ring, |wrapper, ring| {
                wrapper.child(
                    div()
                        .when_some(self.debug_selector, |ring, selector| {
                            ring.debug_selector(move || format!("{selector}-ring"))
                        })
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(TILE_RADIUS)
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
                                .status(AgentStatus::Blocked)
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
                            TileFrame::new("user-terminal", IconName::Terminal, "zsh")
                                .locked(true)
                                .name_suffix(Tag::new("Mine"))
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

#[cfg(test)]
mod tests {
    use gpui::accesskit::{Node, Role};
    use gpui::{AppContext as _, Context, Element as _, Empty};

    use super::*;

    struct NoGhost;

    impl Render for NoGhost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            Empty
        }
    }

    #[test]
    fn the_header_drag_area_has_no_role_and_no_label() {
        let frame = TileFrame::new("t", IconName::Codex, "pty-io")
            .on_header_drag(1_u32, |_, _, _, cx| cx.new(|_| NoGhost));
        let Some(apply) = frame.header_drag else {
            panic!("the frame has no header drag");
        };

        let area = apply(h_flex().id("tile-header"));
        let mut node = Node::new(Role::Unknown);
        area.write_a11y_info(&mut node);

        assert_eq!(area.a11y_role(), None);
        assert_eq!(node.label(), None);
    }
}
