//! IntelliJ's way to add carets from the keyboard: tap Option (Ctrl on Linux), then press it again
//! and keep holding it, and the arrow keys add a caret above or below for as long as it's held.

use gpui::{App, KeyBinding, Modifiers};
use std::time::{Duration, Instant};

/// The second press must come this soon after the first tap.
const WINDOW: Duration = Duration::from_millis(400);

#[derive(Debug, Default)]
pub(crate) struct CaretGesture {
    held: bool,
    /// The modifier went down on its own and nothing else happened while it was held.
    clean: bool,
    last_tap: Option<Instant>,
    /// Held after a tap: arrows add carets.
    pub active: bool,
}

/// The modifier the gesture uses on this platform.
pub(crate) fn gesture_modifier(modifiers: &Modifiers) -> (bool, bool) {
    let ours = if cfg!(target_os = "macos") {
        modifiers.alt
    } else {
        modifiers.control
    };
    let others = modifiers.shift
        || modifiers.platform
        || modifiers.function
        || if cfg!(target_os = "macos") {
            modifiers.control
        } else {
            modifiers.alt
        };
    (ours, ours && !others)
}

impl CaretGesture {
    /// Follows the modifiers. Returns whether `active` changed.
    pub fn modifiers_changed(&mut self, ours: bool, only_ours: bool, now: Instant) -> bool {
        let was = self.active;
        if ours && !self.held {
            self.held = true;
            self.clean = only_ours;
            self.active = only_ours
                && self
                    .last_tap
                    .is_some_and(|tap| now.duration_since(tap) <= WINDOW);
            self.last_tap = None;
        } else if ours && !only_ours && !self.active {
            self.clean = false;
        } else if !ours && self.held {
            self.held = false;
            if !self.active && self.clean {
                self.last_tap = Some(now);
            }
            self.active = false;
        }
        was != self.active
    }

    /// Any other key while the modifier is down, unless the gesture is on, cancels the tap.
    pub fn key_pressed(&mut self) {
        if !self.active {
            self.clean = false;
            self.last_tap = None;
        }
    }
}

/// While the gesture's on, the arrows add carets instead of what they'd do with the modifier.
pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some("Editor && cloning_carets");
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("alt-up", crate::actions::AddCursorAbove, context),
        KeyBinding::new("alt-down", crate::actions::AddCursorBelow, context),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-up", crate::actions::AddCursorAbove, context),
        KeyBinding::new("ctrl-down", crate::actions::AddCursorBelow, context),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(g: &mut CaretGesture, at: Instant) {
        g.modifiers_changed(true, true, at);
    }

    fn release(g: &mut CaretGesture, at: Instant) {
        g.modifiers_changed(false, false, at);
    }

    #[test]
    fn tap_then_hold_turns_it_on_until_released() {
        let mut g = CaretGesture::default();
        let t = Instant::now();
        press(&mut g, t);
        release(&mut g, t + Duration::from_millis(80));
        assert!(!g.active);
        press(&mut g, t + Duration::from_millis(200));
        assert!(g.active);
        // Arrows while it's held don't end it.
        g.key_pressed();
        assert!(g.active);
        release(&mut g, t + Duration::from_secs(2));
        assert!(!g.active);
    }

    #[test]
    fn a_slow_second_press_or_a_key_between_does_not_count() {
        let mut g = CaretGesture::default();
        let t = Instant::now();
        press(&mut g, t);
        release(&mut g, t);
        press(&mut g, t + Duration::from_secs(1));
        assert!(!g.active);
        release(&mut g, t + Duration::from_secs(1));

        press(&mut g, t + Duration::from_secs(2));
        g.key_pressed();
        release(&mut g, t + Duration::from_secs(2));
        press(&mut g, t + Duration::from_millis(2100));
        assert!(!g.active, "the first press was used for a shortcut");
    }

    #[test]
    fn with_another_modifier_it_is_a_shortcut_not_a_tap() {
        let mut g = CaretGesture::default();
        let t = Instant::now();
        g.modifiers_changed(true, false, t);
        release(&mut g, t);
        press(&mut g, t + Duration::from_millis(100));
        assert!(!g.active);
    }
}
