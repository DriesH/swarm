//! A live terminal without a daemon, for the snapshot and frame-stats runs: the sample
//! data has no real terminals, so this feeds one view a fixed screen.

use gpui::{Context, Window};
use swarm_term::{
    Colors, Cursor, CursorStyle, History, Row, Screen, Snapshot, TerminalChannels, TerminalOutput,
    TextFlags, TextSpan,
};
use swarm_term_view::palette_for;
use swarm_ui::ActiveTheme as _;

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
