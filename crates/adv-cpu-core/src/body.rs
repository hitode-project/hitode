//! Direct bodies: host functions that generated code calls in place of a
//! guest function (A64-CALL-2), and the guest memory access they share with
//! the full host-call path (NAT-BODY-17).
//!
//! A body is written once, generic over [`Mem`]. Generated code calls its
//! [`Window`] form: one page class read per 4 KiB page, then plain loads
//! and stores through the window. A page that form may not take, such as a
//! guard, file, shared or watched page, stops the body with [`Stop::Slow`]
//! before it writes anything, and the call runs again on the full path. That
//! path's [`GuestMem`] form completes every access a guest instruction
//! would, and stops with [`Stop::Fault`] only where the guest would fault.
//!
//! Bodies follow AAPCS64 (A64-CALL-2): x0 points at the [`BodyContext`],
//! w1 to w7 carry the inputs their binding names, and the result returns in
//! x0 (EDX:EAX) and x1 (ECX, with the status above it).

use crate::backend::{Guest, Prot};
use crate::memory::{EXEC, READ, SHARED, WRITE};

const PAGE_SHIFT: u32 = 12;
/// Body-page flags, separate from native two-page grants.
pub const ALIAS: u8 = 1 << 5;
pub const NO_ALIAS: u8 = 1 << 6;

/// Why a body stopped before finishing. Slow and Guest precede every write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A page the window form may not take: the full path runs the call.
    Slow,
    /// The guest would fault at `address`.
    Fault { address: u32, write: bool },
    /// Integer division by zero at the bound function entry.
    DivideByZero,
    /// An architectural trap at the bound function entry; the full path
    /// retains writes completed before the trapping arithmetic.
    Trap(u8),
    /// The function would wait, call guest code or request the server:
    /// its guest code or registered service handler runs instead (A64-CALL-6).
    Guest,
}

/// What a body leaves in the guest registers its binding declares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ret {
    /// EDX:EAX.
    pub value: u64,
    pub ecx: u32,
    /// The call completed; let the execution owner yield before continuing.
    pub yield_now: bool,
}

impl From<u32> for Ret {
    fn from(eax: u32) -> Self {
        Self {
            value: u64::from(eax),
            ecx: 0,
            yield_now: false,
        }
    }
}

impl From<u64> for Ret {
    fn from(value: u64) -> Self {
        Self {
            value,
            ecx: 0,
            yield_now: false,
        }
    }
}

/// Where a binding takes a body input from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The stack argument at this index: `[esp + 4 * n]` at the call.
    Stack(u8),
    /// A value fixed when this function binds.
    Const(u32),
    Eax,
    Ecx,
    Edx,
    /// SSE floating-point controls at the call.
    Mxcsr,
    /// MXCSR in the low half and the x87 control word in the high half.
    FloatControl,
}

/// Guest registers a body sets ([`DirectBody::outputs`]).
pub const EAX: u8 = 1;
pub const EDX: u8 = 2;
pub const ECX: u8 = 4;

/// The body stopped: run the call again on its full path.
pub const SLOW: u64 = 1 << 32;
/// The body completed: commit its return and ask the owner to yield.
pub const YIELD: u64 = 1 << 33;

/// Shared host storage and thread identity, captured before native entry.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Services {
    pub clock: *const std::ffi::c_void,
    pub epoch_us: u64,
    pub state: *const std::ffi::c_void,
    pub thread: u32,
    pub teb: u32,
    pub context: u32,
    pub thread_state: *const std::ffi::c_void,
    pub user_state: *const std::ffi::c_void,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            clock: std::ptr::null(),
            epoch_us: 0,
            state: std::ptr::null(),
            thread: 0,
            teb: 0,
            context: 0,
            thread_state: std::ptr::null(),
            user_state: std::ptr::null(),
        }
    }
}

/// What generated code passes in x0.
#[repr(C)]
#[derive(Debug)]
pub struct BodyContext {
    /// Host address of guest address 0 in the window.
    pub window: *mut u8,
    /// Private linear storage, for pages restricted only by window host protection.
    pub alias: *mut u8,
    /// A class byte per 4 KiB guest page. ALIAS selects private storage;
    /// NO_ALIAS confines overlays to the window.
    pub classes: *const u8,
    pub services: Services,
}

impl Default for BodyContext {
    fn default() -> Self {
        Self {
            window: std::ptr::null_mut(),
            alias: std::ptr::null_mut(),
            classes: std::ptr::null(),
            services: Services::default(),
        }
    }
}

