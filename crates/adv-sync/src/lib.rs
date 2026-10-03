//! Host threads exchange ownership, never a shared mutable device lock.
pub mod background;
pub mod input;
pub mod mailbox;
pub mod shared;
pub mod spsc;
mod wake;
pub use wake::WakeEvent;
