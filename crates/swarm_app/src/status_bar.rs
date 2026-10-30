//! The status bar: tray, "+ Terminal" and counts.

use gpui::{Context, Role, SharedString, Window, px};
use swarm_ui::prelude::*;

use crate::actions::NewTerminal;
use crate::model::{AgentKind, TerminalId, TerminalKind};
use crate::workspace::Workspace;

pub(crate) fn render(
    workspace: &Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let task = workspace.active_task();
    let agents = workspace.data.agents_of(task.id).count();
    let (processes, user_terminals) = (task.processes.len(), task.user_terminals.len());
    let branch = task.branch.clone();

    let chips: Vec<_> = workspace
        .layout
        .tray
        .iter()
        .filter_map(|id| tray_chip(workspace, TerminalId(id.0), cx))
        .collect();

    let mono = |text: SharedString| {
        Label::new(text)
            .mono()
            .size(LabelSize::XSmall)
            .color(Color::Faint)
    };

    h_flex()
        .size_full()
        .gap(px(6.))
        .px(px(10.))
        .child(
            h_flex()
                .id("tray")
                .role(Role::List)
                .aria_label("Tray")
                .gap(px(6.))
                .children(chips),
        )
        .child(
            Button::new("new-terminal", "Terminal")
                .variant(ButtonVariant::Ghost)
                .icon(IconName::Plus)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewTerminal), cx)),
        )
        .child(gpui::div().flex_1())
        .child(
            h_flex()
                .gap(px(14.))
                .pr(px(6.))
                .when(!task.is_start, |bar| {
                    bar.child(stat(
                        "branch",
                        IconName::Branch,
                        branch.clone(),
                        &branch,
                        mono,
                    ))
                })
                .child(stat(
                    "agents",
                    IconName::Agent,
                    count(agents, "agent", "agents"),
                    &count(agents, "agent", "agents"),
                    mono,
                ))
                .child(stat(
                    "processes",
                    IconName::Terminal,
                    count(processes, "process", "processes"),
                    &count(processes, "process", "processes"),
                    mono,
                ))
                .when(user_terminals > 0, |bar| {
                    let text = count(user_terminals, "user terminal", "user terminals");
                    bar.child(stat(
                        "user-terminals",
                        IconName::User,
                        text.clone(),
                        &text,
                        mono,
                    ))
                })
                .child(
                    Label::new("permissions skipped")
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Warn),
                )
                .child(mono("⌘K".into())),
        )
}

fn count(number: usize, one: &str, many: &str) -> SharedString {
    let word = if number == 1 { one } else { many };

    format!("{number} {word}").into()
}

fn stat(
    id: &'static str,
    icon: IconName,
    text: SharedString,
    label: &SharedString,
    mono: impl Fn(SharedString) -> Label,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .role(Role::Group)
        .aria_label(label.clone())
        .gap(px(5.))
        .child(Icon::new(icon).size(IconSize::Small).color(Color::Faint))
        .child(mono(text))
}

fn tray_chip(
    workspace: &Workspace,
    id: TerminalId,
    cx: &mut Context<Workspace>,
) -> Option<TrayChip> {
    let terminal = workspace.data.terminal(id)?;
    let agent = workspace
        .data
        .agents
        .iter()
        .find(|agent| agent.terminal == id);

    let icon = match (terminal.kind, agent.map(|agent| agent.kind)) {
        (TerminalKind::Process | TerminalKind::User, _) => IconName::Terminal,
        (_, Some(AgentKind::Codex)) => IconName::Codex,
        _ => IconName::ClaudeCode,
    };
    let status = agent.map_or(AgentStatus::Working, |agent| agent.status);

    Some(
        TrayChip::new(("tray-chip", id.0), icon, terminal.title.clone(), status).on_click(
            cx.listener(move |workspace, _, _, cx| {
                workspace.restore(id, cx);
                workspace.focus_terminal(id, cx);
            }),
        ),
    )
}
