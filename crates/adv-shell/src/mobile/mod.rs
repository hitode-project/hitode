//! Platform-neutral mobile input and lifecycle policy.
mod touch;
pub use touch::{Touch, TouchAction, position};
mod lifecycle;
pub use lifecycle::{Lifecycle, State};
mod gesture;
pub use gesture::{DRAG_PIXELS, Gesture, Gestures, LONG_PRESS_MS, SCROLL_PIXELS};
