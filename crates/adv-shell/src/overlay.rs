//! Input routing between the native game view and the overlay webview above it.
//!
//! The launcher draws its in-game UI in a transparent webview stacked over the
//! game view in the same window. The webview publishes an [`OverlayInput`]:
//! the rectangles that hold interactive UI, whether a modal layer covers the
//! game, and whether it wants the keyboard. The host view consults it on every
//! pointer press and on focus changes:
//!
//! - a press inside a region, or any press while modal, goes to the overlay;
//!   every other press goes to the game;
//! - a press keeps its target until it is released, even if the regions change
//!   or the pointer leaves them, so a drag never splits between the two views;
//! - each pointer is routed on its own (multi-touch), and moves of a pointer
//!   that is not pressed (hover) follow the same region test without capture;
//! - the keyboard goes to the overlay while it wants it, otherwise to the game.
//!
//! Coordinates are the webview's logical pixels (CSS pixels at zoom 1), with
//! the origin at its top-left corner and y growing downwards. The host view
//! must convert its own event coordinates into that space.

/// An axis-aligned rectangle in overlay coordinates; `x`/`y` is the top-left
/// corner. Points on the right and bottom edges are outside.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// What the overlay currently claims. The default claims nothing: every
/// press and the keyboard go to the game.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverlayInput {
    /// Interactive overlay UI; presses inside any of them go to the overlay.
    pub regions: Vec<Rect>,
    /// A modal layer covers the game: every press goes to the overlay.
    pub modal: bool,
    /// The overlay holds a focused control that takes key input.
    pub wants_keyboard: bool,
    /// The overlay draws something; otherwise the host may hide it.
    pub visible: bool,
}

impl OverlayInput {
    /// Everything goes to the overlay; used while no game is running.
    pub fn exclusive() -> Self {
        Self {
            regions: Vec::new(),
            modal: true,
            wants_keyboard: true,
            visible: true,
        }
    }

    /// The view a new press or a hover at `x`, `y` belongs to.
    pub fn target_at(&self, x: f64, y: f64) -> Target {
        if self.modal || self.regions.iter().any(|r| r.contains(x, y)) {
            Target::Overlay
        } else {
            Target::Game
        }
    }

