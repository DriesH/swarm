//! Streaming parser for OSC 7501 (Program Status Protocol). It only watches the
//! byte stream; libghostty still gets the bytes and ignores the sequence.
//! Sequences may be split across PTY reads.

use swarm_term::{BlockedKind, ProgramState, ProgramStatus};

/// Whole sequence, OSC through ST (spec limit).
const MAX_SEQUENCE: usize = 4096;
const MAX_KEY: usize = 16;
const MAX_MSG_ENCODED: usize = 2732;
const PREFIX: &[u8] = b"7501;";

/// The reply to `OSC 7501 ; ? ST`.
pub(crate) const QUERY_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    Report(ProgramStatus),
    /// `OSC 7501 ; ? ST`: the program asks if we support the protocol.
    Query,
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
pub(crate) struct Parser {
    mode: Mode,
    /// ESC seen inside an OSC; the next byte decides if it is ST.
    osc_esc: bool,
    buf: Vec<u8>,
}

impl Parser {
    pub(crate) fn feed(&mut self, bytes: &[u8], out: &mut Vec<Parsed>) {
        for &byte in bytes {
            self.byte(byte, out);
        }
    }

    fn byte(&mut self, byte: u8, out: &mut Vec<Parsed>) {
        match self.mode {
            Mode::Ground => {
                if byte == 0x1b {
                    self.mode = Mode::Esc;
                }
            }
            Mode::Esc => {
                self.mode = match byte {
                    b']' => {
                        self.buf.clear();
                        self.osc_esc = false;
                        Mode::Osc
                    }
                    0x1b => Mode::Esc,
                    _ => Mode::Ground,
                };
            }
            Mode::Osc | Mode::Skip => self.osc_byte(byte, out),
        }
    }

    fn osc_byte(&mut self, byte: u8, out: &mut Vec<Parsed>) {
        if self.osc_esc {
            self.osc_esc = false;
            if byte == b'\\' {
                self.finish(out);
                return;
            }

            // An ESC that is not ST aborts the OSC and starts a new sequence.
            self.mode = Mode::Esc;
            self.byte(byte, out);
            return;
        }

        match byte {
            0x07 => self.finish(out),
            0x1b => self.osc_esc = true,
            // CAN and SUB cancel a control sequence.
            0x18 | 0x1a => self.mode = Mode::Ground,
            _ if self.mode == Mode::Osc => {
                self.buf.push(byte);
                let len = self.buf.len();

                // `ESC ]` plus ST take 4 of the 4096 bytes.
                if len <= PREFIX.len() && self.buf[..] != PREFIX[..len] || len > MAX_SEQUENCE - 4 {
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
fn parse_body(body: &[u8]) -> Option<Parsed> {
    let body = std::str::from_utf8(body).ok()?;
    if body.trim() == "?" {
        return Some(Parsed::Query);
    }

    let mut state = None;
    let mut kind = None;
    let mut id = None;
    let mut msg = None;
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
            "msg" => msg = Some(value),
            _ => {}
        }
    }

    // Only the root record counts.
    if let Some(id) = id {
        log::debug!("ignored OSC 7501 record with id {id}");
        return None;
    }

    let state = match state? {
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "done" => ProgramState::Done,
        "error" => ProgramState::Error,
        "blocked" => ProgramState::Blocked(match kind {
            Some("permission") => BlockedKind::Permission,
            Some("question") => BlockedKind::Question,
            Some("auth") => BlockedKind::Auth,
            _ => BlockedKind::Other,
        }),
        "clear" => ProgramState::Clear,
        // Unknown states are ignored whole, so they never turn into idle.
        _ => return None,
    };

    let message = match msg {
        Some(encoded) if encoded.len() > MAX_MSG_ENCODED => return None,
        Some(encoded) => {
            let text = String::from_utf8(base64_decode(encoded)?).ok()?;
            if text.chars().any(char::is_control) {
                return None;
            }

            Some(text).filter(|text| !text.is_empty())
        }
        None => None,
    };

    Some(Parsed::Report(ProgramStatus { state, message }))
}

fn is_value_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_.,+/=-".contains(&c)
}

/// Standard base64; padding is optional.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }

    // A single leftover symbol (6 bits) cannot be valid base64.
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<Parsed> {
        let mut parser = Parser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut out);
        }

