//! Drives a `TerminalView` in a test window through the channel types, the way an
//! emulator would.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, EntityInputHandler as _,
    InteractiveElement as _, IntoElement, Modifiers, ParentElement, Render, ScrollDelta,
    ScrollEvent, ScrollSource, Styled as _, TestAppContext, TouchPhase, VisualContext as _,
    VisualTestContext, Window, div, point, px,
};
use swarm_term::{
    BgSpan, Colors, Cursor, CursorStyle, EmulatorChannels, ExitStatus, History, Palette, Rgb, Row,
    Screen, Snapshot, TerminalInput, TerminalOutput, TextFlags, TextSpan, Update,
};
use swarm_term_view::{DEFAULT_FONT_SIZE, TerminalEvent, TerminalView, palette_for};
use swarm_ui::{Appearance, Theme};

type Seen = Rc<RefCell<Vec<String>>>;

/// Records the keys that bubble past the terminal.
struct Harness {
    terminal: Entity<TerminalView>,
    seen: Seen,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let seen = Rc::clone(&self.seen);

        div()
            .size_full()
            .on_key_down(move |event, _, _| seen.borrow_mut().push(event.keystroke.key.clone()))
            .child(self.terminal.clone())
    }
}

struct Setup<'a> {
    terminal: Entity<TerminalView>,
    emulator: EmulatorChannels,
    seen: Seen,
    cx: &'a mut VisualTestContext,
}

fn setup(cx: &mut TestAppContext) -> Setup<'_> {
    cx.update(swarm_ui::init);

    let (channels, emulator) = swarm_term::channels();
    let seen = Seen::default();
    let harness_seen = Rc::clone(&seen);

    let (harness, cx) = cx.add_window_view(|window, cx| {
        let terminal =
            cx.new(|cx| TerminalView::new(channels, "Agent terminal".into(), window, cx));

        Harness {
            terminal,
            seen: harness_seen,
        }
    });
    let terminal = harness.read_with(cx, |harness, _| harness.terminal.clone());
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    Setup {
        terminal,
        emulator,
        seen,
        cx,
    }
}

fn screen(rows: u16, history: History) -> Screen {
    Screen {
        cols: 80,
        rows,
        colors: Colors {
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: None,
        },
        cursor: Cursor {
            col: 3,
            row: 1,
            style: CursorStyle::Block,
            visible: true,
        },
        alternate: false,
        history,
    }
}

fn row(text: &str) -> Row {
    Row {
        bgs: vec![BgSpan {
            col: 0,
            len: 2,
            color: Rgb(10, 20, 30),
        }],
        texts: vec![TextSpan {
            col: 0,
            cells: u16::try_from(text.chars().count()).unwrap_or(0),
            text: text.to_owned(),
            fg: Rgb(200, 200, 200),
            flags: TextFlags::default(),
        }],
    }
}

fn snapshot(rows: &[&str], history: History) -> TerminalOutput {
    TerminalOutput::Snapshot(Snapshot {
        screen: screen(u16::try_from(rows.len()).unwrap_or(0), history),
        rows: rows.iter().map(|text| row(text)).collect(),
        history_rows: Vec::new(),
        title: None,
        working_directory: None,
    })
}