/// x0 and x1 on return.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyResult {
    pub value: u64,
    /// ECX in the low half; [`SLOW`] and completed-call [`YIELD`] above it.
    pub extra: u64,
}

impl From<Result<Ret, Stop>> for BodyResult {
    fn from(result: Result<Ret, Stop>) -> Self {
        match result {
            Ok(ret) => Self {
                value: ret.value,
                extra: u64::from(ret.ecx) | if ret.yield_now { YIELD } else { 0 },
            },
            Err(_) => Self {
                value: 0,
                extra: SLOW,
            },
        }
    }
}

/// The window form of a body, which generated code calls.
pub type BodyFn =
    unsafe extern "C" fn(*const BodyContext, u32, u32, u32, u32, u32, u32, u32) -> BodyResult;

/// An exact memory operation that a constant-length call can expand inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    Move,
    Set,
    Compare,
}

/// A body as generated code calls it.
#[derive(Clone, Copy, Debug)]
pub struct DirectBody {
    pub call: BodyFn,
    /// An exact expansion for the first three inputs: destination, source or byte, length.
    pub inline: Option<Inline>,
    /// What w1 onwards carry: packed inputs followed by `None` padding.
    pub inputs: [Option<Input>; 7],
    /// The guest registers it sets: [`EAX`], [`EDX`], [`ECX`].
    pub outputs: u8,
    /// Argument bytes the function's return pops.
    pub pop: u16,
}

/// Cold preparation of fixed inputs from the bound function's guest bytes.
pub type BindInputs = fn(&dyn Guest, u32, &[u8]) -> Result<[Option<Input>; 7], String>;

/// A body: its window form and its full-path form.
#[derive(Clone, Copy)]
pub struct Body {
    pub direct: DirectBody,
    pub bind: Option<BindInputs>,
    pub full: fn(&mut GuestMem<'_>, [u32; 7]) -> Result<Ret, Stop>,
}

/// Defines `$name`, a [`Body`] running `$body`, a function generic over
/// [`Mem`] taking the inputs as `[u32; 7]`.
#[macro_export]
macro_rules! body {
    ($vis:vis $name:ident = $body:path, inputs: $inputs:expr, outputs: $outputs:expr, pop: $pop:expr) => {
        $crate::body!($vis $name = $body, inputs: $inputs, outputs: $outputs, pop: $pop, inline: None);
    };
    ($vis:vis $name:ident = $body:path, inputs: $inputs:expr, outputs: $outputs:expr, pop: $pop:expr, inline: $inline:expr) => {
        $vis const $name: $crate::body::Body = {
            #[allow(clippy::too_many_arguments)]
            unsafe extern "C" fn call(
                context: *const $crate::body::BodyContext,
                a: u32,
                b: u32,
                c: u32,
                d: u32,
                e: u32,
                f: u32,
                g: u32,
            ) -> $crate::body::BodyResult {
                // SAFETY: generated code passes the owner's live context.
                let mut window = unsafe { $crate::body::Window::new(&*context) };
                $body(&mut window, [a, b, c, d, e, f, g]).into()
            }
            let declared: &[$crate::body::Input] = $inputs;
            let mut inputs = [None; 7];
            let mut n = 0;
            while n < declared.len() {
                inputs[n] = Some(declared[n]);
                n += 1;
            }
            $crate::body::Body {
                direct: $crate::body::DirectBody {
                    call,
                    inline: $inline,
                    inputs,
                    outputs: $outputs,
                    pop: $pop,
                },
                bind: None,
                full: |memory, inputs| $body(memory, inputs),
            }
        };
    };
}

/// Guest memory as a body sees it. Ranges are checked once per 4 KiB page;
/// the pointers stay valid until the body returns. Overlapping input and
/// output ranges are the body's to order.
pub trait Mem {
    /// The window form retries fault-capable floating work before any write.
    const WINDOW: bool = false;

    /// Shared host state; a body never reaches the machine or its extensions.
    fn services(&self) -> Services {
        Services::default()
    }

