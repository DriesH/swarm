//! A live terminal without a daemon, for the snapshot and frame-stats runs: the sample
//! data has no real terminals, so this feeds one view a fixed screen.

use gpui::{Context, SharedString, Window};
use swarm_term::{
    Colors, Cursor, CursorStyle, History, Row, Screen, Snapshot, TerminalChannels, TerminalOutput,
    TextFlags, TextSpan,
};
use swarm_term_view::palette_for;
use swarm_ui::ActiveTheme as _;

use crate::model::TerminalId;
use crate::workspace::Workspace;

const COLUMNS: u16 = 100;
const ROWS: u16 = 60;
/// Lines of scrollback the view believes it has.
const HISTORY: u64 = 1_000;

const LINES: [(&str, usize); 8] = [
    ("> Ran cargo test -p swarm-term", 0),
    ("  14 passed, 0 failed", 2),
    ("> swarm ask orchestrator", 0),
    ("  portable-pty or openpty?", 8),
    ("* Waiting for orchestrator", 3),
    ("  src/pty.rs   +31 -6", 4),
    ("  src/grid.rs  +22 -9", 4),
    ("", 0),
];

/// Gives the selected agent a live terminal that shows [`LINES`] again and again. A
/// terminal the agent already has stays as it is.
pub(crate) fn attach_terminal(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(agent) = workspace
        .selected_agent()
        .and_then(|id| workspace.data.agent(id))
    else {
        return;
    };
    let (terminal, name) = (agent.terminal, agent.name.clone());

    attach(workspace, terminal, name, window, cx);
}

/// Gives every agent of the open task a live terminal, so the tiles show the terminal font.
#[cfg(feature = "snapshot")]
pub(crate) fn attach_task_terminals(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let agents: Vec<_> = workspace
        .data
        .agents_of(workspace.active_task().id)
        .map(|agent| (agent.terminal, agent.name.clone()))
        .collect();

    for (terminal, name) in agents {
        attach(workspace, terminal, name, window, cx);
    }
}

/// The output of a Homebrew install, for the terminal under a row of the system check, with
/// the index of the color of each line. The fix terminal prints the command before it.
#[cfg(feature = "snapshot")]
const INSTALL_OUTPUT: [(&str, Option<usize>); 3] = [
    ("==> Fetching gh", Some(8)),
    ("==> Pouring gh--bottle.tar.gz", Some(8)),
    ("==> Installing gh\u{2026}", Some(4)),
];

/// A terminal that shows the install command and [`INSTALL_OUTPUT`], and then waits.
#[cfg(feature = "snapshot")]
pub(crate) fn install_output(cx: &gpui::App) -> TerminalChannels {
    let (input, input_receiver) = flume::unbounded();
    let (output_sender, output) = flume::unbounded();
    let palette = palette_for(cx.theme());

    let command = format!(
        "$ {}",
        crate::system_check::command_text(&crate::system_check::SAMPLE_INSTALL.map(str::to_owned))
    );
    let lines = std::iter::once((command, None)).chain(
        INSTALL_OUTPUT
            .iter()
            .map(|(text, color)| ((*text).to_owned(), *color)),
    );

    let rows: Vec<Row> = lines
        .map(|(text, color)| Row {
            bgs: Vec::new(),
            texts: vec![TextSpan {
                col: 0,
                cells: u16::try_from(text.chars().count()).unwrap_or(0),
                text,
                fg: color.map_or(palette.fg, |index| palette.ansi[index]),
                flags: TextFlags::default(),
            }],
        })
        .collect();

    let snapshot = TerminalOutput::Snapshot(Snapshot {
        screen: Screen {
            cols: 80,
            rows: u16::try_from(rows.len()).unwrap_or(4),
            colors: Colors {
                fg: palette.fg,
                bg: palette.bg,
                cursor: None,
            },
            cursor: Cursor {
                col: 0,
                row: 0,
                style: CursorStyle::Block,
                visible: false,
            },
            alternate: false,
            history: History { start: 0, len: 0 },
        },
        rows,
        history_rows: Vec::new(),
        title: None,
        working_directory: None,
    });
    if let Err(error) = output_sender.send(snapshot) {
        log::error!("cannot show the install output: {error}");
    }

    // The view ends when its channels close, so both stay open until the run ends.
    Box::leak(Box::new((output_sender, input_receiver)));

    TerminalChannels { input, output }
}

fn attach(
    workspace: &mut Workspace,
    terminal: TerminalId,
    name: SharedString,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.live_terminals.contains_key(&terminal) {
        return;
    }

    let (input, input_receiver) = flume::unbounded();
    let (output_sender, output) = flume::unbounded();
    let live = workspace.live_terminal(
        terminal,
        swarm_protocol::TerminalId(9_000),
        name,
        TerminalChannels { input, output },
        window,
        cx,
    );
    workspace.live_terminals.insert(terminal, live);

    let palette = palette_for(cx.theme());
    let rows: Vec<Row> = (0..ROWS)
        .map(|row| {
            let (text, color) = LINES[usize::from(row) % LINES.len()];

            Row {
                bgs: Vec::new(),
                texts: vec![TextSpan {
                    col: 0,
                    cells: u16::try_from(text.len()).unwrap_or(0),
                    text: text.to_owned(),
                    fg: palette.ansi[color],
                    flags: TextFlags::default(),
                }],
            }
        })
        .collect();

    let snapshot = TerminalOutput::Snapshot(Snapshot {
        screen: Screen {
            cols: COLUMNS,
            rows: ROWS,
            colors: Colors {
                fg: palette.fg,
                bg: palette.bg,
                cursor: None,
            },
            cursor: Cursor {
                col: 0,
                row: 0,
                style: CursorStyle::Block,
                visible: true,
            },
            alternate: false,
            history: History {
                start: 0,
                len: HISTORY,
            },
        },
        rows,
        history_rows: Vec::new(),
        title: None,
        working_directory: None,
    });
    if let Err(error) = output_sender.send(snapshot) {
        log::error!("cannot show the demo screen: {error}");
    }

    // The view ends when its channels close, so both stay open until the run ends.
    Box::leak(Box::new((output_sender, input_receiver)));
}