impl Setup<'_> {
    fn send(&mut self, output: TerminalOutput) {
        assert!(self.emulator.output.send(output).is_ok());
        self.cx.run_until_parked();
    }

    fn focus(&mut self) {
        self.cx.focus(&self.terminal);
        self.cx.update(|window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn inputs(&self) -> Vec<TerminalInput> {
        self.emulator.input.try_iter().collect()
    }

    fn keys(&self) -> Vec<swarm_term::Key> {
        self.inputs()
            .into_iter()
            .filter_map(|input| match input {
                TerminalInput::Key(key) => Some(key),
                _ => None,
            })
            .collect()
    }
}

#[gpui::test]
fn snapshot_renders_and_title_updates(cx: &mut TestAppContext) {
    let mut setup = setup(cx);

    setup.send(snapshot(&["hello", "world"], History::default()));
    setup.send(TerminalOutput::Title("zsh".to_owned()));

    let (text, title) = setup.terminal.read_with(setup.cx, |view, _| {
        (view.visible_text(), view.title().map(str::to_owned))
    });
    assert!(text.starts_with("hello\nworld\n"), "{text:?}");
    assert_eq!(title.as_deref(), Some("zsh"));
}

#[gpui::test]
fn view_sends_its_palette_and_grid_size(cx: &mut TestAppContext) {
    let mut setup = setup(cx);

    setup.send(snapshot(&["a"], History::default()));

    let inputs = setup.inputs();
    let palettes: Vec<&Palette> = inputs
        .iter()
        .filter_map(|input| match input {
            TerminalInput::Palette(palette) => Some(palette),
            _ => None,
        })
        .collect();
    assert_eq!(palettes.len(), 1);
    assert!(
        inputs
            .iter()
            .any(|input| matches!(input, TerminalInput::Resize(_)))
    );
}

fn resizes(inputs: &[TerminalInput]) -> Vec<swarm_term::Size> {
    inputs
        .iter()
        .filter_map(|input| match input {
            TerminalInput::Resize(size) => Some(*size),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn a_bigger_font_measures_a_bigger_cell(cx: &mut TestAppContext) {
    let setup = setup(cx);

    let (small, large) = setup.cx.update(|window, cx| {
        (
            TerminalView::cell_size(12., window, cx),
            TerminalView::cell_size(18., window, cx),
        )
    });

    assert!(large.width > small.width, "{small:?} {large:?}");
    assert!(large.height > small.height, "{small:?} {large:?}");
}

#[gpui::test]
fn a_bigger_font_fits_fewer_columns_and_rows_in_the_same_space(cx: &mut TestAppContext) {
    let setup = setup(cx);
    let space = gpui::size(px(640.), px(400.));

    let (small, large) = setup.cx.update(|window, cx| {
        (
            TerminalView::size_for(space, 12., window, cx),
            TerminalView::size_for(space, 18., window, cx),
        )
    });

    assert!(large.cols < small.cols, "{small:?} {large:?}");
    assert!(large.rows < small.rows, "{small:?} {large:?}");
}

#[gpui::test]
fn a_view_starts_at_the_default_font_size(cx: &mut TestAppContext) {
    let setup = setup(cx);

    let points = setup
        .terminal
        .read_with(setup.cx, |view, _| view.font_size());

    assert!((points - DEFAULT_FONT_SIZE).abs() < f32::EPSILON);
}

#[gpui::test]
fn changing_the_font_size_measures_the_cell_again_and_sends_the_new_grid(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(&["a"], History::default()));
    let before = resizes(&setup.inputs());
    let terminal = setup.terminal.clone();

    setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| view.set_font_size(18., window, cx));
    });
    setup.cx.run_until_parked();
    let after = resizes(&setup.inputs());

    let points = setup
        .terminal
        .read_with(setup.cx, |view, _| view.font_size());
    let (small, large) = (
        before.last().copied().expect("a grid before the change"),
        after.last().copied().expect("a grid after the change"),
    );
    assert!((points - 18.).abs() < f32::EPSILON);
    assert!(large.cols < small.cols, "{small:?} {large:?}");
    assert!(large.rows < small.rows, "{small:?} {large:?}");
    assert!(large.cell_width > small.cell_width, "{small:?} {large:?}");
    assert!(large.cell_height > small.cell_height, "{small:?} {large:?}");
}