    /// The `len` bytes at `at`, to read.
    fn input(&mut self, at: u32, len: u32) -> Result<*const u8, Stop>;
    /// The `len` bytes at `at`, holding their current bytes, to write.
    fn output(&mut self, at: u32, len: u32) -> Result<*mut u8, Stop>;
    /// The length in units of the string of `unit`-byte units at `at`,
    /// before its zero unit, which is readable too; or `max` when none of
    /// the first `max` units is zero, only those being readable.
    fn terminated(&mut self, at: u32, unit: u32, max: u32) -> Result<u32, Stop>;
    /// Check a body's read and write spans together. Overlapping spans share
    /// storage even when the full path cannot borrow the guest's bytes.
    fn spans<const N: usize>(
        &mut self,
        spans: [(u32, u32, bool); N],
    ) -> Result<[*mut u8; N], Stop> {
        let mut pointers = [std::ptr::null_mut(); N];
        for (i, (at, len, write)) in spans.into_iter().enumerate() {
            pointers[i] = if write {
                self.output(at, len)?
            } else {
                self.input(at, len)?.cast_mut()
            };
        }
        Ok(pointers)
    }

    /// Makes outputs visible to the guest. Bodies call it once, last.
    fn commit(&mut self) -> Result<(), Stop>;

    fn u32(&mut self, at: u32) -> Result<u32, Stop> {
        let bytes = self.input(at, 4)?;
        // SAFETY: `input` checked the four bytes.
        Ok(u32::from_le(unsafe {
            bytes.cast::<u32>().read_unaligned()
        }))
    }
}

/// The pages `[at, at + len)` spans, or `None` when it wraps.
fn pages(at: u32, len: u32) -> Option<std::ops::RangeInclusive<u32>> {
    let last = at.checked_add(len.checked_sub(1)?)?;
    Some(at >> PAGE_SHIFT..=last >> PAGE_SHIFT)
}

/// The window form: the page class table decides, as for checked accesses
/// (A64-MEM-7), and anything else is the full path's.
pub struct Window {
    window: *mut u8,
    alias: *mut u8,
    classes: *const u8,
    services: Services,
}

impl Window {
    /// # Safety
    /// `context` must describe the live window and class table.
    pub unsafe fn new(context: &BodyContext) -> Self {
        Self {
            window: context.window,
            alias: context.alias,
            classes: context.classes,
            services: context.services,
        }
    }

    fn class(&self, page: u32) -> u8 {
        // SAFETY: the table has a byte for every page of the 4 GiB space.
        unsafe { *self.classes.add(page as usize) }
    }

    fn check(&self, at: u32, len: u32, need: u8, deny: u8) -> Result<*mut u8, Stop> {
        let mut flags = 0;
        if len != 0 {
            let pages = pages(at, len).ok_or(Stop::Slow)?;
            for page in pages {
                let class = self.class(page);
                if class & (need | deny) != need {
                    return Err(Stop::Slow);
                }
                flags |= class;
            }
        }
        let base = if flags & ALIAS != 0 {
            if flags & NO_ALIAS != 0 {
                return Err(Stop::Slow);
            }
            self.alias
        } else {
            self.window
        };
        Ok(base.wrapping_add(at as usize))
    }
}

impl Mem for Window {
    const WINDOW: bool = true;
    fn services(&self) -> Services {
        self.services
    }

    fn input(&mut self, at: u32, len: u32) -> Result<*const u8, Stop> {
        self.check(at, len, READ, SHARED).map(<*mut u8>::cast_const)
    }

    fn output(&mut self, at: u32, len: u32) -> Result<*mut u8, Stop> {
        self.check(at, len, READ | WRITE, EXEC | SHARED)
    }

    fn terminated(&mut self, at: u32, unit: u32, max: u32) -> Result<u32, Stop> {
        let mut found = 0;
        while found < max {
            let start = at.wrapping_add(found * unit);
            let page = start >> PAGE_SHIFT;
            let mut classes = self.class(page);
            if classes & (READ | SHARED) != READ {
                return Err(Stop::Slow);
            }
            // Finish this page before checking the next one. Only a unit
            // starting in its final byte can require two pages.
            let room = ((((page + 1) << PAGE_SHIFT).wrapping_sub(start)) / unit).max(1);
            let count = room.min(max - found);
            let end = start.checked_add(count * unit - 1).ok_or(Stop::Slow)?;
            if end >> PAGE_SHIFT != page {
                let next = self.class(page + 1);
                if next & (READ | SHARED) != READ {
                    return Err(Stop::Slow);
                }
                classes |= next;
            }
            let base = if classes & ALIAS != 0 {
                if classes & NO_ALIAS != 0 {
                    return Err(Stop::Slow);
                }
                self.alias
            } else {
                self.window
            };
            let bytes = base.wrapping_add(start as usize).cast_const();
            // SAFETY: the pages holding these units are readable.
            let units = unsafe { std::slice::from_raw_parts(bytes, (count * unit) as usize) };
            if let Some(zero) = zero(units, unit) {
                return Ok(found + zero);
            }
            found += count;
        }
        Ok(max)
    }

