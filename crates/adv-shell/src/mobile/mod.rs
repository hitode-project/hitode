//! Platform-neutral mobile input and lifecycle policy.
mod touch;
pub use touch::{TouchAction, client_point, magnification, position};
mod lifecycle;
pub use lifecycle::{Lifecycle, State};
mod gesture;
pub use gesture::{Button, Gesture, Gestures, TouchMode, mouse};
mod keys;
pub use keys::{key_message, overlay_scan_code};
mod layout;
pub use layout::{Insets, Layout, Occlusion, Rect, iphone_housing};
