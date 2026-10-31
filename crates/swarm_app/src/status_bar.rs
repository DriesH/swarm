//! The status bar: tray, "+ Terminal" and counts.

use gpui::{Context, Pixels, Role, SharedString, Window, canvas, div, px};
use swarm_ui::prelude::*;

use crate::actions::NewTerminal;
use crate::model::{AgentKind, TerminalId, TerminalKind};
use crate::review_model::count_comments;
use crate::workspace::{View, Workspace};

const TRAY_GAP: f32 = 6.;

/// A tile in the tray, with what its chip and its menu item show.
struct TrayEntry {
    id: TerminalId,
    title: SharedString,
    icon: IconName,
    status: AgentStatus,
}

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    if workspace.has_no_projects() {
        return h_flex()
            .size_full()
            .px(px(10.))
            .child(div().flex_1())
            .child(
                Label::new("⌘K")
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint),
            )
            .into_any_element();
    }

    let task = workspace.active_task();
    let agents = workspace.data.agents_of(task.id).count();
    let (processes, user_terminals) = (task.processes.len(), task.user_terminals.len());
    let branch = task.branch.clone();

    let mut entries: Vec<TrayEntry> = workspace
        .layout
        .tray
        .iter()
        .filter_map(|id| tray_entry(workspace, TerminalId(id.0)))
        .collect();
    let shown = chips_that_fit(&entries, workspace.tray_width, window);
    let hidden = entries.split_off(shown);

    let comments = (workspace.view == View::Review)
        .then(|| count_comments(&workspace.data.task_comments(task.id)).status_text())
        .flatten();

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
        .child(tray(entries, hidden, workspace.tray_width, cx))
        .child(
            div().flex_none().child(
                Button::new("new-terminal", "Terminal")
                    .debug_selector(|| "new-terminal".to_owned())
                    .variant(ButtonVariant::Ghost)
                    .icon(IconName::Plus)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewTerminal), cx)),
            ),
        )
        .children(comments.map(|text| {
            h_flex()
                .flex_none()
                .id("comment-counts")
                .debug_selector(|| "comment-counts".to_owned())
                .role(Role::Group)
                .aria_label(text.clone())
                .child(mono(text.into()))
        }))
        .child(
            h_flex()
                .flex_none()
                .debug_selector(|| "status-stats".to_owned())
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
        .into_any_element()
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

fn tray_entry(workspace: &Workspace, id: TerminalId) -> Option<TrayEntry> {
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

    Some(TrayEntry {
        id,
        title: terminal.title.clone(),
        icon,
        status: agent.map_or(AgentStatus::Working, |agent| agent.status),
    })
}

/// The chips that fit in the tray, then a "+N" chip for the tiles that do not. The tray
/// clips what is left, so a width that is not known yet shows every chip.
fn tray(
    shown: Vec<TrayEntry>,
    hidden: Vec<TrayEntry>,
    known_width: Option<Pixels>,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let workspace = cx.entity();
    let more = (!hidden.is_empty()).then(|| more_chip(hidden, cx));

    let measure = canvas(
        {
            let workspace = workspace.clone();

            move |bounds, _, cx| {
                let width = bounds.size.width;

                if known_width != Some(width) {
                    let workspace = workspace.clone();
                    cx.defer(move |cx| {
                        workspace.update(cx, |workspace, cx| {
                            workspace.tray_width = Some(width);
                            cx.notify();
                        });
                    });
                }
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_full();

    h_flex()
        .id("tray")
        .debug_selector(|| "tray".to_owned())
        .role(Role::List)
        .aria_label("Tray")
        .relative()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .gap(px(TRAY_GAP))
        .children(shown.into_iter().map(|entry| {
            let id = entry.id;

            TrayChip::new(("tray-chip", id.0), entry.icon, entry.title, entry.status).on_click(
                cx.listener(move |workspace, _, _, cx| {
                    workspace.restore(id, cx);
                    workspace.focus_terminal(id, cx);
                }),
            )
        }))
        .children(more)
        .child(measure)
}

/// "+N": a menu of the tiles that did not fit, each one restored and focused when picked.
fn more_chip(hidden: Vec<TrayEntry>, cx: &mut Context<Workspace>) -> PopoverMenu {
    let count = hidden.len();
    let workspace = cx.entity();

    PopoverMenu::new(
        "tray-more",
        TrayMore::new("tray-more-chip", count).debug_selector(|| "tray-more".to_owned()),
        move |_, _| {
            hidden
                .iter()
                .fold(Menu::new("tray-more-items", "More tiles"), |menu, entry| {
                    let (id, workspace) = (entry.id, workspace.clone());
                    let label = format!("{}, {}", entry.title, entry.status.label().to_lowercase());

                    menu.item(
                        MenuItem::new(("tray-more-item", id.0), label)
                            .icon(entry.icon)
                            .on_select(move |_, cx| {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.restore(id, cx);
                                    workspace.focus_terminal(id, cx);
                                });
                            }),
                    )
                })
        },
    )
    .opens_upward()
}

/// How many of the chips fit in the tray. Without a measured width, all of them.
fn chips_that_fit(entries: &[TrayEntry], width: Option<Pixels>, window: &Window) -> usize {
    let Some(width) = width else {
        return entries.len();
    };

    let widths: Vec<f32> = entries
        .iter()
        .map(|entry| f32::from(TrayChip::width(&entry.title, window)))
        .collect();

    fit_count(&widths, f32::from(width), TRAY_GAP, |hidden| {
        f32::from(TrayMore::width(hidden, window))
    })
}

/// How many chips of these `widths` fit in `space`, with `gap` between them. When some do
/// not fit, the "+N" chip takes room too; `more` gives its width for N hidden chips.
fn fit_count(widths: &[f32], space: f32, gap: f32, more: impl Fn(usize) -> f32) -> usize {
    // used[count] is the width of the first `count` chips and the gaps between them.
    let mut used = vec![0.];
    for (index, width) in widths.iter().enumerate() {
        let lead = if index == 0 { 0. } else { gap };

        used.push(used[index] + lead + width);
    }

    if used[widths.len()] <= space {
        return widths.len();
    }

    (0..widths.len())
        .rev()
        .find(|&count| {
            let lead = if count == 0 { 0. } else { gap };

            used[count] + lead + more(widths.len() - count) <= space
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;

    const GAP: f32 = 6.;

    /// The sample workspace in a window of the smallest size the app allows, with its
    /// tray measured.
    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(swarm_ui::init);
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.simulate_resize(gpui::size(px(1100.), px(700.)));

        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        cx.run_until_parked();

        (workspace, cx)
    }

    fn bounds_of(selector: &'static str, cx: &mut VisualTestContext) -> gpui::Bounds<Pixels> {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        bounds.unwrap_or_default()
    }

    #[gpui::test]
    fn the_tiles_that_do_not_fit_go_behind_a_more_chip(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let tray = bounds_of("tray", cx);
        let more = bounds_of("tray-more", cx);
        let stats = bounds_of("status-stats", cx);
        let button = bounds_of("new-terminal", cx);
        let (width, in_tray) = workspace.read_with(cx, |workspace, _| {
            (workspace.tray_width, workspace.layout.tray.len())
        });

        assert_eq!(width, Some(tray.size.width));
        assert!(in_tray > 1);
        assert!(more.right() <= tray.right());
        assert!(tray.right() <= button.left());
        assert!(button.right() <= stats.left());
        assert!(stats.right() <= px(1100.));
    }

    #[gpui::test]
    fn the_more_chip_opens_a_menu_above_it(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let more = bounds_of("tray-more", cx);

        cx.simulate_click(more.center(), Modifiers::none());
        cx.run_until_parked();

        let menu = bounds_of("tray-more-items", cx);
        assert!(menu.bottom() <= more.top());
        assert!(menu.top() >= px(0.));
    }

    #[gpui::test]
    fn picking_a_tile_in_the_menu_restores_and_focuses_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let first_hidden = cx.update(|window, cx| {
            workspace.read_with(cx, |workspace, _| {
                let entries: Vec<TrayEntry> = workspace
                    .layout
                    .tray
                    .iter()
                    .filter_map(|id| tray_entry(workspace, TerminalId(id.0)))
                    .collect();
                let shown = chips_that_fit(&entries, workspace.tray_width, window);

                entries.get(shown).map(|entry| entry.id)
            })
        });
        let first_hidden = first_hidden.expect("a tile is hidden");
        workspace.update(cx, |workspace, _| {
            workspace.minimized.insert(first_hidden);
        });
        let more = bounds_of("tray-more", cx);

        cx.simulate_click(more.center(), Modifiers::none());
        cx.simulate_keystrokes("enter");

        let (focused, minimized) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.focused_terminal,
                workspace.minimized.contains(&first_hidden),
            )
        });
        assert_eq!(focused, Some(first_hidden));
        assert!(!minimized);
        assert!(cx.debug_bounds("tray-more-items").is_none());
    }

    fn more(_: usize) -> f32 {
        30.
    }

    #[test]
    fn every_chip_fits_when_there_is_room() {
        assert_eq!(fit_count(&[100., 100., 100.], 312., GAP, more), 3);
    }

    #[test]
    fn the_more_chip_takes_room_when_a_chip_does_not_fit() {
        // Three chips need 312. Two chips, a gap and the "+1" chip need 242.
        assert_eq!(fit_count(&[100., 100., 100.], 300., GAP, more), 2);
        assert_eq!(fit_count(&[100., 100., 100.], 241., GAP, more), 1);
    }

    #[test]
    fn only_the_more_chip_shows_when_no_chip_fits() {
        assert_eq!(fit_count(&[100., 100.], 60., GAP, more), 0);
    }

    #[test]
    fn nothing_shows_in_an_empty_tray() {
        assert_eq!(fit_count(&[], 0., GAP, more), 0);
    }

    #[test]
    fn the_width_of_the_more_chip_counts_the_hidden_chips() {
        let wider = |hidden: usize| if hidden >= 10 { 50. } else { 30. };
        let widths = [100.; 12];

        // Nine chips and a "+3" chip fit in 9 * 106 + 30; with ten hidden the chip is wider.
        assert_eq!(fit_count(&widths, 9. * 106. + 30., GAP, wider), 9);
        assert_eq!(fit_count(&widths, 2. * 106. + 49., GAP, wider), 1);
    }
}
