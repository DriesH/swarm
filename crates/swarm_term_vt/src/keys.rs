use anyhow::Result;
use libghostty_vt::{
    Terminal,
    key::{self, Key as GhosttyKey, Mods},
};
use swarm_term::{Key, Modifiers};

pub(crate) struct KeyEncoder {
    encoder: key::Encoder<'static>,
    event: key::Event<'static>,
}

impl KeyEncoder {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            encoder: key::Encoder::new()?,
            event: key::Event::new()?,
        })
    }

    /// Encodes with the terminal's current modes. Empty when the key sends nothing.
    pub(crate) fn encode(&mut self, terminal: &Terminal<'_, '_>, key: &Key) -> Vec<u8> {
        let (ghostty_key, unshifted) = ghostty_key(&key.name);
        let text = key.text.as_deref().filter(|text| !text.is_empty());
        let modifiers = key.modifiers;
        let types_text = !modifiers.control && !modifiers.alt && !modifiers.command;

        // Shifted symbols map to Unidentified; with Kitty flags on, the encoder drops them.
        if ghostty_key == GhosttyKey::Unidentified
            && types_text
            && let Some(text) = text
        {
            return text.as_bytes().to_vec();
        }

        let mut consumed = Mods::empty();
        if text.is_some() && modifiers.shift {
            consumed |= Mods::SHIFT;
        }

        self.event
            .set_action(if key.repeat {
                key::Action::Repeat
            } else {
                key::Action::Press
            })
            .set_key(ghostty_key)
            .set_mods(ghostty_mods(modifiers))
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text);

        let mut bytes = Vec::with_capacity(16);
        let encoded = self
            .encoder
            .set_options_from_terminal(terminal)
            .encode_to_vec(&self.event, &mut bytes);

        if (encoded.is_err() || bytes.is_empty())
            && types_text
            && let Some(text) = text
        {
            bytes.clear();
            bytes.extend_from_slice(text.as_bytes());
        }

        bytes
    }
}

/// Maps a GPUI key name to a ghostty key plus its unshifted codepoint
/// (`'\0'` for keys without one). The Kitty protocol needs both.
fn ghostty_key(name: &str) -> (GhosttyKey, char) {
    let named = match name {
        "enter" => GhosttyKey::Enter,
        "tab" => GhosttyKey::Tab,
        "backspace" => GhosttyKey::Backspace,
        "escape" => GhosttyKey::Escape,
        "space" => return (GhosttyKey::Space, ' '),
        "up" => GhosttyKey::ArrowUp,
        "down" => GhosttyKey::ArrowDown,
        "left" => GhosttyKey::ArrowLeft,
        "right" => GhosttyKey::ArrowRight,
        "home" => GhosttyKey::Home,
        "end" => GhosttyKey::End,
        "pageup" => GhosttyKey::PageUp,
        "pagedown" => GhosttyKey::PageDown,
        "delete" => GhosttyKey::Delete,
        "insert" => GhosttyKey::Insert,
        "f1" => GhosttyKey::F1,
        "f2" => GhosttyKey::F2,
        "f3" => GhosttyKey::F3,
        "f4" => GhosttyKey::F4,
        "f5" => GhosttyKey::F5,
        "f6" => GhosttyKey::F6,
        "f7" => GhosttyKey::F7,
        "f8" => GhosttyKey::F8,
        "f9" => GhosttyKey::F9,
        "f10" => GhosttyKey::F10,
        "f11" => GhosttyKey::F11,
        "f12" => GhosttyKey::F12,
        _ => GhosttyKey::Unidentified,
    };
    if named != GhosttyKey::Unidentified {
        return (named, '\0');
    }

    let mut chars = name.chars();
    let (Some(character), None) = (chars.next(), chars.next()) else {
        return (GhosttyKey::Unidentified, '\0');
    };

    let key = match character {
        'a'..='z' => LETTERS[usize::from(character as u8 - b'a')],
        '0'..='9' => DIGITS[usize::from(character as u8 - b'0')],
        '-' => GhosttyKey::Minus,
        '=' => GhosttyKey::Equal,
        '[' => GhosttyKey::BracketLeft,
        ']' => GhosttyKey::BracketRight,
        '\\' => GhosttyKey::Backslash,
        ';' => GhosttyKey::Semicolon,
        '\'' => GhosttyKey::Quote,
        ',' => GhosttyKey::Comma,
        '.' => GhosttyKey::Period,
        '/' => GhosttyKey::Slash,
        '`' => GhosttyKey::Backquote,
        _ => GhosttyKey::Unidentified,
    };

    (key, character)
}