    fn commit(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}

/// The index of the first zero `unit`-byte unit of `bytes`.
fn zero(bytes: &[u8], unit: u32) -> Option<u32> {
    match unit {
        1 => memchr(bytes),
        2 => {
            #[cfg(not(target_arch = "aarch64"))]
            let offset = 0;
            #[cfg(target_arch = "aarch64")]
            let offset = {
                use std::arch::aarch64::*;
                let mut offset = 0;
                // SAFETY: a vector stays inside the checked page span, and
                // unaligned vector loads are supported by AArch64.
                unsafe {
                    while offset + 16 <= bytes.len() {
                        let lanes = vld1q_u16(bytes.as_ptr().add(offset).cast());
                        if vmaxvq_u16(vceqzq_u16(lanes)) != 0 {
                            break;
                        }
                        offset += 16;
                    }
                }
                offset
            };
            bytes[offset..]
                .as_chunks::<2>()
                .0
                .iter()
                .position(|pair| *pair == [0, 0])
                .map(|at| ((offset / 2) + at) as u32)
        }
        _ => bytes
            .chunks_exact(unit as usize)
            .position(|unit| unit.iter().all(|&byte| byte == 0))
            .map(|at| at as u32),
    }
}

fn memchr(bytes: &[u8]) -> Option<u32> {
    // SAFETY: the range is a live slice.
    let found = unsafe { libc::memchr(bytes.as_ptr().cast(), 0, bytes.len()) };
    (!found.is_null()).then(|| (found as usize - bytes.as_ptr() as usize) as u32)
}

/// The full path's form, through the guest interface: it completes every
/// access an instruction would and copies where the guest's bytes cannot be
/// borrowed.
pub struct GuestMem<'a> {
    cpu: &'a mut dyn Guest,
    scratch: &'a mut Scratch,
    /// Buffers this call holds.
    used: usize,
    services: Services,
}

/// What the full path reuses from call to call.
#[derive(Default)]
pub struct Scratch {
    buffers: Vec<Vec<u8>>,
    /// Outputs copied into buffers: guest address and buffer.
    pending: Vec<(u32, usize, usize, usize)>,
}

impl<'a> GuestMem<'a> {
    pub fn new(cpu: &'a mut dyn Guest, scratch: &'a mut Scratch) -> Self {
        scratch.pending.clear();
        Self {
            cpu,
            scratch,
            used: 0,
            services: Services::default(),
        }
    }

    pub fn for_body(cpu: &'a mut dyn Guest, scratch: &'a mut Scratch, services: Services) -> Self {
        let mut mem = Self::new(cpu, scratch);
        mem.services = services;
        mem
    }

    /// The first byte of `[at, at + len)` that `prot` rules out.
    fn fault(&self, at: u32, len: u32, prot: Prot) -> Stop {
        let write = prot.contains(Prot::WRITE);
        let mut address = at;
        let end = u64::from(at) + u64::from(len);
        while u64::from(address) < end {
            let next = (u64::from(address) + 4096) & !0xfff;
            let chunk = (next.min(end) - u64::from(address)) as u32;
            if !self.cpu.accessible(address, chunk, prot) {
                break;
            }
            address = address.wrapping_add(chunk);
        }
        Stop::Fault { address, write }
    }

    fn buffer(&mut self, len: u32) -> (usize, *mut u8) {
        if self.used == self.scratch.buffers.len() {
            self.scratch.buffers.push(Vec::new());
        }
        let index = self.used;
        self.used += 1;
        let buffer = &mut self.scratch.buffers[index];
        buffer.clear();
        buffer.resize(len as usize, 0);
        (index, buffer.as_mut_ptr())
    }
}

impl Mem for GuestMem<'_> {
    fn services(&self) -> Services {
        self.services
    }

