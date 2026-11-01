//! One render function per settings page.

use std::path::Path;

use gpui::{AnyElement, ElementId, Entity, FontWeight, Role, SharedString, Stateful, div, px};
use swarm_settings::{AccentSetting, AgentKindSetting, ProjectMode};
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::theme::{self, Accent, Appearance};

use super::open_file;
use super::project::{List, Load, ProjectSettings};
use super::{
    AGENTS, DELIVERY, IDLE_OPTIONS, NOTIFY_WHEN, PROJECT_MODES, PROJECT_NOTIFICATIONS, Page,
    SettingsState, SettingsWindow, accent_of, delivery, delivery_mut, mode_index, notify_when,
    notify_when_mut,
};
use crate::notifications::{self, Permission};

type State = Entity<SettingsState>;

pub(super) fn apply_appearance(index: usize, cx: &mut App) {
    if let Some(appearance) = Appearance::ALL.get(index) {
        theme::set_appearance(*appearance, cx);
    }
}

fn accent_index(accent: AccentSetting) -> usize {
    match accent {
        AccentSetting::Blue => 0,
        AccentSetting::Honey => 1,
    }
}

fn accent_setting(index: usize) -> AccentSetting {
    match index {
        1 => AccentSetting::Honey,
        _ => AccentSetting::Blue,
    }
}

