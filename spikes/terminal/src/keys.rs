use gpui::{Keystroke, Modifiers};
use libghostty_vt::key::{Key, Mods};

/// Maps a GPUI key name to a ghostty key plus its unshifted codepoint
/// ('\0' for keys without one). The Kitty protocol needs both.
pub fn ghostty_key(name: &str) -> (Key, char) {
    let named = match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "space" => return (Key::Space, ' '),
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => Key::Unidentified,
    };
    if named != Key::Unidentified {
        return (named, '\0');
    }

    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return (Key::Unidentified, '\0');
    };
    let key = match c {
        'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
        '0'..='9' => DIGITS[(c as u8 - b'0') as usize],
        '-' => Key::Minus,
        '=' => Key::Equal,
        '[' => Key::BracketLeft,
        ']' => Key::BracketRight,
        '\\' => Key::Backslash,
        ';' => Key::Semicolon,
        '\'' => Key::Quote,
        ',' => Key::Comma,
        '.' => Key::Period,
        '/' => Key::Slash,
        '`' => Key::Backquote,
        // Shifted symbols like '!' arrive as their own key name; the text carries them.
        _ => Key::Unidentified,
    };
    (key, c)
}

pub fn ghostty_mods(m: &Modifiers) -> Mods {
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, m.shift);
    mods.set(Mods::ALT, m.alt);
    mods.set(Mods::CTRL, m.control);
    mods.set(Mods::SUPER, m.platform);
    mods
}

/// Text a key press types, if any. GPUI reports Enter/Tab as "\n"/"\t";
/// the encoder derives those from the key itself.
pub fn typed_text(keystroke: &Keystroke) -> Option<&str> {
    keystroke
        .key_char
        .as_deref()
        .filter(|s| !s.chars().any(char::is_control))
}

const LETTERS: [Key; 26] = [
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
];

const DIGITS: [Key; 10] = [
    Key::Digit0,
    Key::Digit1,
    Key::Digit2,
    Key::Digit3,
    Key::Digit4,
    Key::Digit5,
    Key::Digit6,
    Key::Digit7,
    Key::Digit8,
    Key::Digit9,
];