    /// The view that should be first responder for key input.
    pub fn keyboard(&self) -> Target {
        if self.wants_keyboard {
            Target::Overlay
        } else {
            Target::Game
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Overlay,
    Game,
}

/// Identifies one pointer for the duration of a press: the mouse, or a touch
/// as numbered by the platform.
pub type PointerId = u64;

/// Routes pointer events by the latest [`OverlayInput`], keeping each press
/// with the view it started in.
#[derive(Debug, Default)]
pub struct Router {
    input: OverlayInput,
    pressed: Vec<(PointerId, Target)>,
}

impl Router {
    pub fn new(input: OverlayInput) -> Self {
        Self {
            input,
            pressed: Vec::new(),
        }
    }

    pub fn input(&self) -> &OverlayInput {
        &self.input
    }

    /// Replaces the overlay's claim. Presses already in progress keep their
    /// targets; the keyboard target is re-read through [`Router::keyboard`].
    pub fn set_input(&mut self, input: OverlayInput) {
        self.input = input;
    }

    /// A pointer went down. A pointer that is already down (a missed release)
    /// is re-routed as a new press.
    pub fn down(&mut self, pointer: PointerId, x: f64, y: f64) -> Target {
        let target = self.input.target_at(x, y);
        match self.pressed.iter_mut().find(|(id, _)| *id == pointer) {
            Some(press) => press.1 = target,
            None => self.pressed.push((pointer, target)),
        }
        target
    }

    /// A pointer moved: a pressed pointer stays with its target, a hovering
    /// one follows the regions.
    pub fn moved(&self, pointer: PointerId, x: f64, y: f64) -> Target {
        self.captured(pointer)
            .unwrap_or_else(|| self.input.target_at(x, y))
    }

    /// A pointer was released or its press cancelled; returns the view that
    /// receives the release.
    pub fn up(&mut self, pointer: PointerId, x: f64, y: f64) -> Target {
        match self.pressed.iter().position(|(id, _)| *id == pointer) {
            Some(index) => self.pressed.swap_remove(index).1,
            None => self.input.target_at(x, y),
        }
    }

    /// The target a pressed pointer is held by, if it is pressed.
    pub fn captured(&self, pointer: PointerId) -> Option<Target> {
        self.pressed
            .iter()
            .find(|(id, _)| *id == pointer)
            .map(|press| press.1)
    }

    pub fn keyboard(&self) -> Target {
        self.input.keyboard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn button() -> OverlayInput {
        OverlayInput {
            regions: vec![rect(10.0, 10.0, 40.0, 20.0)],
            ..Default::default()
        }
    }

    // spec: HOST-OVL-2
    #[test]
    fn rect_edges_are_half_open() {
        let r = rect(10.0, 10.0, 40.0, 20.0);
        assert!(r.contains(10.0, 10.0));
        assert!(r.contains(49.9, 29.9));
        assert!(!r.contains(50.0, 20.0));
        assert!(!r.contains(20.0, 30.0));
        assert!(!rect(0.0, 0.0, 0.0, 10.0).contains(0.0, 5.0));
    }

    // spec: HOST-OVL-2
    #[test]
    fn presses_follow_regions_and_modal() {
        let mut input = button();
        assert_eq!(input.target_at(20.0, 20.0), Target::Overlay);
        assert_eq!(input.target_at(100.0, 100.0), Target::Game);
        input.modal = true;
        assert_eq!(input.target_at(100.0, 100.0), Target::Overlay);
        assert_eq!(OverlayInput::default().target_at(20.0, 20.0), Target::Game);
        assert_eq!(
            OverlayInput::exclusive().target_at(0.0, 0.0),
            Target::Overlay
        );
    }

    // spec: HOST-OVL-5
    #[test]
    fn keyboard_follows_wants_keyboard() {
        let mut input = button();
        assert_eq!(input.keyboard(), Target::Game);
        input.wants_keyboard = true;
        assert_eq!(input.keyboard(), Target::Overlay);
    }

    // spec: HOST-OVL-3
    #[test]
    fn a_press_keeps_its_target_until_release() {
        let mut router = Router::new(button());
        assert_eq!(router.down(0, 20.0, 20.0), Target::Overlay);
        assert_eq!(router.moved(0, 300.0, 300.0), Target::Overlay);
        // The button disappears mid-press; the release still goes to the overlay.
        router.set_input(OverlayInput::default());
        assert_eq!(router.up(0, 300.0, 300.0), Target::Overlay);
        assert_eq!(router.captured(0), None);
        assert_eq!(router.moved(0, 20.0, 20.0), Target::Game);
    }

    // spec: HOST-OVL-3
    #[test]
    fn a_game_press_ignores_a_modal_that_opens_under_it() {
        let mut router = Router::new(button());
        assert_eq!(router.down(0, 200.0, 200.0), Target::Game);
        router.set_input(OverlayInput {
            modal: true,
            ..button()
        });
        assert_eq!(router.moved(0, 20.0, 20.0), Target::Game);
        assert_eq!(router.up(0, 20.0, 20.0), Target::Game);
        assert_eq!(router.down(0, 200.0, 200.0), Target::Overlay);
    }

    // spec: HOST-OVL-4
    #[test]
    fn pointers_are_routed_independently() {
        let mut router = Router::new(button());
        assert_eq!(router.down(1, 200.0, 200.0), Target::Game);
        assert_eq!(router.down(2, 20.0, 20.0), Target::Overlay);
        assert_eq!(router.moved(1, 20.0, 20.0), Target::Game);
        assert_eq!(router.up(2, 200.0, 200.0), Target::Overlay);
        assert_eq!(router.up(1, 20.0, 20.0), Target::Game);
    }

    // spec: HOST-OVL-3
    #[test]
    fn a_repeated_down_reroutes_and_an_unknown_up_uses_regions() {
        let mut router = Router::new(button());
        router.down(0, 20.0, 20.0);
        assert_eq!(router.down(0, 200.0, 200.0), Target::Game);
        assert_eq!(router.up(0, 20.0, 20.0), Target::Game);
        assert_eq!(router.up(7, 20.0, 20.0), Target::Overlay);
    }
}
