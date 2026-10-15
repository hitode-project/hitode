//! Platform-neutral mobile input and lifecycle policy.
mod touch;
pub use touch::{Touch, TouchAction, position};
mod lifecycle;
pub use lifecycle::{Lifecycle, State};
