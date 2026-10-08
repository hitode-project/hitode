//! Exact-PC recovery for canonical checkpoint version 1.
//!
//! The successful native path touches no TLS. One chain-wide context supplies
//! only a raw CpuState pointer, window bounds, and a fixed pending record. The
//! signal path never borrows a Reservation or inspects mutable page metadata;
//! its ordinary Rust caller classifies the returned fault with DirectFaultView.
//! This initial lane requires an anonymous direct window and synchronous SIGSEGV.
use super::{capture, install};
use crate::CpuState;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

unsafe extern "C" {
    fn adv_posix_fault_set_result(context: *mut c_void, value: u32);
}

const MAX_SITES: usize = 65_536;
const MAX_REGISTRATIONS: usize = 4096;
const PAGE_BYTES: usize = 4096;

/// Every site uses canonical checkpoint version 1: all architectural state,
/// flags, and fuel are materialized, eip is guest_pc, and fault_addr is the EA.
/// No host register rematerialization or interrupted operation is performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Site {
    pub host_pc: usize,
    pub recovery_pc: usize,
    pub guest_pc: u32,
    pub next_pc: u32,
    pub width: u8,
    pub write: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fault {
    pub site: Site,
    pub address: u32,
    pub host_address: usize,
}

struct Snapshot {
    sites: Vec<Site>,
}
struct Region {
    id: u64,
    start: usize,
    end: usize,
    sites: Vec<Site>,
}
struct Writer {
    next_id: u64,
    regions: Vec<Region>,
}
struct Registry {
    snapshot: AtomicPtr<Snapshot>,
    readers: AtomicUsize,
    writer: Mutex<Writer>,
}

impl Registry {
    const fn new() -> Self {
        Self {
            snapshot: AtomicPtr::new(std::ptr::null_mut()),
            readers: AtomicUsize::new(0),
            writer: Mutex::new(Writer {
                next_id: 1,
                regions: Vec::new(),
            }),
        }
    }

    fn read(&self) -> Reader<'_> {
        self.readers.fetch_add(1, Ordering::SeqCst);
        Reader {
            registry: self,
            snapshot: self.snapshot.load(Ordering::SeqCst),
        }
    }

    fn lookup(&self, pc: usize) -> Option<Site> {
        let reader = self.read();
        if reader.snapshot.is_null() {
            return None;
        }
        // SAFETY: publication is immutable, and this reader remains counted
        // until the Site has been copied out of the snapshot.
        let sites = unsafe { &(*reader.snapshot).sites };
        sites
            .binary_search_by_key(&pc, |site| site.host_pc)
            .ok()
            .map(|index| sites[index])
    }

    fn publish(&self, start: usize, len: usize, sites: Vec<Site>) -> Result<u64, String> {
        let end = start.checked_add(len).ok_or("inline code range overflow")?;
        if start == 0
            || len == 0
            || sites.len() > MAX_SITES
            || sites
                .windows(2)
                .any(|pair| pair[0].host_pc >= pair[1].host_pc)
            || sites.iter().any(|site| {
                site.host_pc < start
                    || site.host_pc >= end
                    || site.recovery_pc < start
                    || site.recovery_pc >= end
                    || !matches!(site.width, 1 | 2 | 4)
            })
        {
            return Err("invalid inline exact-PC recovery table".into());
        }
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if writer.regions.len() >= MAX_REGISTRATIONS
            || writer
                .regions
                .iter()
                .map(|region| region.sites.len())
                .sum::<usize>()
                + sites.len()
                > MAX_SITES
        {
            return Err("inline recovery registry limit exceeded".into());
        }
        if writer
            .regions
            .iter()
            .any(|region| start < region.end && region.start < end)
        {
            return Err("overlapping inline code registrations".into());
        }
        let id = writer.next_id;
        writer.next_id = id.checked_add(1).ok_or("inline registration id overflow")?;
        writer.regions.push(Region {
            id,
            start,
            end,
            sites,
        });
        self.replace(&writer);
        Ok(id)
    }

    fn unregister(&self, id: u64) {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = writer.regions.iter().position(|region| region.id == id) {
            writer.regions.swap_remove(index);
            self.replace(&writer);
        }
    }

    fn replace(&self, writer: &Writer) {
        let mut sites: Vec<_> = writer
            .regions
            .iter()
            .flat_map(|region| region.sites.iter().copied())
            .collect();
        sites.sort_unstable_by_key(|site| site.host_pc);
        let next = if sites.is_empty() {
            std::ptr::null_mut()
        } else {
            Box::into_raw(Box::new(Snapshot { sites }))
        };
        let old = self.snapshot.swap(next, Ordering::SeqCst);
        if old.is_null() {
            return;
        }
        // All counter and pointer operations are SeqCst. A reader that can
        // load old must increment before this swap, and must copy its Site
        // before decrementing. A zero count observed after the swap therefore
        // makes old unreachable. Readers starting after that observation load
        // next. Only normal-context writers wait; a signal reader never waits,
        // locks, allocates, or enters a mutable container. The writer mutex
        // serializes both replacement and reclamation, including retirement.
        while self.readers.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
        unsafe { drop(Box::from_raw(old)) };
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        // SAFETY: exclusive ownership excludes readers and writers. The global
        // registry never drops; this also lets isolated synthetic tests own one.
        let snapshot = *self.snapshot.get_mut();
        if !snapshot.is_null() {
            unsafe { drop(Box::from_raw(snapshot)) };
        }
    }
}
struct Reader<'a> {
    registry: &'a Registry,
    snapshot: *mut Snapshot,
}
impl Drop for Reader<'_> {
    fn drop(&mut self) {
        self.registry.readers.fetch_sub(1, Ordering::SeqCst);
    }
}
static REGISTRY: Registry = Registry::new();