/// Saves the accent and applies it at once, like the theme control does.
pub(super) fn apply_accent(state: &State) -> impl Fn(usize, &mut Window, &mut App) + 'static {
    let state = state.clone();

    move |index, _, cx| {
        let accent = accent_setting(index);

        state.update(cx, |state, cx| {
            state.settings.appearance.accent = accent;
            cx.notify();
        });
        theme::set_accent(accent_of(accent), cx);
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

pub(super) fn page_title(title: impl Into<SharedString>) -> impl IntoElement {
    Label::new(title)
        .size(LabelSize::Title)
        .weight(FontWeight::SEMIBOLD)
}

/// A labeled group: a small heading above a card.
pub(super) fn section(heading: &'static str, body: impl IntoElement) -> impl IntoElement {
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

pub(super) fn plain_row() -> gpui::Div {
    h_flex().gap_3().min_h(px(46.)).px(px(14.))
}

/// The line that says why the settings file could not be read or written, in red.
pub(super) fn save_error(state: &SettingsState, cx: &App) -> Option<impl IntoElement> {
    state.save_error.clone().map(|message| {
        div()
            .id("settings-save-error")
            .role(Role::Alert)
            .aria_label(message.clone())
            .text_size(px(12.))
            .text_color(Color::Del.hsla(cx))
            .child(message)
    })
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
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<gpui::Div> {
    let colors = cx.theme().colors();
    let (accent, raised) = (colors.accent, colors.raised);

    let row = h_flex()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .debug_selector(|| id.to_owned())
        .gap(px(6.))
        .h(px(40.))
        .px(px(14.));

    let row = if enabled {
        row.focusable()
            .tab_stop(true)
            .cursor_pointer()
            .hover(move |style| style.bg(raised))
            .focus_visible(move |style| focus_ring(style, accent))
            .on_click(on_click)
    } else {
        row.opacity(0.5)
    };

    row.child(
        Icon::new(IconName::Plus)
            .size(IconSize::Small)
            .color(Color::Muted),
    )
    .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
}

/// A row of a list: its text field and a button that removes it.
fn list_row(
    input: &Entity<TextInput>,
    id: (&'static str, usize),
    remove_label: String,
    enabled: bool,
    on_remove: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    plain_row()
        .child(div().flex_1().min_w_0().child(input.clone()))
        .child(
            IconButton::new(id, IconName::Close, remove_label)
                .debug_selector(move || format!("{}-{}", id.0, id.1))
                .disabled(!enabled)
                .on_click(on_remove),
        )
}

/// The path with the home folder shortened to `~`.
fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME");

    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

impl SettingsWindow {
    pub(super) fn render_page(&self, cx: &Context<Self>) -> AnyElement {
        match self.page {
            Page::Agents => self.agents_page(cx),
            Page::Notifications => self.notifications_page(cx),
            Page::Appearance => self.appearance_page(cx),
            Page::Project(index) => self.project_page(index, cx),
            Page::Terminals => self.terminals_page(cx),
            Page::Prompts => self.prompts.clone().into_any_element(),
            Page::Usage => self.usage.clone().into_any_element(),
            Page::Keyboard => Self::keyboard_page(),
            Page::General => self.general_page(cx),
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

    fn appearance_page(&self, cx: &Context<Self>) -> AnyElement {
        let state = &self.state;
        let settings = state.read(cx).settings.appearance;

        let selected = Appearance::ALL
            .iter()
            .position(|appearance| *appearance == theme::appearance(cx))
            .unwrap_or(0);

        let control = SegmentedControl::new(
            "appearance",
            "Theme",
            Appearance::ALL.map(Appearance::label),
        )
        .debug_selector(|| "theme".to_owned())
        .selected(selected)
        .on_select(|index, _, cx| apply_appearance(index, cx));

        let accent = SegmentedControl::new("accent", "Accent", Accent::ALL.map(Accent::label))
            .debug_selector(|| "accent".to_owned())
            .selected(accent_index(settings.accent))
            .on_select(apply_accent(state));

        let eggs = Panel::new()
            .row(
                PanelRow::new("Easter eggs").end_slot(
                    Switch::new("easter-eggs", "Easter eggs")
                        .debug_selector(|| "easter-eggs".to_owned())
                        .toggle_state(settings.easter_eggs)
                        .on_toggle(toggle(state, |state, on| {
                            state.settings.appearance.easter_eggs = on;
                        })),
                ),
            )
            .row(
                PanelRow::new("Honeycomb behind the Swarm view").end_slot(
                    Switch::new("swarm-view-honeycomb", "Honeycomb behind the Swarm view")
                        .debug_selector(|| "swarm-view-honeycomb".to_owned())
                        .toggle_state(settings.swarm_view_honeycomb && settings.easter_eggs)
                        .disabled(!settings.easter_eggs)
                        .on_toggle(toggle(state, |state, on| {
                            state.settings.appearance.swarm_view_honeycomb = on;
                        })),
                ),
            );

        v_flex()
            .gap(px(26.))
            .child(page_title("Appearance"))
            .child(section(
                "Theme",
                Panel::new().row(PanelRow::new("Theme").end_slot(control)),
            ))
            .child(section(
                "Accent",
                Panel::new().row(PanelRow::new("Accent").end_slot(accent)),
            ))
            .child(section("Easter eggs", eggs))
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

        let editable = settings.load.editable();
        let loading = settings.load == Load::Loading;

        let header = Self::project_header(settings, cx);

        let unreadable = match &settings.load {
            Load::Unreadable(message) => Some(Self::unreadable(index, message.clone(), cx)),
            _ => None,
        };

        let (branches, selected): (Vec<SharedString>, usize) = if loading {
            (vec!["Loading\u{2026}".into()], 0)
        } else {
            (
                settings
                    .base_branch_options()
                    .into_iter()
                    .map(SharedString::from)
                    .collect(),
                settings.base_branch_index(),
            )
        };

        let base_branch = PanelRow::new("Base branch").end_slot(
            h_flex().h(px(28.)).child(
                Dropdown::new(("base-branch", index), "Base branch", branches, selected)
                    .debug_selector(|| "base-branch".to_owned())
                    .disabled(!editable)
                    .on_change(choose(state, move |state, option| {
                        state.choose_base_branch(index, option);
                    })),
            ),
        );

        let worktrees = Panel::new()
            .row(base_branch)
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

        let copy = self.list_card(index, List::Copy, cx);
        let checks = self.checks_card(index, cx);
        let prompts = self.prompts_card(index, cx);

        let run_checks = self.run_checks_row(index, cx);

        v_flex()
            .gap(px(26.))
            .child(header)
            .children(unreadable)
            .child(section("Worktrees", worktrees))
            .child(section("Setup script", setup))
            .child(section("Copy into worktrees", copy))
            .child(
                v_flex()
                    .child(section("Checks before PR", checks))
                    .child(run_checks),
            )
            .children(prompts.map(|prompts| {
                div()
                    .id("project-prompts")
                    .debug_selector(|| "project-prompts".to_owned())
                    .child(section("Prompts", prompts))
            }))
            .child(
                h_flex().child(
                    Button::new(("remove-project", index), "Remove project\u{2026}")
                        .variant(ButtonVariant::SecondaryDestructive)
                        .debug_selector(|| "remove-project".to_owned())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_remove_project(index, window, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    /// **Run checks now** and the short result of the run.
    fn run_checks_row(&self, index: usize, cx: &Context<Self>) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let can_run = state.read(cx).can_run_checks(index, cx);
        let result = state
            .read(cx)
            .projects
            .get(index)
            .and_then(|settings| settings.checking.pill());

        let button = Button::new("run-checks", "Run checks now")
            .debug_selector(|| "run-checks".to_owned())
            .disabled(!can_run)
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| state.run_checks(index, cx));
            });
        let result = result.map(|(tone, text)| {
            div()
                .id("run-checks-result")
                .role(Role::Status)
                .aria_label(text.clone())
                .debug_selector(|| "run-checks-result".to_owned())
                .child(StatusPill::new(tone).label(text))
        });

        h_flex().mt(px(12.)).gap_3().child(button).children(result)
    }

    /// The project's tile, name and path, and next to the name what the page is doing.
    fn project_header(settings: &ProjectSettings, cx: &App) -> AnyElement {
        let location = settings.project.root.as_deref().map_or_else(
            || ".swarm/config.toml".to_owned(),
            |root| format!("{} \u{b7} .swarm/config.toml", tilde(root)),
        );

        let status: Option<AnyElement> = if settings.load == Load::Loading {
            Some(
                StatusPill::new(AgentStatus::Working)
                    .label("Loading")
                    .into_any_element(),
            )
        } else {
            settings.refusal.clone().map(|message| {
                div()
                    .id("project-refusal")
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .debug_selector(|| "project-refusal".to_owned())
                    .min_w_0()
                    .text_size(px(12.))
                    .text_color(Color::Del.hsla(cx))
                    .child(message)
                    .into_any_element()
            })
        };

        h_flex()
            .gap_3()
            .child(
                ProjectTile::new(settings.project.letter, settings.project.tile_color)
                    .size(px(28.)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(page_title(settings.project.name.clone()))
                            .children(status),
                    )
                    .child(
                        Label::new(location)
                            .mono()
                            .size(LabelSize::XSmall)
                            .color(Color::Faint),
                    ),
            )
            .into_any_element()
    }

    /// The error that makes the page read-only, and the button that reads the file again.
    fn unreadable(index: usize, message: SharedString, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();

        h_flex()
            .id("config-error")
            .role(Role::Alert)
            .aria_label(message.clone())
            .debug_selector(|| "config-error".to_owned())
            .gap_3()
            .px(px(14.))
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(colors.line)
            .bg(colors.del_bg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(message).size(LabelSize::Small).color(Color::Del)),
            )
            .child(
                Button::new(("reload-config", index), "Reload")
                    .debug_selector(|| "reload-config".to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| this.load_project(index, cx))),
            )
            .into_any_element()
    }

    /// The files to copy into worktrees.
    fn list_card(&self, index: usize, list: List, cx: &Context<Self>) -> Panel {
        let (Some(settings), Some(inputs)) = (
            self.state.read(cx).projects.get(index),
            self.inputs.projects.get(index),
        ) else {
            return Panel::new();
        };
        let editable = settings.load.editable();

        settings
            .copy_files
            .iter()
            .zip(&inputs.copy)
            .enumerate()
            .fold(Panel::new(), |panel, (file_index, (file, row))| {
                let remove = if file.trim().is_empty() {
                    "Remove empty row".to_owned()
                } else {
                    format!("Stop copying {file}")
                };

                panel.row(list_row(
                    &row.input,
                    ("copy-remove", file_index),
                    remove,
                    editable,
                    cx.listener(move |this, _, _, cx| {
                        this.remove_item(index, list, file_index, cx);
                    }),
                ))
            })
            .row(add_row(
                "add-file",
                "Add file",
                editable,
                cx.listener(move |this, _, window, cx| this.add_item(index, list, window, cx)),
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
        let editable = settings.load.editable();

        let required = {
            let state = state.clone();

            Switch::new(("checks-required", index), "Required to pass")
                .toggle_state(settings.checks_required)
                .disabled(!editable)
                .on_toggle(move |on, _, cx| {
                    state.update(cx, |state, cx| {
                        if let Some(project) = state.projects.get_mut(index) {
                            project.checks_required = on;
                        }

                        cx.notify();
                    });
                })
        };

        let panel = settings
            .checks
            .iter()
            .zip(&inputs.checks)
            .enumerate()
            .fold(Panel::new(), |panel, (check_index, (check, row))| {
                let remove = if check.trim().is_empty() {
                    "Remove empty check".to_owned()
                } else {
                    format!("Remove {check}")
                };

                panel.row(list_row(
                    &row.input,
                    ("check-remove", check_index),
                    remove,
                    editable,
                    cx.listener(move |this, _, _, cx| {
                        this.remove_item(index, List::Checks, check_index, cx);
                    }),
                ))
            })
            .row(add_row(
                "add-check",
                "Add check",
                editable,
                cx.listener(move |this, _, window, cx| {
                    this.add_item(index, List::Checks, window, cx);
                }),
                cx,
            ))
            .row(PanelRow::new("Required to pass").end_slot(required));

        if settings.load == Load::Sample {
            panel.row(
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
        } else {
            panel
        }
    }

    /// The prompts the project overrides in `.swarm/prompts/`. `None` when there are none.
    fn prompts_card(&self, index: usize, cx: &Context<Self>) -> Option<Panel> {
        let settings = self.state.read(cx).projects.get(index)?;

        if settings.prompts.is_empty() {
            return None;
        }

        let panel = settings.prompts.iter().enumerate().fold(
            Panel::new(),
            |panel, (prompt_index, prompt)| {
                let path = prompt.path.clone();

                let open = Button::new(("open-prompt", prompt_index), "Open")
                    .accessible_label(format!("Open {}", prompt.name))
                    .debug_selector(move || format!("open-prompt-{prompt_index}"))
                    .on_click(move |_, _, cx| open_file(&path, cx));

                panel.row(
                    plain_row()
                        .child(
                            div().flex_1().min_w_0().child(
                                Label::new(format!("{}.md", prompt.name))
                                    .mono()
                                    .size(LabelSize::Small)
                                    .truncate(),
                            ),
                        )
                        .child(open),
                )
            },
        );

        Some(panel)
    }
}