/// The first extent in pixels, counted from `from`, where `along` reads the same at both
/// font sizes while the cell is not the same. `space` makes the bounds from one extent.
fn extent_where_the_grid_stays(
    setup: &mut Setup<'_>,
    space: impl Fn(f32) -> gpui::Size<gpui::Pixels>,
    along: impl Fn(swarm_term::Size) -> u16,
    from: f32,
) -> Option<f32> {
    setup.cx.update(|window, cx| {
        (0_u16..400)
            .map(|step| from + f32::from(step))
            .find(|extent| {
                let (small, large) = (
                    TerminalView::size_for(space(*extent), 13., window, cx),
                    TerminalView::size_for(space(*extent), 14., window, cx),
                );

                along(small) == along(large) && along(small) > 2
            })
    })
}

#[gpui::test]
fn a_font_change_tells_the_emulator_the_new_cell_even_when_rows_and_columns_stay(
    cx: &mut TestAppContext,
) {
    let mut setup = setup(cx);
    let width = extent_where_the_grid_stays(
        &mut setup,
        |width| gpui::size(px(width), px(400.)),
        |grid| grid.cols,
        60.,
    )
    .expect("a width where both fonts fit the same columns");
    let height = extent_where_the_grid_stays(
        &mut setup,
        |height| gpui::size(px(width), px(height)),
        |grid| grid.rows,
        60.,
    )
    .expect("a height where both fonts fit the same rows");
    setup.cx.simulate_resize(gpui::size(px(width), px(height)));
    setup.send(snapshot(&["a"], History::default()));
    setup.cx.update(|window, _| window.refresh());
    setup.cx.run_until_parked();
    let before = resizes(&setup.inputs());
    let terminal = setup.terminal.clone();

    setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| view.set_font_size(14., window, cx));
    });
    setup.cx.update(|window, _| window.refresh());
    setup.cx.run_until_parked();
    let after = resizes(&setup.inputs());

    let (first, second) = (
        before.last().copied().expect("a grid before the change"),
        after.last().copied().expect("a grid after the change"),
    );
    assert_eq!(after.len(), 1, "the grid was sent again once: {after:?}");
    assert_eq!((first.cols, first.rows), (second.cols, second.rows));
    assert!(
        second.cell_height > first.cell_height,
        "{first:?} {second:?}"
    );
}

#[gpui::test]
fn the_grid_in_a_space_is_the_one_the_view_sends(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(&["a"], History::default()));
    setup.cx.simulate_resize(gpui::size(px(703.), px(517.)));
    setup.cx.update(|window, _| window.refresh());
    setup.cx.run_until_parked();
    let sent = resizes(&setup.inputs())
        .last()
        .copied()
        .expect("the view sent a grid");

    let counted = setup.cx.update(|window, cx| {
        let cell = TerminalView::cell_size(DEFAULT_FONT_SIZE, window, cx);

        TerminalView::grid_in(gpui::size(px(703.), px(517.)), cell)
    });

    assert_eq!(counted, sent);
}

#[test]
fn the_default_font_size_is_the_default_of_the_settings() {
    assert!(
        (DEFAULT_FONT_SIZE - f32::from(swarm_settings::FONT_SIZE_DEFAULT)).abs() < f32::EPSILON
    );
}

#[gpui::test]
fn setting_the_same_font_size_again_does_nothing(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(&["a"], History::default()));
    setup.inputs();
    let terminal = setup.terminal.clone();

    setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| {
            view.set_font_size(DEFAULT_FONT_SIZE, window, cx);
        });
    });
    setup.cx.run_until_parked();

    assert!(resizes(&setup.inputs()).is_empty());
}

#[gpui::test]
fn key_press_sends_the_key_name_and_modifiers(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.focus();
    setup.inputs();

    setup.cx.simulate_keystrokes("a shift-up");

    let keys = setup.keys();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].name, "a");
    assert_eq!(keys[0].text.as_deref(), Some("a"));
    assert_eq!(keys[1].name, "up");
    assert!(keys[1].modifiers.shift);
    assert!(!keys[1].modifiers.command);
}

#[gpui::test]
fn handled_keys_do_not_reach_the_parent(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.focus();

    setup.cx.simulate_keystrokes("a");

    assert!(setup.seen.borrow().is_empty());
}

