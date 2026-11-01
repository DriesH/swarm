//! The workspace outlives its window.
//!
//! Closing the main window keeps the workspace and its daemon connection, so the menu bar
//! stays live. The app then has no Dock icon, when the status item is there to bring it
//! back. **Open Swarm**, a Dock click and a second launch open a new window on the same
//! workspace and bring the Dock icon back.

use gpui::{ActivationPolicy, App, Entity, Global, QuitMode, WindowId};

use crate::workspace::Workspace;

struct Lifecycle {
    /// Held for the whole run, so the workspace is alive with no window.
    workspace: Entity<Workspace>,
    policy: ActivationPolicy,
    /// How often the app came to the front. The test platform has no front to read.
    #[cfg(test)]
    activations: usize,
}

impl Global for Lifecycle {}

/// Opens the first window and starts the menu bar.
pub(crate) fn start(cx: &mut App) -> anyhow::Result<()> {
    let handle = crate::open_workspace(cx)?;
    let workspace = handle.entity(cx)?;

    keep(workspace.clone(), cx);
    crate::settings::follow_font_size(workspace.clone(), cx);
    crate::settings::follow_appearance(workspace.clone(), cx);
    crate::menu_bar::install(workspace.clone(), cx);
    crate::notifications::install(workspace, cx);
    cx.activate(true);

    Ok(())
}

/// Holds `workspace` app-wide and watches its window. The app does not quit when its last
/// window closes.
pub(crate) fn keep(workspace: Entity<Workspace>, cx: &mut App) {
    cx.set_quit_mode(QuitMode::Explicit);
    cx.set_global(Lifecycle {
        workspace,
        policy: ActivationPolicy::Regular,
        #[cfg(test)]
        activations: 0,
    });
    cx.on_window_closed(window_closed).detach();
}

fn window_closed(cx: &mut App, window: WindowId) {
    let workspace = cx.global::<Lifecycle>().workspace.clone();
    let is_main = workspace
        .read(cx)
        .window
        .is_some_and(|main| main.window_id() == window);
    if !is_main {
        return;
    }

    workspace.update(cx, Workspace::window_closed);
    leave_dock(cx);
}

/// A Dock click or a second launch of the app, while no window is open.
pub(crate) fn reopened(cx: &mut App) {
    if cx.has_global::<Lifecycle>() {
        show_main_window(cx);
    }
}

/// Brings the main window back, or focuses it when it is open.
pub(crate) fn show_main_window(cx: &mut App) {
    let workspace = cx.global::<Lifecycle>().workspace.clone();

    let focused = workspace.read(cx).window.is_some_and(|window| {
        window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    });
    if focused {
        activate_later(cx);
        return;
    }

    set_policy(ActivationPolicy::Regular, cx);

    let opened = cx.open_window(crate::window_options(cx), |window, cx| {
        workspace.update(cx, |workspace, cx| workspace.attach_window(window, cx));

        workspace.clone()
    });

    match opened {
        Ok(_) => activate_later(cx),
        Err(error) => {
            log::error!("could not open the main window: {error:#}");
            leave_dock(cx);
        }
    }
}

/// Brings the app to the front after the current update. Activating calls back into GPUI,
/// which a click listener must not wait for.
fn activate_later(cx: &mut App) {
    cx.defer(|cx| {
        #[cfg(test)]
        {
            cx.global_mut::<Lifecycle>().activations += 1;
        }

        cx.activate(true);
    });
}

/// Drops the Dock icon, when the status item is another way back. Without it the Dock icon
/// stays: it is the only way to the window.
fn leave_dock(cx: &mut App) {
    if crate::menu_bar::has_status_item(cx) {
        set_policy(ActivationPolicy::Accessory, cx);
    }
}

fn set_policy(policy: ActivationPolicy, cx: &mut App) {
    cx.global_mut::<Lifecycle>().policy = policy;
    cx.set_activation_policy(policy);
}

/// The policy last set. The test platform has no Dock, so tests read it here.
#[cfg(test)]
pub(crate) fn policy(cx: &App) -> ActivationPolicy {
    cx.global::<Lifecycle>().policy
}

/// How often the app came to the front.
#[cfg(test)]
pub(crate) fn activations(cx: &App) -> usize {
    cx.global::<Lifecycle>().activations
}