    fn input(&mut self, at: u32, len: u32) -> Result<*const u8, Stop> {
        if len == 0 {
            return Ok(std::ptr::NonNull::dangling().as_ptr());
        }
        if !self.cpu.accessible(at, len, Prot::READ) {
            return Err(self.fault(at, len, Prot::READ));
        }
        if let Some(bytes) = self.cpu.shared_bytes(at, len as usize) {
            return Ok(bytes.as_ptr());
        }
        let (index, pointer) = self.buffer(len);
        self.cpu
            .read(at, &mut self.scratch.buffers[index])
            .map_err(|fault| Stop::Fault {
                address: fault.addr,
                write: false,
            })?;
        Ok(pointer.cast_const())
    }

    fn output(&mut self, at: u32, len: u32) -> Result<*mut u8, Stop> {
        if len == 0 {
            return Ok(std::ptr::NonNull::dangling().as_ptr());
        }
        if !self.cpu.accessible(at, len, Prot::RW) {
            return Err(self.fault(at, len, Prot::RW));
        }
        if let Some(bytes) = self.cpu.writable_bytes(at, len as usize) {
            return Ok(bytes.as_mut_ptr());
        }
        let (index, pointer) = self.buffer(len);
        self.cpu
            .read(at, &mut self.scratch.buffers[index])
            .map_err(|fault| Stop::Fault {
                address: fault.addr,
                write: false,
            })?;
        self.scratch.pending.push((at, index, 0, len as usize));
        Ok(pointer)
    }

    fn spans<const N: usize>(
        &mut self,
        spans: [(u32, u32, bool); N],
    ) -> Result<[*mut u8; N], Stop> {
        let mut pointers = [std::ptr::null_mut(); N];
        let mut groups = std::array::from_fn::<_, N, _>(|i| i);
        for (i, &(at, len, write)) in spans.iter().enumerate() {
            let prot = if write { Prot::RW } else { Prot::READ };
            if len != 0 && !self.cpu.accessible(at, len, prot) {
                return Err(self.fault(at, len, prot));
            }
            pointers[i] = if len == 0 {
                std::ptr::NonNull::dangling().as_ptr()
            } else if write {
                self.cpu
                    .writable_bytes(at, len as usize)
                    .map_or(std::ptr::null_mut(), |bytes| bytes.as_mut_ptr())
            } else {
                self.cpu
                    .shared_bytes(at, len as usize)
                    .map_or(std::ptr::null_mut(), |bytes| bytes.as_ptr().cast_mut())
            };
            for j in 0..i {
                let (other, size, _) = spans[j];
                if len != 0
                    && size != 0
                    && u64::from(at) < u64::from(other) + u64::from(size)
                    && u64::from(other) < u64::from(at) + u64::from(len)
                {
                    let old = groups[i];
                    let new = groups[j];
                    for group in &mut groups {
                        if *group == old {
                            *group = new;
                        }
                    }
                }
            }
        }
        for group in 0..N {
            if !(0..N).any(|i| groups[i] == group && pointers[i].is_null()) {
                continue;
            }
            let mut begin = u64::MAX;
            let mut end = 0;
            for (i, &(at, len, _)) in spans.iter().enumerate() {
                if groups[i] == group {
                    begin = begin.min(u64::from(at));
                    end = end.max(u64::from(at) + u64::from(len));
                }
            }
            let (index, pointer) = self.buffer((end - begin) as u32);
            for (i, &(at, len, write)) in spans.iter().enumerate() {
                if groups[i] != group {
                    continue;
                }
                let offset = (u64::from(at) - begin) as usize;
                self.cpu
                    .read(
                        at,
                        &mut self.scratch.buffers[index][offset..offset + len as usize],
                    )
                    .map_err(|fault| Stop::Fault {
                        address: fault.addr,
                        write: false,
                    })?;
                // SAFETY: every member lies inside this group's shared buffer.
                pointers[i] = unsafe { pointer.add(offset) };
                if write {
                    self.scratch.pending.push((at, index, offset, len as usize));
                }
            }
        }
        Ok(pointers)
    }

    fn terminated(&mut self, at: u32, unit: u32, max: u32) -> Result<u32, Stop> {
        let mut found = 0;
        let mut chunk = [0u8; 4096];
        while found < max {
            let start = at.wrapping_add(found * unit);
            let room =
                (((((start >> PAGE_SHIFT) + 1) << PAGE_SHIFT).wrapping_sub(start)) / unit).max(1);
            let count = room.min(max - found);
            let len = count * unit;
            if !self.cpu.accessible(start, len, Prot::READ) {
                return Err(self.fault(start, len, Prot::READ));
            }
            let bytes = if let Some(bytes) = self.cpu.shared_bytes(start, len as usize) {
                bytes
            } else {
                let bytes = &mut chunk[..len as usize];
                self.cpu.read(start, bytes).map_err(|fault| Stop::Fault {
                    address: fault.addr,
                    write: false,
                })?;
                bytes
            };
            if let Some(zero) = zero(bytes, unit) {
                return Ok(found + zero);
            }
            found += count;
        }
        Ok(max)
    }

