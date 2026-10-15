//! Software frames beyond the display rate (DEV-FRAME-9): a large frame
//! that follows the previous one within a refresh period holds the drawing
//! context's next empty PeekMessage until the vsync, or until a message,
//! timer or input arrives. Frames above the display rate are never shown.
use adv_machine::{CtxId, Env};

/// Frames closer together than this beat the 60 Hz presentation cap.
const PERIOD_US: u64 = 16_667;

#[derive(Default)]
struct FrameCap {
    /// When the last large frame was published or released.
    last_us: Option<u64>,
    /// The vsync the drawing context's next empty poll waits for.
    until: Option<(CtxId, u64)>,
}

/// A frame boundary published `damaged` of `total` pixels.
pub(crate) fn published(env: &mut Env<'_>, (damaged, total): (u64, u64)) {
    // Small updates (loading icons, cursors) never pace the loop.
    if damaged.saturating_mul(4) < total {
        return;
    }
    let pacer = env.sys.frame_pacer.clone();
    let now = env.sys.clock.now_us();
    let context = env.context;
    let cap = env.ext.get_or_default::<FrameCap>();
    let until = pacer
        .filter(|p| !p.input_held())
        .filter(|_| cap.last_us.is_some_and(|last| now < last + PERIOD_US))
        .and_then(|p| p.until_vsync())
        .map(|d| now + d.as_micros() as u64);
    cap.until = until.map(|until| (context, until));
    cap.last_us = Some(until.unwrap_or(now));
}

/// The vsync the current context's empty poll waits for, once.
pub(crate) fn take(env: &mut Env<'_>) -> Option<u64> {
    let now = env.sys.clock.now_us();
    let context = env.context;
    let cap = env.ext.try_get_mut::<FrameCap>()?;
    match cap.until.take() {
        Some((owner, until)) if owner == context && until > now => Some(until),
        other => {
            cap.until = other.filter(|&(owner, _)| owner != context);
            None
        }
    }
}
