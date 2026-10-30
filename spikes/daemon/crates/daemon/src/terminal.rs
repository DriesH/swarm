//! One thread per terminal owns the libghostty `Terminal` (which is `!Send`),
//! its render state and the PTY. Everything else talks to it over a channel.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions, key,
    render::{CellIterator, CursorVisualStyle, Dirty, RowIterator},
    screen::{Screen as GhosttyScreen, TrackedGridRef},
    style::RgbColor,
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
        PointCoordinate, PointSpace, PrimaryDeviceAttributes, ScrollViewport,
        SecondaryDeviceAttributes,
    },
};
use portable_pty::PtySize;
use proto::{
    Colors, Cursor, CursorStyle, Event, History, Row, Screen, Size, Snapshot, TermId, TerminalInfo,
    Update,
};

use crate::encode;
use crate::keys;
use crate::pty::{Output, Pty};

const FOREGROUND: RgbColor = RgbColor {
    r: 0xed,
    g: 0xed,
    b: 0xef,
};
const BACKGROUND: RgbColor = RgbColor {
    r: 0x0d,
    g: 0x0d,
    b: 0x0f,
};
/// Ghostty limits scrollback in bytes, not lines (the C header says lines).
/// 10k lines of 200 columns need about 20 MB.
const SCROLLBACK_BYTES: usize = 32 << 20;
/// While output keeps coming, send at most one update per this interval.
const FLUSH_INTERVAL: Duration = Duration::from_millis(8);
const STATS_INTERVAL: Duration = Duration::from_secs(2);

pub type ClientId = u64;
/// An encoded frame, shared by every client that receives it.
pub type Frame = Arc<Vec<u8>>;

pub enum Cmd {
    Output,
    Eof,
    Attach {
        client: ClientId,
        tx: flume::Sender<Frame>,
        history: u32,
    },
    Detach {
        client: ClientId,
    },
    Key(proto::Key),
    Paste(String),
    Input(Vec<u8>),
    Resize(Size),
    FetchHistory {
        client: ClientId,
        from: u64,
        count: u32,
    },
    Kill,
}

#[derive(Clone)]
pub struct Handle {
    pub tx: flume::Sender<Cmd>,
    pub info: Arc<Mutex<TerminalInfo>>,
}

pub struct Spawn {
    pub id: TermId,
    pub cmd: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub size: Size,
}

