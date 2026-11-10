//! What a key sends to the program in the terminal, as xterm would send it: text as itself, Ctrl
//! with a letter as its control character, and the arrows and function keys as escape sequences,
//! which change when the program asks for application cursor keys.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// xterm's number for the modifiers held with a key, or none without any.
fn modifier_code(keystroke: &Keystroke) -> Option<u8> {
    let m = &keystroke.modifiers;
    let code = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    (code > 1).then_some(code)
}

/// A key like an arrow: `ESC [ final`, `ESC O final` for application cursor keys, or with
/// modifiers `ESC [ 1 ; m final`.
fn cursor_key(keystroke: &Keystroke, mode: TermMode, last: char) -> Vec<u8> {
    match modifier_code(keystroke) {
        Some(m) => format!("\x1b[1;{m}{last}").into_bytes(),
        None if mode.contains(TermMode::APP_CURSOR) => format!("\x1bO{last}").into_bytes(),
        None => format!("\x1b[{last}").into_bytes(),
    }
}

/// A key like Page Up: `ESC [ n ~`, or with modifiers `ESC [ n ; m ~`.
fn tilde_key(keystroke: &Keystroke, number: u8) -> Vec<u8> {
    match modifier_code(keystroke) {
        Some(m) => format!("\x1b[{number};{m}~").into_bytes(),
        None => format!("\x1b[{number}~").into_bytes(),
    }
}

/// The control character Ctrl makes of a key, as `Ctrl+C` makes 3.
fn control_byte(key: &str) -> Option<u8> {
    let [byte] = key.as_bytes() else {
        return match key {
            "space" => Some(0),
            _ => None,
        };
    };
    match byte.to_ascii_lowercase() {
        c @ b'a'..=b'z' => Some(c - b'a' + 1),
        b'@' | b'2' => Some(0),
        b'[' | b'3' => Some(27),
        b'\\' | b'4' => Some(28),
        b']' | b'5' => Some(29),
        b'^' | b'6' => Some(30),
        b'_' | b'/' | b'7' => Some(31),
        b'?' | b'8' => Some(127),
        _ => None,
    }
}

/// The bytes a keystroke sends, or none for a key that's the IDE's, like any with Cmd.
pub fn to_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let key = keystroke.key.as_str();
    if m.platform {
        // Cmd with a cursor key edits the line, as macOS terminals do: to its start or end, or
        // deleting back to its start.
        return match key {
            "left" => Some(vec![0x01]),
            "right" => Some(vec![0x05]),
            "backspace" => Some(vec![0x15]),
            _ => None,
        };
    }
    let alt_prefixed = |mut bytes: Vec<u8>| {
        if m.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    let bytes = match key {
        "enter" => alt_prefixed(b"\r".to_vec()),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.control => vec![0x08],
        "backspace" => alt_prefixed(vec![0x7f]),
        // Option with Left and Right moves a word, as shells take ESC b and ESC f.
        "left" if m.alt && !m.control && !m.shift => b"\x1bb".to_vec(),
        "right" if m.alt && !m.control && !m.shift => b"\x1bf".to_vec(),
        "up" => cursor_key(keystroke, mode, 'A'),
        "down" => cursor_key(keystroke, mode, 'B'),
        "right" => cursor_key(keystroke, mode, 'C'),
        "left" => cursor_key(keystroke, mode, 'D'),
        "home" => cursor_key(keystroke, mode, 'H'),
        "end" => cursor_key(keystroke, mode, 'F'),
        "insert" => tilde_key(keystroke, 2),
        "delete" => tilde_key(keystroke, 3),
        "pageup" => tilde_key(keystroke, 5),
        "pagedown" => tilde_key(keystroke, 6),
        "f1" | "f2" | "f3" | "f4" => {
            let last = (b'P' + key[1..].parse::<u8>().ok()? - 1) as char;
            match modifier_code(keystroke) {
                Some(m) => format!("\x1b[1;{m}{last}").into_bytes(),
                None => format!("\x1bO{last}").into_bytes(),
            }
        }
        "f5" => tilde_key(keystroke, 15),
        "f6" => tilde_key(keystroke, 17),
        "f7" => tilde_key(keystroke, 18),
        "f8" => tilde_key(keystroke, 19),
        "f9" => tilde_key(keystroke, 20),
        "f10" => tilde_key(keystroke, 21),
        "f11" => tilde_key(keystroke, 23),
        "f12" => tilde_key(keystroke, 24),
        _ if m.control => alt_prefixed(vec![control_byte(key)?]),
        "space" => alt_prefixed(b" ".to_vec()),
        _ => {
            // Text as it was typed, including what Option composes on macOS; elsewhere Alt
            // sends ESC before the key, as Meta.
            let text = match &keystroke.key_char {
                Some(text) if !(m.alt && cfg!(not(target_os = "macos"))) => text.clone(),
                _ if key.chars().count() == 1 => {
                    let text = if m.shift {
                        key.to_uppercase()
                    } else {
                        key.to_string()
                    };
                    return Some(alt_prefixed(text.into_bytes()));
                }
                _ => return None,
            };
            text.into_bytes()
        }
    };
    Some(bytes)
}