#[gpui::test]
fn cmd_shortcuts_are_not_consumed(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.focus();
    setup.inputs();

    setup.cx.simulate_keystrokes("cmd-k cmd-c");

    assert!(setup.keys().is_empty());
    assert_eq!(*setup.seen.borrow(), ["k", "c"]);
}

#[gpui::test]
fn cmd_v_pastes_the_clipboard(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.focus();
    setup.inputs();
    setup
        .cx
        .write_to_clipboard(ClipboardItem::new_string("pasted text".to_owned()));

    setup.cx.simulate_keystrokes("cmd-v");

    assert!(
        setup
            .inputs()
            .contains(&TerminalInput::Paste("pasted text".to_owned()))
    );
    assert!(setup.seen.borrow().is_empty());
}

#[gpui::test]
fn focus_in_and_out_are_reported(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.inputs();

    setup.focus();
    let got = setup.inputs();
    assert!(got.contains(&TerminalInput::Focus(true)), "{got:?}");

    setup.cx.update(Window::blur);
    setup.cx.run_until_parked();
    assert!(setup.inputs().contains(&TerminalInput::Focus(false)));
}

#[gpui::test]
fn scrolling_up_fetches_missing_history(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(
        &["a", "b", "c"],
        History {
            start: 0,
            len: 1_000,
        },
    ));
    setup.inputs();

    setup.cx.simulate_event(ScrollEvent {
        position: point(px(100.), px(100.)),
        delta: ScrollDelta::Lines(point(0., 3.)),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });

    // The view now shows lines 997..1000, none cached; 200 lines of margin go before.
    assert!(setup.inputs().contains(&TerminalInput::FetchHistory {
        from: 797,
        count: 203
    }));
}

#[gpui::test]
fn typing_scrolls_back_to_the_bottom(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(
        &["a", "b", "c"],
        History {
            start: 0,
            len: 1_000,
        },
    ));
    setup.focus();
    setup.cx.simulate_event(ScrollEvent {
        position: point(px(100.), px(100.)),
        delta: ScrollDelta::Lines(point(0., 3.)),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });

    setup.cx.simulate_keystrokes("x");

    let text = setup
        .terminal
        .read_with(setup.cx, |view, _| view.visible_text());
    assert!(text.starts_with("a\nb\nc\n"), "{text:?}");
}

fn scroll_up(setup: &mut Setup<'_>, lines: f32) {
    setup.cx.simulate_event(ScrollEvent {
        position: point(px(100.), px(100.)),
        delta: ScrollDelta::Lines(point(0., lines)),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });
}

#[gpui::test]
fn scrolling_moves_the_scrollback_position(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(
        &["a", "b", "c"],
        History {
            start: 0,
            len: 1_000,
        },
    ));

    scroll_up(&mut setup, 3.);
    let scrolled = setup
        .terminal
        .read_with(setup.cx, |view, _| view.scrolled_back());
    scroll_up(&mut setup, -3.);
    let back = setup
        .terminal
        .read_with(setup.cx, |view, _| view.scrolled_back());

    assert_eq!((scrolled, back), (3, 0));
}

fn alternate_snapshot(rows: &[&str]) -> TerminalOutput {
    let TerminalOutput::Snapshot(mut snapshot) = snapshot(rows, History::default()) else {
        unreachable!("snapshot returns a snapshot");
    };
    snapshot.screen.alternate = true;

    TerminalOutput::Snapshot(snapshot)
}

fn scrolls(setup: &Setup<'_>) -> Vec<(i32, u16, u16, bool)> {
    setup
        .inputs()
        .into_iter()
        .filter_map(|input| match input {
            TerminalInput::Scroll {
                lines,
                column,
                row,
                keys,
            } => Some((lines, column, row, keys)),
            _ => None,
        })
        .collect()
}

