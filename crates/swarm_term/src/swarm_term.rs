//! Plain data for one terminal: the screen rows a renderer paints, the input it sends
//! back, and the channel pair between the two. No GPUI and no libghostty, so the app
//! builds without Zig.
//!
//! The emulator side (`swarm_term_vt`) turns PTY output into [`Snapshot`] and [`Update`]
//! values. The view side (`swarm_term_view`) paints them and sends [`TerminalInput`].

use serde::{Deserialize, Serialize};

mod screen_model;

pub use screen_model::{Changed, ScreenModel};

/// Grid size in cells, plus the cell size in pixels (the PTY reports both).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Option<Rgb>,
}

/// The colors a program's default and 16 ANSI colors resolve to. The view sends it
/// so the terminal follows the app theme.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub style: CursorStyle,
    /// A hidden cursor still has a position: IME text is drawn there.
    pub visible: bool,
}

/// Scrollback lines have absolute numbers that never change, so a client can cache
/// them while new output scrolls. Lines `start..start + len` exist now.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    pub start: u64,
    pub len: u64,
}

impl History {
    pub fn end(self) -> u64 {
        self.start + self.len
    }
}

/// Small per-frame state, sent whole with every update.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub colors: Colors,
    pub cursor: Cursor,
    /// The program uses the alternate screen, so there is no scrollback to scroll.
    pub alternate: bool,
    pub history: History,
}

/// One terminal row with colors already resolved: what the renderer paints.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub bgs: Vec<BgSpan>,
    pub texts: Vec<TextSpan>,
}

/// Cells whose background differs from the screen background.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BgSpan {
    pub col: u16,
    pub len: u16,
    pub color: Rgb,
}

/// Adjacent cells with one style. A wide character is always its own span.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    pub col: u16,
    pub cells: u16,
    pub text: String,
    pub fg: Rgb,
    pub flags: TextFlags,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextFlags(pub u8);

impl TextFlags {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const FAINT: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const CURLY: Self = Self(1 << 4);
    pub const STRIKE: Self = Self(1 << 5);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for TextFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for TextFlags {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// The whole screen, sent when a view attaches.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub screen: Screen,
    pub rows: Vec<Row>,
    /// The last lines of scrollback, ending at `screen.history.end()`.
    pub history_rows: Vec<Row>,
    /// The latest title and working directory, so a view that attaches late has them.
    pub title: Option<String>,
    pub working_directory: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub screen: Screen,
    /// Changed viewport rows by row index.
    pub rows: Vec<(u16, Row)>,
    /// Cached scrollback is stale (reflow or clear); drop it.
    pub history_reset: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "one bool per modifier key")]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub command: bool,
}

/// A key press as the view saw it. The emulator encodes it, because the bytes depend
/// on terminal modes (Kitty keyboard flags, cursor-key mode).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// GPUI key name, e.g. `a`, `enter`, `up`, `f5`.
    pub name: String,
    pub modifiers: Modifiers,
    /// The text the key produces, if any (after the keyboard layout).
    pub text: Option<String>,
    pub repeat: bool,
}

/// View → emulator.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    Key(Key),
    /// Committed text that is not a key press, such as IME output.
    Text(String),
    /// Wrapped in bracketed paste when the program asked for it.
    Paste(String),
    Resize(Size),
    /// Takes effect for the whole screen, including rows already drawn.
    Palette(Palette),
    /// Asks for scrollback lines `from..from + count`; answered with
    /// [`TerminalOutput::History`].
    FetchHistory {
        from: u64,
        count: u32,
    },
    /// Reported to the program when it asked for focus events (mode 1004).
    Focus(bool),
    Kill,
}

/// Emulator → view.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TerminalOutput {
    Snapshot(Snapshot),
    Update(Update),
    History { from: u64, rows: Vec<Row> },
    Title(String),
    WorkingDirectory(String),
    Bell,
    Exited(ExitStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    Code(u32),
    Signal(String),
}

/// The view's end of one terminal.
pub struct TerminalChannels {
    pub input: flume::Sender<TerminalInput>,
    pub output: flume::Receiver<TerminalOutput>,
}

/// The emulator's end of one terminal.
pub struct EmulatorChannels {
    pub input: flume::Receiver<TerminalInput>,
    pub output: flume::Sender<TerminalOutput>,
}

/// A connected pair of channel ends.
pub fn channels() -> (TerminalChannels, EmulatorChannels) {
    let (input_sender, input_receiver) = flume::unbounded();
    let (output_sender, output_receiver) = flume::unbounded();

    (
        TerminalChannels {
            input: input_sender,
            output: output_receiver,
        },
        EmulatorChannels {
            input: input_receiver,
            output: output_sender,
        },
    )
}