/// Unpublishes its exact-PC table before the owner releases executable storage.
/// This token does not own or extend the lifetime of the native code itself.
pub(crate) struct Registration {
    id: u64,
}
impl Registration {
    /// # Safety
    /// The finalized code range must remain executable and unchanged until
    /// this registration drops. Every site is the exact scalar instruction PC;
    /// the emitter separately validates its actual 1/2/4-byte machine scalar
    /// and its same-frame epilogue returning Exit::Fallback. That epilogue must
    /// neither commit the faulting MOV nor use its uncommitted result. State is
    /// fully canonical before the instruction, including fuel and fault_addr.
    /// A store has naturally aligned private backing wholly inside one guest
    /// and host page, with all-or-none fault behavior. Code and window owners
    /// must keep them alive for each entire chain, and prevent execution or an
    /// in-flight recovery before dropping this token or reusing code addresses.
    pub(crate) unsafe fn publish(
        start: usize,
        len: usize,
        sites: Vec<Site>,
    ) -> Result<Self, String> {
        REGISTRY.publish(start, len, sites).map(|id| Self { id })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        REGISTRY.unregister(self.id);
    }
}

struct Context {
    state: *mut CpuState,
    window: usize,
    window_len: usize,
    guest_base: u32,
    permissions: *const u8,
    dispatching: Cell<bool>,
    pending: Cell<Option<Fault>>,
}
impl Context {
    unsafe fn fault(&self, site: Site, host_address: usize) -> Option<Fault> {
        if self.state.is_null() || !matches!(site.width, 1 | 2 | 4) {
            return None;
        }
        // SAFETY: the caller keeps CpuState live and aligned through the chain.
        // Raw field reads avoid borrowing it across native/helper execution.
        let guest_pc = unsafe { std::ptr::addr_of!((*self.state).eip).read() };
        let address = unsafe { std::ptr::addr_of!((*self.state).fault_addr).read() };
        if guest_pc != site.guest_pc {
            return None;
        }
        let width = usize::from(site.width);
        let last_address = address.checked_add(u32::from(site.width) - 1)?;
        let offset = address.checked_sub(self.guest_base)? as usize;
        let end = offset.checked_add(width)?;
        let start = self.window.checked_add(offset)?;
        let last = start.checked_add(width - 1)?;
        if end > self.window_len
            || host_address < start
            || host_address > last
            || (site.write
                && (address as usize % width != 0
                    || start % width != 0
                    || address as usize / PAGE_BYTES != last_address as usize / PAGE_BYTES
                    || start / PAGE_BYTES != last / PAGE_BYTES))
        {
            return None;
        }
        if !self.permissions.is_null() {
            // The native chain owns this immutable permission snapshot. Its
            // EXEC bit is the code-write barrier. Revocations exit the chain;
            // file promotions may leave conservative SHARED bits, which the
            // emitter excludes. Inline sites use 4 KiB host pages only.
            let permission = unsafe { self.permissions.add(offset / PAGE_BYTES).read() };
            use crate::memory::{EXEC, READ, WRITE};
            let accessible = if site.write {
                permission & (READ | WRITE | EXEC) == READ | WRITE
            } else {
                permission & READ != 0
            };
            if accessible {
                return None;
            }
        }
        Some(Fault {
            site,
            address,
            host_address,
        })
    }
}
thread_local! {
    static ACTIVE: Cell<*const Context> = const { Cell::new(std::ptr::null()) };
}
struct ContextScope(*const Context);
impl Drop for ContextScope {
    fn drop(&mut self) {
        ACTIVE.with(|slot| slot.set(self.0));
    }
}
struct DispatchScope<'a>(&'a Cell<bool>);
impl Drop for DispatchScope<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Installs a minimal scoped TLS context once for the entire native chain.
/// Nested scopes and unwinding restore the previous context. Checked helpers
/// may change mappings: no RefCell or frozen DirectFaultView is borrowed here.
///
/// # Safety
/// state is live, aligned, exclusively accessed, and free of Rust references
/// while native code can mutate it or a signal can inspect it. The whole host
/// anonymous direct window and every executing Registration remain alive until the chain and
/// its same-frame recovery epilogue return. Native sites meet the version 1
/// checkpoint/store contract documented on Registration::publish. A claimed
/// fault exits this chain with Exit::Fallback before any further guest effect;
/// the ordinary caller then classifies it using a fresh immutable fault view,
/// accounts for it, and interprets only the uncommitted faulting MOV.
#[cfg(test)]
pub(crate) unsafe fn with_context<R>(
    state: *mut CpuState,
    window: usize,
    window_len: usize,
    guest_base: u32,
    run: impl FnOnce() -> R,
) -> Result<(R, Option<Fault>), String> {
    unsafe { with_protection(state, window, window_len, guest_base, None, run) }
}