fn ghostty_mods(modifiers: Modifiers) -> Mods {
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, modifiers.shift);
    mods.set(Mods::ALT, modifiers.alt);
    mods.set(Mods::CTRL, modifiers.control);
    mods.set(Mods::SUPER, modifiers.command);
    mods
}

const LETTERS: [GhosttyKey; 26] = [
    GhosttyKey::A,
    GhosttyKey::B,
    GhosttyKey::C,
    GhosttyKey::D,
    GhosttyKey::E,
    GhosttyKey::F,
    GhosttyKey::G,
    GhosttyKey::H,
    GhosttyKey::I,
    GhosttyKey::J,
    GhosttyKey::K,
    GhosttyKey::L,
    GhosttyKey::M,
    GhosttyKey::N,
    GhosttyKey::O,
    GhosttyKey::P,
    GhosttyKey::Q,
    GhosttyKey::R,
    GhosttyKey::S,
    GhosttyKey::T,
    GhosttyKey::U,
    GhosttyKey::V,
    GhosttyKey::W,
    GhosttyKey::X,
    GhosttyKey::Y,
    GhosttyKey::Z,
];

const DIGITS: [GhosttyKey; 10] = [
    GhosttyKey::Digit0,
    GhosttyKey::Digit1,
    GhosttyKey::Digit2,
    GhosttyKey::Digit3,
    GhosttyKey::Digit4,
    GhosttyKey::Digit5,
    GhosttyKey::Digit6,
    GhosttyKey::Digit7,
    GhosttyKey::Digit8,
    GhosttyKey::Digit9,
];

#[cfg(test)]
mod tests {
    use libghostty_vt::TerminalOptions;

    use super::*;

    fn terminal() -> Terminal<'static, 'static> {
        Terminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 1024,
        })
        .expect("terminal")
    }

    fn press(name: &str, text: Option<&str>, modifiers: Modifiers) -> Key {
        Key {
            name: name.to_owned(),
            modifiers,
            text: text.map(str::to_owned),
            repeat: false,
        }
    }

    fn encode(terminal: &Terminal<'_, '_>, key: &Key) -> Vec<u8> {
        KeyEncoder::new().expect("encoder").encode(terminal, key)
    }

    #[test]
    fn enter_sends_carriage_return() {
        let bytes = encode(&terminal(), &press("enter", None, Modifiers::default()));

        assert_eq!(bytes, b"\r");
    }

    #[test]
    fn ctrl_c_sends_etx() {
        let modifiers = Modifiers {
            control: true,
            ..Modifiers::default()
        };

        let bytes = encode(&terminal(), &press("c", None, modifiers));

        assert_eq!(bytes, [0x03]);
    }

    #[test]
    fn arrow_up_uses_csi_in_normal_mode() {
        let bytes = encode(&terminal(), &press("up", None, Modifiers::default()));

        assert_eq!(bytes, b"\x1b[A");
    }

    #[test]
    fn arrow_up_uses_ss3_in_application_cursor_mode() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b[?1h");

        let bytes = encode(&terminal, &press("up", None, Modifiers::default()));

        assert_eq!(bytes, b"\x1bOA");
    }

    #[test]
    fn shifted_symbol_sends_its_text_with_kitty_flags_on() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b[>1u");
        let modifiers = Modifiers {
            shift: true,
            ..Modifiers::default()
        };

        let bytes = encode(&terminal, &press("!", Some("!"), modifiers));

        assert_eq!(bytes, b"!");
    }
}
