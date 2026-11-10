//! The mouse, told to a program that asked for it, as xterm tells it: clicks, drags and the wheel,
//! at the cell under the pointer, in the SGR form where the program asked for that and the older
//! one of single bytes otherwise.

use alacritty_terminal::term::TermMode;

/// What the mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Moved, with a button held if there is one.
    Move(Option<MouseButton>),
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }
}

/// Modifiers held, as the report carries them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// Whether the program wants to hear about the mouse at all.
pub fn wants_mouse(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// The bytes telling the program of `action` at a cell (from 0), or none where it didn't ask to
/// hear of that kind of thing.
pub fn report(
    action: MouseAction,
    column: usize,
    row: usize,
    modifiers: MouseModifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !wants_mouse(mode) {
        return None;
    }
    let (mut code, released) = match action {
        MouseAction::Press(button) => (button.code(), false),
        MouseAction::Release(button) => (button.code(), true),
        MouseAction::Move(button) => {
            let heard = match button {
                Some(_) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
                None => mode.contains(TermMode::MOUSE_MOTION),
            };
            if !heard {
                return None;
            }
            (32 + button.map_or(3, MouseButton::code), false)
        }
        MouseAction::WheelUp => (64, false),
        MouseAction::WheelDown => (65, false),
    };
    code += 4 * u8::from(modifiers.shift)
        + 8 * u8::from(modifiers.alt)
        + 16 * u8::from(modifiers.control);
    let (x, y) = (column + 1, row + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if released { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    // The old form has a byte for each, so it can't say where past column 223, and a release
    // doesn't say which button.
    if x > 223 || y > 223 {
        return None;
    }
    let code = if released { 3 + (code & !3) } else { code };
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + code,
        32 + x as u8,
        32 + y as u8,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICKS_SGR: TermMode = TermMode::MOUSE_REPORT_CLICK.union(TermMode::SGR_MOUSE);

    fn sgr(action: MouseAction, mode: TermMode) -> Option<String> {
        report(action, 4, 9, MouseModifiers::default(), mode).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn nothing_is_told_to_a_program_that_didnt_ask() {
        assert_eq!(sgr(MouseAction::WheelUp, TermMode::NONE), None);
        assert_eq!(
            sgr(MouseAction::Press(MouseButton::Left), TermMode::SGR_MOUSE),
            None
        );
    }

    #[test]
    fn the_wheel_and_clicks_in_sgr_form() {
        assert_eq!(
            sgr(MouseAction::WheelUp, CLICKS_SGR).as_deref(),
            Some("\x1b[<64;5;10M")
        );
        assert_eq!(
            sgr(MouseAction::WheelDown, CLICKS_SGR).as_deref(),
            Some("\x1b[<65;5;10M")
        );
        assert_eq!(
            sgr(MouseAction::Press(MouseButton::Left), CLICKS_SGR).as_deref(),
            Some("\x1b[<0;5;10M")
        );
        assert_eq!(
            sgr(MouseAction::Release(MouseButton::Right), CLICKS_SGR).as_deref(),
            Some("\x1b[<2;5;10m")
        );
        let ctrl = MouseModifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            report(MouseAction::WheelUp, 0, 0, ctrl, CLICKS_SGR),
            Some(b"\x1b[<80;1;1M".to_vec())
        );
    }

    #[test]
    fn moves_are_told_only_as_asked() {
        let held = MouseAction::Move(Some(MouseButton::Left));
        assert_eq!(sgr(held, CLICKS_SGR), None);
        let drags = CLICKS_SGR | TermMode::MOUSE_DRAG;
        assert_eq!(sgr(held, drags).as_deref(), Some("\x1b[<32;5;10M"));
        assert_eq!(sgr(MouseAction::Move(None), drags), None);
        let all = CLICKS_SGR | TermMode::MOUSE_MOTION;
        assert_eq!(
            sgr(MouseAction::Move(None), all).as_deref(),
            Some("\x1b[<35;5;10M")
        );
    }

    #[test]
    fn the_old_form_in_single_bytes() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let none = MouseModifiers::default();
        assert_eq!(
            report(MouseAction::Press(MouseButton::Left), 4, 9, none, mode),
            Some(vec![0x1b, b'[', b'M', 32, 37, 42])
        );
        assert_eq!(
            report(MouseAction::Release(MouseButton::Left), 4, 9, none, mode),
            Some(vec![0x1b, b'[', b'M', 35, 37, 42])
        );
        assert_eq!(
            report(MouseAction::WheelUp, 300, 0, none, mode),
            None,
            "past what a byte can say"
        );
    }
}
