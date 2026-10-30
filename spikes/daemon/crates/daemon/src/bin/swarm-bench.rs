//! Headless client that measures attach cost for a large screen with full
//! scrollback: `swarm-bench [cols] [rows] [lines]` (default 200 60 12000).

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use proto::{Event, Request, Size};

fn main() -> Result<()> {
    let args: Vec<u32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let cols = args.first().copied().unwrap_or(200) as u16;
    let rows = args.get(1).copied().unwrap_or(60) as u16;
    let lines = args.get(2).copied().unwrap_or(12_000);

    let dir = std::env::var_os("SWARM_SPIKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("swarm-spike"));
    let stream = UnixStream::connect(dir.join("daemon.sock"))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    let mut buf = Vec::new();
    let mut send = |req: &Request| -> Result<()> {
        proto::write(&mut writer, req)?;
        writer.flush()?;
        Ok(())
    };

    // Styled lines that fill the width: color, bold and a long payload.
    let script = format!(
        "seq 1 {lines} | awk '{{ printf \"\\033[3%dm%06d \\033[1m\", $1 % 7 + 1, $1; \
         for (i = 0; i < {w}; i++) printf \"%c\", 97 + (i + $1) % 26; printf \"\\033[0m\\n\" }}'; \
         exec sleep 100000",
        w = cols - 8
    );
    send(&Request::Create {
        cmd: Some(vec!["/bin/sh".into(), "-c".into(), script]),
        cwd: None,
        size: Size {
            cols,
            rows,
            cell_width: 8,
            cell_height: 17,
        },
    })?;
    let id = loop {
        match proto::read::<Event>(&mut reader, &mut buf)? {
            Event::Created { id } => break id,
            Event::Error { message } => bail!(message),
            _ => {}
        }
    };
    // Let the generator finish.
    std::thread::sleep(Duration::from_secs(3));

    for history in [0, 1_000, 10_000] {
        let mut times = Vec::new();
        let mut size = 0;
        let mut got = 0;
        for _ in 0..5 {
            let start = Instant::now();
            send(&Request::Attach { id, history })?;
            loop {
                if let Event::Snapshot(s) = proto::read::<Event>(&mut reader, &mut buf)? {
                    got = s.history_rows.len();
                    break;
                }
            }
            times.push(start.elapsed());
            size = buf.len();
            send(&Request::Detach { id })?;
        }
        times.sort();
        println!(
            "{cols}x{rows} + {got} history lines: {:.0} KB, attach median {:.1} ms (min {:.1}, max {:.1})",
            size as f64 / 1024.,
            times[2].as_secs_f64() * 1e3,
            times[0].as_secs_f64() * 1e3,
            times[4].as_secs_f64() * 1e3,
        );
    }
    send(&Request::Kill { id })?;
    Ok(())
}