    fn commit(&mut self) -> Result<(), Stop> {
        for &(at, index, offset, len) in &self.scratch.pending {
            self.cpu
                .write(at, &self.scratch.buffers[index][offset..offset + len])
                .map_err(|fault| Stop::Fault {
                    address: fault.addr,
                    write: true,
                })?;
        }
        self.scratch.pending.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(classes: &[(u32, u8)], bytes: &mut [u8]) -> (Window, Vec<u8>) {
        let mut table = vec![0u8; 1 << 20];
        for &(page, class) in classes {
            table[page as usize] = class;
        }
        let window = Window {
            window: bytes.as_mut_ptr(),
            alias: bytes.as_mut_ptr(),
            classes: table.as_ptr(),
            services: Services::default(),
        };
        (window, table)
    }

    // spec: A64-CALL-4
    #[test]
    fn the_window_takes_only_pages_an_instruction_could() {
        let mut bytes = vec![0u8; 3 << 12];
        let (mut w, _table) = window(
            &[(0, READ | WRITE), (1, READ), (2, READ | WRITE | EXEC)],
            &mut bytes,
        );
        assert!(w.input(0, 2 << 12).is_ok());
        assert!(w.output(0, 1 << 12).is_ok());
        assert_eq!(w.output(0xff0, 0x20), Err(Stop::Slow), "a read-only page");
        assert_eq!(w.output(2 << 12, 4), Err(Stop::Slow), "a watched page");
        assert_eq!(w.input(3 << 12, 1), Err(Stop::Slow), "an unmapped page");
        assert_eq!(w.input(u32::MAX, 2), Err(Stop::Slow), "a wrapping range");
        assert!(w.input(3 << 12, 0).is_ok(), "no bytes");
    }

    // spec: A64-CALL-4
    #[test]
    fn strings_end_at_their_zero_unit_across_pages() {
        let mut bytes = vec![b'a'; 2 << 12];
        bytes[0x1004] = 0;
        bytes[0x1006] = 0;
        bytes[0x1007] = 0;
        let (mut w, _table) = window(&[(0, READ), (1, READ)], &mut bytes);
        assert_eq!(w.terminated(0xffe, 1, u32::MAX), Ok(6));
        assert_eq!(w.terminated(0xffe, 2, u32::MAX), Ok(4));
        assert_eq!(w.terminated(0xffe, 1, 3), Ok(3));
        let mut bytes = vec![b'a'; 2 << 12];
        let (mut w, _table) = window(&[(0, READ)], &mut bytes);
        assert_eq!(w.terminated(0xffe, 1, u32::MAX), Err(Stop::Slow));
        assert_eq!(w.terminated(0xffe, 1, 2), Ok(2), "only units it reads");
    }
    // spec: A64-CALL-4, A64-MEM-8
    #[test]
    fn body_aliases_take_only_private_unwatched_pages() {
        let mut window_bytes = vec![0; 8192];
        let mut alias_bytes = vec![7; 8192];
        let (mut mem, mut table) = window(
            &[(0, READ | WRITE | ALIAS), (1, READ | WRITE)],
            &mut window_bytes,
        );
        mem.alias = alias_bytes.as_mut_ptr();
        // SAFETY: both checked pages use ordinary private alias storage.
        assert_eq!(unsafe { *mem.input(4090, 12).unwrap() }, 7);
        let output = mem.output(4090, 12).unwrap();
        // SAFETY: the checked output spans the same private alias pages.
        unsafe { std::ptr::write_bytes(output, 9, 12) };
        assert_eq!(&alias_bytes[4090..4102], &[9; 12]);
        assert_eq!(&window_bytes[4090..4102], &[0; 12]);
        table[1] |= NO_ALIAS;
        assert_eq!(mem.input(4090, 12), Err(Stop::Slow));
        table[0] |= EXEC;
        assert_eq!(mem.output(0, 1), Err(Stop::Slow));
    }
}