fn scroll_at(setup: &mut Setup<'_>, lines: f32, x: f32, y: f32) {
    setup.cx.simulate_event(ScrollEvent {
        position: point(px(x), px(y)),
        delta: ScrollDelta::Lines(point(0., lines)),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });
}

#[gpui::test]
fn the_wheel_on_the_alternate_screen_goes_to_the_program(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(alternate_snapshot(&["a", "b", "c"]));
    setup.inputs();

    scroll_at(&mut setup, 3., 9., 9.);
    scroll_at(&mut setup, -2., 9., 9.);

    assert_eq!(scrolls(&setup), [(3, 0, 0, true), (-2, 0, 0, true)]);
    let scrolled = setup
        .terminal
        .read_with(setup.cx, |view, _| view.scrolled_back());
    assert_eq!(scrolled, 0);
}

#[gpui::test]
fn the_wheel_names_the_cell_under_the_pointer(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(alternate_snapshot(&["a", "b", "c"]));
    setup.inputs();
    let cell = setup
        .cx
        .update(|window, cx| TerminalView::cell_size(DEFAULT_FONT_SIZE, window, cx));

    // The grid starts 8 px in from the corner. Aim at the middle of column 3, row 4.
    let x = 8. + 3.5 * f32::from(cell.width);
    let y = 8. + 4.5 * f32::from(cell.height);
    scroll_at(&mut setup, 1., x, y);

    assert_eq!(scrolls(&setup), [(1, 3, 4, true)]);
}

#[gpui::test]
fn small_wheel_turns_on_the_alternate_screen_add_up(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(alternate_snapshot(&["a"]));
    setup.inputs();

    scroll_at(&mut setup, 0.5, 9., 9.);
    let first = scrolls(&setup);
    scroll_at(&mut setup, 0.5, 9., 9.);
    let second = scrolls(&setup);

    assert!(first.is_empty());
    assert_eq!(second, [(1, 0, 0, true)]);
}

#[gpui::test]
fn a_closed_typing_gate_still_sends_the_wheel_to_the_program(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(alternate_snapshot(&["a"]));
    gate(&mut setup, &Rc::new(Cell::new(false)));
    setup.inputs();

    scroll_at(&mut setup, 2., 9., 9.);

    assert_eq!(scrolls(&setup), [(2, 0, 0, false)]);
}

#[gpui::test]
fn the_wheel_may_send_arrow_keys_only_while_typing_is_allowed(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(alternate_snapshot(&["a"]));
    let open = Rc::new(Cell::new(true));
    gate(&mut setup, &open);
    setup.inputs();

    scroll_at(&mut setup, 1., 9., 9.);
    let allowed = scrolls(&setup);
    open.set(false);
    scroll_at(&mut setup, 1., 9., 9.);
    let denied = scrolls(&setup);

    assert_eq!(allowed, [(1, 0, 0, true)]);
    assert_eq!(denied, [(1, 0, 0, false)]);
}

#[gpui::test]
fn the_wheel_on_the_main_screen_scrolls_history_and_sends_nothing(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(
        &["a", "b", "c"],
        History {
            start: 0,
            len: 1_000,
        },
    ));
    setup.inputs();

    scroll_up(&mut setup, 3.);

    let scrolled = setup
        .terminal
        .read_with(setup.cx, |view, _| view.scrolled_back());
    assert_eq!(scrolled, 3);
    assert!(scrolls(&setup).is_empty());
}

fn gate(setup: &mut Setup<'_>, open: &Rc<Cell<bool>>) {
    let open = Rc::clone(open);

    setup.terminal.update(setup.cx, |view, _| {
        view.set_typing_gate(move |_| open.get());
    });
}

#[gpui::test]
fn a_closed_typing_gate_keeps_typing_from_the_program(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let open = Rc::new(Cell::new(false));
    gate(&mut setup, &open);
    setup.focus();
    setup.inputs();
    setup
        .cx
        .write_to_clipboard(ClipboardItem::new_string("pasted".to_owned()));

    setup.cx.simulate_keystrokes("a enter cmd-v");
    let terminal = setup.terminal.clone();
    setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| {
            view.replace_text_in_range(None, "x", window, cx);
        });
    });

    assert!(setup.inputs().is_empty());
    assert_eq!(*setup.seen.borrow(), ["a", "enter", "v"]);
}