/// Text pasted into the terminal: wrapped as a bracketed paste when the program asked for that,
/// so a shell doesn't run each line as it arrives; otherwise with its line breaks as Enter.
pub fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // A paste mustn't be able to end the bracket early.
        let text = text.replace("\x1b[201~", "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(keys: &'static str, mode: TermMode) -> Option<Vec<u8>> {
        to_bytes(&Keystroke::parse(keys).unwrap(), mode)
    }

    fn sent(keys: &'static str) -> String {
        String::from_utf8(bytes(keys, TermMode::NONE).unwrap()).unwrap()
    }

    #[test]
    fn control_keys_send_control_characters() {
        assert_eq!(sent("ctrl-c"), "\x03");
        assert_eq!(sent("ctrl-d"), "\x04");
        assert_eq!(sent("ctrl-z"), "\x1a");
        assert_eq!(sent("ctrl-["), "\x1b");
        assert_eq!(sent("ctrl-space"), "\0");
        assert_eq!(sent("ctrl-alt-b"), "\x1b\x02");
    }

    #[test]
    fn cursor_keys_follow_the_mode_and_modifiers() {
        assert_eq!(sent("up"), "\x1b[A");
        assert_eq!(
            bytes("up", TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA".to_vec()
        );
        assert_eq!(sent("shift-right"), "\x1b[1;2C");
        assert_eq!(sent("ctrl-left"), "\x1b[1;5D");
        assert_eq!(sent("alt-left"), "\x1bb");
        assert_eq!(sent("pageup"), "\x1b[5~");
        assert_eq!(sent("ctrl-delete"), "\x1b[3;5~");
        assert_eq!(sent("f1"), "\x1bOP");
        assert_eq!(sent("f5"), "\x1b[15~");
        assert_eq!(sent("shift-tab"), "\x1b[Z");
    }

    #[test]
    fn plain_keys_send_themselves_and_cmd_keys_stay_with_the_ide() {
        assert_eq!(sent("a"), "a");
        assert_eq!(sent("shift-a"), "A");
        assert_eq!(sent("enter"), "\r");
        assert_eq!(sent("backspace"), "\x7f");
        assert_eq!(sent("space"), " ");
        assert_eq!(bytes("cmd-c", TermMode::NONE), None);
        assert_eq!(sent("cmd-left"), "\x01");
        assert_eq!(sent("cmd-backspace"), "\x15");
    }

    #[test]
    fn pastes_are_bracketed_when_asked_and_cannot_close_the_bracket() {
        let bracketed = paste_bytes("a\n\x1b[201~b", TermMode::BRACKETED_PASTE);
        assert_eq!(bracketed, b"\x1b[200~a\nb\x1b[201~".to_vec());
        assert_eq!(paste_bytes("a\nb", TermMode::NONE), b"a\rb".to_vec());
    }
}
