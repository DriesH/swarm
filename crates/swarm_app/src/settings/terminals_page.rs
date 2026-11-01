//! Settings › Terminals: the font size of every terminal.

use gpui::{AnyElement, px};
use swarm_settings::{FONT_SIZE_DEFAULT, FONT_SIZE_MAX, FONT_SIZE_MIN, clamp_font_size};
use swarm_ui::prelude::*;

use super::SettingsWindow;
use super::pages::{page_title, save_error, section};

impl SettingsWindow {
    pub(super) fn terminals_page(&self, cx: &Context<Self>) -> AnyElement {
        let state = self.state.clone();
        let settings = state.read(cx);
        let points = settings.settings.terminals.font_size;

        let stepper = Stepper::new(
            "font-size",
            "Font size",
            u32::from(points),
            u32::from(FONT_SIZE_MIN),
            u32::from(FONT_SIZE_MAX),
        )
        .unit("pt")
        .debug_selector(|| "font-size".to_owned())
        .on_change(move |value, _, cx| {
            let points = u8::try_from(value).unwrap_or(FONT_SIZE_DEFAULT);

            state.update(cx, |state, cx| {
                state.settings.terminals.font_size = clamp_font_size(points);
                cx.notify();
            });
        });

        let font = Panel::new().row(PanelRow::new("Font size").end_slot(stepper));
        let error = save_error(settings, cx);

        v_flex()
            .gap(px(26.))
            .child(page_title("Terminals"))
            .children(error)
            .child(section("Font", font))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::rc::Rc;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
    use swarm_protocol::TerminalId as DaemonTerminalId;
    use swarm_term::{EmulatorChannels, TerminalInput};

    use super::super::{
        Page, SAVE_DELAY, SettingsWindow, follow_font_size, open_window, set_terminal_font_size,
        terminal_font_size,
    };
    use crate::daemon::tests::{redraw, setup};
    use crate::model::TerminalId;
    use crate::views::layout_style;
    use crate::workspace::Workspace;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "swarm-terminals-{name}-{}.toml",
            std::process::id()
        ))
    }

    fn click(selector: &'static str, visual: &mut VisualTestContext) {
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));

        visual.simulate_click(bounds.center(), Modifiers::none());
        visual.run_until_parked();
    }

    fn terminals_page_at(path: &std::path::Path, cx: &mut TestAppContext) -> VisualTestContext {
        cx.update(swarm_ui::init);
        let window = cx
            .update(|cx| open_window(Some(path.to_path_buf()), None, cx))
            .expect("window opens");
        window
            .update(cx, |settings: &mut SettingsWindow, _, cx| {
                settings.select_page(Page::Terminals, cx);
            })
            .expect("window is open");

        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1280.), px(900.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));

        visual
    }

    fn save_pause(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_stepper_saves_the_font_size(cx: &mut TestAppContext) {
        let path = temp_path("stepper");
        let mut visual = terminals_page_at(&path, cx);

        click("font-size-increase", &mut visual);
        click("font-size-increase", &mut visual);
        click("font-size-decrease", &mut visual);
        save_pause(&mut visual);

        let saved = swarm_settings::load(&path).expect("read the settings");
        let text = std::fs::read_to_string(&path).expect("read the file");
        std::fs::remove_file(&path).ok();
        assert_eq!(saved.terminals.font_size, 14);
        assert!(text.contains("font_size = 14"), "{text}");
    }

    #[gpui::test]
    fn the_stepper_stops_at_the_ends_of_the_range(cx: &mut TestAppContext) {
        let path = temp_path("limits");
        let mut saved = swarm_settings::Settings::default();
        saved.terminals.font_size = 20;
        swarm_settings::save(&path, &saved).expect("write the settings");
        let mut visual = terminals_page_at(&path, cx);

        click("font-size-increase", &mut visual);
        save_pause(&mut visual);

        let after = swarm_settings::load(&path).expect("read the settings");
        std::fs::remove_file(&path).ok();
        assert_eq!(after.terminals.font_size, 20);
    }

    #[gpui::test]
    fn the_page_starts_at_the_saved_font_size(cx: &mut TestAppContext) {
        let path = temp_path("saved");
        let mut saved = swarm_settings::Settings::default();
        saved.terminals.font_size = 17;
        swarm_settings::save(&path, &saved).expect("write the settings");
        let mut visual = terminals_page_at(&path, cx);

        click("font-size-decrease", &mut visual);
        save_pause(&mut visual);

        let after = swarm_settings::load(&path).expect("read the settings");
        std::fs::remove_file(&path).ok();
        assert_eq!(after.terminals.font_size, 16);
    }

    /// A live terminal on each kind of tile of the open task: the orchestrator, a worker, a
    /// process and the user's own. Returns what each view sent to its emulator.
    fn attach_every_kind(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Vec<(&'static str, TerminalId, EmulatorChannels)> {
        // The user's terminal changes the layout, so the process tile is picked after it.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.new_user_terminal(window, cx);
        });
        redraw(cx);

        workspace.update_in(cx, |workspace, window, cx| {
            let task = workspace.active_task();
            let terminal_of = |agent| workspace.data.agent(agent).map(|agent| agent.terminal);
            let kinds = [
                ("orchestrator", task.orchestrator.and_then(terminal_of)),
                (
                    "worker",
                    task.workers.first().copied().and_then(terminal_of),
                ),
                (
                    "process",
                    task.processes
                        .iter()
                        .copied()
                        .find(|id| {
                            workspace
                                .layout
                                .rect(swarm_layout::TileId(id.0))
                                .is_some_and(|rect| rect.size.width > 0.)
                        })
                        .or_else(|| task.processes.first().copied()),
                ),
                ("user", task.user_terminals.first().copied()),
            ];

            let mut attached = Vec::new();
            for (index, (kind, terminal)) in kinds.into_iter().enumerate() {
                let terminal = terminal.unwrap_or_else(|| panic!("the sample has a {kind}"));
                let (channels, emulator) = swarm_term::channels();
                let live = workspace.live_terminal(
                    terminal,
                    DaemonTerminalId(9_000 + index as u64),
                    kind.into(),
                    channels,
                    window,
                    cx,
                );
                workspace.live_terminals.insert(terminal, live);
                attached.push((kind, terminal, emulator));
            }

            attached
        })
    }

    fn last_grid(kind: &str, emulator: &EmulatorChannels) -> swarm_term::Size {
        emulator
            .input
            .try_iter()
            .filter_map(|input| match input {
                TerminalInput::Resize(size) => Some(size),
                _ => None,
            })
            .last()
            .unwrap_or_else(|| panic!("the {kind} view told its emulator no grid"))
    }

    fn font_sizes(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<f32> {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .live_terminals
                .values()
                .map(|live| live.view.read(cx).font_size())
                .collect()
        })
    }

    #[gpui::test]
    fn a_new_font_size_reaches_every_terminal_view(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        main.simulate_resize(size(px(1800.), px(2000.)));
        attach_every_kind(&workspace, main);
        redraw(main);
        let before = font_sizes(&workspace, main);

        main.update(|_, cx| set_terminal_font_size(17, cx));
        redraw(main);
        let after = font_sizes(&workspace, main);

        assert_eq!(before, [13.; 4]);
        assert_eq!(after, [17.; 4]);
    }

    #[gpui::test]
    fn tiles_keep_their_pixels_and_get_fewer_rows_and_columns(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        main.simulate_resize(size(px(1800.), px(2000.)));
        redraw(main);
        let attached = attach_every_kind(&workspace, main);
        redraw(main);
        let before: Vec<_> = attached
            .iter()
            .map(|(kind, _, emulator)| (*kind, last_grid(kind, emulator)))
            .collect();
        let tiles: Vec<_> = workspace.read_with(main, |workspace, _| {
            attached
                .iter()
                .map(|(_, terminal, _)| {
                    workspace
                        .layout
                        .rect(swarm_layout::TileId(terminal.0))
                        .map(|rect| rect.size.width)
                })
                .collect()
        });

        main.update(|_, cx| set_terminal_font_size(18, cx));
        redraw(main);
        let after: Vec<_> = attached
            .iter()
            .map(|(kind, _, emulator)| (*kind, last_grid(kind, emulator)))
            .collect();
        let widths: Vec<_> = workspace.read_with(main, |workspace, _| {
            attached
                .iter()
                .map(|(_, terminal, _)| {
                    workspace
                        .layout
                        .rect(swarm_layout::TileId(terminal.0))
                        .map(|rect| rect.size.width)
                })
                .collect()
        });

        // The orchestrator and the user's terminal keep their height too: they lose rows.
        for ((kind, small), (_, large)) in before.iter().zip(&after) {
            assert!(large.cols < small.cols, "{kind}: {small:?} {large:?}");
            assert!(large.cell_height > small.cell_height, "{kind}");
        }
        for kind in ["orchestrator", "user"] {
            let rows = |grids: &[(&str, swarm_term::Size)]| {
                grids
                    .iter()
                    .find(|(name, _)| *name == kind)
                    .map(|(_, size)| size.rows)
            };
            assert!(rows(&after) < rows(&before), "{kind} kept its rows");
        }
        assert_eq!(tiles, widths);
    }

    #[gpui::test]
    fn the_cell_based_sizes_of_the_layout_follow_the_new_cell(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        redraw(main);
        let (cell, small, process) = workspace.read_with(main, |workspace, _| {
            let style = layout_style(workspace);

            (
                workspace.cell_size,
                style.small_tile_height,
                style.process_tile_height,
            )
        });

        main.update(|_, cx| set_terminal_font_size(18, cx));
        redraw(main);
        let (larger_cell, larger_small, larger_process) =
            workspace.read_with(main, |workspace, _| {
                let style = layout_style(workspace);

                (
                    workspace.cell_size,
                    style.small_tile_height,
                    style.process_tile_height,
                )
            });

        let height = |cell: Option<(f32, f32)>| cell.map(|(_, height)| height).unwrap_or_default();
        assert!(height(larger_cell) > height(cell));
        assert!(larger_small > small + 20.);
        assert!(larger_process > process + 10.);
    }

    #[gpui::test]
    fn a_terminal_made_later_starts_at_the_current_font_size(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        main.update(|_, cx| set_terminal_font_size(19, cx));

        attach_every_kind(&workspace, main);

        assert_eq!(font_sizes(&workspace, main), [19.; 4]);
    }

    #[gpui::test]
    fn only_a_font_size_change_redraws_the_workspace(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        main.update(|_, cx| follow_font_size(workspace.clone(), cx));
        let redraws = Rc::new(std::cell::Cell::new(0_u32));
        let counted = Rc::clone(&redraws);
        let _watch =
            main.update(|_, cx| cx.observe(&workspace, move |_, _| counted.set(counted.get() + 1)));
        main.run_until_parked();
        let start = redraws.get();

        main.update(|_, cx| {
            super::super::change_notifications(|notifications| notifications.sound = true, cx);
        });
        main.run_until_parked();
        let after_other = redraws.get();

        main.update(|_, cx| set_terminal_font_size(15, cx));
        main.run_until_parked();
        let after_font = redraws.get();

        assert_eq!(after_other, start);
        assert!(after_font > after_other);
        assert!((main.update(|_, cx| terminal_font_size(cx)) - 15.).abs() < f32::EPSILON);
    }
}