#[gpui::test]
fn a_closed_typing_gate_keeps_composing_text_off_the_screen(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let open = Rc::new(Cell::new(false));
    gate(&mut setup, &open);
    let terminal = setup.terminal.clone();
    let mark = |setup: &mut Setup<'_>| {
        setup.cx.update(|window, cx| {
            terminal.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "か", None, window, cx);
            });
        });

        setup
            .cx
            .update(|window, cx| terminal.update(cx, |view, cx| view.marked_text_range(window, cx)))
    };

    let closed = mark(&mut setup);
    open.set(true);
    let opened = mark(&mut setup);

    assert_eq!(closed, None);
    assert_eq!(opened, Some(0..1));
}

#[gpui::test]
fn an_open_typing_gate_lets_typing_through_again(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let open = Rc::new(Cell::new(false));
    gate(&mut setup, &open);
    setup.focus();
    setup.inputs();

    setup.cx.simulate_keystrokes("a");
    open.set(true);
    setup.cx.simulate_keystrokes("b");

    let keys = setup.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "b");
}

#[gpui::test]
fn a_closed_typing_gate_still_scrolls_and_takes_focus(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(
        &["a", "b", "c"],
        History {
            start: 0,
            len: 1_000,
        },
    ));
    gate(&mut setup, &Rc::new(Cell::new(false)));
    setup.inputs();

    setup.focus();
    scroll_up(&mut setup, 3.);

    let scrolled = setup
        .terminal
        .read_with(setup.cx, |view, _| view.scrolled_back());
    assert_eq!(scrolled, 3);
    assert!(setup.inputs().contains(&TerminalInput::Focus(true)));
}

#[gpui::test]
fn theme_change_sends_a_new_palette(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(&["a"], History::default()));
    setup
        .cx
        .update(|_, cx| swarm_ui::theme::set_appearance(Appearance::Dark, cx));
    setup.cx.run_until_parked();
    setup.inputs();

    setup
        .cx
        .update(|_, cx| swarm_ui::theme::set_appearance(Appearance::Light, cx));
    setup.cx.run_until_parked();

    assert!(
        setup
            .inputs()
            .contains(&TerminalInput::Palette(palette_for(&Theme::light())))
    );
}

#[gpui::test]
fn composition_marks_text_and_commits_it_as_text(cx: &mut TestAppContext) {
    let setup = setup(cx);
    setup.inputs();
    let terminal = setup.terminal.clone();

    setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "か", None, window, cx);
        });
    });
    let marked = setup
        .cx
        .update(|window, cx| terminal.update(cx, |view, cx| view.marked_text_range(window, cx)));
    assert_eq!(marked, Some(0..1));
    assert!(setup.inputs().is_empty());

    setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| {
            view.replace_text_in_range(None, "漢", window, cx);
        });
    });
    let marked = setup
        .cx
        .update(|window, cx| terminal.update(cx, |view, cx| view.marked_text_range(window, cx)));
    assert_eq!(marked, None);
    assert_eq!(setup.inputs(), [TerminalInput::Text("漢".to_owned())]);
}

#[gpui::test]
fn updates_after_a_snapshot_change_rows(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    setup.send(snapshot(&["a", "b"], History::default()));

    setup.send(TerminalOutput::Update(Update {
        screen: screen(2, History::default()),
        rows: vec![(1, row("B"))],
        history_reset: false,
    }));

    let text = setup
        .terminal
        .read_with(setup.cx, |view, _| view.visible_text());
    assert!(text.starts_with("a\nB\n"), "{text:?}");
}

