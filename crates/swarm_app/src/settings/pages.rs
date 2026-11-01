//! One render function per settings page.

use gpui::{AnyElement, ElementId, Entity, FontWeight, Role, SharedString, Stateful, div, px};
use swarm_settings::{AgentKindSetting, ProjectMode};
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::theme::{self, Appearance};

use super::{
    AGENTS, DELIVERY, IDLE_OPTIONS, NOTIFY_WHEN, PROJECT_MODES, PROJECT_NOTIFICATIONS, Page,
    SettingsState, SettingsWindow, delivery, delivery_mut, mode_index, notify_when,
    notify_when_mut,
};
use crate::notifications::{self, Permission};

type State = Entity<SettingsState>;

pub(super) fn apply_appearance(index: usize, cx: &mut App) {
    if let Some(appearance) = Appearance::ALL.get(index) {
        theme::set_appearance(*appearance, cx);
    }
}

fn toggle(
    state: &State,
    apply: impl Fn(&mut SettingsState, bool) + 'static,
) -> impl Fn(bool, &mut Window, &mut App) + 'static {
    let state = state.clone();

    move |on, _, cx| {
        state.update(cx, |state, cx| {
            apply(state, on);
            cx.notify();
        });
    }
}

fn choose(
    state: &State,
    apply: impl Fn(&mut SettingsState, usize) + 'static,
) -> impl Fn(usize, &mut Window, &mut App) + 'static {
    let state = state.clone();

    move |index, _, cx| {
        state.update(cx, |state, cx| {
            apply(state, index);
            cx.notify();
        });
    }
}

fn options(labels: &[&str]) -> Vec<SharedString> {
    labels
        .iter()
        .map(|label| SharedString::from(label.to_string()))
        .collect()
}

fn page_title(title: impl Into<SharedString>) -> impl IntoElement {
    Label::new(title)
        .size(LabelSize::Title)
        .weight(FontWeight::SEMIBOLD)
}

/// A labeled group: a small heading above a card.
fn section(heading: &'static str, body: impl IntoElement) -> impl IntoElement {
    v_flex()
        .id(heading)
        .role(Role::Group)
        .aria_label(heading)
        .child(
            div().mb(px(8.)).child(
                Label::new(heading)
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM)
                    .color(Color::Faint),
            ),
        )
        .child(body)
}

fn plain_row() -> gpui::Div {
    h_flex().gap_3().min_h(px(46.)).px(px(14.))
}

fn switch_row(
    state: &State,
    id: impl Into<ElementId>,
    label: &'static str,
    on: bool,
    apply: impl Fn(&mut SettingsState, bool) + 'static,
) -> PanelRow {
    PanelRow::new(label).end_slot(
        Switch::new(id, label)
            .toggle_state(on)
            .on_toggle(toggle(state, apply)),
    )
}

fn dropdown_row(
    state: &State,
    id: impl Into<ElementId>,
    label: &'static str,
    labels: Vec<SharedString>,
    selected: usize,
    apply: impl Fn(&mut SettingsState, usize) + 'static,
) -> PanelRow {
    PanelRow::new(label).end_slot(
        h_flex()
            .h(px(28.))
            .child(Dropdown::new(id, label, labels, selected).on_change(choose(state, apply))),
    )
}

fn input_row(label: &'static str, input: &Entity<impl Render>, width: f32) -> PanelRow {
    PanelRow::new(label).end_slot(div().w(px(width)).child(input.clone()))
}

fn add_row(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<gpui::Div> {
    let colors = cx.theme().colors();
    let (accent, raised) = (colors.accent, colors.raised);

    h_flex()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .cursor_pointer()
        .gap(px(6.))
        .h(px(40.))
        .px(px(14.))
        .hover(move |style| style.bg(raised))
        .focus_visible(move |style| focus_ring(style, accent))
        .on_click(on_click)
        .child(
            Icon::new(IconName::Plus)
                .size(IconSize::Small)
                .color(Color::Muted),
        )
        .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
}

fn removable_row(
    id: impl Into<ElementId>,
    text: SharedString,
    remove_label: String,
    on_remove: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    plain_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(text).mono().size(LabelSize::Small).truncate()),
        )
        .child(IconButton::new(id, IconName::Close, remove_label).on_click(on_remove))
}