        out
    }

    fn states(parsed: &[Parsed]) -> Vec<ProgramState> {
        parsed
            .iter()
            .filter_map(|parsed| match parsed {
                Parsed::Report(status) => Some(status.state),
                Parsed::Query => None,
            })
            .collect()
    }

    #[test]
    fn report_with_st_is_parsed() {
        let out = parse_all(&[b"hi\x1b]7501;state=working:app=codex\x1b\\there"]);

        assert_eq!(states(&out), vec![ProgramState::Working]);
    }

    #[test]
    fn report_with_bel_is_parsed() {
        let out = parse_all(&[b"\x1b]7501;state=idle\x07"]);

        assert_eq!(states(&out), vec![ProgramState::Idle]);
    }

    #[test]
    fn report_split_at_every_byte_is_parsed() {
        let seq = b"x\x1b]7501;state=blocked:kind=permission:msg=SGk=\x1b\\y";
        let chunks: Vec<&[u8]> = seq.chunks(1).collect();

        let out = parse_all(&chunks);

        assert_eq!(
            out,
            vec![Parsed::Report(ProgramStatus {
                state: ProgramState::Blocked(BlockedKind::Permission),
                message: Some("Hi".into()),
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
    fn clear_maps_to_clear() {
        let out = parse_all(&[b"\x1b]7501;state=clear\x1b\\"]);

        assert_eq!(states(&out), vec![ProgramState::Clear]);
    }

    #[test]
    fn blocked_without_known_kind_is_other() {
        let out = parse_all(&[b"\x1b]7501;state=blocked:kind=weird\x07"]);

        assert_eq!(
            states(&out),
            vec![ProgramState::Blocked(BlockedKind::Other)]
        );
    }

    #[test]
    fn query_is_recognized() {
        let out = parse_all(&[b"\x1b]7501;?\x1b\\\x1b[c"]);

        assert_eq!(out, vec![Parsed::Query]);
    }

    #[test]
    fn query_with_bel_is_recognized() {
        let out = parse_all(&[b"\x1b]7501;?\x07"]);

        assert_eq!(out, vec![Parsed::Query]);
    }

    #[test]
    fn query_split_at_every_byte_is_recognized() {
        let seq = b"\x1b]7501;?\x07\x1b[c";
        let chunks: Vec<&[u8]> = seq.chunks(1).collect();

        let out = parse_all(&chunks);

        assert_eq!(out, vec![Parsed::Query]);
    }

    #[test]
    fn msg_with_newline_discards_report() {
        // "a\nb"
        let out = parse_all(&[b"\x1b]7501;state=done:msg=YQpi\x1b\\"]);

        assert!(out.is_empty());
    }

    #[test]
    fn bad_base64_discards_report() {
        let out = parse_all(&[b"\x1b]7501;state=done:msg=a.b\x1b\\"]);

        assert!(out.is_empty());
    }

    #[test]
    fn oversized_sequence_is_dropped() {
        let mut seq = b"\x1b]7501;state=idle:x=".to_vec();
        seq.extend(std::iter::repeat_n(b'a', 5000));
        seq.extend_from_slice(b"\x1b\\\x1b]7501;state=done\x1b\\");

        let out = parse_all(&[&seq]);

        assert_eq!(states(&out), vec![ProgramState::Done]);
    }

    #[test]
    fn esc_inside_osc_restarts_sequence() {
        let out = parse_all(&[b"\x1b]7501;state=idle\x1b]7501;state=working\x07"]);

        assert_eq!(states(&out), vec![ProgramState::Working]);
    }

    #[test]
    fn malformed_pair_is_skipped() {
        let out = parse_all(&[b"\x1b]7501;junk: state = done :app=claude-code\x07"]);

        assert_eq!(states(&out), vec![ProgramState::Done]);
    }

    #[test]
    fn last_key_wins() {
        let out = parse_all(&[b"\x1b]7501;state=idle:state=done\x07"]);

        assert_eq!(states(&out), vec![ProgramState::Done]);
    }

    #[test]
    fn record_with_id_is_ignored() {
        let out = parse_all(&[b"\x1b]7501;state=working:id=build/test\x07"]);

        assert!(out.is_empty());
    }
}
