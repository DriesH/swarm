//! Streaming parser for OSC 7501 (Program Status Protocol, revision 0.4).
//!
//! It only watches the byte stream; the bytes still go to libghostty, which
//! ignores unknown OSCs. Sequences may be split across PTY reads.

use proto::{BlockedKind, State};

/// Whole sequence, OSC through ST (spec limit).
const MAX_SEQUENCE: usize = 4096;
const MAX_KEY: usize = 16;
const MAX_MSG_ENCODED: usize = 2732;
const MAX_APP: usize = 32;
const MAX_ID: usize = 128;
const PREFIX: &[u8] = b"7501;";

/// The reply to `OSC 7501 ; ? ST`.
pub const QUERY_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Report(Status),
    /// `OSC 7501 ; ? ST`: the program asks if we support the protocol.
    Query,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// `None` for `state=clear`.
    pub state: Option<State>,
    pub id: Option<String>,
    pub app: Option<String>,
    pub msg: String,
    pub progress: Option<u8>,
    /// The body as received, for logs.
    pub raw: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    #[default]
    Ground,
    Esc,
    /// Inside an OSC that may still be ours.
    Osc,
    /// Inside an OSC that is not ours, or too long: wait for its end.
    Skip,
}

#[derive(Default)]
pub struct Parser {
    mode: Mode,
    /// ESC seen inside an OSC; the next byte decides if it is ST.
    osc_esc: bool,
    buf: Vec<u8>,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Parsed>) {
        for &b in bytes {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut Vec<Parsed>) {
        match self.mode {
            Mode::Ground => {
                if b == 0x1b {
                    self.mode = Mode::Esc;
                }
            }
            Mode::Esc => {
                self.mode = match b {
                    b']' => {
                        self.buf.clear();
                        self.osc_esc = false;
                        Mode::Osc
                    }
                    0x1b => Mode::Esc,
                    _ => Mode::Ground,
                };
            }
            Mode::Osc | Mode::Skip => self.osc_byte(b, out),
        }
    }

    fn osc_byte(&mut self, b: u8, out: &mut Vec<Parsed>) {
        if self.osc_esc {
            self.osc_esc = false;
            if b == b'\\' {
                self.finish(out);
                return;
            }
            // ESC that is not ST aborts the OSC and starts a new sequence.
            self.mode = Mode::Esc;
            self.byte(b, out);
            return;
        }
        match b {
            0x07 => self.finish(out),
            0x1b => self.osc_esc = true,
            // CAN and SUB cancel a control sequence.
            0x18 | 0x1a => self.mode = Mode::Ground,
            _ if self.mode == Mode::Osc => {
                self.buf.push(b);
                let n = self.buf.len();
                // `ESC ]` plus ST take 4 of the 4096 bytes.
                if n <= PREFIX.len() && self.buf[..] != PREFIX[..n] || n > MAX_SEQUENCE - 4 {
                    self.mode = Mode::Skip;
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, out: &mut Vec<Parsed>) {
        if self.mode == Mode::Osc
            && let Some(body) = self.buf.strip_prefix(PREFIX)
            && let Some(parsed) = parse_body(body)
        {
            out.push(parsed);
        }
        self.mode = Mode::Ground;
        self.buf.clear();
    }
}

/// Parses the part after `7501;`. `None` means the report is ignored.
pub fn parse_body(body: &[u8]) -> Option<Parsed> {
    let body = std::str::from_utf8(body).ok()?;
    if body.trim() == "?" {
        return Some(Parsed::Query);
    }
    let mut state = None;
    let mut kind = None;
    let mut id = None;
    let mut app = None;
    let mut msg = None;
    let mut progress = None;
    for pair in body.split(':') {
        // Malformed pairs are skipped; the rest still counts.
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty()
            || key.len() > MAX_KEY
            || !key.bytes().all(|c| c.is_ascii_lowercase())
            || !value.bytes().all(is_value_byte)
        {
            continue;
        }
        // Repeated keys: the last one wins.
        match key {
            "state" => state = Some(value),
            "kind" => kind = Some(value),
            "id" => id = Some(value),
            "app" => app = Some(value),
            "msg" => msg = Some(value),
            "progress" => progress = Some(value),
            _ => {}
        }
    }
    let state = match state? {
        "idle" => Some(State::Idle),
        "working" => Some(State::Working),
        "done" => Some(State::Done),
        "error" => Some(State::Error),
        "blocked" => Some(State::Blocked(match kind {
            Some("permission") => BlockedKind::Permission,
            Some("question") => BlockedKind::Question,
            Some("auth") => BlockedKind::Auth,
            _ => BlockedKind::Other,
        })),
        "clear" => None,
        // Unknown states are ignored whole, so they never turn into idle.
        _ => return None,
    };
    if let Some(id) = id
        && !valid_id(id)
    {
        return None;
    }
    let msg = match msg {
        Some(m) if m.len() > MAX_MSG_ENCODED => return None,
        Some(m) => {
            let text = String::from_utf8(base64_decode(m)?).ok()?;
            if text.chars().any(char::is_control) {
                return None;
            }
            text
        }
        None => String::new(),
    };
    let app = app.filter(|a| {
        (1..=MAX_APP).contains(&a.len())
            && a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
    });
    let progress = matches!(state, Some(State::Working | State::Blocked(_)))
        .then(|| progress.and_then(|p| p.parse::<u8>().ok()).filter(|p| *p <= 100))
        .flatten();
    Some(Parsed::Report(Status {
        state,
        id: id.map(str::to_owned),
        app: app.map(str::to_owned),
        msg,
        progress,
        raw: body.to_owned(),
    }))
}

fn is_value_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_.,+/=-".contains(&c)
}

fn valid_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('/').collect();
    id.len() <= MAX_ID
        && segments.len() <= 8
        && segments.iter().all(|s| {
            (1..=32).contains(&s.len())
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
        })
}

/// Standard base64; padding is optional.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // A single leftover symbol (6 bits) cannot be valid base64.
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<Parsed> {
        let mut p = Parser::default();
        let mut out = Vec::new();
        for c in chunks {
            p.feed(c, &mut out);
        }
        out
    }

    fn state_of(parsed: &[Parsed]) -> Vec<Option<State>> {
        parsed
            .iter()
            .filter_map(|p| match p {
                Parsed::Report(s) => Some(s.state),
                Parsed::Query => None,
            })
            .collect()
    }

    #[test]
    fn report_with_st_is_parsed() {
        let out = parse_all(&[b"hi\x1b]7501;state=working:app=codex\x1b\\there"]);
        assert_eq!(state_of(&out), vec![Some(State::Working)]);
    }

    #[test]
    fn report_with_bel_is_parsed() {
        let out = parse_all(&[b"\x1b]7501;state=idle\x07"]);
        assert_eq!(state_of(&out), vec![Some(State::Idle)]);
    }

    #[test]
    fn report_split_at_every_byte_is_parsed() {
        let seq = b"x\x1b]7501;state=blocked:kind=permission:msg=SGk=\x1b\\y";
        let chunks: Vec<&[u8]> = seq.chunks(1).collect();
        let out = parse_all(&chunks);
        assert_eq!(
            out,
            vec![Parsed::Report(Status {
                state: Some(State::Blocked(BlockedKind::Permission)),
                id: None,
                app: None,
                msg: "Hi".into(),
                progress: None,
                raw: "state=blocked:kind=permission:msg=SGk=".into(),
            })]
        );
    }

    #[test]
    fn other_oscs_are_ignored() {
        let out = parse_all(&[b"\x1b]0;title\x07\x1b]8;;http://x\x1b\\\x1b]75010;state=idle\x07"]);
        assert!(out.is_empty());
    }

    #[test]
    fn unknown_state_is_ignored() {
        let out = parse_all(&[b"\x1b]7501;state=sleeping\x1b\\"]);
        assert!(out.is_empty());
    }

    #[test]
    fn clear_has_no_state() {
        let out = parse_all(&[b"\x1b]7501;state=clear\x1b\\"]);
        assert_eq!(state_of(&out), vec![None]);
    }

    #[test]
    fn query_is_recognized() {
        let out = parse_all(&[b"\x1b]7501;?\x1b\\\x1b[c"]);
        assert_eq!(out, vec![Parsed::Query]);
    }

    #[test]
    fn msg_with_newline_discards_report() {
        // "a\nb"
        let out = parse_all(&[b"\x1b]7501;state=done:msg=YQpi\x1b\\"]);
        assert!(out.is_empty());
    }

    #[test]
    fn oversized_sequence_is_dropped() {
        let mut seq = b"\x1b]7501;state=idle:x=".to_vec();
        seq.extend(std::iter::repeat_n(b'a', 5000));
        seq.extend_from_slice(b"\x1b\\\x1b]7501;state=done\x1b\\");
        let out = parse_all(&[&seq]);
        assert_eq!(state_of(&out), vec![Some(State::Done)]);
    }

    #[test]
    fn esc_inside_osc_restarts_sequence() {
        let out = parse_all(&[b"\x1b]7501;state=idle\x1b]7501;state=working\x07"]);
        assert_eq!(state_of(&out), vec![Some(State::Working)]);
    }

    #[test]
    fn malformed_pair_is_skipped() {
        let out = parse_all(&[b"\x1b]7501;junk: state = done :app=claude-code\x07"]);
        assert_eq!(state_of(&out), vec![Some(State::Done)]);
    }

    #[test]
    fn child_record_keeps_id() {
        let out = parse_all(&[b"\x1b]7501;state=working:id=build/test\x07"]);
        assert!(matches!(&out[0], Parsed::Report(s) if s.id.as_deref() == Some("build/test")));
    }
}
