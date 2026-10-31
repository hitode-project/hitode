//! When a repeated close gesture forces the game (HOST-LIFE-8).
//!
//! Each press of the close button or Quit asks the game to close, as each
//! click on a Windows close box posts `WM_CLOSE`. Pressing again soon after
//! the press that asked forces the game, so a hung game can still be left.
//! Earlier presses carry no weight: a player who declined the game's own
//! prompt is asked again, not forced. A second press inside the double-click
//! interval is a double click, not a decision, and does nothing.
use std::time::{Duration, Instant};

/// How long after asking a repeated press forces the game.
pub const FORCE_WINDOW: Duration = Duration::from_secs(3);
/// The double-click interval counts for at most this long.
const DOUBLE_CLICK_LIMIT: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
pub enum Press {
    /// Ask the game to close and remember when.
    Ask,
    /// The same click repeated: nothing to do.
    Repeat,
    /// The player insists: force the game.
    Force,
}

/// Decides what a press at `now` does, given when a press last asked the game.
pub fn press(asked: Option<Instant>, now: Instant, double_click: Duration) -> Press {
    match asked.map(|asked| now.saturating_duration_since(asked)) {
        Some(since) if since < double_click.min(DOUBLE_CLICK_LIMIT) => Press::Repeat,
        Some(since) if since <= FORCE_WINDOW => Press::Force,
        _ => Press::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOUBLE: Duration = Duration::from_millis(500);

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn a_first_press_asks() {
        // spec: HOST-LIFE-8
        assert_eq!(press(None, Instant::now(), DOUBLE), Press::Ask);
    }

    #[test]
    fn a_double_click_only_asks_once() {
        let start = Instant::now();
        assert_eq!(press(Some(start), at(start, 200), DOUBLE), Press::Repeat);
    }

    #[test]
    fn pressing_again_soon_forces() {
        let start = Instant::now();
        assert_eq!(press(Some(start), at(start, 500), DOUBLE), Press::Force);
        assert_eq!(press(Some(start), at(start, 3000), DOUBLE), Press::Force);
    }

    #[test]
    fn a_press_after_the_window_asks_again() {
        // A player who declined the game's prompt is asked again, not forced.
        let start = Instant::now();
        assert_eq!(press(Some(start), at(start, 3001), DOUBLE), Press::Ask);
        assert_eq!(press(Some(start), at(start, 60_000), DOUBLE), Press::Ask);
    }

    #[test]
    fn a_long_double_click_setting_still_leaves_time_to_force() {
        let start = Instant::now();
        let slow = Duration::from_secs(5);
        assert_eq!(press(Some(start), at(start, 900), slow), Press::Repeat);
        assert_eq!(press(Some(start), at(start, 1000), slow), Press::Force);
    }
}
