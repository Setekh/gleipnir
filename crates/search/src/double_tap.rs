//! Recognizes a modifier key tapped twice on its own, which is how IntelliJ opens Search
//! Everywhere. Only a clean tap counts: pressing Shift to type a capital letter, or with another
//! modifier, never triggers it.

use std::time::{Duration, Instant};

/// Two taps further apart than this are two separate taps.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(400);

#[derive(Debug, Default)]
pub struct DoubleTap {
    /// The modifier is down, and nothing else has happened since it went down.
    clean_press: bool,
    held: bool,
    last_tap: Option<Instant>,
}

impl DoubleTap {
    /// Reports a change in modifiers. `only_ours` is whether the watched modifier is the only one
    /// down, and `ours` whether it's down at all. Returns true when the second tap completes.
    pub fn modifiers_changed(&mut self, ours: bool, only_ours: bool, now: Instant) -> bool {
        if ours && !self.held {
            self.held = true;
            self.clean_press = only_ours;
            return false;
        }
        if ours && !only_ours {
            // Another modifier joined in, as in Shift+Cmd: not a tap.
            self.clean_press = false;
            self.last_tap = None;
            return false;
        }
        if !ours && self.held {
            self.held = false;
            if !self.clean_press {
                self.last_tap = None;
                return false;
            }
            let fired = self
                .last_tap
                .is_some_and(|last| now.duration_since(last) <= DOUBLE_TAP_WINDOW);
            self.last_tap = if fired { None } else { Some(now) };
            return fired;
        }
        false
    }

    /// Reports any other key going down, which breaks a tap in progress.
    pub fn key_pressed(&mut self) {
        self.clean_press = false;
        self.last_tap = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(detector: &mut DoubleTap, at: Instant) -> bool {
        detector.modifiers_changed(true, true, at);
        detector.modifiers_changed(false, false, at + Duration::from_millis(40))
    }

    #[test]
    fn two_quick_taps_fire_once() {
        let mut d = DoubleTap::default();
        let t = Instant::now();
        assert!(!tap(&mut d, t));
        assert!(tap(&mut d, t + Duration::from_millis(200)));
        // A third tap starts over rather than firing again.
        assert!(!tap(&mut d, t + Duration::from_millis(400)));
    }

    #[test]
    fn slow_taps_do_not_fire() {
        let mut d = DoubleTap::default();
        let t = Instant::now();
        tap(&mut d, t);
        assert!(!tap(&mut d, t + Duration::from_millis(900)));
        // But that slow tap counts as the first of a new pair.
        assert!(tap(&mut d, t + Duration::from_millis(1100)));
    }

    #[test]
    fn typing_a_capital_in_between_breaks_it() {
        let mut d = DoubleTap::default();
        let t = Instant::now();
        tap(&mut d, t);
        d.modifiers_changed(true, true, t + Duration::from_millis(100));
        d.key_pressed();
        assert!(!d.modifiers_changed(false, false, t + Duration::from_millis(150)));
    }

    #[test]
    fn a_key_after_the_first_tap_breaks_it() {
        let mut d = DoubleTap::default();
        let t = Instant::now();
        tap(&mut d, t);
        d.key_pressed();
        assert!(!tap(&mut d, t + Duration::from_millis(150)));
    }

    #[test]
    fn another_modifier_with_it_is_not_a_tap() {
        let mut d = DoubleTap::default();
        let t = Instant::now();
        tap(&mut d, t);
        d.modifiers_changed(true, true, t + Duration::from_millis(100));
        d.modifiers_changed(true, false, t + Duration::from_millis(110));
        assert!(!d.modifiers_changed(false, false, t + Duration::from_millis(150)));
    }

    #[test]
    fn pressing_while_another_modifier_is_down_is_not_a_tap() {
        let mut d = DoubleTap::default();
        let t = Instant::now();
        d.modifiers_changed(true, false, t);
        d.modifiers_changed(false, false, t);
        assert!(!tap(&mut d, t + Duration::from_millis(100)));
    }
}
