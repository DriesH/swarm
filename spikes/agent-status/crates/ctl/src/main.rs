//! `swarm-ctl`: headless client for the agent-status spike daemon.
//!
//! Also the hook command: Claude Code runs `swarm-ctl hook` with the hook JSON
//! on stdin, and it forwards that to the daemon socket.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use proto::{AgentKind, Event, Request, Row, Size, TermId};

const USAGE: &str = "usage: swarm-ctl <command>
  create [--agent claude|codex] [--cwd DIR] [--env K=V]... [--size COLSxROWS] -- CMD...
  resume ID                 start a stopped agent again from its session ID
  send ID TEXT              type TEXT, then press Enter
  keys ID TEXT              send raw bytes; understands \\r \\n \\t \\e \\xNN
  screen ID                 print the visible screen
  list                      terminals with agent status
  watch                     print every status report as it arrives
  wait ID STATE SECS        wait until the merged state starts with STATE
  kill ID
  hook                      forward hook JSON from stdin (used by agent hooks)";

fn main() {
    if let Err(e) = run() {
        eprintln!("swarm-ctl: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        bail!("{USAGE}");
    };
    match (cmd.as_str(), rest) {
        ("hook", _) => hook(),
        ("create", rest) => create(rest),
        ("resume", [id]) => {
            let mut c = Client::connect()?;
            c.send(&Request::Resume {
                id: id.parse()?,
                size: default_size(),
            })?;
            print_created(&mut c)
        }
        ("send", [id, text]) => {
            let id = id.parse()?;
            let mut c = Client::connect()?;
            c.send(&Request::Input {
                id,
                bytes: text.as_bytes().to_vec(),
            })?;
            // TUIs treat a fast Enter right after text as part of a paste.
            std::thread::sleep(Duration::from_millis(150));
            c.send(&Request::Input {
                id,
                bytes: b"\r".to_vec(),
            })?;
            println!("enter_us={}", now_us());
            Ok(())
        }
        ("keys", [id, text]) => Client::connect()?.send(&Request::Input {
            id: id.parse()?,
            bytes: unescape(text)?,
        }),
        ("screen", [id]) => screen(id.parse()?),
        ("list", []) => list(),
        ("watch", []) => watch(None),
        ("wait", [id, state, secs]) => watch(Some((id.parse()?, state, secs.parse()?))),
        ("kill", [id]) => Client::connect()?.send(&Request::Kill { id: id.parse()? }),
        _ => bail!("{USAGE}"),
    }
}

struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
    buf: Vec<u8>,
}

impl Client {
    fn connect() -> Result<Self> {
        let path = socket_path();
        let stream =
            UnixStream::connect(&path).with_context(|| format!("connect {}", path.display()))?;
        Ok(Self {
            read: BufReader::new(stream.try_clone()?),
            write: stream,
            buf: Vec::new(),
        })
    }

    fn send(&mut self, req: &Request) -> Result<()> {
        proto::write(&mut self.write, req)?;
        self.write.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Event> {
        Ok(proto::read(&mut self.read, &mut self.buf)?)
    }
}

fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SWARM_SOCKET") {
        return p.into();
    }
    std::env::var_os("SWARM_SPIKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("swarm-agent-status"))
        .join("daemon.sock")
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

fn default_size() -> Size {
    Size {
        cols: 120,
        rows: 40,
        cell_width: 8,
        cell_height: 16,
    }
}

/// Never fails loudly: a broken hook must not disturb the agent.
fn hook() -> Result<()> {
    let sent_us = now_us();
    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);
    let Some(agent) = std::env::var("SWARM_AGENT_ID")
        .ok()
        .and_then(|a| a.parse::<TermId>().ok())
    else {
        return Ok(());
    };
    if let Ok(mut c) = Client::connect() {
        let _ = c.send(&Request::Hook {
            agent,
            sent_us,
            payload,
        });
    }
    Ok(())
}

