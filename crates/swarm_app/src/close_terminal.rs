//! The Close terminal dialog: asks before a user terminal with a running program closes.

use gpui::{
    AppContext as _, Context, EventEmitter, FocusHandle, Role, SharedString, Window, div, px,
};
use swarm_ui::prelude::*;

use crate::model::{TerminalId, TerminalKind};
use crate::palette::Overlay;
use crate::workspace::Workspace;

const INFO: &str = "The program in it stops.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseTerminalEvent {
    Dismissed,
    Confirmed {
        /// "Do not show this again" was on.
        stop_asking: bool,
    },
}

/// The open dialog. It reports what the user chose and leaves closing to the workspace.
pub(crate) struct CloseTerminalDialog {
    terminal: TerminalId,
    title: SharedString,
    stop_asking: bool,
    focus_handle: FocusHandle,
    cancel_focus: FocusHandle,
    /// What had the focus before the dialog opened.
    pub(crate) opener: Option<FocusHandle>,
}

impl EventEmitter<CloseTerminalEvent> for CloseTerminalDialog {}

impl gpui::Focusable for CloseTerminalDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CloseTerminalDialog {
    pub(crate) fn new(
        terminal: TerminalId,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let opener = window.focused(cx);
        let cancel_focus = cx.focus_handle().tab_stop(true);
        window.focus(&cancel_focus, cx);

        Self {
            terminal,
            title,
            stop_asking: false,
            focus_handle: cx.focus_handle(),
            cancel_focus,
            opener,
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(CloseTerminalEvent::Confirmed {
            stop_asking: self.stop_asking,
        });
    }

    fn heading(&self) -> String {
        format!("Close terminal \u{201c}{}\u{201d}?", self.title)
    }

    #[cfg(test)]
    fn cancel_focus(&self) -> &FocusHandle {
        &self.cancel_focus
    }
}

impl Render for CloseTerminalDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();
        let on_toggle = cx.entity();

        let info = div()
            .id("close-terminal-info")
            .role(Role::Label)
            .aria_label(INFO)
            .child(Label::new(INFO).color(Color::Muted));

        let skip = h_flex()
            .debug_selector(|| "close-terminal-skip".to_owned())
            .child(
                Checkbox::new("close-terminal-skip", "Do not show this again")
                    .toggle_state(self.stop_asking)
                    .on_toggle(move |on, _, cx| {
                        on_toggle.update(cx, |dialog, cx| {
                            dialog.stop_asking = on;
                            cx.notify();
                        });
                    }),
            );

        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("close-terminal-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .track_focus(&self.cancel_focus)
                    .debug_selector(|| "close-terminal-cancel".to_owned())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseTerminalEvent::Dismissed))),
            )
            .child(
                Button::new("close-terminal-confirm", "Close terminal")
                    .variant(ButtonVariant::Destructive)
                    .accessible_label(format!("Close terminal {}", self.title))
                    .debug_selector(|| "close-terminal-confirm".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
            );

        Dialog::new("close-terminal-dialog", self.heading(), &self.focus_handle)
            .width(px(420.))
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, |_, cx| cx.emit(CloseTerminalEvent::Dismissed));
            })
            .child(info)
            .child(skip)
            .footer(footer)
    }
}

impl Workspace {
    /// Whether the program in the terminal still runs.
    fn terminal_runs(&self, terminal: TerminalId, cx: &App) -> bool {
        match self.live_terminals.get(&terminal) {
            Some(live) => live.view.read(cx).exit_status().is_none(),
            None => self
                .data
                .terminal(terminal)
                .is_some_and(|entry| entry.exit.is_none()),
        }
    }

    /// A close the user started. A terminal whose program runs asks first, unless the
    /// settings say not to. Closes the app or the daemon starts call `close_user_terminal`.
    pub(crate) fn request_close_terminal(
        &mut self,
        terminal: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self
            .data
            .terminal(terminal)
            .filter(|entry| entry.kind == TerminalKind::User)
            .map(|entry| entry.title.clone());
        let Some(title) = title else {
            return;
        };

        if !crate::settings::confirm_terminal_close(cx) || !self.terminal_runs(terminal, cx) {
            self.close_user_terminal(terminal, cx);
            return;
        }

        let view = cx.new(|cx| CloseTerminalDialog::new(terminal, title, window, cx));
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                move |this, _, event, window, cx| match event {
                    CloseTerminalEvent::Dismissed => {
                        this.close_overlay(window, cx);

                        if this.data.terminal(terminal).is_some() {
                            this.claim_focus(terminal, cx);
                        }
                    }
                    CloseTerminalEvent::Confirmed { stop_asking } => {
                        this.close_overlay(window, cx);

                        if *stop_asking {
                            crate::settings::stop_confirming_terminal_close(cx);
                        }
                        this.close_user_terminal(terminal, cx);
                    }
                },
            );

