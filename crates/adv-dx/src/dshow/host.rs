//! Assembly seams for Wine's files, notifications and guest allocation.
use super::*;

/// A source opened by file name, with the handle that keeps its sharing lock.
/// Contents read out through the guest come without one: their handle was
/// closed through the guest as it was opened.
pub type Opened = Box<
    dyn FnOnce(&mut Env<'_>, Result<(adv_device::media::Decoder, Option<u32>), u32>) -> Outcome,
>;
pub type ReadFile = Box<dyn FnOnce(&mut Env<'_>, Result<(Vec<u8>, Option<u32>), u32>) -> Outcome>;

#[derive(Clone, Copy)]
pub struct Host {
    pub allocator: crate::interop::GuestAllocator,
    pub open: fn(&mut Env<'_>, u32, Opened) -> Outcome,
    pub read: fn(&mut Env<'_>, u32, ReadFile) -> Outcome,
    pub close: fn(&mut Env<'_>, u32),
    pub thread: fn(&mut Env<'_>, u32) -> Outcome,
    pub event: fn(&mut Env<'_>) -> Result<u32, String>,
    pub signal: fn(&mut Env<'_>, u32, bool),
    pub post: fn(&mut Env<'_>, [u32; 4]),
}

pub(crate) fn close(env: &mut Env<'_>, handle: u32) {
    if let Some(host) = env.ext.try_get::<Host>() {
        (host.close)(env, handle);
    } else {
        env.sys.objects.close(handle);
    }
}