#[gpui::test]
fn hidden_cursor_still_places_the_input_method_window(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let TerminalOutput::Snapshot(mut hidden) = snapshot(&["a", "b"], History::default()) else {
        unreachable!("snapshot builds a snapshot");
    };
    hidden.screen.cursor.visible = false;
    setup.send(TerminalOutput::Snapshot(hidden));
    let terminal = setup.terminal.clone();

    let bounds = setup.cx.update(|window, cx| {
        terminal.update(cx, |view, cx| {
            view.bounds_for_range(0..0, gpui::Bounds::default(), window, cx)
        })
    });

    let bounds = bounds.expect("the cursor cell has bounds");
    assert!(bounds.size.width > px(0.));
    assert!(bounds.origin.x > px(0.));
}

#[gpui::test]
fn snapshot_sets_title_and_working_directory(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let TerminalOutput::Snapshot(mut snapshot) = snapshot(&["a"], History::default()) else {
        unreachable!("snapshot builds a snapshot");
    };
    snapshot.title = Some("vim".to_owned());
    snapshot.working_directory = Some("/tmp".to_owned());

    setup.send(TerminalOutput::Snapshot(snapshot));

    let (title, directory) = setup.terminal.read_with(setup.cx, |view, _| {
        (
            view.title().map(str::to_owned),
            view.working_directory().map(str::to_owned),
        )
    });
    assert_eq!(title.as_deref(), Some("vim"));
    assert_eq!(directory.as_deref(), Some("/tmp"));
}

#[gpui::test]
fn outputs_emit_events_and_update_getters(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let events: Rc<RefCell<Vec<TerminalEvent>>> = Rc::default();
    let sink = Rc::clone(&events);
    let _subscription = setup.cx.update(|_, cx| {
        cx.subscribe(&setup.terminal, move |_, event: &TerminalEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
    });

    setup.send(TerminalOutput::Title("zsh".to_owned()));
    setup.send(TerminalOutput::WorkingDirectory("/home".to_owned()));
    setup.send(TerminalOutput::Bell);
    setup.send(TerminalOutput::Exited(ExitStatus::Code(2)));

    assert_eq!(
        *events.borrow(),
        [
            TerminalEvent::TitleChanged("zsh".to_owned()),
            TerminalEvent::WorkingDirectoryChanged("/home".to_owned()),
            TerminalEvent::Bell,
            TerminalEvent::Exited(ExitStatus::Code(2)),
        ]
    );
    let (directory, status) = setup.terminal.read_with(setup.cx, |view, _| {
        (
            view.working_directory().map(str::to_owned),
            view.exit_status().cloned(),
        )
    });
    assert_eq!(directory.as_deref(), Some("/home"));
    assert_eq!(status, Some(ExitStatus::Code(2)));
}

#[gpui::test]
fn ending_the_output_without_an_exit_emits_disconnected(cx: &mut TestAppContext) {
    let setup = setup(cx);
    let events: Rc<RefCell<Vec<TerminalEvent>>> = Rc::default();
    let sink = Rc::clone(&events);
    let _subscription = setup.cx.update(|_, cx| {
        cx.subscribe(&setup.terminal, move |_, event: &TerminalEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
    });

    drop(setup.emulator);
    setup.cx.run_until_parked();

    assert_eq!(*events.borrow(), [TerminalEvent::Disconnected]);
}

#[gpui::test]
fn an_exit_is_not_followed_by_disconnected(cx: &mut TestAppContext) {
    let mut setup = setup(cx);
    let events: Rc<RefCell<Vec<TerminalEvent>>> = Rc::default();
    let sink = Rc::clone(&events);
    let _subscription = setup.cx.update(|_, cx| {
        cx.subscribe(&setup.terminal, move |_, event: &TerminalEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
    });

    setup.send(TerminalOutput::Exited(ExitStatus::Code(0)));
    drop(setup.emulator);
    setup.cx.run_until_parked();

    assert_eq!(
        *events.borrow(),
        [TerminalEvent::Exited(ExitStatus::Code(0))]
    );
}