/// Production variant with a chain-pinned immutable native permission table.
/// In addition to `with_context`'s contract, inline sites must use a 4 KiB host
/// page plan and `permissions` must reflect its active native grants/barriers.
pub(crate) unsafe fn with_protection<R>(
    state: *mut CpuState,
    window: usize,
    window_len: usize,
    guest_base: u32,
    permissions: Option<&[u8]>,
    run: impl FnOnce() -> R,
) -> Result<(R, Option<Fault>), String> {
    if state.is_null()
        || !(state as usize).is_multiple_of(std::mem::align_of::<CpuState>())
        || window == 0
        || window_len == 0
        || window.checked_add(window_len).is_none()
        || window_len as u64 > (1u64 << 32) - u64::from(guest_base)
        || permissions.is_some_and(|table| table.len() < window_len.div_ceil(PAGE_BYTES))
    {
        return Err("invalid inline native-chain context".into());
    }
    install()?;
    let context = Context {
        state,
        window,
        window_len,
        guest_base,
        permissions: permissions.map_or(std::ptr::null(), |table| table.as_ptr()),
        dispatching: Cell::new(false),
        pending: Cell::new(None),
    };
    let previous = ACTIVE.with(|slot| slot.replace(&context));
    let scope = ContextScope(previous);
    let result = run();
    drop(scope);
    Ok((result, context.pending.get()))
}

/// Called only after the POSIX boundary filters synchronous SIGSEGV causes.
pub(super) unsafe fn dispatch(
    signal: i32,
    pc: usize,
    host_address: usize,
    access: usize,
    host_context: *mut c_void,
) -> usize {
    if signal != libc::SIGSEGV {
        return 0;
    }
    // Exact immutable lookup precedes any TLS or CpuState access, and its reader
    // is released with a copied Site before context validation or capture.
    let Some(site) = REGISTRY.lookup(pc) else {
        return 0;
    };
    // Linux ARM64 may omit ESR. In that case the exact, validated scalar
    // instruction supplies the direction; a reported mismatch is never claimed.
    if (access != usize::MAX && access != usize::from(site.write)) || host_context.is_null() {
        return 0;
    }
    ACTIVE
        .try_with(|slot| {
            let pointer = slot.get();
            if pointer.is_null() {
                return 0;
            }
            // SAFETY: the installed stack context stays live until native return.
            let context = unsafe { &*pointer };
            if context.pending.get().is_some() || context.dispatching.replace(true) {
                return 0;
            }
            let _scope = DispatchScope(&context.dispatching);
            let Some(fault) = (unsafe { context.fault(site, host_address) }) else {
                return 0;
            };
            // Even canonical checkpoints must reject unsupported context extensions.
            // The platform adapter preserves the native snapshot via sigreturn; no
            // register rematerialization or snapshot storage is needed in this lane.
            if unsafe { capture(host_context, pc) }.is_none() {
                return 0;
            }
            context.pending.set(Some(fault));
            // The emitter validates a complete frame-restoration suffix that
            // neither needs live SSA values nor overwrites the ABI result.
            unsafe { adv_posix_fault_set_result(host_context, crate::Exit::Fallback as u32) };
            site.recovery_pc
        })
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "inline_tests.rs"]
mod tests;