fn create(args: &[String]) -> Result<()> {
    let mut agent = None;
    let mut cwd = None;
    let mut env = Vec::new();
    let mut size = default_size();
    let mut it = args.iter();
    let mut cmd = Vec::new();
    while let Some(arg) = it.next() {
        let mut value = || it.next().context("missing value");
        match arg.as_str() {
            "--agent" => {
                agent = Some(match value()?.as_str() {
                    "claude" => AgentKind::Claude,
                    "codex" => AgentKind::Codex,
                    other => bail!("unknown agent {other}"),
                });
            }
            "--cwd" => cwd = Some(value()?.clone()),
            "--env" => {
                let (k, v) = value()?.split_once('=').context("--env needs K=V")?;
                env.push((k.to_owned(), v.to_owned()));
            }
            "--size" => {
                let (c, r) = value()?.split_once('x').context("--size needs COLSxROWS")?;
                size.cols = c.parse()?;
                size.rows = r.parse()?;
            }
            "--" => {
                cmd.extend(it.by_ref().cloned());
                break;
            }
            _ => bail!("unknown option {arg}\n{USAGE}"),
        }
    }
    let mut c = Client::connect()?;
    c.send(&Request::Create {
        cmd: (!cmd.is_empty()).then_some(cmd),
        cwd,
        size,
        agent,
        env,
    })?;
    print_created(&mut c)
}

fn print_created(c: &mut Client) -> Result<()> {
    match c.recv()? {
        Event::Created { id } => {
            println!("{id}");
            Ok(())
        }
        Event::Error { message } => bail!("{message}"),
        other => bail!("unexpected reply {other:?}"),
    }
}

fn screen(id: TermId) -> Result<()> {
    let mut c = Client::connect()?;
    c.send(&Request::Attach { id, history: 0 })?;
    loop {
        match c.recv()? {
            Event::Snapshot(snap) => {
                for row in &snap.rows {
                    println!("{}", row_text(row).trim_end());
                }
                return c.send(&Request::Detach { id });
            }
            Event::Error { message } => bail!("{message}"),
            _ => {}
        }
    }
}

fn row_text(row: &Row) -> String {
    let mut line = String::new();
    let mut col = 0u16;
    for span in &row.texts {
        while col < span.col {
            line.push(' ');
            col += 1;
        }
        line.push_str(&span.text);
        col = span.col + span.cells;
    }
    line
}

fn list() -> Result<()> {
    let mut c = Client::connect()?;
    c.send(&Request::List)?;
    let Event::Terminals(list) = c.recv()? else {
        bail!("unexpected reply");
    };
    for t in list {
        let a = &t.agent;
        println!(
            "{} {:?} state={} msg={:?} session={} alive={} cmd={}",
            t.id,
            a.kind,
            a.state,
            a.msg,
            a.session_id.as_deref().unwrap_or("-"),
            a.alive,
            t.cmd
        );
    }
    Ok(())
}

/// Prints one line per report. With `until`, stops once terminal `id`
/// reaches a state starting with the given text and prints the wait time.
fn watch(until: Option<(TermId, &String, u64)>) -> Result<()> {
    let mut c = Client::connect()?;
    c.send(&Request::Watch)?;
    let start = Instant::now();
    if let Some((_, _, secs)) = until {
        c.read
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(secs)))?;
    }
    loop {
        let event = match c.recv() {
            Ok(e) => e,
            Err(e) if until.is_some() => bail!("timed out ({e})"),
            Err(e) => return Err(e),
        };
        let Event::Agent { id, view, report } = event else {
            continue;
        };
        let state = view.state.to_string();
        if let Some(r) = &report {
            let lag = r
                .sent_us
                .map(|s| format!(" hook_lag={:.1}ms", r.at_us.saturating_sub(s) as f64 / 1e3))
                .unwrap_or_default();
            println!(
                "{} term={id} {:?} report={} detail={:?} msg={:?} => {state} session={}{lag}",
                r.at_us,
                r.source,
                r.state.map_or("-".into(), |s| s.to_string()),
                r.detail,
                r.msg,
                view.session_id.as_deref().unwrap_or("-"),
            );
        } else if until.is_none() {
            println!("{} term={id} current => {state} alive={}", now_us(), view.alive);
        }
        std::io::stdout().flush()?;
        if let Some((want, prefix, _)) = until
            && id == want
            && state.starts_with(prefix.as_str())
        {
            println!("reached {state} after {} ms", start.elapsed().as_millis());
            return Ok(());
        }
    }
}

fn unescape(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'r' => out.push(b'\r'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'e' => out.push(0x1b),
            b'\\' => out.push(b'\\'),
            b'x' => {
                let hex = s.get(i + 1..i + 3).context("\\x needs two hex digits")?;
                out.push(u8::from_str_radix(hex, 16)?);
                i += 2;
            }
            other => bail!("unknown escape \\{}", other as char),
        }
        i += 1;
    }
    Ok(out)
}
