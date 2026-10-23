//! Assembly services for private guest storage and memory DC interoperation.
use adv_abi::{Env, Outcome};
use adv_device::surface::PixelFormat;

/// Private DirectX allocations; memory freed through a Windows API uses that API instead.
#[derive(Clone, Copy)]
pub struct GuestAllocator {
    pub allocate: fn(&mut Env<'_>, u32) -> Option<u32>,
    pub free: fn(&mut Env<'_>, u32),
}

/// Real guest-visible storage, kept alive by the frontend's surface lease.
#[derive(Clone, Copy)]
pub struct DcSurface {
    /// Common software surface identity, when the storage belongs to that store.
    pub surface: Option<u32>,
    pub pixels: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub format: PixelFormat,
}

pub type DcCreated = Box<dyn FnOnce(&mut Env<'_>, Result<u32, String>) -> Outcome>;
pub type DcFinished = Box<dyn FnOnce(&mut Env<'_>, Result<(), String>) -> Outcome>;

/// Operations may call guest Windows interfaces and complete after calls or waits.
/// The frontend finishes GPU work/readback before create and uploads after flush.
/// Release ends GDI ownership; discard handles abandoned contexts/process teardown
/// without running guest code. Assemblies own any deferred teardown in that case.
#[derive(Clone, Copy)]
pub struct MemoryDc {
    pub create: fn(&mut Env<'_>, DcSurface, DcCreated) -> Outcome,
    pub flush: fn(&mut Env<'_>, u32, DcSurface, DcFinished) -> Outcome,
    pub release: fn(&mut Env<'_>, u32, DcFinished) -> Outcome,
    pub discard: fn(&mut Env<'_>, u32),
}