        self.overlay = Some(Overlay::CloseTerminal {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// The terminal is gone: its dialog has nothing left to ask.
    pub(crate) fn drop_close_terminal_dialog(&mut self, terminal: TerminalId, cx: &App) {
        let asks_about_it = matches!(
            &self.overlay,
            Some(Overlay::CloseTerminal { view, .. }) if view.read(cx).terminal == terminal
        );

        if asks_about_it {
            self.overlay = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use swarm_term::ExitStatus;

    use super::*;
    use crate::actions;

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();

        (workspace, cx)
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    /// Opens a fake user terminal in the open task and returns it.
    fn open_terminal(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> TerminalId {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.new_user_terminal(window, cx);
        });
        redraw(cx);

        workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .user_terminals
                .last()
                .copied()
                .expect("a user terminal was opened")
        })
    }

    fn ask(workspace: &Entity<Workspace>, terminal: TerminalId, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.request_close_terminal(terminal, window, cx);
        });
        redraw(cx);
    }

    fn dialog(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<Entity<CloseTerminalDialog>> {
        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::CloseTerminal { view, .. }) => Some(view.clone()),
            _ => None,
        })
    }

    fn is_open(
        workspace: &Entity<Workspace>,
        terminal: TerminalId,
        cx: &VisualTestContext,
    ) -> bool {
        workspace.read_with(cx, |workspace, _| {
            workspace.active_task().user_terminals.contains(&terminal)
        })
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));

        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
    }

    /// Clicks the checkbox at the left end of the row `selector` names.
    fn click_on_the_box(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        let on_the_box = bounds.origin + gpui::point(px(8.), bounds.size.height / 2.);

        cx.simulate_click(on_the_box, Modifiers::none());
        redraw(cx);
    }

    fn turn_off_the_question(cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            crate::settings::state(cx).update(cx, |state, _| {
                state.settings.terminals.confirm_close = false;
            });
        });
    }

    #[gpui::test]
    fn closing_a_terminal_whose_program_runs_asks_first(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);

        ask(&workspace, terminal, cx);

        assert!(dialog(&workspace, cx).is_some());
        assert!(is_open(&workspace, terminal, cx));
    }

    #[gpui::test]
    fn the_dialog_names_the_terminal_and_focus_starts_on_cancel(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);

        ask(&workspace, terminal, cx);

        let dialog = dialog(&workspace, cx).expect("the dialog is open");
        let heading = dialog.read_with(cx, |dialog, _| dialog.heading());
        let cancel = dialog.read_with(cx, |dialog, _| dialog.cancel_focus().clone());
        assert_eq!(heading, "Close terminal \u{201c}zsh\u{201d}?");
        assert!(cx.update(|window, _| cancel.is_focused(window)));
    }

    #[gpui::test]
    fn cancel_leaves_the_terminal_open(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);

        click("close-terminal-cancel", cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(is_open(&workspace, terminal, cx));
    }

    #[gpui::test]
    fn escape_leaves_the_terminal_open(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);

        cx.simulate_keystrokes("escape");
        redraw(cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(is_open(&workspace, terminal, cx));
    }

    #[gpui::test]
    fn closing_the_terminal_gives_the_focus_back_to_what_had_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        let opener = workspace
            .read_with(cx, |workspace, _| {
                workspace.project_focus.values().next().cloned()
            })
            .expect("a project has a row");
        cx.update(|window, cx| window.focus(&opener, cx));
        redraw(cx);
        ask(&workspace, terminal, cx);

        click("close-terminal-confirm", cx);

        assert!(cx.update(|window, _| opener.is_focused(window)));
    }

    #[gpui::test]
    fn close_terminal_closes_it_and_keeps_asking(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);

        click("close-terminal-confirm", cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(!is_open(&workspace, terminal, cx));
        assert!(cx.update(|_, cx| crate::settings::confirm_terminal_close(cx)));
    }

    #[gpui::test]
    fn do_not_show_this_again_turns_the_question_off_and_closes(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);

        click_on_the_box("close-terminal-skip", cx);
        click("close-terminal-confirm", cx);

        assert!(!is_open(&workspace, terminal, cx));
        assert!(!cx.update(|_, cx| crate::settings::confirm_terminal_close(cx)));
    }

    #[gpui::test]
    fn cancel_after_do_not_show_this_again_keeps_asking(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);

        click_on_the_box("close-terminal-skip", cx);
        click("close-terminal-cancel", cx);

        assert!(is_open(&workspace, terminal, cx));
        assert!(cx.update(|_, cx| crate::settings::confirm_terminal_close(cx)));
    }

    #[gpui::test]
    fn without_the_question_a_terminal_closes_at_once(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        turn_off_the_question(cx);

        ask(&workspace, terminal, cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(!is_open(&workspace, terminal, cx));
    }

    #[gpui::test]
    fn a_terminal_whose_program_ended_closes_at_once(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        workspace.update(cx, |workspace, _| {
            workspace
                .data
                .set_terminal_exit(terminal, ExitStatus::Code(0));
        });

        ask(&workspace, terminal, cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(!is_open(&workspace, terminal, cx));
    }

    #[gpui::test]
    fn the_dialog_goes_when_the_terminal_closes_by_itself(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);

        workspace.update(cx, |workspace, cx| {
            workspace.close_user_terminal(terminal, cx);
        });
        redraw(cx);

        assert!(dialog(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn the_dialog_goes_and_saves_nothing_when_its_task_closes(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        ask(&workspace, terminal, cx);
        click_on_the_box("close-terminal-skip", cx);

        workspace.update(cx, |workspace, cx| {
            let task = workspace.active_task().id;
            workspace.remove_closed_task(task, cx);
        });
        redraw(cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(cx.update(|_, cx| crate::settings::confirm_terminal_close(cx)));
    }

    #[gpui::test]
    fn cancel_from_the_sidebar_button_gives_the_focus_back_to_the_terminal(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);
        click("sidebar-close-terminal-14", cx);

        click("close-terminal-cancel", cx);

        let handle = workspace.read_with(cx, |workspace, _| {
            workspace.terminal_focus.get(&terminal).cloned()
        });
        let handle = handle.expect("the terminal has a focus handle");
        assert!(cx.update(|window, _| handle.is_focused(window)));
    }

    #[gpui::test]
    fn the_close_button_of_the_tile_asks_first(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = open_terminal(&workspace, cx);

        assert_eq!(terminal, TerminalId(14));
        click("tile-close-terminal-14", cx);

        assert!(dialog(&workspace, cx).is_some());
        assert!(is_open(&workspace, terminal, cx));
    }
}
