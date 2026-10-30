use gpui::{AnyElement, App, ElementId, Pixels, Role, SharedString, Window, div, px, svg};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// Icons from the design mockups, as inline 24×24 SVGs. GPUI draws an SVG as
/// a mask in the element's text color, so the stroke and fill color here do
/// not matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconName {
    Agent,
    Bee,
    Branch,
    Check,
    ChevronDown,
    ChevronRight,
    ClaudeCode,
    Close,
    Codex,
    Diff,
    ExternalLink,
    File,
    Lock,
    Minimize,
    Plus,
    PlusSquare,
    PullRequest,
    Restore,
    Search,
    Settings,
    Sidebar,
    Tab,
    Terminal,
    User,
}

macro_rules! stroke_icon {
    ($width:literal, $body:literal) => {
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width=""##,
            $width,
            r##"" stroke-linecap="round" stroke-linejoin="round">"##,
            $body,
            "</svg>"
        )
    };
}

macro_rules! fill_icon {
    ($path:literal) => {
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" fill-rule="evenodd" clip-rule="evenodd" d=""##,
            $path,
            r##""/></svg>"##
        )
    };
}

impl IconName {
    pub const ALL: [Self; 24] = [
        Self::Agent,
        Self::Bee,
        Self::Branch,
        Self::Check,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::ClaudeCode,
        Self::Close,
        Self::Codex,
        Self::Diff,
        Self::ExternalLink,
        Self::File,
        Self::Lock,
        Self::Minimize,
        Self::Plus,
        Self::PlusSquare,
        Self::PullRequest,
        Self::Restore,
        Self::Search,
        Self::Settings,
        Self::Sidebar,
        Self::Tab,
        Self::Terminal,
        Self::User,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Bee => "bee",
            Self::Branch => "branch",
            Self::Check => "check",
            Self::ChevronDown => "chevron_down",
            Self::ChevronRight => "chevron_right",
            Self::ClaudeCode => "claude_code",
            Self::Close => "close",
            Self::Codex => "codex",
            Self::Diff => "diff",
            Self::ExternalLink => "external_link",
            Self::File => "file",
            Self::Lock => "lock",
            Self::Minimize => "minimize",
            Self::Plus => "plus",
            Self::PlusSquare => "plus_square",
            Self::PullRequest => "pull_request",
            Self::Restore => "restore",
            Self::Search => "search",
            Self::Settings => "settings",
            Self::Sidebar => "sidebar",
            Self::Tab => "tab",
            Self::Terminal => "terminal",
            Self::User => "user",
        }
    }

    pub fn svg(self) -> &'static str {
        match self {
            Self::Agent => stroke_icon!(
                "1.9",
                r#"<rect x="4" y="8" width="16" height="12" rx="3"/><path d="M12 4v4M9 13v1M15 13v1"/>"#
            ),
            Self::Bee => stroke_icon!(
                "2",
                r#"<path d="M12 9c-1.6-3.6-6.4-4.4-7.2-1.8C4.1 9.6 8 10.6 12 9z"/><path d="M12 9c1.6-3.6 6.4-4.4 7.2-1.8.7 2.4-3.2 3.4-7.2 1.8z"/><ellipse cx="12" cy="15" rx="4.5" ry="5.5"/><path d="M7.7 13.5h8.6M7.9 16.8h8.2M12 20.5V22"/>"#
            ),
            Self::Branch => stroke_icon!(
                "1.8",
                r#"<circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="8" r="2.5"/><path d="M6 8.5v7M18 10.5c0 4-5 4-10 6"/>"#
            ),
            Self::Check => stroke_icon!("2.6", r#"<path d="m5 12 5 5 9-10"/>"#),
            Self::ChevronDown => stroke_icon!("2", r#"<path d="m6 9 6 6 6-6"/>"#),
            Self::ChevronRight => stroke_icon!("2", r#"<path d="m9 6 6 6-6 6"/>"#),
            Self::ClaudeCode => fill_icon!(
                "M20.998 10.949H24v3.102h-3v3.028h-1.487V20H18v-2.921h-1.487V20H15v-2.921H9V20H7.488v-2.921H6V20H4.487v-2.921H3V14.05H0V10.95h3V5h17.998v5.949zM6 10.949h1.488V8.102H6v2.847zm10.51 0H18V8.102h-1.49v2.847z"
            ),
            Self::Close => stroke_icon!("2", r#"<path d="M6 6l12 12M18 6 6 18"/>"#),
            Self::Codex => fill_icon!(
                "M8.086.457a6.105 6.105 0 013.046-.415c1.333.153 2.521.72 3.564 1.7a.117.117 0 00.107.029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a5.655 5.655 0 01-.18 1.631.167.167 0 00.04.155 5.982 5.982 0 011.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a6.063 6.063 0 01-2.934 1.851.162.162 0 00-.108.102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a.145.145 0 00-.14-.032c-.518.167-1.04.191-1.604.185a5.924 5.924 0 01-2.595-.622 6.058 6.058 0 01-2.146-1.781c-.203-.269-.404-.522-.551-.821a7.74 7.74 0 01-.495-1.283 6.11 6.11 0 01-.017-3.064.166.166 0 00.008-.074.115.115 0 00-.037-.064 5.958 5.958 0 01-1.38-2.202 5.196 5.196 0 01-.333-1.589 6.915 6.915 0 01.188-2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a.129.129 0 00.087-.087A6.016 6.016 0 015.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a.848.848 0 00-1.473.842l1.694 2.965-1.688 2.848a.849.849 0 001.46.864l1.94-3.272a.849.849 0 00.007-.854l-1.94-3.393zm5.446 6.24a.849.849 0 000 1.695h4.848a.849.849 0 000-1.696h-4.848z"
            ),
            Self::Diff => stroke_icon!(
                "1.8",
                r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M12 11v6M9 14h6"/>"#
            ),
            Self::ExternalLink => stroke_icon!(
                "1.8",
                r#"<path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>"#
            ),
            Self::File => stroke_icon!(
                "1.8",
                r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>"#
            ),
            Self::Lock => stroke_icon!(
                "1.9",
                r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/>"#
            ),
            Self::Minimize => stroke_icon!("2", r#"<path d="M5 18h14"/>"#),
            Self::Plus => stroke_icon!("1.8", r#"<path d="M12 5v14M5 12h14"/>"#),
            Self::PlusSquare => stroke_icon!(
                "1.8",
                r#"<rect x="3" y="3" width="18" height="18" rx="4"/><path d="M12 8v8M8 12h8"/>"#
            ),
            Self::PullRequest => stroke_icon!(
                "1.8",
                r#"<circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="18" r="2.5"/><path d="M6 8.5v7M18 15.5V11a3 3 0 0 0-3-3h-4"/><path d="m13 5-2 3 2 3"/>"#
            ),
            Self::Restore => stroke_icon!("2", r#"<path d="M7 17 17 7M9 7h8v8"/>"#),
            Self::Search => stroke_icon!(
                "1.8",
                r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#
            ),
            Self::Settings => stroke_icon!(
                "1.8",
                r#"<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1 7 17M17 7l2.1-2.1"/>"#
            ),
            Self::Sidebar => stroke_icon!(
                "1.7",
                r#"<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M9 4v16"/>"#
            ),
            Self::Tab => stroke_icon!(
                "1.8",
                r#"<rect x="3" y="5" width="18" height="14" rx="3"/><path d="M3 9h7V5"/>"#
            ),
            Self::Terminal => stroke_icon!(
                "1.8",
                r#"<rect x="3" y="4" width="18" height="16" rx="3"/><path d="m7 9 3 3-3 3M12 15h5"/>"#
            ),
            Self::User => stroke_icon!(
                "1.8",
                r#"<circle cx="12" cy="8" r="4"/><path d="M4 21c1.5-4 4.5-6 8-6s6.5 2 8 6"/>"#
            ),
        }
    }

    /// Brand marks keep their own color; everything else uses the given color.
    fn fixed_color(self) -> Option<Color> {
        match self {
            Self::ClaudeCode => Some(Color::Claude),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum IconSize {
    /// 11px
    XSmall,
    /// 12px
    Small,
    /// 14px
    #[default]
    Medium,
    /// 16px
    Large,
    Custom(Pixels),
}

impl IconSize {
    pub fn pixels(self) -> Pixels {
        match self {
            Self::XSmall => px(11.),
            Self::Small => px(12.),
            Self::Medium => px(14.),
            Self::Large => px(16.),
            Self::Custom(size) => size,
        }
    }
}

/// An icon. Decorative by default; call `accessible` when the icon carries
/// meaning that no nearby text gives.
#[derive(IntoElement)]
#[must_use]
pub struct Icon {
    name: IconName,
    size: IconSize,
    color: Color,
    accessible: Option<(ElementId, SharedString)>,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: IconSize::default(),
            color: Color::Default,
            accessible: None,
        }
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Exposes the icon to VoiceOver as an image with this label.
    pub fn accessible(mut self, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        self.accessible = Some((id.into(), label.into()));
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.name.fixed_color().unwrap_or(self.color).hsla(cx);
        let size = self.size.pixels();
        let icon = svg()
            .data(self.name.svg().as_bytes())
            .size(size)
            .flex_none()
            .text_color(color);

        match self.accessible {
            Some((id, label)) => div()
                .id(id)
                .role(Role::Image)
                .aria_label(label)
                .flex_none()
                .child(icon)
                .into_any_element(),
            None => icon.into_any_element(),
        }
    }
}

impl Component for Icon {
    fn name() -> &'static str {
        "Icon"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "All icons",
                IconName::ALL
                    .into_iter()
                    .map(|name| example(name.name(), Icon::new(name).size(IconSize::Large))),
            ))
            .child(example_group(
                "Sizes",
                [
                    ("11", IconSize::XSmall),
                    ("12", IconSize::Small),
                    ("14", IconSize::Medium),
                    ("16", IconSize::Large),
                ]
                .into_iter()
                .map(|(label, size)| example(label, Icon::new(IconName::Bee).size(size))),
            ))
            .child(example_group(
                "Colors",
                [
                    ("Default", Color::Default),
                    ("Muted", Color::Muted),
                    ("Faint", Color::Faint),
                    ("Accent", Color::Accent),
                    ("Ok", Color::Ok),
                    ("Warn", Color::Warn),
                ]
                .into_iter()
                .map(|(label, color)| {
                    example(
                        label,
                        Icon::new(IconName::Terminal)
                            .size(IconSize::Large)
                            .color(color),
                    )
                }),
            ))
            .child(example_group(
                "Agent kinds",
                [
                    example(
                        "Claude Code",
                        Icon::new(IconName::ClaudeCode)
                            .size(IconSize::Large)
                            .accessible("claude-code", "Claude Code"),
                    ),
                    example(
                        "Codex",
                        Icon::new(IconName::Codex)
                            .size(IconSize::Large)
                            .accessible("codex", "Codex"),
                    ),
                ],
            ))
            .into_any_element()
    }
}