/// Starts the terminal thread. `on_spawned` gets the child PID; `on_exit` runs
/// on the terminal thread after the child exited and all clients were told.
pub fn spawn(
    req: Spawn,
    on_spawned: impl FnOnce(Option<u32>),
    on_exit: impl FnOnce() + Send + 'static,
) -> Result<Handle> {
    let (tx, rx) = flume::unbounded();
    let (ready_tx, ready_rx) = flume::bounded(1);
    let wake = tx.clone();
    let info = Arc::new(Mutex::new(TerminalInfo {
        id: req.id,
        cmd: String::new(),
        size: req.size,
        title: String::new(),
    }));
    let term_info = info.clone();
    thread::Builder::new()
        .name(format!("term-{}", req.id))
        .spawn(move || {
            let mut term = match Term::new(&req, wake, term_info) {
                Ok((term, pid)) => {
                    let _ = ready_tx.send(Ok(pid));
                    term
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = term.run(&rx) {
                eprintln!("[term {}] failed: {e:#}", req.id);
            }
            on_exit();
        })
        .context("spawn terminal thread")?;

    let pid = ready_rx
        .recv()
        .map_err(|_| anyhow!("terminal thread died"))??;
    on_spawned(pid);
    Ok(Handle { tx, info })
}

struct Subscriber {
    client: ClientId,
    tx: flume::Sender<Frame>,
}

#[derive(Default)]
struct Stats {
    bytes: usize,
    parse: Duration,
    flushes: u32,
    sent_bytes: usize,
    encode: Duration,
    /// Key received → update with the echo queued for clients.
    input_lag_us: Vec<u32>,
    since: Option<Instant>,
}

struct Term {
    id: TermId,
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    pty: Pty,
    output: Arc<Output>,
    info: Arc<Mutex<TerminalInfo>>,
    size: Size,
    /// The viewport as last sent; used to send only rows that really changed.
    rows: Vec<Row>,
    screen: Option<Screen>,
    history: History,
    /// Tracks the first active row at the last flush. Its new position tells
    /// how many lines scrolled into history since then.
    anchor: TrackedGridRef,
    history_reset: bool,
    subscribers: Vec<Subscriber>,
    dirty: bool,
    pending_key: Option<Instant>,
    text: String,
    stats: Stats,
}

impl Term {
    fn new(
        req: &Spawn,
        wake: flume::Sender<Cmd>,
        info: Arc<Mutex<TerminalInfo>>,
    ) -> Result<(Self, Option<u32>)> {
        let size = req.size;
        let spawned = Pty::spawn(
            req.cmd.as_deref(),
            req.cwd.as_deref(),
            pty_size(size),
            move |eof| {
                let _ = wake.send(if eof { Cmd::Eof } else { Cmd::Output });
            },
        )?;

        let mut term = Terminal::new(TerminalOptions {
            cols: size.cols,
            rows: size.rows,
            max_scrollback: SCROLLBACK_BYTES,
        })?;
        term.set_default_fg_color(Some(FOREGROUND))?
            .set_default_bg_color(Some(BACKGROUND))?;
        let replies = spawned.pty.writer();
        term.on_pty_write(move |_, data| {
            let _ = replies.send(data.to_vec());
        })?
        // TUIs probe DA1 at startup and may stall without an answer.
        .on_device_attributes(|_| {
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(
                    ConformanceLevel::VT220,
                    &[DeviceAttributeFeature::ANSI_COLOR],
                ),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: Default::default(),
            })
        })?
        .on_xtversion(|_| Some("swarm-spike"))?;
        info.lock().unwrap_or_else(|e| e.into_inner()).cmd = spawned.cmd;
        let anchor = term.track_grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))?;

        let term = Self {
            id: req.id,
            term,
            render: RenderState::new()?,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            pty: spawned.pty,
            output: spawned.output,
            info,
            size,
            rows: Vec::new(),
            screen: None,
            history: History::default(),
            anchor,
            history_reset: false,
            subscribers: Vec::new(),
            dirty: true,
            pending_key: None,
            text: String::new(),
            stats: Stats::default(),
        };
        Ok((term, spawned.pid))
    }

    fn run(&mut self, rx: &flume::Receiver<Cmd>) -> Result<()> {
        let mut next_flush = Instant::now();
        loop {
            let msg = if self.dirty {
                match rx.recv_deadline(next_flush) {
                    Ok(msg) => Some(msg),
                    Err(flume::RecvTimeoutError::Timeout) => None,
                    Err(flume::RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                Some(rx.recv()?)
            };
            if let Some(msg) = msg
                && !self.handle(msg)?
            {
                return Ok(());
            }
            // The first change after a quiet spell goes out at once (low echo
            // latency); later ones are batched to one update per interval.
            let now = Instant::now();
            if self.dirty && now >= next_flush {
                self.flush()?;
                next_flush = now + FLUSH_INTERVAL;
            }
            self.report_stats();
        }
    }

    /// Returns false when the terminal is gone.
    fn handle(&mut self, msg: Cmd) -> Result<bool> {
        match msg {
            Cmd::Output => {
                let bytes = self.output.take();
                let start = Instant::now();
                self.term.vt_write(&bytes);
                self.stats.parse += start.elapsed();
                self.stats.bytes += bytes.len();
                self.dirty = true;
            }
            Cmd::Eof => {
                // Parse what is left, then tell clients.
                let bytes = self.output.take();
                self.term.vt_write(&bytes);
                self.flush()?;
                self.pty.wait();
                self.broadcast(&Event::Exited { id: self.id })?;
                return Ok(false);
            }
            Cmd::Attach {
                client,
                tx,
                history,
            } => self.attach(client, tx, history)?,
            Cmd::Detach { client } => self.subscribers.retain(|s| s.client != client),
            Cmd::Key(k) => self.key(&k),
            Cmd::Paste(text) => self.paste(&text),
            Cmd::Input(bytes) => self.pty.write(bytes),
            Cmd::Resize(size) => self.resize(size)?,
            Cmd::FetchHistory {
                client,
                from,
                count,
            } => {
                self.flush()?;
                let from = from.max(self.history.start);
                let rows = self.read_history(from, count)?;
                let frame = Arc::new(proto::encode(&Event::History {
                    id: self.id,
                    from,
                    rows,
                })?);
                if let Some(s) = self.subscribers.iter().find(|s| s.client == client) {
                    let _ = s.tx.send(frame);
                }
            }
            Cmd::Kill => self.pty.kill(),
        }
        Ok(true)
    }

    fn attach(&mut self, client: ClientId, tx: flume::Sender<Frame>, history: u32) -> Result<()> {
        let start = Instant::now();
        self.flush()?;
        let screen = self.screen.clone().context("no screen after flush")?;
        let count = u64::from(history).min(self.history.len);
        let history_rows = self.read_history(self.history.end() - count, history)?;
        let read = start.elapsed();
        let frame = proto::encode(&Event::Snapshot(Snapshot {
            id: self.id,
            screen,
            rows: self.rows.clone(),
            history_rows,
        }))?;
        eprintln!(
            "[term {}] snapshot for client {client}: {}x{} + {} history lines, {} KB, read {:.1} ms, total {:.1} ms",
            self.id,
            self.size.cols,
            self.size.rows,
            count,
            frame.len() / 1024,
            read.as_secs_f64() * 1e3,
            start.elapsed().as_secs_f64() * 1e3,
        );
        if tx.send(Arc::new(frame)).is_ok() {
            self.subscribers.retain(|s| s.client != client);
            self.subscribers.push(Subscriber { client, tx });
        }
        Ok(())
    }

    fn key(&mut self, k: &proto::Key) {
        let (gkey, unshifted) = keys::ghostty_key(&k.name);
        let text = k.text.as_deref();
        let mut consumed = key::Mods::empty();
        if text.is_some() && k.mods & proto::Key::SHIFT != 0 {
            consumed |= key::Mods::SHIFT;
        }
        self.key_event
            .set_action(if k.repeat {
                key::Action::Repeat
            } else {
                key::Action::Press
            })
            .set_key(gkey)
            .set_mods(keys::ghostty_mods(k.mods))
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text);
        let mut out = Vec::with_capacity(16);
        // Shifted symbols arrive as their own key name and map to Unidentified;
        // with the Kitty protocol on (fish 4 enables it) the encoder drops them.
        if gkey == key::Key::Unidentified
            && k.mods & (proto::Key::CTRL | proto::Key::ALT) == 0
            && let Some(text) = text
        {
            self.pending_key.get_or_insert_with(Instant::now);
            self.pty.write(text.as_bytes().to_vec());
            return;
        }
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.term)
            .encode_to_vec(&self.key_event, &mut out);
        if (encoded.is_err() || out.is_empty())
            && let Some(text) = text
        {
            out.clear();
            out.extend_from_slice(text.as_bytes());
        }
        if !out.is_empty() {
            self.pending_key.get_or_insert_with(Instant::now);
            self.pty.write(out);
        }
    }

    fn paste(&mut self, text: &str) {
        let bracketed = self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut out = Vec::with_capacity(text.len() + 12);
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
        }
        out.extend_from_slice(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        if bracketed {
            out.extend_from_slice(b"\x1b[201~");
        }
        self.pty.write(out);
    }

    fn resize(&mut self, size: Size) -> Result<()> {
        if size == self.size {
            return Ok(());
        }
        self.size = size;
        self.term.resize(
            size.cols,
            size.rows,
            size.cell_width.into(),
            size.cell_height.into(),
        )?;
        self.pty.resize(pty_size(size))?;
        // Reflow rewraps scrollback, so cached lines are no longer valid.
        self.history_reset = true;
        self.dirty = true;
        self.info.lock().unwrap_or_else(|e| e.into_inner()).size = size;
        Ok(())
    }

    /// Updates `history` from the anchor. Skipped on the alternate screen,
    /// which has no scrollback; the primary history is frozen meanwhile.
    fn sync_history(&mut self) -> Result<()> {
        if self.term.active_screen()? != GhosttyScreen::Primary {
            return Ok(());
        }
        let len = self.term.scrollback_rows()? as u64;
        match self.anchor.point(PointSpace::Screen)? {
            // The anchor was line `history.end()`; it now sits at screen row `p.y`.
            Some(p) => self.history.start = self.history.end().saturating_sub(u64::from(p.y)),
            None => {
                // Pruned or cleared: start a fresh numbering past everything sent.
                self.history.start = self.history.end() + u64::from(self.size.rows);
                self.history_reset = true;
            }
        }
        self.history.len = len;
        self.anchor.set(
            &mut self.term,
            Point::Active(PointCoordinate { x: 0, y: 0 }),
        )?;
        Ok(())
    }

    /// Reads scrollback lines `from..from + count` by moving the viewport over
    /// them. The next flush then sees a full redraw, which the row diff absorbs.
    fn read_history(&mut self, from: u64, count: u32) -> Result<Vec<Row>> {
        let end = (from + u64::from(count)).min(self.history.end());
        if from >= end || self.term.active_screen()? != GhosttyScreen::Primary {
            return Ok(Vec::new());
        }
        let start = Instant::now();
        let mut out = Vec::with_capacity((end - from) as usize);
        let mut y = (from - self.history.start) as usize;
        let last = (end - self.history.start) as usize;
        while y < last {
            self.term.scroll_viewport(ScrollViewport::Row(y));
            let snap = self.render.update(&self.term)?;
            let colors = snap.colors()?;
            let mut rows = self.row_it.update(&snap)?;
            let mut taken = 0;
            while taken < last - y
                && let Some(row) = rows.next()
            {
                out.push(encode::row(
                    row,
                    &mut self.cell_it,
                    colors.foreground,
                    colors.background,
                    &mut self.text,
                )?);
                taken += 1;
            }
            if taken == 0 {
                break;
            }
            y += taken;
        }
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.stats.encode += start.elapsed();
        self.dirty = true;
        Ok(out)
    }

    /// Sends changed rows, cursor and history state to every client.
    fn flush(&mut self) -> Result<()> {
        self.dirty = false;
        self.stats.flushes += 1;
        let start = Instant::now();
        self.sync_history()?;

        let snap = self.render.update(&self.term)?;
        let c = snap.colors()?;
        let colors = Colors {
            fg: encode::rgb(c.foreground),
            bg: encode::rgb(c.background),
            cursor: c.cursor.map(encode::rgb),
        };
        let cursor = if snap.cursor_visible()? {
            snap.cursor_viewport()?.map(|vp| Cursor {
                col: vp.x,
                row: vp.y,
                style: match snap
                    .cursor_visual_style()
                    .unwrap_or(CursorVisualStyle::Block)
                {
                    CursorVisualStyle::Bar => CursorStyle::Bar,
                    CursorVisualStyle::Underline => CursorStyle::Underline,
                    CursorVisualStyle::BlockHollow => CursorStyle::BlockHollow,
                    _ => CursorStyle::Block,
                },
            })
        } else {
            None
        };
        let row_count = usize::from(snap.rows()?);
        let full = snap.dirty()? == Dirty::Full || self.rows.len() != row_count;
        self.rows.resize_with(row_count, Row::default);

        let mut changed = Vec::new();
        let mut rows = self.row_it.update(&snap)?;
        let mut y: u16 = 0;
        while let Some(row) = rows.next() {
            if full || row.dirty()? {
                let encoded = encode::row(
                    row,
                    &mut self.cell_it,
                    c.foreground,
                    c.background,
                    &mut self.text,
                )?;
                let slot = &mut self.rows[usize::from(y)];
                if *slot != encoded {
                    changed.push((y, encoded.clone()));
                    *slot = encoded;
                }
                row.set_dirty(false)?;
            }
            y += 1;
        }
        snap.set_dirty(Dirty::Clean)?;
        self.stats.encode += start.elapsed();

        let screen = Screen {
            cols: self.size.cols,
            rows: row_count as u16,
            colors,
            cursor,
            alt_screen: self.term.active_screen()? == GhosttyScreen::Alternate,
            history: self.history,
        };
        let reset = std::mem::take(&mut self.history_reset);
        if changed.is_empty() && !reset && self.screen.as_ref() == Some(&screen) {
            return Ok(());
        }
        if !changed.is_empty()
            && let Some(t) = self.pending_key.take()
        {
            self.stats
                .input_lag_us
                .push(t.elapsed().as_micros().min(u128::from(u32::MAX)) as u32);
        }
        self.screen = Some(screen.clone());
        self.broadcast(&Event::Update(Update {
            id: self.id,
            screen,
            rows: changed,
            history_reset: reset,
        }))
    }

    fn broadcast(&mut self, event: &Event) -> Result<()> {
        if self.subscribers.is_empty() {
            return Ok(());
        }
        let frame = Arc::new(proto::encode(event)?);
        self.stats.sent_bytes += frame.len();
        // A failed send means the client's writer is gone.
        self.subscribers
            .retain(|s| s.tx.send(frame.clone()).is_ok());
        Ok(())
    }

    fn report_stats(&mut self) {
        let since = *self.stats.since.get_or_insert_with(Instant::now);
        if since.elapsed() < STATS_INTERVAL {
            return;
        }
        let s = std::mem::take(&mut self.stats);
        if s.bytes == 0 && s.input_lag_us.is_empty() {
            return;
        }
        let mut lag = s.input_lag_us;
        lag.sort_unstable();
        let pct = |p: usize| lag.get((lag.len() * p / 100).min(lag.len().saturating_sub(1)));
        eprintln!(
            "[term {}] parsed {:.1} MB in {:.0} ms | {} flushes, encode {:.0} ms, sent {:.1} MB | key→update n={} p50 {:?}us p99 {:?}us",
            self.id,
            s.bytes as f64 / 1e6,
            s.parse.as_secs_f64() * 1e3,
            s.flushes,
            s.encode.as_secs_f64() * 1e3,
            s.sent_bytes as f64 / 1e6,
            lag.len(),
            pct(50),
            pct(99),
        );
    }
}

fn pty_size(s: Size) -> PtySize {
    PtySize {
        cols: s.cols,
        rows: s.rows,
        pixel_width: s.cell_width,
        pixel_height: s.cell_height,
    }
}
