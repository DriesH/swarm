use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Role, SharedString, Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

/// A terminal in the tray. Clicking restores it to the layout.
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
            .gap(px(7.))
            .h(px(24.))
            .px(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(line)
            .bg(raised)
            .hover(|style| style.bg(active))
            .focus_visible(|style| focus_ring(style, accent))
            .child(Icon::new(self.icon).size(IconSize::Medium))
            .child(
                Label::new(self.name)
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM),
            )
            .child(StatusDot::new(self.status).size(px(6.)))
            .child(
                Icon::new(IconName::Restore)
                    .size(IconSize::XSmall)
                    .color(Color::Faint),
            )
            .when_some(self.on_click, |chip, handler| chip.on_click(handler))
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
                        "Waiting",
                        TrayChip::new("pty-io", IconName::Codex, "pty-io", AgentStatus::Waiting),
                    ),
                ],
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
