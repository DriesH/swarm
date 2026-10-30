//! Wire protocol between `swarm-daemon` and its clients.
//!
//! A frame is a little-endian `u32` length followed by one postcard-encoded
//! [`Request`] (client → daemon) or [`Event`] (daemon → client).

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const VERSION: u32 = 1;

/// Guards against a corrupt length prefix allocating gigabytes.
const MAX_FRAME: usize = 256 << 20;

pub type TermId = u32;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request {
    Hello {
        version: u32,
    },
    List,
    /// `cmd: None` runs the user's login shell.
    Create {
        cmd: Option<Vec<String>>,
        cwd: Option<String>,
        size: Size,
    },
    /// Answered with a [`Snapshot`] holding up to `history` scrollback lines,
    /// then an [`Update`] stream until [`Request::Detach`].
    Attach {
        id: TermId,
        history: u32,
    },
    Detach {
        id: TermId,
    },
    /// Encoded in the daemon, because the encoding depends on terminal modes.
    Key {
        id: TermId,
        key: Key,
    },
    Paste {
        id: TermId,
        text: String,
    },
    Input {
        id: TermId,
        bytes: Vec<u8>,
    },
    Resize {
        id: TermId,
        size: Size,
    },
    /// Scrollback lines by absolute line number; see [`History`].
    FetchHistory {
        id: TermId,
        from: u64,
        count: u32,
    },
    Kill {
        id: TermId,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Key {
    /// GPUI key name, e.g. `a`, `enter`, `up`.
    pub name: String,
    pub mods: u8,
    pub text: Option<String>,
    pub repeat: bool,
}

impl Key {
    pub const SHIFT: u8 = 1;
    pub const ALT: u8 = 2;
    pub const CTRL: u8 = 4;
    pub const SUPER: u8 = 8;
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Event {
    Hello {
        version: u32,
        pid: u32,
    },
    Terminals(Vec<TerminalInfo>),
    Created {
        id: TermId,
    },
    Snapshot(Snapshot),
    Update(Update),
    History {
        id: TermId,
        from: u64,
        rows: Vec<Row>,
    },
    Exited {
        id: TermId,
    },
    Error {
        message: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TerminalInfo {
    pub id: TermId,
    pub cmd: String,
    pub size: Size,
    pub title: String,
}

/// Scrollback lines have absolute numbers that never change, so a client can
/// cache them while new output scrolls. Lines `start..start + len` exist now.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    pub start: u64,
    pub len: u64,
}

impl History {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Snapshot {
    pub id: TermId,
    pub screen: Screen,
    pub rows: Vec<Row>,
    /// The last lines of scrollback, ending at `screen.history.end()`.
    pub history_rows: Vec<Row>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Update {
    pub id: TermId,
    pub screen: Screen,
    /// Changed viewport rows by row index.
    pub rows: Vec<(u16, Row)>,
    /// Cached scrollback is stale (reflow or clear); drop it.
    pub history_reset: bool,
}

/// Small per-frame state, sent whole with every update.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub colors: Colors,
    pub cursor: Option<Cursor>,
    pub alt_screen: bool,
    pub history: History,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Option<Rgb>,
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
}

/// One terminal row, already resolved to colors: what the renderer paints.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub bgs: Vec<BgSpan>,
    pub texts: Vec<TextSpan>,
}

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
    pub flags: u8,
}

impl TextSpan {
    pub const BOLD: u8 = 1;
    pub const ITALIC: u8 = 2;
    pub const FAINT: u8 = 4;
    pub const UNDERLINE: u8 = 8;
    pub const CURLY: u8 = 16;
    pub const STRIKE: u8 = 32;
}

/// Encodes one length-prefixed frame.
pub fn encode<T: Serialize>(msg: &T) -> postcard::Result<Vec<u8>> {
    let mut frame = postcard::to_extend(msg, vec![0; 4])?;
    let len = u32::try_from(frame.len() - 4).map_err(|_| postcard::Error::SerializeBufferFull)?;
    frame[..4].copy_from_slice(&len.to_le_bytes());
    Ok(frame)
}

pub fn write<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let frame = encode(msg).map_err(io::Error::other)?;
    w.write_all(&frame)
}

/// Reads one frame into `buf` and decodes it. `buf` is reused between calls.
pub fn read<T: DeserializeOwned>(r: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<T> {
    let mut len = [0; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    buf.resize(len, 0);
    r.read_exact(buf)?;
    postcard::from_bytes(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips() {
        let msg = Event::History {
            id: 3,
            from: 42,
            rows: vec![Row {
                bgs: vec![],
                texts: vec![TextSpan {
                    col: 0,
                    cells: 5,
                    text: "hello".into(),
                    fg: Rgb(1, 2, 3),
                    flags: TextSpan::BOLD,
                }],
            }],
        };
        let frame = encode(&msg).unwrap();
        let back: Event = read(&mut frame.as_slice(), &mut Vec::new()).unwrap();
        assert!(
            matches!(back, Event::History { id: 3, from: 42, rows } if rows[0].texts[0].text == "hello")
        );
    }
}