impl SettingsWindow {
    pub(super) fn render_page(&self, cx: &Context<Self>) -> AnyElement {
        match self.page {
            Page::Agents => self.agents_page(cx),
            Page::Notifications => self.notifications_page(cx),
            Page::Appearance => Self::appearance_page(cx),
            Page::Project(index) => self.project_page(index, cx),
            other => page_title(other.title()).into_any_element(),
        }
    }

    fn agents_page(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.state;
        let settings = state.read(cx);

        let cards = AGENTS.iter().enumerate().map(|(index, info)| {
            let inputs = &self.inputs.agents[index];
            let icon_color = if index == 0 {
                Color::Claude
            } else {
                Color::Default
            };

            let header = plain_row()
                .child(Icon::new(info.icon).size(IconSize::Large).color(icon_color))
                .child(
                    div()
                        .flex_1()
                        .child(Label::new(info.name).weight(FontWeight::MEDIUM)),
                )
                .child(StatusPill::new(AgentStatus::Done).label(info.version));

            let card = Panel::new()
                .row(header)
                .row(input_row("Command", &inputs.command, 160.))
                .row(input_row(
                    "Orchestrator model",
                    &inputs.orchestrator_model,
                    160.,
                ))
                .row(input_row("Worker model", &inputs.worker_model, 160.));

            section(info.name, card)
        });

        let agent_names: Vec<&str> = AGENTS.iter().map(|info| info.name).collect();

        let workers = Panel::new()
            .row(dropdown_row(
                state,
                "orchestrator",
                "Orchestrator agent",
                options(&agent_names),
                settings.orchestrator_index(),
                |state, value| {
                    state.settings.agents.orchestrator = if value == 0 {
                        AgentKindSetting::ClaudeCode
                    } else {
                        AgentKindSetting::Codex
                    };
                },
            ))
            .row(switch_row(
                state,
                "agent-skip",
                "Skip permission prompts",
                settings.settings.agents.skip_permissions,
                |state, on| state.settings.agents.skip_permissions = on,
            ))
            .row(switch_row(
                state,
                "worker-typing",
                "Allow typing in worker terminals",
                settings.settings.terminals.allow_worker_typing,
                |state, on| state.settings.terminals.allow_worker_typing = on,
            ))
            .row(dropdown_row(
                state,
                "idle-stop",
                "Stop idle agents after",
                options(&IDLE_OPTIONS),
                settings.idle_stop,
                |state, value| state.idle_stop = value,
            ))
            .row(input_row(
                "Max agents per task",
                &self.inputs.max_agents_per_task,
                64.,
            ))
            .row(input_row(
                "Max agents in total",
                &self.inputs.max_agents_total,
                64.,
            ));

        let error = settings.save_error.clone().map(|message| {
            div()
                .id("settings-save-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        v_flex()
            .gap(px(26.))
            .child(page_title("Agents"))
            .children(error)
            .children(cards)
            .child(section("Workers", workers))
            .into_any_element()
    }

    fn notifications_page(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.state;
        let settings = state.read(cx);
        let notifications = &settings.settings.notifications;

        let when = NOTIFY_WHEN
            .iter()
            .enumerate()
            .fold(Panel::new(), |panel, (index, label)| {
                panel.row(
                    PanelRow::new(*label).end_slot(
                        Switch::new(("notify-when", index), *label)
                            .toggle_state(notify_when(notifications, index))
                            .debug_selector(|| format!("notify-when-{index}"))
                            .on_toggle(toggle(state, move |state, on| {
                                *notify_when_mut(&mut state.settings.notifications, index) = on;
                            })),
                    ),
                )
            });

        let delivery_panel =
            DELIVERY
                .iter()
                .enumerate()
                .fold(Panel::new(), |panel, (index, label)| {
                    panel.row(
                        PanelRow::new(*label).end_slot(
                            Switch::new(("delivery", index), *label)
                                .toggle_state(delivery(notifications, index))
                                .debug_selector(|| format!("delivery-{index}"))
                                .on_toggle(toggle(state, move |state, on| {
                                    *delivery_mut(&mut state.settings.notifications, index) = on;
                                })),
                        ),
                    )
                });

        let projects =
            settings
                .projects
                .iter()
                .enumerate()
                .fold(Panel::new(), |panel, (index, entry)| {
                    let name = entry.project.name.clone();
                    let root = entry.project.root.clone();
                    let mode = root
                        .as_deref()
                        .map_or(ProjectMode::All, |root| notifications.mode(root));

                    let picker = Dropdown::new(
                        ("project-notifications", index),
                        name.clone(),
                        options(&PROJECT_NOTIFICATIONS),
                        mode_index(mode),
                    )
                    .debug_selector(|| format!("project-notifications-{index}"))
                    .disabled(root.is_none())
                    .on_change(choose(state, move |state, value| {
                        if let (Some(root), Some(mode)) = (&root, PROJECT_MODES.get(value)) {
                            state.settings.notifications.set_mode(root, *mode);
                        }
                    }));

                    let row = plain_row()
                        .child(ProjectTile::new(
                            entry.project.letter,
                            entry.project.tile_color,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(name).weight(FontWeight::MEDIUM)),
                        )
                        .child(h_flex().h(px(28.)).child(picker));

                    panel.row(row)
                });

        let permission = settings.permission;
        let denied = permission == Some(Permission::NotAllowed);
        let status = permission.map(|permission| {
            let tone = match permission {
                Permission::Allowed => AgentStatus::Done,
                Permission::NotAllowed => AgentStatus::Error,
                Permission::NotAsked => AgentStatus::Idle,
                Permission::Unavailable => AgentStatus::Unknown,
            };

            StatusPill::new(tone).label(permission.label())
        });

        let permission_row = PanelRow::new("macOS permission").end_slot(
            h_flex().gap_2().children(status).when(denied, |row| {
                row.child(
                    Button::new("open-system-settings", "Open System Settings")
                        .debug_selector(|| "open-system-settings".to_owned())
                        .on_click(|_, _, cx| notifications::open_system_settings(cx)),
                )
            }),
        );

        let error = settings.save_error.clone().map(|message| {
            div()
                .id("settings-save-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        v_flex()
            .gap(px(28.))
            .child(page_title("Notifications"))
            .children(error)
            .child(section("Permission", Panel::new().row(permission_row)))
            .child(section("Notify me when", when))
            .child(section("Delivery", delivery_panel))
            .child(section("Projects", projects))
            .child(
                h_flex().child(
                    Button::new("test-notification", "Send test notification")
                        .debug_selector(|| "test-notification".to_owned())
                        .on_click(|_, _, cx| notifications::send_test(cx)),
                ),
            )
            .into_any_element()
    }

    fn appearance_page(cx: &Context<Self>) -> AnyElement {
        let selected = Appearance::ALL
            .iter()
            .position(|appearance| *appearance == theme::appearance(cx))
            .unwrap_or(0);

        let control = SegmentedControl::new(
            "appearance",
            "Theme",
            Appearance::ALL.map(Appearance::label),
        )
        .selected(selected)
        .on_select(|index, _, cx| apply_appearance(index, cx));

        v_flex()
            .gap(px(26.))
            .child(page_title("Appearance"))
            .child(section(
                "Theme",
                Panel::new().row(PanelRow::new("Theme").end_slot(control)),
            ))
            .into_any_element()
    }

    fn project_page(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let state = &self.state;
        let (Some(settings), Some(inputs)) = (
            state.read(cx).projects.get(index),
            self.inputs.projects.get(index),
        ) else {
            return div().into_any_element();
        };

        let header = h_flex()
            .gap_3()
            .child(
                ProjectTile::new(settings.project.letter, settings.project.tile_color)
                    .size(px(28.)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .child(page_title(settings.project.name.clone()))
                    .child(
                        Label::new(format!(
                            "~/personal/{} · .swarm/config.toml",
                            settings.project.name
                        ))
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint),
                    ),
            );

        let branches: Vec<SharedString> = settings
            .base_branches
            .iter()
            .map(|branch| SharedString::from(branch.clone()))
            .collect();

        let worktrees = Panel::new()
            .row(dropdown_row(
                state,
                ("base-branch", index),
                "Base branch",
                branches,
                settings.base_branch,
                move |state, value| {
                    if let Some(project) = state.projects.get_mut(index) {
                        project.base_branch = value;
                    }
                },
            ))
            .row(input_row("Worktree folder", &inputs.worktree_folder, 160.))
            .row(input_row("Port range", &inputs.port_range, 110.))
            .row(input_row(
                "Ports per worktree",
                &inputs.ports_per_worktree,
                64.,
            ));

        let setup = div()
            .id("setup-script-field")
            .child(inputs.setup_script.clone());

        let copy = self.copy_card(index, cx);
        let checks = self.checks_card(index, cx);

        v_flex()
            .gap(px(26.))
            .child(header)
            .child(section("Worktrees", worktrees))
            .child(section("Setup script", setup))
            .child(section("Copy into worktrees", copy))
            .child(
                v_flex().child(section("Checks before PR", checks)).child(
                    h_flex().mt(px(12.)).child(
                        Button::new("run-checks", "Run checks now").on_click(|_, _, _| {
                            log::info!("Running checks is not built yet");
                        }),
                    ),
                ),
            )
            .into_any_element()
    }

    fn copy_card(&self, index: usize, cx: &Context<Self>) -> Panel {
        let state = &self.state;
        let Some(settings) = state.read(cx).projects.get(index) else {
            return Panel::new();
        };

        settings
            .copy_files
            .iter()
            .enumerate()
            .fold(Panel::new(), |panel, (file_index, file)| {
                let state = state.clone();

                panel.row(removable_row(
                    ("copy-remove", file_index),
                    file.clone().into(),
                    format!("Stop copying {file}"),
                    move |_, _, cx| {
                        state.update(cx, |state, cx| {
                            state.remove_copy_file(index, file_index, cx);
                        });
                    },
                ))
            })
            .row(add_row(
                "add-file",
                "Add file",
                {
                    let state = state.clone();
                    move |_, _, cx| state.update(cx, |state, cx| state.add_copy_file(index, cx))
                },
                cx,
            ))
    }

    fn checks_card(&self, index: usize, cx: &Context<Self>) -> Panel {
        let state = &self.state;
        let (Some(settings), Some(inputs)) = (
            state.read(cx).projects.get(index),
            self.inputs.projects.get(index),
        ) else {
            return Panel::new();
        };

        settings
            .checks
            .iter()
            .zip(&inputs.checks)
            .enumerate()
            .fold(Panel::new(), |panel, (check_index, (check, input))| {
                let row = plain_row()
                    .child(div().flex_1().min_w_0().child(input.clone()))
                    .child(
                        IconButton::new(
                            ("check-remove", check_index),
                            IconName::Close,
                            format!("Remove {check}"),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_check(index, check_index, cx);
                        })),
                    );

                panel.row(row)
            })
            .row(add_row(
                "add-check",
                "Add check",
                cx.listener(move |this, _, _, cx| this.add_check(index, cx)),
                cx,
            ))
            .row(switch_row(
                state,
                ("checks-required", index),
                "Required to pass",
                settings.checks_required,
                move |state, on| {
                    if let Some(project) = state.projects.get_mut(index) {
                        project.checks_required = on;
                    }
                },
            ))
            .row(
                PanelRow::new("Git pre-push hook").end_slot(
                    h_flex()
                        .gap_2()
                        .child(
                            Label::new("lefthook")
                                .mono()
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(StatusPill::new(AgentStatus::Done).label("Detected")),
                ),
            )
    }
}