#[cfg(test)]
mod tests {
    use gpui::{AnyWindowHandle, Focusable as _, TestAppContext, VisualTestContext, WeakEntity};
    use swarm_protocol::{AgentStatus as DaemonAgentStatus, Event, Request};
    use swarm_term::BlockedKind;

    use super::*;
    use crate::actions::ToggleCommandPalette;
    use crate::daemon::tests::{
        agent_info, fake_daemon_replying, info, real_task_workspace, wait_until,
    };
    use crate::menu_bar::harness;

    /// The sample workspace in a window, held by the lifecycle, with a status item in the
    /// menu bar.
    fn kept(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        harness::init(cx);
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        harness::install(&workspace, cx);

        (workspace, cx)
    }

    /// Like [`kept`], for an app whose status item could not be made.
    fn kept_without_status_item(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        harness::init(cx);
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        harness::install_without_status_item(&workspace, cx);

        (workspace, cx)
    }

    fn close_window(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    fn policy_now(cx: &VisualTestContext) -> ActivationPolicy {
        cx.read_global(|_: &Lifecycle, cx| policy(cx))
    }

    /// Open Swarm, from outside any window, as the popover does.
    fn open_swarm(cx: &mut VisualTestContext) {
        let app: &TestAppContext = cx;

        app.update(show_main_window);
        cx.run_until_parked();
    }

    /// A Dock click, from outside any window, as `AppKit` sends it.
    fn dock_click(cx: &mut VisualTestContext) {
        let app: &TestAppContext = cx;

        app.update(reopened);
        cx.run_until_parked();
    }

    /// A window for the workspace after the first one closed.
    fn second_window(cx: &VisualTestContext) -> VisualTestContext {
        let window = cx.windows().first().copied().expect("a window is open");

        VisualTestContext::from_window(window, cx)
    }

    /// A workspace with one user terminal attached through a fake daemon. The receiver
    /// gets the terminal of every detach request.
    fn with_a_terminal(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        &mut VisualTestContext,
        flume::Receiver<swarm_protocol::TerminalId>,
    ) {
        let listed = info(
            1,
            swarm_protocol::TerminalKind::User,
            "swarm/terminal-renderer",
        );
        let (on_detach, detaches) = flume::unbounded();
        let socket = fake_daemon_replying({
            let listed = listed.clone();

            move |request| match request {
                Request::List { call } => Some(Event::Terminals {
                    call,
                    terminals: vec![listed.clone()],
                }),
                Request::Detach { id } => {
                    if let Err(error) = on_detach.send(id) {
                        log::error!("the test stopped listening for detaches: {error}");
                    }

                    None
                }
                _ => None,
            }
        });
        let (workspace, cx) = kept(cx);
        cx.executor().allow_parking();
        let client = swarm_protocol::Client::connect(&socket).expect("connect to the fake daemon");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, std::slice::from_ref(&listed), window, cx);
        });

        (workspace, cx, detaches)
    }

    fn views(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<gpui::EntityId> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .live_terminals
                .values()
                .map(|live| live.view.entity_id())
                .collect()
        })
    }

    #[gpui::test]
    fn closing_the_window_keeps_the_workspace_and_drops_the_dock_icon(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        let weak: WeakEntity<Workspace> = workspace.downgrade();
        let before = policy_now(cx);
        drop(workspace);

        close_window(cx);

        assert_eq!(before, ActivationPolicy::Regular);
        assert!(cx.windows().is_empty());
        assert!(
            weak.upgrade().is_some(),
            "the workspace went with its window"
        );
        assert_eq!(policy_now(cx), ActivationPolicy::Accessory);
        let window_gone = weak
            .read_with(cx, |workspace, _| workspace.window.is_none())
            .expect("the workspace is alive");
        assert!(window_gone);
    }

    #[gpui::test]
    fn another_window_closing_changes_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        let other = cx.add_window(|_, _| gpui::Empty);

        cx.update(|_, cx| {
            other
                .update(cx, |_, window, _| window.remove_window())
                .expect("the other window is open");
        });
        cx.run_until_parked();

        assert_eq!(policy_now(cx), ActivationPolicy::Regular);
        assert!(workspace.read_with(cx, |workspace, _| workspace.window.is_some()));
    }

    #[gpui::test]
    fn open_swarm_opens_a_new_window_on_the_same_workspace(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        close_window(cx);

        open_swarm(cx);

        let windows = cx.windows();
        let root = windows
            .first()
            .and_then(AnyWindowHandle::downcast::<Workspace>)
            .map(|window| window.entity(cx).expect("the window has its root"));
        assert_eq!(windows.len(), 1);
        assert_eq!(
            root.map(|root| root.entity_id()),
            Some(workspace.entity_id())
        );
        assert_eq!(policy_now(cx), ActivationPolicy::Regular);
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.window),
            windows.first().copied()
        );
    }

    #[gpui::test]
    fn open_swarm_with_the_window_open_opens_no_second_window(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        let before = workspace.read_with(cx, |workspace, _| workspace.window);

        open_swarm(cx);

        assert_eq!(cx.windows().len(), 1);
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.window),
            before
        );
    }

    #[gpui::test]
    fn the_workspace_keeps_its_state_across_a_close_and_a_new_window(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.select_task(crate::model::TaskId(1), cx);
        });
        close_window(cx);

        open_swarm(cx);

        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        assert_eq!(active, crate::model::TaskId(1));
    }

    #[gpui::test]
    fn the_new_window_draws_the_workspace_and_focuses_it(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        workspace.update(cx, |workspace, _| workspace.focused_terminal = None);
        close_window(cx);
        open_swarm(cx);

        let mut window = second_window(cx);
        window.update(|window, cx| window.draw(cx).clear(cx));
        let focused = window.update(|window, cx| {
            workspace
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        });

        assert!(window.debug_bounds("project-row-0").is_some());
        assert!(focused, "nothing has focus, so Tab goes nowhere");
    }

    #[gpui::test]
    fn the_new_window_gets_the_activation_observer_of_the_workspace(cx: &mut TestAppContext) {
        let (workspace, cx) = kept(cx);
        workspace.update(cx, |workspace, _| workspace.fixed_now = None);
        close_window(cx);
        open_swarm(cx);
        let mut window = second_window(cx);

        window.update(|window, _| window.activate_window());
        window.run_until_parked();
        let running = workspace.read_with(cx, |workspace, _| workspace.usage_clock.is_some());
        let heard = workspace.read_with(cx, |workspace, _| workspace.activation_changes);
        window.deactivate_window();

        let stopped = workspace.read_with(cx, |workspace, _| workspace.usage_clock.is_none());
        assert!(running, "the clock never started in the new window");
        assert!(
            stopped,
            "the clock keeps running in a window that is not active"
        );
        assert!(heard > 0, "the new window's activation is not observed");
    }

    #[gpui::test]
    fn closing_the_window_closes_its_dialog_and_stops_what_needs_an_active_window(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = kept(cx);
        workspace.update(cx, |workspace, _| workspace.fixed_now = None);
        cx.update(|window, _| window.activate_window());
        cx.dispatch_action(ToggleCommandPalette);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let had_dialog = workspace.read_with(cx, |workspace, _| workspace.overlay.is_some());
        let clock = workspace.read_with(cx, |workspace, _| workspace.usage_clock.is_some());

        close_window(cx);

        assert!(had_dialog);
        assert!(clock);
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.overlay.is_none());
            assert!(workspace.usage_clock.is_none());
            assert!(workspace.terminal_area.is_none());
        });
    }

    #[gpui::test]
    fn the_new_window_makes_the_terminal_views_again(cx: &mut TestAppContext) {
        let (workspace, cx, _detaches) = with_a_terminal(cx);
        let before = views(&workspace, cx);
        close_window(cx);

        open_swarm(cx);
        let mut window = second_window(cx);
        let remade = wait_until(&mut window, |cx| {
            let now = views(&workspace, cx);

            now.len() == 1 && now != before
        });

        assert_eq!(before.len(), 1);
        assert!(remade, "the views of the closed window are still in use");
    }

    #[gpui::test]
    fn the_new_window_focuses_the_terminal_view_it_makes_again(cx: &mut TestAppContext) {
        let (workspace, cx, _detaches) = with_a_terminal(cx);
        let terminal = workspace.read_with(cx, |workspace, _| {
            *workspace.live_terminals.keys().next().expect("a tile")
        });
        workspace.update(cx, |workspace, cx| workspace.claim_focus(terminal, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let before = views(&workspace, cx);
        close_window(cx);

        open_swarm(cx);
        let mut window = second_window(cx);
        window.update(|window, cx| window.draw(cx).clear(cx));
        let remade = wait_until(&mut window, |cx| {
            let now = views(&workspace, cx);

            now.len() == 1 && now != before
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        let focused = window.update(|window, cx| {
            workspace
                .read(cx)
                .live_terminals
                .get(&terminal)
                .is_some_and(|live| live.view.focus_handle(cx).is_focused(window))
        });

        assert!(remade);
        assert!(focused, "the keys go nowhere in the new window");
    }

    #[gpui::test]
    fn closing_the_window_detaches_the_terminal_views(cx: &mut TestAppContext) {
        let (workspace, cx, detaches) = with_a_terminal(cx);
        let quiet = detaches.is_empty();

        close_window(cx);
        let heard = wait_until(cx, |_| !detaches.is_empty());

        assert!(quiet);
        assert!(heard, "the old views still read the terminal");
        assert_eq!(
            detaches.try_iter().collect::<Vec<_>>(),
            [swarm_protocol::TerminalId(1)]
        );
        assert_eq!(
            views(&workspace, cx).len(),
            1,
            "the tile stays for the next window"
        );
    }

    #[gpui::test]
    fn without_a_status_item_the_dock_icon_stays_when_the_window_closes(cx: &mut TestAppContext) {
        let (workspace, cx) = kept_without_status_item(cx);

        close_window(cx);

        assert_eq!(policy_now(cx), ActivationPolicy::Regular);
        assert!(workspace.read_with(cx, |workspace, _| workspace.window.is_none()));
    }

    #[gpui::test]
    fn a_dock_click_opens_the_main_window_again(cx: &mut TestAppContext) {
        let (workspace, cx) = kept_without_status_item(cx);
        close_window(cx);

        dock_click(cx);

        assert_eq!(cx.windows().len(), 1);
        assert_eq!(policy_now(cx), ActivationPolicy::Regular);
        assert!(workspace.read_with(cx, |workspace, _| workspace.window.is_some()));
    }

    #[gpui::test]
    fn a_dock_click_with_the_window_open_opens_no_second_window(cx: &mut TestAppContext) {
        let (_workspace, cx) = kept(cx);

        dock_click(cx);

        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn a_dock_click_before_the_lifecycle_started_does_nothing(cx: &mut TestAppContext) {
        harness::init(cx);

        cx.update(reopened);

        assert!(cx.windows().is_empty());
    }

    #[gpui::test]
    fn the_app_comes_to_the_front_after_the_update_that_opened_the_window(cx: &mut TestAppContext) {
        let (_workspace, cx) = kept(cx);
        close_window(cx);
        let app: &TestAppContext = cx;

        let (during, after) = {
            let during = app.update(|cx| {
                show_main_window(cx);

                activations(cx)
            });

            (during, app.read(activations))
        };

        assert_eq!(
            during, 0,
            "activating inside the update calls back into GPUI"
        );
        assert_eq!(after, 1);
    }

    #[gpui::test]
    fn the_app_comes_to_the_front_after_the_update_that_focused_the_window(
        cx: &mut TestAppContext,
    ) {
        let (_workspace, cx) = kept(cx);
        let app: &TestAppContext = cx;

        let during = app.update(|cx| {
            show_main_window(cx);

            activations(cx)
        });

        assert_eq!(during, 0);
        assert_eq!(app.read(activations), 1);
    }

    #[gpui::test]
    fn the_daemon_connection_and_its_events_outlive_the_window(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::Attach { .. } => Some(Event::AgentChanged {
                agent: agent_info(1, 7, DaemonAgentStatus::Blocked(BlockedKind::Permission)),
            }),
            _ => None,
        });
        let (workspace, cx, client) = real_task_workspace(&socket, cx);
        let bar = harness::install(&workspace, cx);
        let quiet = bar.last_look().is_some_and(|look| !look.dot);
        close_window(cx);

        client
            .attach(swarm_protocol::TerminalId(99), 0)
            .expect("send the attach");
        let dot = wait_until(cx, |_| bar.last_look().is_some_and(|look| look.dot));

        let (connected, window) = workspace.read_with(cx, |workspace, _| {
            (
                workspace
                    .daemon
                    .as_ref()
                    .is_some_and(swarm_protocol::Client::is_connected),
                workspace.window,
            )
        });
        assert!(quiet);
        assert!(window.is_none());
        assert!(connected);
        assert!(dot, "the status item did not hear of the blocked agent");
    }
}
