use std::collections::{BTreeMap, BTreeSet};
use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic as M, OpKind, Register as R,
};

use crate::linking::{NativeContext, Slot, Table};
use crate::{CompileStrategy, CpuState, EXEC, Exit, Memory, READ, Region, WRITE};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
mod contract;
pub(crate) mod coverage;
mod diagnostics;
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
mod fault;
#[cfg(target_arch = "aarch64")]
pub(crate) mod fp_environment;
mod gir;
mod guards;
mod invariants;
mod ir_counts;
mod loops;
mod mmx;
mod nest;
mod nest_emit;
// Retain the analysis tests; observable exits cannot discard guest state.
#[cfg(test)]
mod register_liveness;
mod safepoints;
mod scalar;
mod shadow;
mod simd;
mod sse;
mod string;
mod vector;
mod write_summary;
pub(crate) mod x87;

type Entry = unsafe extern "C" fn(*mut NativeContext, *mut u8) -> u32;

/// Integration tests enable the feature through the package's self dev-dependency:
/// their release library dependency does not receive `cfg(test)`.
fn verifier_enabled() -> bool {
    cfg!(any(test, debug_assertions, feature = "jit-verifier"))
        || std::env::var("ADV_JIT_VERIFY").is_ok_and(|mode| !matches!(mode.as_str(), "" | "0"))
}

/// Borrowed controls for one bounded native entry and its linked regions.
pub(crate) struct NativeRun<'a> {
    pub rows: Option<&'a crate::rows::RowPool>,
    pub links: Option<&'a Table>,
    pub stop: u32,
    pub interrupt: &'a std::sync::atomic::AtomicBool,
    pub shadow: Option<&'a mut crate::linking::shadow::Stack>,
}

pub(crate) struct Compiled {
    /// Immutable absolute checked entry, initialized before publication. Code
    /// mappings need not be in ADRP/rel32 reach of the polling body.
    _poll_target: Option<Box<usize>>,
    perf_ranges: Vec<crate::perf_map::Registration>,
    // Keeps executable pages alive until the entry pointer can no longer be called.
    module: Option<JITModule>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    unwind: Vec<crate::host_fault::UnwindRegistration>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_sites: Vec<crate::host_fault::FaultSite>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_recipes: Vec<crate::host_fault::recipe::Recipe>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    body_len: usize,
    entry: Entry,
    pub tail_entry: usize,
    pub pages: Vec<(usize, u64)>,
    input: Snapshot,
    window_base: u32,
    window_len: usize,
    pub entries: BTreeSet<u32>,
    /// Entry forms deliberately left to the interpreter after code generation.
    pub excluded_entries: BTreeMap<u32, iced_x86::Code>,
    pub code_size: usize,
    pub instruction_count: usize,
    #[cfg(test)]
    pub gir_ssa_instructions: usize,
    #[cfg(test)]
    pub gir_opaque_instructions: usize,
    pub decoded_bytes: usize,
    ir_counts: Option<ir_counts::Summary>,
    /// x87 instructions called from generated code by address.
    _x87: Box<[crate::floating::NativeX87]>,
    _nests: Box<[crate::rows::Descriptor]>,
    /// Owner-side recency sampled at Rust entries, never read by generated code.
    // Controller-only metadata; native descriptors never reference this cell.
    pub cache_stamp: std::cell::Cell<u64>,
    #[cfg(all(test, target_arch = "x86_64"))]
    disasm: String,
    #[cfg(all(test, target_arch = "x86_64"))]
    versioned_loops: usize,
    #[cfg(all(test, target_arch = "x86_64"))]
    vectorized_loops: usize,
    #[cfg(all(test, target_arch = "x86_64"))]
    invariant_body: bool,
}

impl Drop for Compiled {
    fn drop(&mut self) {
        self.perf_ranges.clear();
        // SAFETY: the entry pointer is private, run borrows self, and no generated
        // code can still be executing while this exclusive drop runs.
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        self.unwind.clear();
        unsafe { self.module.take().unwrap().free_memory() };
    }
}

impl Compiled {
    pub(crate) fn record_ir_counts(&self, service: std::time::Duration) {
        if let Some(counts) = &self.ir_counts {
            eprintln!("compile-ir: {}", counts.json(self.code_size, service));
        }
    }

    pub(crate) fn profile(&self) -> Option<&Profile> {
        self.input.profile.as_deref()
    }

    pub(crate) fn shared_profile(&self) -> Option<Arc<Profile>> {
        self.input.profile.clone()
    }
    pub(crate) fn parallel_entry(&self) -> crate::parallel::Entry {
        assert!(!self.input.shadow_returns);
        self.entry
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    pub(crate) fn storage_bytes(&self) -> Result<(usize, usize), String> {
        unsafe { crate::jit_memory::code_storage_bytes([self.tail_entry, self.entry as usize]) }
    }
    pub fn valid(&self, memory: &Memory) -> bool {
        memory.base == self.window_base
            && memory.len == self.window_len
            && self
                .pages
                .iter()
                .all(|&(page, version)| memory.versions.get(page) == Some(&version))
    }
    /// A write to mutable data on an executable page is not necessarily a code
    /// change. On the cold invalidation path, compare the immutable input before
    /// discarding native code. Mapping/permission changes still invalidate, as do
    /// all byte changes inside the captured region and its instruction tail.
    pub fn revalidate(&mut self, memory: &Memory) -> bool {
        if !self.input.same_code(memory) {
            return false;
        }
        for (page, version) in &mut self.pages {
            *version = memory.versions[*page];
        }
        true
    }

    pub fn run(
        &self,
        state: &mut CpuState,
        memory: &mut Memory,
        bridge: &mut crate::trap::Bridge<'_>,
        mut controls: NativeRun<'_>,
    ) -> (u32, u64) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        crate::jit_memory_macos::synchronize_execution();
        #[cfg(target_arch = "aarch64")]
        let _fp_environment = fp_environment::Guard::enter();
        // Authored/private callers may reuse compiled code with another Memory
        // window. Establish that owner's code barriers before its first entry.
        for &(page, _) in &self.pages {
            memory.watch_code(self.window_base + (page * 4096) as u32, 4096);
        }
        bridge.memory = memory;
        assert!(!self.input.shadow_returns || controls.shadow.is_some());
        let shadow = controls
            .shadow
            .as_deref_mut()
            .map_or(std::ptr::null_mut(), |stack| {
                stack.clear();
                stack as *mut _
            });
        let (slots, mask) = controls.links.map_or((std::ptr::null(), 0), Table::parts);
        // Helpers may promote file pages before this chain returns. Keep one
        // immutable metadata snapshot; raw Memory permissions remain free to
        // change without cloning a megabyte on each first-touch promotion.
        let permissions = memory.native_permissions();
        let mut context = NativeContext {
            state,
            rows: controls
                .rows
                .map_or(std::ptr::null(), |pool| pool as *const _),
            written: 0,
            perms: permissions.as_ptr(),
            bridge: (bridge as *mut crate::trap::Bridge<'_>).cast(),
            interrupt: controls.interrupt,
            slots,
            mask,
            remaining: crate::linking::CHAIN_LIMIT,
            stop: controls.stop,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_reads: 0,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            recovery_slot: std::ptr::null(),
            linked_entries: 0,
            shadow,
        };
        // SAFETY: generated function has this ABI; exclusive borrows keep its pointers
        // stable. Checked accesses retain their guards; verified ordinary reads
        // may use the synchronized OS protection and the owner's fault table.
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        if !self.fault_sites.is_empty()
            && state.eip == self.fault_recipes[0].entry_pc
            && memory.native_fault_reads_available()
            && let Some(window) = memory.native_fault_window()
            && let Some(result) = self.run_fault_owner(
                state,
                &mut context,
                window,
                memory.base,
                memory.ptr.as_ptr(),
            )
        {
            if let Some(stack) = controls.shadow.as_deref_mut() {
                stack.clear();
            }
            return (result, context.linked_entries);
        }
        let memory_pointer = memory.ptr.as_ptr();
        let result = unsafe { (self.entry)(&mut context, memory_pointer) };
        if let Some(stack) = controls.shadow {
            stack.clear();
        }
        (result, context.linked_entries)
    }

    // Keep TLS/metadata validation and its large stack frame out of the normal
    // checked entry. This opt-in proof lane is not used for default memory.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    #[inline(never)]
    fn run_fault_owner(
        &self,
        state: &mut CpuState,
        context: &mut NativeContext,
        window: (usize, usize),
        guest_base: u32,
        pointer: *mut u8,
    ) -> Option<u32> {
        let region = unsafe {
            crate::host_fault::CodeRegion::with_recipes(
                self,
                self.tail_entry,
                self.body_len,
                &self.fault_sites,
                &self.fault_recipes,
            )
        }
        .ok()?;
        let recovery_slot = std::cell::Cell::new(0);
        context.recovery_slot = &recovery_slot;
        let result = unsafe {
            crate::host_fault::with_native_session(
                state,
                &[region],
                window,
                guest_base,
                &recovery_slot,
                |state| {
                    context.state = state;
                    context.fault_reads = 1;
                    (self.entry)(context, pointer)
                },
            )
        };
        context.recovery_slot = std::ptr::null();
        match result {
            Ok((result, _)) => Some(result),
            Err(_) => {
                context.fault_reads = 0;
                None
            }
        }
    }
}

/// Counters written by Fast code that may be recompiled: executed blocks,
/// including those entered through region links, and the latest target of
/// each indirect call site. Plain racy stores; only a hint for tier-up.
#[derive(Debug, Default)]
pub(crate) struct Profile {
    heat: AtomicU64,
    calls: [AtomicU32; CALL_SITES],
}

const CALL_SITES: usize = 8;

impl Profile {
    pub(crate) fn heat(&self) -> u64 {
        self.heat.load(Ordering::Relaxed)
    }

    pub(crate) fn call_targets(&self) -> Vec<u32> {
        let mut targets: Vec<u32> = self
            .calls
            .iter()
            .map(|cell| cell.load(Ordering::Relaxed))
            .filter(|&target| target != 0)
            .collect();
        targets.sort_unstable();
        targets.dedup();
        targets
    }
}

/// An immutable, Send compilation input. No guest allocation is borrowed or
/// retained: only bounded instruction bytes and their page stamps are copied.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    parallel: bool,
    pub region: Box<Region>,
    pub(crate) base: u32,
    len: usize,
    /// Guest address of `bytes[0]`: the region start, or a callee below it.
    origin: u32,
    bytes: Vec<u8>,
    pages: Vec<PageStamp>,
    // Published reachable regions compare only their decoded instruction bytes.
    // A pending snapshot still stamps the full bounded window conservatively.
    covered: Option<Vec<(usize, usize)>>,
    pub(crate) shadow_returns: bool,
    /// Present for Fast code that may tier up; owned by the compiled result.
    pub(crate) profile: Option<Arc<Profile>>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    native_fault_reads: bool,
}

#[derive(Clone, Debug)]
struct PageStamp {
    page: usize,
    version: u64,
    permissions: u8,
    mapped: bool,
}

impl Snapshot {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn origin(&self) -> u32 {
        self.origin
    }

    /// Slice an immutable input without consulting active guest memory. Keep
    /// tail/negative-page stamps and the optional hardware-read proof capability.
    pub(crate) fn fragment(&self, region: Region) -> Result<Self, String> {
        let offset = region
            .start
            .checked_sub(self.origin)
            .ok_or("fragment starts before its snapshot")? as usize;
        if region.len == 0 || offset + region.len > self.bytes.len() {
            return Err("fragment extends beyond its snapshot".into());
        }
        let end =
            (offset + region.len + if region.complete_tail { 14 } else { 0 }).min(self.bytes.len());
        let first = (region.start - self.base) as usize / 4096;
        let last = ((self.origin - self.base) as usize + end) / 4096;
        Ok(Self {
            parallel: self.parallel,
            origin: region.start,
            region: Box::new(region),
            base: self.base,
            len: self.len,
            bytes: self.bytes[offset..end].to_vec(),
            pages: self
                .pages
                .iter()
                .filter(|stamp| (first..=last).contains(&stamp.page))
                .cloned()
                .collect(),
            covered: None,
            shadow_returns: self.shadow_returns,
            profile: None,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            native_fault_reads: self.native_fault_reads,
        })
    }

    pub fn capture(region: &Region, memory: &mut Memory) -> Result<Self, String> {
        if region.len == 0 || region.len > 16 * 1024 || !memory.executable(region.start, region.len)
        {
            return Err("region must contain 1..16384 executable bytes".into());
        }
        // Capture is owner-only. Selective grants are revoked before sampling;
        // conservative native peers already forbid all executable stores.
        memory.quiesce_code_observation();
        let mut region = region.clone();
        let origin = callee_origin(&mut region, memory);
        let extent = (region.start - origin) as usize + region.len;
        memory.watch_code(origin, extent + if region.complete_tail { 15 } else { 0 });
        let mut bytes = memory
            .read_data(origin, (region.start - origin) as usize + region.len)?
            .to_vec();
        let first = (origin - memory.base) as usize / 4096;
        let last = ((region.start - memory.base) as usize + region.len - 1) / 4096;
        let mut pages: BTreeSet<usize> = (first..=last).collect();
        if region.complete_tail {
            for offset in bytes.len()..bytes.len() + 14 {
                let Some(address) = u64::from(origin)
                    .checked_add(offset as u64)
                    .and_then(|address| u32::try_from(address).ok())
                else {
                    break;
                };
                // Even an inaccessible tail page can determine whether a final
                // instruction is truncated; retain its negative dependency.
                if let Ok(range) = memory.range(address, 1) {
                    pages.insert(range.start / 4096);
                }
                if !memory.executable(address, 1) {
                    break;
                }
                bytes.push(memory.read_data(address, 1)?[0]);
            }
            if let Some(address) = u64::from(origin)
                .checked_add(bytes.len() as u64)
                .and_then(|address| u32::try_from(address).ok())
                && let Ok(range) = memory.range(address, 1)
            {
                pages.insert(range.start / 4096);
            }
        }
        Ok(Self {
            parallel: memory.is_parallel(),
            region: Box::new(region),
            origin,
            base: memory.base,
            len: memory.len,
            bytes,
            covered: None,
            shadow_returns: false,
            profile: None,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            native_fault_reads: memory.native_fault_reads_available(),
            pages: pages
                .into_iter()
                .map(|page| PageStamp {
                    page,
                    version: memory.versions[page],
                    permissions: memory.perms[page],
                    mapped: memory.mapped[page],
                })
                .collect(),
        })
    }

    pub fn valid(&self, memory: &Memory) -> bool {
        self.parallel == memory.is_parallel()
            && memory.base == self.base
            && memory.len == self.len
            && self.pages.iter().all(|stamp| {
                memory.versions.get(stamp.page) == Some(&stamp.version)
                    && memory.perms.get(stamp.page) == Some(&stamp.permissions)
                    && memory.mapped.get(stamp.page) == Some(&stamp.mapped)
            })
    }

    fn same_code(&self, memory: &Memory) -> bool {
        memory.base == self.base
            && memory.len == self.len
            && self.pages.iter().all(|stamp| {
                memory.perms.get(stamp.page) == Some(&stamp.permissions)
                    && memory.mapped.get(stamp.page) == Some(&stamp.mapped)
            })
            && self.covered.as_ref().map_or_else(
                || {
                    memory
                        .read_data(self.origin, self.bytes.len())
                        .is_ok_and(|bytes| bytes == self.bytes)
                },
                |ranges| {
                    ranges.iter().all(|&(offset, len)| {
                        memory
                            .read_data(self.origin + offset as u32, len)
                            .is_ok_and(|bytes| bytes == &self.bytes[offset..offset + len])
                    })
                },
            )
    }

    fn with_coverage(
        &self,
        instructions: &BTreeMap<u32, Instruction>,
        pages: &BTreeSet<usize>,
    ) -> Self {
        let mut input = self.clone();
        if self.region.reachable {
            input.covered = Some(
                instructions
                    .values()
                    .map(|instruction| {
                        (
                            (instruction.ip32() - self.origin) as usize,
                            instruction.len(),
                        )
                    })
                    .collect(),
            );
            input.pages.retain(|stamp| pages.contains(&stamp.page));
        }
        input
    }

    /// Admit tiny direct leaves already fully captured in the caller's window.
    /// Do not extend capture or relax any boundary except the admitted entry.
    pub(crate) fn direct_leaf_callees(&self) -> Vec<u32> {
        if !self.region.reachable {
            return Vec::new();
        }
        let start = u64::from(self.region.start);
        let end = start + self.region.len as u64;
        let slice = |pc: u32| {
            (start..end).contains(&u64::from(pc)).then(|| {
                &self.bytes[(pc - self.origin) as usize..(end - u64::from(self.origin)) as usize]
            })
        };
        let mut pending = self.region.entries.clone();
        pending.push(self.region.start);
        let mut visited = BTreeSet::new();
        let mut candidates = BTreeSet::new();
        while let Some(pc) = pending.pop() {
            let Some(bytes) = slice(pc) else { continue };
            let mut decoder = Decoder::with_ip(32, bytes, u64::from(pc), DecoderOptions::NONE);
            while decoder.can_decode() {
                let pc = decoder.ip() as u32;
                if self.region.frontier.contains(&pc) || !visited.insert(pc) {
                    break;
                }
                let i = decoder.decode();
                if i.is_invalid() {
                    break;
                }
                if i.op0_kind() == OpKind::NearBranch32 {
                    if i.flow_control() == FlowControl::Call {
                        candidates.insert(i.near_branch32());
                    } else {
                        pending.push(i.near_branch32());
                    }
                }
                if !matches!(
                    i.flow_control(),
                    FlowControl::Next
                        | FlowControl::Call
                        | FlowControl::IndirectCall
                        | FlowControl::ConditionalBranch
                ) {
                    break;
                }
            }
        }
        let mut factory = iced_x86::InstructionInfoFactory::new();
        candidates
            .into_iter()
            .filter(|&target| {
                if visited.contains(&target) {
                    return false;
                }
                let Some(bytes) = slice(target) else {
                    return false;
                };
                let mut decoder =
                    Decoder::with_ip(32, bytes, u64::from(target), DecoderOptions::NONE);
                for _ in 0..8 {
                    if !decoder.can_decode()
                        || (decoder.ip() != u64::from(target)
                            && self.region.frontier.contains(&(decoder.ip() as u32)))
                    {
                        return false;
                    }
                    let i = decoder.decode();
                    if i.is_invalid()
                        || i.has_lock_prefix()
                        || i.has_rep_prefix()
                        || i.has_repne_prefix()
                    {
                        return false;
                    }
                    if i.code() == iced_x86::Code::Retnd {
                        return true;
                    }
                    if i.flow_control() != FlowControl::Next
                        || !supported(&i)
                        || !matches!(
                            i.mnemonic(),
                            M::Nop
                                | M::Mov
                                | M::Movzx
                                | M::Movsx
                                | M::Add
                                | M::Sub
                                | M::And
                                | M::Or
                                | M::Xor
                                | M::Cmp
                                | M::Test
                                | M::Inc
                                | M::Dec
                                | M::Neg
                                | M::Not
                                | M::Paddb
                        )
                    {
                        return false;
                    }
                    let info = factory.info(&i);
                    if !info.used_memory().is_empty()
                        || info
                            .used_registers()
                            .iter()
                            .any(|r| !r.register().is_gpr() && !r.register().is_xmm())
                        || info.used_registers().iter().any(|r| {
                            r.register().full_register32() == R::ESP
                                && matches!(
                                    r.access(),
                                    iced_x86::OpAccess::Write
                                        | iced_x86::OpAccess::CondWrite
                                        | iced_x86::OpAccess::ReadWrite
                                        | iced_x86::OpAccess::ReadCondWrite
                                )
                        })
                    {
                        return false;
                    }
                }
                false
            })
            .collect()
    }

    /// Straight-line code of each captured callee through its return. Any
    /// other transfer, or undecodable bytes, stays a transfer out of the unit.
    pub(crate) fn callee_code(&self, decoded: &BTreeMap<u32, Instruction>) -> Vec<Instruction> {
        const LIMIT: usize = 64;
        let mut code = BTreeMap::new();
        for &callee in &self.region.callees {
            let Some(offset) = callee.checked_sub(self.origin) else {
                continue;
            };
            let Some(bytes) = self.bytes.get(offset as usize..) else {
                continue;
            };
            let mut decoder = Decoder::with_ip(32, bytes, u64::from(callee), DecoderOptions::NONE);
            for _ in 0..LIMIT {
                let pc = decoder.ip() as u32;
                if !decoder.can_decode()
                    || decoded.contains_key(&pc)
                    || code.contains_key(&pc)
                    || self.region.frontier.contains(&pc)
                {
                    break;
                }
                let i = decoder.decode();
                if i.is_invalid()
                    || !matches!(i.flow_control(), FlowControl::Next | FlowControl::Return)
                {
                    break;
                }
                code.insert(pc, i);
                if i.flow_control() == FlowControl::Return {
                    break;
                }
            }
        }
        code.into_values().collect()
    }

    pub(crate) fn page_index(&self, address: u32) -> Option<usize> {
        let offset = address.checked_sub(self.base)? as usize;
        (offset < self.len).then_some(offset / 4096)
    }

    fn version(&self, page: usize) -> u64 {
        self.pages
            .iter()
            .find(|stamp| stamp.page == page)
            .expect("captured instruction dependency")
            .version
    }
}

/// Keep the callees that share one bounded executable window with the region,
/// nearest first, and return the window's lowest address.
fn callee_origin(region: &mut Region, memory: &Memory) -> u32 {
    const WINDOW: u64 = 16 * 1024;
    let start = region.start;
    let end = u64::from(start) + region.len as u64;
    let mut origin = start;
    if !region.reachable {
        region.callees.clear();
    }
    region.callees.sort_unstable_by(|a, b| b.cmp(a));
    region.callees.dedup();
    region.callees.retain(|&callee| {
        if callee >= start {
            return u64::from(callee) < end;
        }
        let low = origin.min(callee);
        let fits = end - u64::from(low) <= WINDOW && memory.executable(low, (start - low) as usize);
        if fits {
            origin = low;
        }
        fits
    });
    origin
}

pub(crate) fn compile(
    region: &Region,
    memory: &mut Memory,
    shadow_returns: bool,
) -> Result<Compiled, String> {
    let mut input = Snapshot::capture(region, memory)?;
    input.shadow_returns = shadow_returns;
    let started = std::time::Instant::now();
    let compiled = compile_snapshot(&input)?;
    compiled.record_ir_counts(started.elapsed());
    Ok(compiled)
}

pub(crate) fn compile_snapshot(snapshot: &Snapshot) -> Result<Compiled, String> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let _write = crate::jit_memory_macos::WriteScope::new();
    let ir_counts =
        ir_counts::enabled().then(|| std::cell::RefCell::new(ir_counts::Counts::default()));
    let region = &snapshot.region;
    let bytes = &snapshot.bytes;
    let crate::gir::cfg::Native {
        instructions,
        pages,
        callee_code,
        mut entries,
        returns,
    } = crate::gir::cfg::Native::decode(snapshot)?;
    // x87 stays in the region: guarded scalar lowering handles the exact subset,
    // and the remaining cases call the interpreter's transaction in place.
    let x87: Box<[crate::floating::NativeX87]> = instructions
        .values()
        .filter(|i| !supported(i) && crate::floating::native_x87(i))
        .map(|i| {
            let start = (i.ip32() - snapshot.origin) as usize;
            // Same FOP derivation as the interpreter's decoded entry.
            let opcode = bytes[start..start + i.len()]
                .windows(2)
                .find(|bytes| (0xd8..=0xdf).contains(&bytes[0]))
                .map(|bytes| ((bytes[0] as u16 & 7) << 8) | bytes[1] as u16);
            crate::floating::NativeX87 {
                instruction: *i,
                opcode,
                fast: crate::floating::fast::classify(i),
            }
        })
        .collect();
    let x87_segments = x87::segments(&instructions, &region.entries);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let fault_pc = snapshot
        .native_fault_reads
        .then(|| fault::candidate(region.start, &instructions))
        .flatten();
    // Hardware-fault recipes describe one checked entry path; keep such
    // regions unversioned.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let versioned = fault_pc.is_none();
    #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
    let versioned = true;
    let (mut plans, mut rejected) = if versioned {
        loops::survey(&instructions, &entries, |i| {
            supported(i)
                && crate::strings::operation(i).is_none()
                && !string::supported(i)
                && !matches!(i.mnemonic(), M::Int | M::Rdtsc)
        })
    } else {
        (Vec::new(), Vec::new())
    };
    // A memory bound is stable only while this guest thread owns execution.
    // Another guest worker may change it even if this loop never does.
    if snapshot.parallel {
        plans.retain(|plan| {
            if matches!(plan.bound, loops::Bound::Memory(_)) {
                rejected.push(loops::Rejected {
                    header: plan.header,
                    latch: plan.latch,
                    reason: "shared execution memory bound".into(),
                });
                false
            } else {
                true
            }
        });
    }
    // A kernel leaves at an access its windows reject; the region resumes
    // the loop's checked code there.
    for plan in &plans {
        entries.extend(plan.windows.iter().map(|&(pc, _)| pc));
    }
    let nests = if snapshot.parallel || plans.is_empty() {
        Vec::new()
    } else {
        nest::nests(&instructions, &entries, &plans)
    };
    let analyzed: Vec<_> = plans
        .iter()
        .map(|plan| vector::batch(plan, &instructions))
        .collect();
    let batches: Vec<_> = analyzed
        .iter()
        .map(|batch| batch.as_ref().ok().cloned())
        .collect();
    let coverage = if coverage::enabled() {
        coverage::record(region.start, &instructions, &plans, &rejected, &analyzed)
    } else {
        BTreeMap::new()
    };
    #[cfg(all(test, target_arch = "x86_64"))]
    let vectorized_loops = batches.iter().flatten().count();
    #[cfg(all(test, target_arch = "x86_64"))]
    let versioned_loops = plans.len();
    let return_hint = if returns.len() == 1 {
        returns.first().copied()
    } else {
        None
    };

    let opt_level = match if snapshot.parallel {
        CompileStrategy::Fast
    } else {
        region.compile_strategy
    } {
        CompileStrategy::Speed => "speed",
        CompileStrategy::Fast => "none",
    };
    let invariant_plan =
        (opt_level == "speed" && !snapshot.parallel && plans.is_empty() && nests.is_empty())
            .then(|| invariants::analyze(&instructions))
            .flatten()
            .filter(|plan| {
                let scope = instructions
                    .range(plan.header..=plan.latch)
                    .map(|(&pc, i)| (pc, *i))
                    .collect();
                write_summary::Summary::for_region(&scope).is_some()
            });
    let gir = {
        let mut opaque = BTreeSet::new();
        for plan in &plans {
            opaque.extend(
                instructions
                    .range(plan.header..=plan.latch)
                    .map(|(&pc, _)| pc),
            );
        }
        if let Some(plan) = &invariant_plan {
            opaque.extend(
                instructions
                    .range(plan.header..=plan.latch)
                    .map(|(&pc, _)| pc),
            );
        }
        opaque.extend(x87_segments.interiors.iter().copied());
        opaque.extend(x87_segments.integers.iter().copied());
        crate::gir::lift::tier2(&instructions, &entries, &opaque).map_err(|e| e.to_string())?
    };
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let invariant_plan = invariant_plan.filter(|_| !snapshot.native_fault_reads);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let recovery_cfg = snapshot.native_fault_reads;
    #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
    let recovery_cfg = false;
    let backend_flags = [
        ("opt_level", opt_level),
        ("preserve_frame_pointers", "true"),
        (
            "machine_code_cfg_info",
            if recovery_cfg { "true" } else { "false" },
        ),
        (
            "enable_verifier",
            if verifier_enabled() { "true" } else { "false" },
        ),
    ];
    #[cfg(feature = "gir-capture")]
    let backend_flags = {
        let mut flags = backend_flags.to_vec();
        if std::env::var_os("ADV_GIR_NATIVE").is_some() {
            flags.push(("machine_code_cfg_info", "true"));
        }
        flags
    };
    let mut builder = JITBuilder::with_flags(&backend_flags, default_libcall_names())
        .map_err(|e| e.to_string())?;
    builder.symbol("adv_trap_call", crate::trap::trap_call as *const u8);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    builder.memory_provider(Box::new(crate::jit_memory::WindowsMemory::default()));
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    builder.memory_provider(Box::new(crate::jit_memory_android::AndroidMemory::default()));
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    builder.memory_provider(Box::new(crate::jit_memory_macos::MacMemory::default()));
    builder.symbol("adv_repeat", crate::strings::repeat as *const u8);
    builder.symbol("adv_x87", x87::helper_address());
    builder.symbol("adv_atomic_store", crate::atomic_memory::store as *const u8);
    builder.symbol("adv_file_read", crate::file_memory::read as *const u8);
    builder.symbol("adv_file_write", crate::file_memory::write as *const u8);
    builder.symbol("adv_file_prepare", crate::file_memory::prepare as *const u8);
    builder.symbol(
        "adv_file_write_prepared",
        crate::file_memory::write_prepared as *const u8,
    );
    builder.symbol("adv_range_ok", loops::range_ok as *const u8);
    builder.symbol("adv_nest_run", crate::rows::run as *const u8);
    builder.symbol(
        "adv_read_timestamp",
        crate::trap::read_timestamp as *const u8,
    );
    let mut module = JITModule::new(builder);
    let ptr_ty = module.target_config().pointer_type();
    let mut ctx = module.make_context();
    let dump_dir = std::env::var_os("ADV_JIT_DUMP_DIR").filter(|_| {
        std::env::var("ADV_JIT_DUMP_REGIONS")
            .ok()
            .is_none_or(|list| {
                list.split(',').any(|pc| {
                    u32::from_str_radix(pc.trim().trim_start_matches("0x"), 16).ok()
                        == Some(region.start)
                })
            })
    });
    if dump_dir.is_some() {
        ctx.set_disasm(true);
    }
    #[cfg(all(test, target_arch = "x86_64"))]
    ctx.set_disasm(true);
    ctx.func.signature = contract::signature(&module);
    let tail_signature = ctx.func.signature.clone();
    let id = module
        .declare_function("guest_region", Linkage::Local, &ctx.func.signature)
        .map_err(|e| e.to_string())?;
    // Stateful helpers and existing loop kernels keep their established fuel
    // and recovery contracts. Direct recovery metadata describes one body,
    // so it cannot yet be shared by the checked and polling copies.
    // Recompute the plan from the final region CFG.
    let poll_plan = (crate::safepoint::enabled()
        && !snapshot.parallel
        && plans.is_empty()
        && invariant_plan.is_none()
        && versioned
        && write_summary::Summary::for_region(&instructions).is_some())
    .then(|| safepoints::Plan::new(&instructions, &entries))
    .flatten();
    let mut poll_target = poll_plan.as_ref().map(|_| Box::new(0usize));
    let poll_cell = poll_target
        .as_deref()
        .map(|target| std::ptr::from_ref(target) as usize);
    let poll_id = poll_plan
        .as_ref()
        .map(|_| {
            module
                .declare_function("guest_poll", Linkage::Local, &tail_signature)
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let invariant_id = invariant_plan
        .as_ref()
        .map(|_| {
            module
                .declare_function("guest_invariant", Linkage::Local, &tail_signature)
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut trap_signature = module.make_signature();
    trap_signature.params.extend([AbiParam::new(ptr_ty); 2]);
    trap_signature.params.push(AbiParam::new(types::I32));
    trap_signature.returns.push(AbiParam::new(types::I32));
    let host_id = module
        .declare_function("adv_trap_call", Linkage::Import, &trap_signature)
        .map_err(|e| e.to_string())?;
    let mut repeat_signature = module.make_signature();
    repeat_signature.params.extend([AbiParam::new(ptr_ty); 2]);
    repeat_signature.returns.push(AbiParam::new(types::I32));
    repeat_signature.params.push(AbiParam::new(types::I32));
    let repeat_id = module
        .declare_function("adv_repeat", Linkage::Import, &repeat_signature)
        .map_err(|e| e.to_string())?;
    let mut x87_signature = module.make_signature();
    x87_signature.params.extend([AbiParam::new(ptr_ty); 3]);
    x87_signature.returns.push(AbiParam::new(types::I32));
    let x87_id = module
        .declare_function("adv_x87", Linkage::Import, &x87_signature)
        .map_err(|e| e.to_string())?;
    let mut atomic_store_signature = module.make_signature();
    atomic_store_signature.params.extend([
        AbiParam::new(ptr_ty),
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
    ]);
    let atomic_store_id = module
        .declare_function("adv_atomic_store", Linkage::Import, &atomic_store_signature)
        .map_err(|e| e.to_string())?;
    let mut file_signature = module.make_signature();
    file_signature.params.extend([
        AbiParam::new(ptr_ty),
        AbiParam::new(types::I32),
        AbiParam::new(ptr_ty),
        AbiParam::new(types::I32),
    ]);
    file_signature.returns.push(AbiParam::new(types::I32));
    let file_read_id = module
        .declare_function("adv_file_read", Linkage::Import, &file_signature)
        .map_err(|e| e.to_string())?;
    let file_write_id = module
        .declare_function("adv_file_write", Linkage::Import, &file_signature)
        .map_err(|e| e.to_string())?;
    file_signature.returns.clear();
    let file_prepared_id = module
        .declare_function("adv_file_write_prepared", Linkage::Import, &file_signature)
        .map_err(|e| e.to_string())?;
    let mut prepare_signature = module.make_signature();
    prepare_signature.params.extend([
        AbiParam::new(ptr_ty),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
    ]);
    prepare_signature.returns.push(AbiParam::new(types::I32));
    let prepare_id = module
        .declare_function("adv_file_prepare", Linkage::Import, &prepare_signature)
        .map_err(|e| e.to_string())?;
    let mut timestamp_signature = module.make_signature();
    timestamp_signature.params.push(AbiParam::new(ptr_ty));
    timestamp_signature.returns.push(AbiParam::new(types::I64));
    let timestamp_id = module
        .declare_function("adv_read_timestamp", Linkage::Import, &timestamp_signature)
        .map_err(|e| e.to_string())?;
    let mut range_signature = module.make_signature();
    range_signature.params.push(AbiParam::new(ptr_ty));
    range_signature
        .params
        .extend([AbiParam::new(types::I64); 3]);
    range_signature
        .params
        .extend([AbiParam::new(types::I32); 2]);
    range_signature.returns.push(AbiParam::new(types::I32));
    let range_id = module
        .declare_function("adv_range_ok", Linkage::Import, &range_signature)
        .map_err(|e| e.to_string())?;
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let mut fault_draft = None;
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    if fault_pc.is_some() {
        ctx.func.collect_debug_info();
    }
    // Fault recipes read their own source locations; keep them untagged.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let source_map =
        (diagnostics::instruction_map_enabled() || ir_counts.is_some()) && fault_pc.is_none();
    #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
    let source_map = diagnostics::instruction_map_enabled() || ir_counts.is_some();
    let mut nest_signature = module.make_signature();
    nest_signature.params.extend([AbiParam::new(ptr_ty); 4]);
    nest_signature.returns.push(AbiParam::new(types::I32));
    let nest_id = module
        .declare_function("adv_nest_run", Linkage::Import, &nest_signature)
        .map_err(|e| e.to_string())?;
    // Builds the region function, or with `kernel` the separate function for
    // that versioned loop's clone. `kernels` holds each loop's finalized kernel
    // address for the region to call.
    // Windows fault-recovery metadata is the closure's only mutable capture.
    #[cfg_attr(
        not(all(windows, target_arch = "x86_64", target_env = "msvc")),
        allow(unused_mut)
    )]
    let mut emit = |module: &mut JITModule,
                    func: &mut ir::Function,
                    fb_ctx: &mut FunctionBuilderContext,
                    kernel: Option<usize>,
                    kernels: &[usize],
                    track_writes: bool,
                    descriptors: &[usize],
                    invariant_body: bool,
                    polling_body: bool| {
        let poll = polling_body.then(|| {
            (
                poll_plan.as_ref().expect("poll body has a bounded plan"),
                poll_cell.expect("poll body owns an absolute target cell"),
            )
        });
        // Shadow returns always target the checked entry. An external callee
        // can change a base register, invalidating the old invocation's proof.
        let shadow_ref = (snapshot.shadow_returns && kernel.is_none())
            .then(|| module.declare_func_in_func(id, func));
        let invariant_ref = (!invariant_body)
            .then_some(invariant_id)
            .flatten()
            .map(|id| module.declare_func_in_func(id, func));
        let invariant_resume = invariant_body.then(|| module.declare_func_in_func(id, func));
        let nest_ref = module.declare_func_in_func(nest_id, func);
        let trap_ref = module.declare_func_in_func(host_id, func);
        let repeat_ref = module.declare_func_in_func(repeat_id, func);
        let x87_ref = module.declare_func_in_func(x87_id, func);
        let atomic_store_ref = module.declare_func_in_func(atomic_store_id, func);
        let file_read_ref = module.declare_func_in_func(file_read_id, func);
        let file_write_ref = module.declare_func_in_func(file_write_id, func);
        let file_prepared_ref = module.declare_func_in_func(file_prepared_id, func);
        let file_prepare_ref = module.declare_func_in_func(prepare_id, func);
        let timestamp_ref = module.declare_func_in_func(timestamp_id, func);
        let range_ok_ref = module.declare_func_in_func(range_id, func);
        // A kernel loads and stores only the state its loop can touch.
        let scope: Vec<&Instruction> = match kernel {
            Some(index) => instructions
                .range(plans[index].header..=plans[index].latch)
                .map(|(_, i)| i)
                .collect(),
            None if invariant_body => {
                let plan = invariant_plan.as_ref().unwrap();
                instructions
                    .range(plan.header..=plan.latch)
                    .map(|(_, i)| i)
                    .collect()
            }
            None => instructions.values().collect(),
        };
        let scoped_instructions = scope.iter().map(|&i| (i.ip32(), *i)).collect();
        let write_summary = write_summary::Summary::for_region(&scoped_instructions);
        let mut b = FunctionBuilder::new(func, fb_ctx);
        let prologue = b.create_block();
        b.append_block_params_for_function_params(prologue);
        b.switch_to_block(prologue);
        if source_map {
            // Context and guest-state loads on region entry.
            b.set_srcloc(diagnostics::overhead_source(diagnostics::Overhead::Entry));
        }
        b.seal_block(prologue);
        let params = b.block_params(prologue).to_vec();
        let state = b.ins().load(
            ptr_ty,
            MemFlags::trusted(),
            params[0],
            offset_of!(NativeContext, state) as i32,
        );
        let perms = b.ins().load(
            ptr_ty,
            MemFlags::trusted(),
            params[0],
            offset_of!(NativeContext, perms) as i32,
        );
        let bridge = b.ins().load(
            ptr_ty,
            MemFlags::trusted(),
            params[0],
            offset_of!(NativeContext, bridge) as i32,
        );
        let interrupt = b.ins().load(
            ptr_ty,
            MemFlags::trusted(),
            params[0],
            offset_of!(NativeContext, interrupt) as i32,
        );
        let tail_ref = b.import_signature(tail_signature.clone());
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        let fault_mode = fault_pc.map(|_| {
            let variable = b.declare_var(types::I8);
            let enabled = b.ins().load(
                types::I8,
                MemFlags::trusted(),
                params[0],
                offset_of!(NativeContext, fault_reads) as i32,
            );
            b.def_var(variable, enabled);
            variable
        });
        let regs: [Variable; 8] = std::array::from_fn(|_| b.declare_var(types::I32));
        let mut xmm = [None; 8];
        for instruction in scope.iter().filter(|i| supported(i)) {
            for operand in 0..instruction.op_count() {
                if let Some(index) = simd::register(instruction.op_register(operand))
                    && xmm[index].is_none()
                {
                    let var = b.declare_var(types::I8X16);
                    let value = b.ins().load(
                        types::I8X16,
                        MemFlags::new(),
                        state,
                        (offset_of!(CpuState, xmm) + index * 16) as i32,
                    );
                    b.def_var(var, value);
                    xmm[index] = Some(var);
                }
            }
        }
        let mut mmx = [None; 8];
        let has_mmx = scope.iter().any(|i| supported(i) && mmx::supported(i));
        for instruction in scope.iter().filter(|i| supported(i) && mmx::supported(i)) {
            for operand in 0..instruction.op_count() {
                if let Some(index) = mmx::register(instruction.op_register(operand))
                    && mmx[index].is_none()
                {
                    let var = b.declare_var(types::I8X16);
                    let bits = b.ins().load(
                        types::I64,
                        MemFlags::new(),
                        state,
                        (offset_of!(CpuState, mmx) + index * 8) as i32,
                    );
                    let vector = b.ins().scalar_to_vector(types::I64X2, bits);
                    let value = b.ins().bitcast(
                        types::I8X16,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        vector,
                    );
                    b.def_var(var, value);
                    mmx[index] = Some(var);
                }
            }
        }
        let mmx_mode = has_mmx.then(|| {
            let var = b.declare_var(types::I32);
            let value = b.ins().load(
                types::I32,
                MemFlags::new(),
                state,
                offset_of!(CpuState, mmx_mode) as i32,
            );
            b.def_var(var, value);
            var
        });
        let flags = b.declare_var(types::I32);
        let lazy: [Variable; 7] = std::array::from_fn(|_| b.declare_var(types::I32));
        let zero = b.ins().iconst(types::I32, 0);
        let invariant_gate = invariant_ref.map(|entry| {
            let attempted = b.declare_var(types::I32);
            b.def_var(attempted, zero);
            (entry, attempted)
        });
        let written = track_writes.then(|| {
            let var = b.declare_var(types::I32);
            b.def_var(var, zero);
            var
        });
        let row_hooks = if kernel.is_none() {
            nests
                .iter()
                .zip(descriptors)
                .map(|(nest, &descriptor)| {
                    let valid = b.declare_var(types::I32);
                    b.def_var(valid, zero);
                    let previous = std::array::from_fn(|_| {
                        let var = b.declare_var(types::I32);
                        b.def_var(var, zero);
                        var
                    });
                    nest_emit::Hook {
                        nest: nest.clone(),
                        descriptor,
                        valid,
                        previous,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let fuel = b.declare_var(types::I64);
        for (i, reg) in regs.iter().enumerate() {
            let value = b
                .ins()
                .extractlane(params[contract::GPRS + i / 4], (i % 4) as u8);
            b.def_var(*reg, value);
        }
        let f = params[contract::FLAGS];
        b.def_var(flags, f);
        for var in lazy {
            b.def_var(var, zero);
        }
        let carry = b.ins().band_imm(f, 1);
        b.def_var(lazy[4], carry);
        b.def_var(fuel, params[contract::FUEL]);
        let pc = params[contract::PC];
        let blocks: BTreeMap<_, _> = entries
            .iter()
            .filter(|&&pc| {
                !invariant_body
                    || invariant_plan
                        .as_ref()
                        .is_some_and(|plan| plan.header <= pc && pc <= plan.latch)
            })
            .map(|&pc| (pc, b.create_block()))
            .collect();
        let fast_blocks: Vec<BTreeMap<_, _>> = plans
            .iter()
            .map(|plan| {
                plan.starts
                    .iter()
                    .map(|&pc| (pc, b.create_block()))
                    .collect()
            })
            .collect();
        let cooldowns: Vec<Variable> = plans
            .iter()
            .map(|_| {
                let variable = b.declare_var(types::I32);
                b.def_var(variable, zero);
                variable
            })
            .collect();
        let slot_vars: Vec<Vec<Variable>> = plans
            .iter()
            .map(|plan| {
                plan.slots
                    .iter()
                    .map(|_| {
                        let variable = b.declare_var(types::I32);
                        b.def_var(variable, zero);
                        variable
                    })
                    .collect()
            })
            .collect();
        let windows: Vec<BTreeMap<(u32, bool), (Variable, Variable)>> = plans
            .iter()
            .map(|plan| {
                plan.windows
                    .iter()
                    .map(|&site| {
                        let low = b.declare_var(types::I32);
                        let span = b.declare_var(types::I32);
                        b.def_var(low, zero);
                        b.def_var(span, zero);
                        (site, (low, span))
                    })
                    .collect()
            })
            .collect();
        let dispatch = b.create_block();
        b.append_block_param(dispatch, types::I32);
        if shadow_ref.is_some() {
            b.append_block_param(dispatch, ptr_ty);
        }
        let shared_exit = b.create_block();
        b.set_cold_block(shared_exit);
        b.append_block_param(shared_exit, types::I32); // Exit reason.
        b.append_block_param(shared_exit, types::I32); // Exact guest PC.
        if shadow_ref.is_some() {
            b.append_block_param(shared_exit, ptr_ty);
        }
        let hot_dispatch = b.create_block();
        b.append_block_param(hot_dispatch, types::I32);
        if shadow_ref.is_some() {
            b.append_block_param(hot_dispatch, ptr_ty);
        }
        let hot_exits = kernel.is_none()
            && !invariant_body
            && write_summary.is_some()
            && write_summary::crosses_regions(&instructions);
        if !hot_exits && kernel.is_none() {
            if shadow_ref.is_some() {
                let absent = b.ins().iconst(ptr_ty, 0);
                b.ins().jump(dispatch, &[pc.into(), absent.into()]);
            } else {
                b.ins().jump(dispatch, &[pc.into()]);
            }
        }
        let local_return_hint = return_hint.filter(|&next| {
            shadow_ref.is_some()
                && instructions
                    .values()
                    .filter(|i| {
                        matches!(
                            i.flow_control(),
                            FlowControl::Call | FlowControl::IndirectCall
                        )
                    })
                    .all(|i| {
                        i.op0_kind() == OpKind::NearBranch32
                            && i.next_ip32() == next
                            && blocks.contains_key(&i.near_branch32())
                    })
        });
        let mut e = Emitter {
            poll,
            poll_block: region.start,
            b,
            optimizing: opt_level == "speed",
            memory_scratch: None,
            invariant_plan: invariant_plan.clone(),
            invariant_body,
            invariant_gate,
            invariant_resume,
            row_hooks,
            nest_ref,
            written,
            regs,
            xmm,
            mmx,
            mmx_mode,
            mmx_effects: None,
            flags,
            lazy,
            fuel,
            state,
            mem: params[1],
            abi_state: params[contract::GPRS..contract::GPRS + 2]
                .try_into()
                .unwrap(),
            perms,
            interrupt,
            context: params[0],
            tail_ref,
            ptr_ty,
            base: snapshot.base,
            len: snapshot.len,
            blocks,
            plans: plans.clone(),
            fast_blocks,
            cooldowns,
            slot_vars,
            windows,
            fast: None,
            kernels: kernels.to_vec(),
            batches: batches.clone(),
            lanes: None,
            coverage: coverage.clone(),
            range_ok_ref,
            site: None,
            frames: BTreeMap::new(),
            kernel: None,
            dispatch,
            shared_exit,
            flag_exits: BTreeMap::new(),
            guard_exits: BTreeMap::new(),
            pc: region.start,
            return_hint,
            local_return_hint,
            shadow_ref,
            shadow_entries: if shadow_ref.is_some() {
                entries
                    .iter()
                    .copied()
                    .filter(|pc| supported(&instructions[pc]))
                    .collect()
            } else {
                BTreeSet::new()
            },
            hot_dispatch,
            direct_footers: BTreeMap::new(),
            write_summary,
            hot_exits,
            bridge,
            trap_ref,
            repeat_ref,
            x87_ref,
            x87_native: x87::enabled(),
            x87_cold: None,
            x87_cold_reads: 0,
            parallel: snapshot.parallel,
            heat: snapshot
                .profile
                .as_ref()
                .map(|profile| &profile.heat as *const _ as usize),
            call_sites: snapshot
                .profile
                .as_ref()
                .map_or_else(BTreeMap::new, |profile| {
                    instructions
                        .values()
                        .filter(|i| i.flow_control() == FlowControl::IndirectCall)
                        .zip(&profile.calls)
                        .map(|(i, cell)| (i.ip32(), cell as *const _ as usize))
                        .collect()
                }),
            lane: None,
            x87: x87
                .iter()
                .map(|op| (op.instruction.ip32(), (op as *const _ as usize, op.opcode)))
                .collect(),
            atomic_store_ref,
            file_read_ref,
            file_write_ref,
            file_prepare_ref,
            file_prepared_ref,
            timestamp_ref,
            source_map,
            source_origin: region.start,
            source: if source_map {
                diagnostics::overhead_source(diagnostics::Overhead::Entry)
            } else {
                ir::SourceLoc::default()
            },
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_pc,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_mode,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_entry: region.start,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_seeds: Vec::new(),
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_draft: None,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_return: None,
        };
        // Incoming flags are canonical; local transfers retain lazy recipes.
        // Keep their dispatches separate so a known local recipe is not widened
        // before a normal exit.
        if hot_exits {
            e.interrupt_at(pc, true);
            let entry_miss = e.b.create_block();
            e.b.set_cold_block(entry_miss);
            let mut switch = Switch::new();
            for (&pc, &block) in &e.blocks {
                switch.set_entry(u128::from(pc), block);
            }
            switch.emit(&mut e.b, pc, entry_miss);
            e.b.switch_to_block(entry_miss);
            let reason = e.constant(Exit::Dispatch as u32);
            e.b.ins().return_(&[reason]);
        }
        // A kernel runs its clone from the entry; every ordinary block becomes
        // a transfer back to the region.
        if let Some(index) = kernel {
            e.enter_kernel(index);
        }
        let mut work: Vec<_> = e
            .blocks
            .iter()
            .map(|(&pc, &block)| (pc, block, None))
            .collect();
        if let Some(index) = kernel {
            let blocks = &e.fast_blocks[index];
            work.extend(blocks.iter().map(|(&pc, &block)| (pc, block, Some(index))));
        }
        for (start, block, fast) in work {
            e.fast = fast;
            if x87_segments.interiors.contains(&start) || x87_segments.integers.contains(&start) {
                e.b.set_cold_block(block);
            }
            e.b.switch_to_block(block);
            e.pc = start;
            e.poll_block = start;
            if kernel.is_some() && fast.is_none() {
                e.begin_mmx_block();
                e.exit_const(Exit::Dispatch, start);
                continue;
            }
            e.mark_source(diagnostics::overhead_source(diagnostics::Overhead::Budget));
            e.begin_mmx_block();
            if e.written.is_some()
                && let Some(index) = fast
            {
                let mask = nest_emit::block_writes(&instructions, start, &e.plans[index].starts);
                e.record_writes(mask);
            }
            let budget = e.b.use_var(e.fuel);
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            if start == region.start && fast.is_none() && fault_pc.is_some() {
                use crate::host_fault::recipe::Input;
                for (index, variable) in e.regs.iter().enumerate() {
                    e.fault_seeds
                        .push((e.b.use_var(*variable), Input::Register(index as u8)));
                }
                e.fault_seeds.push((e.b.use_var(e.flags), Input::Flags));
                e.fault_seeds.push((budget, Input::Fuel));
                for (index, variable) in e.lazy.iter().enumerate() {
                    e.fault_seeds.push((
                        e.b.use_var(*variable),
                        if index == 4 {
                            Input::Carry
                        } else {
                            Input::Zero
                        },
                    ));
                }
            }
            // A kernel charges its blocks when it leaves (`settle_fuel`).
            if fast.is_none() {
                if e.poll.is_none() {
                    let zero = e.b.ins().icmp_imm(IntCC::Equal, budget, 0);
                    e.guard(zero, Exit::Budget, None);
                }
                let remaining = e.b.ins().iadd_imm(budget, -1);
                e.b.def_var(e.fuel, remaining);
                if let Some(heat) = e.heat {
                    let cell = e.b.ins().iconst(ptr_ty, heat as i64);
                    let count = e.b.ins().load(types::I64, MemFlags::trusted(), cell, 0);
                    let count = e.b.ins().iadd_imm(count, 1);
                    e.b.ins().store(MemFlags::trusted(), count, cell, 0);
                }
            }
            let mut pc = start;
            loop {
                let boundary = match fast {
                    Some(index) => e.fast_blocks[index].contains_key(&pc),
                    None => e.blocks.contains_key(&pc),
                };
                if pc != start && boundary {
                    e.mark_source(diagnostics::overhead_source(
                        diagnostics::Overhead::Transfer,
                    ));
                    e.direct(pc);
                    break;
                }
                let Some(instruction) = instructions.get(&pc) else {
                    e.mark_source(diagnostics::overhead_source(
                        diagnostics::Overhead::Transfer,
                    ));
                    e.exit_const(Exit::Dispatch, pc);
                    break;
                };
                e.at(pc);
                if !supported(instruction) && !e.x87.contains_key(&pc) {
                    e.exit_const(Exit::Fallback, pc);
                    break;
                }
                if let Some(segment) = x87_segments.starts.get(&pc) {
                    e.native_x87_segment(segment);
                    pc = segment.last().unwrap().next_ip32();
                    continue;
                }
                if x87_segments.interiors.contains(&pc) {
                    // Alternate/restart entries see fully canonical state. The
                    // normal success lane bypasses these cold helper blocks.
                    e.canonical_x87_instruction(instruction);
                    break;
                }
                let operation = gir
                    .operations
                    .get(&pc)
                    .expect("GIR covers every native entry");
                let (finished, next) = e.gir_operation(operation);
                if finished {
                    break;
                }
                pc = next;
            }
        }
        if let Some(index) = kernel {
            e.emit_lanes(index, &instructions);
        }
        e.fast = None;
        e.emit_canonical_x87_recovery();
        e.mark_source(diagnostics::overhead_source(
            diagnostics::Overhead::Transfer,
        ));
        // Runtime indirect predecessors each carry their own mode. No
        // individual guest block's recipe dominates this block.
        e.mmx_effects = None;
        e.b.switch_to_block(dispatch);
        e.reset_rows();
        let target = e.b.block_params(dispatch)[0];
        let prediction = shadow_ref.map(|_| e.b.block_params(dispatch)[1]);
        // Atomic rather than readonly/trusted ordinary data: this flag can
        // change while a generated loop is running and must not be hoisted.
        e.interrupt_at(target, true);
        let outside = e.b.create_block();
        if !hot_exits {
            e.b.set_cold_block(outside);
        }
        let mut switch = Switch::new();
        for (&pc, &block) in &e.blocks {
            switch.set_entry(u128::from(pc), block);
        }
        switch.emit(&mut e.b, target, outside);
        e.b.switch_to_block(outside);
        e.exit_predicted(Exit::Dispatch, target, prediction);
        e.emit_guard_exits();
        // Merge only compatible flag recipes at specialized cold exits. A
        // single generic merge makes kind/width and unused operands live on
        // every predecessor. Limit duplication and retain the generic exit for
        // dynamic recipes, including kernel fuel settlement.
        // Keep recipe-specific arithmetic separate, then merge only its final
        // EFLAGS word. Canonical stores and link probing are emitted once.
        let cold_publish = e.optimizing.then(|| {
            let block = e.b.create_block();
            e.b.set_cold_block(block);
            for _ in 0..3 {
                e.b.append_block_param(block, types::I32);
            }
            if shadow_ref.is_some() {
                e.b.append_block_param(block, ptr_ty);
            }
            block
        });
        let flag_exits = std::mem::take(&mut e.flag_exits);
        for (recipe, block) in flag_exits
            .into_iter()
            .map(|(recipe, block)| (Some(recipe), block))
            .chain(std::iter::once((None, shared_exit)))
        {
            e.b.switch_to_block(block);
            e.mark_source(diagnostics::overhead_source(diagnostics::Overhead::Exit));
            // Each predecessor already committed its own deferred MMX prefix.
            e.mmx_effects = None;
            if let Some((kind, carry_kind)) = recipe {
                let kind = e.constant(kind);
                let carry_kind = e.constant(carry_kind);
                e.b.def_var(e.lazy[3], kind);
                e.b.def_var(e.lazy[6], carry_kind);
            }
            let reason = e.b.block_params(block)[0];
            let pc = e.b.block_params(block)[1];
            let prediction = shadow_ref.map(|_| e.b.block_params(block)[2]);
            if let Some(publish) = cold_publish {
                let flags = e.materialize_flags();
                let mut args = vec![reason.into(), pc.into(), flags.into()];
                if let Some(prediction) = prediction {
                    args.push(prediction.into());
                }
                e.b.ins().jump(publish, &args);
            } else {
                e.publish_cold_exit(pc, reason, None, prediction, hot_exits || kernel.is_some());
            }
        }
        if let Some(publish) = cold_publish {
            e.b.switch_to_block(publish);
            let params = e.b.block_params(publish).to_vec();
            let prediction = shadow_ref.map(|_| params[3]);
            e.publish_cold_exit(
                params[1],
                params[0],
                Some(params[2]),
                prediction,
                hot_exits || kernel.is_some(),
            );
        }
        // Normal transfers do not merge with fault/budget prefixes. Bound the
        // number of constant-target footers, sharing all remaining destinations.
        let direct_footers = std::mem::take(&mut e.direct_footers);
        let hot_chain = e.b.create_block();
        e.b.append_block_param(hot_chain, types::I32); // PC.
        e.b.append_block_param(hot_chain, types::I32); // Precomputed target hash.
        if shadow_ref.is_some() {
            e.b.append_block_param(hot_chain, ptr_ty);
        }
        for (target, block) in direct_footers {
            e.b.switch_to_block(block);
            e.mmx_effects = None;
            let pc = e.constant(target);
            e.save_link_state();
            let hash = e.constant(crate::linking::hash(target));
            if shadow_ref.is_some() {
                let absent = e.b.ins().iconst(ptr_ty, 0);
                e.b.ins()
                    .jump(hot_chain, &[pc.into(), hash.into(), absent.into()]);
            } else {
                e.b.ins().jump(hot_chain, &[pc.into(), hash.into()]);
            }
        }
        if hot_exits {
            e.b.switch_to_block(hot_dispatch);
            e.mmx_effects = None;
            let pc = e.b.block_params(hot_dispatch)[0];
            e.save_link_state();
            let high = e.b.ins().ushr_imm(pc, 16);
            let mixed = e.b.ins().bxor(pc, high);
            let mixed = e.b.ins().imul_imm(mixed, 0x9e37_79b1_u32 as i64);
            let high = e.b.ins().ushr_imm(mixed, 16);
            let hash = e.b.ins().bxor(mixed, high);
            if shadow_ref.is_some() {
                let prediction = e.b.block_params(hot_dispatch)[1];
                e.b.ins()
                    .jump(hot_chain, &[pc.into(), hash.into(), prediction.into()]);
            } else {
                e.b.ins().jump(hot_chain, &[pc.into(), hash.into()]);
            }
            e.b.switch_to_block(hot_chain);
            let pc = e.b.block_params(hot_chain)[0];
            let hash = e.b.block_params(hot_chain)[1];
            let prediction = shadow_ref.map(|_| e.b.block_params(hot_chain)[2]);
            e.chain_or_return(pc, Some(hash), None, prediction);
        }
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        if let Some(block) = e.fault_return {
            block.emit(&mut e.b, ptr_ty);
            fault_draft = e.fault_draft.take();
        }
        e.b.seal_all_blocks();
        e.b.finalize();
        ir_counts::Counts::function(ir_counts.as_ref(), func, region.start, &instructions);
    };
    // Kernels are finalized first: the region calls their absolute addresses,
    // since JIT allocations need not stay within a rel32 reach of each other.
    let mut kernel_functions = Vec::new();
    let mut kernel_locations = Vec::new();
    let jobs: Vec<_> = (0..plans.len())
        .flat_map(|index| {
            std::iter::once((index, false)).chain(
                nests
                    .iter()
                    .any(|nest| nest.inner == index)
                    .then_some((index, true)),
            )
        })
        .collect();
    for (index, row) in jobs {
        let mut kernel_ctx = module.make_context();
        if dump_dir.is_some() {
            kernel_ctx.set_disasm(true);
        }
        kernel_ctx.func.signature = tail_signature.clone();
        emit(
            &mut module,
            &mut kernel_ctx.func,
            &mut fb_ctx,
            Some(index),
            &[],
            row,
            &[],
            false,
            false,
        );
        let kernel_id = module
            .declare_function(
                &format!("guest_kernel_{index}_row_{row}"),
                Linkage::Local,
                &tail_signature,
            )
            .map_err(|e| e.to_string())?;
        #[cfg(feature = "cross-dump")]
        let cross = dump_dir.as_ref().map(|_| kernel_ctx.func.clone());
        module
            .define_function(kernel_id, &mut kernel_ctx)
            .map_err(|e| format!("{e:?}"))?;
        let name = if row {
            format!("region-{:08x}.row-kernel-{index}", region.start)
        } else {
            format!("region-{:08x}.kernel-{index}", region.start)
        };
        dump(&dump_dir, &name, &kernel_ctx)?;
        #[cfg(feature = "cross-dump")]
        if let (Some(directory), Some(function)) = (&dump_dir, cross) {
            let code = aarch64_vcode(function, opt_level)?;
            std::fs::write(
                std::path::Path::new(directory).join(format!("{name}.aarch64.vcode")),
                code,
            )
            .map_err(|e| e.to_string())?;
        }
        kernel_functions.push((kernel_id, kernel_ctx));
        kernel_locations.push((index, row));
    }
    let row_thunk = if nests.is_empty() {
        None
    } else {
        let result = nest_emit::thunk(
            &mut module,
            &mut fb_ctx,
            &tail_signature,
            ir_counts.as_ref(),
        )?;
        let id = result.0;
        kernel_functions.push(result);
        Some(id)
    };
    if !kernel_functions.is_empty() {
        module.finalize_definitions().map_err(|e| e.to_string())?;
    }
    let mut kernels = vec![0; plans.len()];
    let mut row_kernels = vec![0; plans.len()];
    for (&(index, row), &(id, _)) in kernel_locations.iter().zip(&kernel_functions) {
        let address = module.get_finalized_function(id) as usize;
        if row {
            row_kernels[index] = address;
        } else {
            kernels[index] = address;
        }
    }
    let thunk = row_thunk.map_or(0, |id| module.get_finalized_function(id) as usize);
    let descriptors: Box<[_]> = nests
        .iter()
        .map(|nest| {
            nest_emit::descriptor(
                nest,
                &plans[nest.inner],
                snapshot.base,
                snapshot.len,
                thunk,
                row_kernels[nest.inner],
            )
        })
        .collect();
    let addresses: Vec<_> = descriptors
        .iter()
        .map(|desc| desc as *const _ as usize)
        .collect();
    if let Some(id) = invariant_id {
        let mut invariant_ctx = module.make_context();
        invariant_ctx.func.signature = tail_signature.clone();
        if dump_dir.is_some() {
            invariant_ctx.set_disasm(true);
        }
        emit(
            &mut module,
            &mut invariant_ctx.func,
            &mut fb_ctx,
            None,
            &kernels,
            false,
            &addresses,
            true,
            false,
        );
        #[cfg(feature = "cross-dump")]
        let cross = dump_dir.as_ref().map(|_| invariant_ctx.func.clone());
        module
            .define_function(id, &mut invariant_ctx)
            .map_err(|e| format!("{e:?}"))?;
        let name = format!("region-{:08x}.invariant", region.start);
        dump(&dump_dir, &name, &invariant_ctx)?;
        #[cfg(feature = "cross-dump")]
        if let (Some(directory), Some(function)) = (&dump_dir, cross) {
            let code = aarch64_vcode(function, opt_level)?;
            std::fs::write(
                std::path::Path::new(directory).join(format!("{name}.aarch64.vcode")),
                code,
            )
            .map_err(|e| e.to_string())?;
        }
        // Finalize after the checked body: its shadow-return address and this
        // body's absolute func_addr references must both be defined first.
        kernel_functions.push((id, invariant_ctx));
    }
    emit(
        &mut module,
        &mut ctx.func,
        &mut fb_ctx,
        None,
        &kernels,
        false,
        &addresses,
        false,
        false,
    );
    if let Some(poll_id) = poll_id {
        let mut poll_ctx = module.make_context();
        poll_ctx.func.signature = tail_signature.clone();
        emit(
            &mut module,
            &mut poll_ctx.func,
            &mut fb_ctx,
            None,
            &kernels,
            false,
            &addresses,
            false,
            true,
        );
        module
            .define_function(poll_id, &mut poll_ctx)
            .map_err(|e| format!("{e:?}"))?;
        kernel_functions.push((poll_id, poll_ctx));
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let fault_draft = fault_draft.and_then(|pending| pending.resolve(&mut ctx.func));
    #[cfg(feature = "cross-dump")]
    let cross = dump_dir.as_ref().map(|_| ctx.func.clone());
    module
        .define_function(id, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    let name = format!("region-{:08x}", region.start);
    dump(&dump_dir, &name, &ctx)?;
    #[cfg(feature = "cross-dump")]
    if let (Some(directory), Some(function)) = (&dump_dir, cross) {
        let code = aarch64_vcode(function, opt_level)?;
        std::fs::write(
            std::path::Path::new(directory).join(format!("{name}.aarch64.vcode")),
            code,
        )
        .map_err(|e| e.to_string())?;
    }
    let source_ranges = diagnostics::instruction_map_enabled().then(|| {
        ctx.compiled_code().map_or_else(Vec::new, |code| {
            code.buffer
                .get_srclocs_sorted()
                .iter()
                .map(|range| (range.start, range.end, range.loc))
                .collect::<Vec<_>>()
        })
    });
    let mut code_size = ctx
        .compiled_code()
        .map_or(0, |code| code.code_buffer().len());
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let body_unwind = ctx
        .compiled_code()
        .unwrap()
        .create_unwind_info(module.isa())
        .map_err(|e| e.to_string())?
        .ok_or("missing Windows body unwind")?;
    #[cfg(all(test, target_arch = "x86_64"))]
    let disasm = ctx
        .compiled_code()
        .and_then(|code| code.vcode.clone())
        .unwrap_or_default();
    // JIT allocations are not guaranteed to stay within x64's rel32 reach.
    // Finalize the Tail entry first, then call its absolute address from the
    // platform-ABI trampoline. Region-to-region transfers already use slots.
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let body_entry = module.get_finalized_function(id) as usize;
    if let Some(target) = poll_target.as_deref_mut() {
        *target = body_entry;
    }
    let tail_entry = module.get_finalized_function(poll_id.unwrap_or(id)) as usize;
    // Optional native-PC attribution for external sampling. Addresses are
    // process-local; this does not enable execution profiling or disable links.
    let recorded =
        diagnostics::record_body(region, opt_level, body_entry, code_size, instructions.len());
    if let Some(ranges) = source_ranges {
        diagnostics::record_instructions(region, recorded, body_entry, ranges, &instructions);
    }
    for (id, kernel_ctx) in &kernel_functions {
        let base = module.get_finalized_function(*id) as usize;
        let Some(code) = kernel_ctx.compiled_code() else {
            continue;
        };
        let size = code.code_buffer().len();
        code_size += size;
        let kind = if Some(*id) == invariant_id {
            opt_level
        } else {
            "kernel"
        };
        let recorded = diagnostics::record_body(region, kind, base, size, 0);
        if diagnostics::instruction_map_enabled() {
            let ranges = code
                .buffer
                .get_srclocs_sorted()
                .iter()
                .map(|range| (range.start, range.end, range.loc));
            diagnostics::record_instructions(region, recorded, base, ranges, &instructions);
        }
    }
    let mut wrapper = module.make_context();
    wrapper.func.signature = module.make_signature();
    wrapper
        .func
        .signature
        .params
        .extend([AbiParam::new(ptr_ty); 2]);
    wrapper
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I32));
    let wrapper_id = module
        .declare_function("guest_entry", Linkage::Local, &wrapper.func.signature)
        .map_err(|e| e.to_string())?;
    let callee = wrapper.func.import_signature(ctx.func.signature.clone());
    {
        let mut b = FunctionBuilder::new(&mut wrapper.func, &mut fb_ctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let params = b.block_params(entry).to_vec();
        let args = contract::load(&mut b, ptr_ty, params[0], params[1]);
        let address = b.ins().iconst(ptr_ty, tail_entry as i64);
        let call = b.ins().call_indirect(callee, address, &args);
        let result = b.inst_results(call)[0];
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize();
    }
    ir_counts::Counts::function(
        ir_counts.as_ref(),
        &wrapper.func,
        region.start,
        &instructions,
    );
    module
        .define_function(wrapper_id, &mut wrapper)
        .map_err(|e| format!("{e:?}"))?;
    for context in std::iter::once(&ctx)
        .chain(std::iter::once(&wrapper))
        .chain(kernel_functions.iter().map(|(_, context)| context))
    {
        if let Some(code) = context.compiled_code() {
            ir_counts::Counts::native(ir_counts.as_ref(), code, region.start, &instructions);
            #[cfg(feature = "gir-capture")]
            crate::gir_capture::native(context, std::env::consts::ARCH);
        }
    }
    code_size += wrapper
        .compiled_code()
        .map_or(0, |code| code.code_buffer().len());
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let wrapper_unwind = wrapper
        .compiled_code()
        .unwrap()
        .create_unwind_info(module.isa())
        .map_err(|e| e.to_string())?
        .ok_or("missing Windows wrapper unwind")?;
    module.finalize_definitions().map_err(|e| e.to_string())?;
    // SAFETY: exact signature declared above, finalized memory is owned by module.
    let perf_ranges = std::iter::once((id, &ctx))
        .chain(std::iter::once((wrapper_id, &wrapper)))
        .chain(kernel_functions.iter().map(|(id, context)| (*id, context)))
        .filter_map(|(id, context)| {
            crate::perf_map::register(
                module.get_finalized_function(id) as usize,
                context.compiled_code()?.code_buffer().len(),
                crate::perf_map::Category::Tier2,
            )
        })
        .collect();
    let entry = unsafe {
        std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(wrapper_id))
    };
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let mut unwind = Vec::new();
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    for (address, length, info) in [
        (
            body_entry,
            ctx.compiled_code().unwrap().code_buffer().len(),
            body_unwind,
        ),
        (
            entry as usize,
            wrapper.compiled_code().unwrap().code_buffer().len(),
            wrapper_unwind,
        ),
    ] {
        let cranelift_codegen::isa::unwind::UnwindInfo::WindowsX64(info) = info else {
            return Err("unsupported Windows native unwind format".into());
        };
        match unsafe { crate::host_fault::UnwindRegistration::new(address, length, &info) } {
            Ok(registration) => unwind.push(registration),
            // A future unsupported prologue cannot enable hardware sites. Keep
            // the current software-checked entry and retire any partial table.
            Err(_) => {
                unwind.clear();
                break;
            }
        }
    }
    // Unsupported entry instructions belong to the cold executor. This also prevents
    // a one-unit scheduler slice from repeatedly entering native code without progress.
    // Callee code stays owned by its own unit; only calls from here reach it.
    let mut excluded_entries = BTreeMap::new();
    entries.retain(|pc| {
        if callee_code.contains(pc) {
            return false;
        }
        let admitted =
            supported(&instructions[pc]) || x87.iter().any(|op| op.instruction.ip32() == *pc);
        if !admitted {
            excluded_entries.insert(*pc, instructions[pc].code());
        }
        admitted
    });
    let input = snapshot.with_coverage(&instructions, &pages);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let (fault_sites, fault_recipes) = if unwind.len() == 2 {
        fault_draft
            .and_then(|draft| draft.finish(ctx.compiled_code().unwrap(), tail_entry))
            .map_or_else(
                || (Vec::new(), Vec::new()),
                |(site, recipe)| (vec![site], vec![recipe]),
            )
    } else {
        (Vec::new(), Vec::new())
    };
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    if unwind.len() == 2 {
        for (id, kernel_ctx) in &kernel_functions {
            let code = kernel_ctx.compiled_code().unwrap();
            let Some(cranelift_codegen::isa::unwind::UnwindInfo::WindowsX64(info)) = code
                .create_unwind_info(module.isa())
                .map_err(|e| e.to_string())?
            else {
                return Err("missing Windows kernel unwind".into());
            };
            let address = module.get_finalized_function(*id) as usize;
            let length = code.code_buffer().len();
            if let Ok(registration) =
                unsafe { crate::host_fault::UnwindRegistration::new(address, length, &info) }
            {
                unwind.push(registration);
            }
        }
    }
    #[cfg(feature = "gir-capture")]
    crate::gir_capture::record(region, snapshot.origin, bytes, &instructions);
    #[cfg(feature = "gir-lift")]
    crate::gir::diagnostics::record(&instructions, region.start);
    Ok(Compiled {
        _poll_target: poll_target,
        perf_ranges,
        ir_counts: ir_counts.map(|counts| {
            counts.into_inner().finish(
                if opt_level == "speed" {
                    "speed"
                } else {
                    "fast"
                },
                &instructions,
            )
        }),
        instruction_count: instructions.len(),
        #[cfg(test)]
        gir_ssa_instructions: gir.ssa_instructions,
        #[cfg(test)]
        gir_opaque_instructions: gir.opaque_instructions,
        decoded_bytes: instructions.values().map(Instruction::len).sum(),
        module: Some(module),
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        unwind,
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        fault_sites,
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        fault_recipes,
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        body_len: ctx.compiled_code().unwrap().code_buffer().len(),
        entry,
        tail_entry,
        pages: pages
            .into_iter()
            .map(|page| (page, snapshot.version(page)))
            .collect(),
        input,
        window_base: snapshot.base,
        window_len: snapshot.len,
        entries,
        excluded_entries,
        code_size,
        _x87: x87,
        _nests: descriptors,
        cache_stamp: std::cell::Cell::new(0),
        #[cfg(all(test, target_arch = "x86_64"))]
        disasm,
        #[cfg(all(test, target_arch = "x86_64"))]
        versioned_loops,
        #[cfg(all(test, target_arch = "x86_64"))]
        vectorized_loops,
        #[cfg(all(test, target_arch = "x86_64"))]
        invariant_body: invariant_id.is_some(),
    })
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn test_backend_keeps_cranelift_verification() {
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &[0x90]).unwrap();
        let compiled =
            compile_snapshot(&Snapshot::capture(&Region::new(0x10000, 1), &mut memory).unwrap())
                .unwrap();
        assert!(
            compiled
                .module
                .as_ref()
                .unwrap()
                .isa()
                .flags()
                .enable_verifier()
        );
    }

    #[test]
    fn snapshots_copy_only_the_region_and_tail_and_keep_old_versions() {
        let mut memory = Memory::new(0x10000, 0x10000).unwrap();
        memory
            .protect(0x10000, 0x10000, READ | WRITE | EXEC)
            .unwrap();
        memory.write(0x10000, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
        let mut region = Region::new(0x10000, 6);
        let strict = Snapshot::capture(&region, &mut memory).unwrap();
        assert_eq!(strict.bytes.len(), 6);
        region.complete_tail = true;
        let old = Snapshot::capture(&region, &mut memory).unwrap();
        assert_eq!(old.bytes.len(), 20);
        assert!(old.valid(&memory));
        // The compiler receives only the immutable old bytes, not this write.
        memory.write(0x10001, &73u32.to_le_bytes()).unwrap();
        assert!(!old.valid(&memory));
        assert_eq!(old.bytes[1], 1);
        let old_result = compile_snapshot(&old).unwrap();
        assert!(!old_result.valid(&memory));
        let fresh = Snapshot::capture(&region, &mut memory).unwrap();
        assert!(compile_snapshot(&fresh).unwrap().valid(&memory));
        memory.protect(0x10000, 4096, READ).unwrap();
        assert!(!fresh.valid(&memory));
        assert!(Snapshot::capture(&region, &mut memory).is_err());
    }

    #[test]
    fn unmapped_tail_is_a_negative_dependency_of_the_compiled_result() {
        let mut memory = Memory::reserve(0x10000, 8192).unwrap();
        memory.map(0x10000, 4096, READ | EXEC).unwrap();
        memory.write(0x10fff, &[0xb8]).unwrap();
        let mut region = Region::new(0x10fff, 1);
        region.complete_tail = true;
        let old = Snapshot::capture(&region, &mut memory).unwrap();
        assert_eq!(old.bytes, [0xb8]);
        assert_eq!(old.pages.len(), 2);
        let compiled = compile_snapshot(&old).unwrap();
        assert!(compiled.valid(&memory));
        memory.map(0x11000, 4096, READ | EXEC).unwrap();
        memory.write(0x11000, &[91, 0, 0, 0]).unwrap();
        assert!(!old.valid(&memory));
        assert!(!compiled.valid(&memory));
        let fresh = Snapshot::capture(&region, &mut memory).unwrap();
        let compiled = compile_snapshot(&fresh).unwrap();
        assert!(compiled.entries.contains(&0x10fff));
        assert!(compiled.valid(&memory));
    }

    #[test]
    fn maximum_prefix_length_at_a_page_edge_keeps_a_stamp_for_the_negative_page() {
        let mut memory = Memory::new(0x10000, 8192).unwrap();
        memory.protect(0x10000, 8192, READ | EXEC).unwrap();
        memory.write(0x10ff1, &[0x66; 15]).unwrap();
        let mut region = Region::new(0x10ff1, 1);
        region.complete_tail = true;
        let old = Snapshot::capture(&region, &mut memory).unwrap();
        // A malformed prefix sequence must not ask for an uncaptured version.
        compile_snapshot(&old).unwrap();
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn memory_counted_loops_get_an_invariant_body_only_in_serial_speed_code() {
        use iced_x86::code_asm::*;
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.push(ebp).unwrap();
        a.mov(ebp, esp).unwrap();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(ebp - 8)).unwrap();
        a.add(eax, dword_ptr(ebp - 4)).unwrap();
        a.mov(dword_ptr(esi), eax).unwrap();
        a.dec(dword_ptr(edi)).unwrap();
        a.jnz(top).unwrap();
        a.pop(ebp).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &bytes).unwrap();
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            for parallel in [false, true] {
                let mut region = Region::new(0x10000, bytes.len());
                region.compile_strategy = strategy;
                let mut input = Snapshot::capture(&region, &mut memory).unwrap();
                input.parallel = parallel;
                let compiled = compile_snapshot(&input).unwrap();
                assert_eq!(compiled.versioned_loops, 0);
                assert_eq!(
                    compiled.invariant_body,
                    strategy == CompileStrategy::Speed && !parallel
                );
            }
        }
    }

    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    #[test]
    fn first_scalar_load_has_a_verified_machine_fault_site_without_hot_checkpoints() {
        let mut memory = Memory::reserve(0, 1usize << 32).unwrap();
        memory.map(0x10000, 4096, READ | EXEC).unwrap();
        memory.write(0x10000, &[0x8b, 0x06, 0xc3]).unwrap();
        memory.enable_native_protection().unwrap();
        let compiled = compile(&Region::new(0x10000, 3), &mut memory, false).unwrap();
        assert_eq!(compiled.fault_sites.len(), 1, "{}", compiled.disasm);
        assert_eq!(compiled.fault_recipes.len(), 1);
        assert!(compiled.fault_recipes[0].valid());
        assert_eq!(compiled.fault_sites[0].guest_pc, 0x10000);
    }

    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    #[test]
    fn incomplete_merge_recipes_and_default_memory_keep_checked_scalar_loads() {
        for policy in [false, true] {
            let mut memory = Memory::reserve(0, 1usize << 32).unwrap();
            memory.map(0x10000, 4096, READ | EXEC).unwrap();
            memory
                .write(0x10000, &[0x83, 0xc3, 1, 0x8b, 0x06, 0xc3])
                .unwrap();
            if policy {
                memory.enable_native_protection().unwrap();
            }
            let mut region = Region::new(0x10000, 6);
            if policy {
                region.entries.push(0x10003);
            }
            let compiled = compile(&region, &mut memory, false).unwrap();
            assert!(compiled.fault_sites.is_empty());
            assert!(compiled.fault_recipes.is_empty());
        }
    }

    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    #[test]
    fn production_owner_registers_body_and_wrapper_and_retires_tables_first() {
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &[0x90, 0xc3]).unwrap();
        let mut compiled = compile(&Region::new(0x10000, 2), &mut memory, false).unwrap();
        assert_eq!(compiled.unwind.len(), 2);
        assert!(crate::host_fault::registered(compiled.tail_entry));
        assert!(crate::host_fault::registered(compiled.entry as usize));
        compiled.unwind.clear();
        // Keep code allocations alive during inspection to prevent address
        // recycling in simultaneous compiler tests.
        assert!(!crate::host_fault::registered(compiled.tail_entry));
        assert!(!crate::host_fault::registered(compiled.entry as usize));
        drop(compiled);
    }
    use iced_x86::code_asm::*;

    #[test]
    fn counted_pointer_loops_are_versioned() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.xor(eax, eax).unwrap();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut machine = crate::Executor::new(0x10000, 4096).unwrap();
        machine.write(0x10000, &bytes).unwrap();
        machine.protect(0x10000, 4096, READ | EXEC).unwrap();
        let region = Region::new(0x10000, bytes.len());
        let compiled = compile(&region, &mut machine.memory, false).unwrap();
        assert_eq!(compiled.versioned_loops, 1);
        assert_eq!(compiled.vectorized_loops, 1);
    }

    #[test]
    fn memory_bound_kernels_require_single_guest_execution() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.inc(ecx).unwrap();
        a.cmp(ecx, dword_ptr(ebp)).unwrap();
        a.jb(top).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &bytes).unwrap();
        let region = Region::new(0x10000, bytes.len());
        let mut snapshot = Snapshot::capture(&region, &mut memory).unwrap();
        let serial = compile_snapshot(&snapshot).unwrap();
        assert_eq!(serial.versioned_loops, 1);
        assert_eq!(serial.vectorized_loops, 0);
        snapshot.parallel = true;
        let parallel = compile_snapshot(&snapshot).unwrap();
        assert_eq!(parallel.versioned_loops, 0);
    }

    #[test]
    fn loop_coverage_counts_iterations_by_outcome() {
        const CODE: u32 = 0x5_0000;
        const STOP: u32 = 0x5_1000;
        const STACK: u32 = 0x5_2f00;
        const SRC: u32 = 0x6_0000;
        const DST: u32 = 0x7_0000;
        coverage::testing::FORCE.set(true);
        let mut a = CodeAssembler::new(32).unwrap();
        let mut copy = a.create_label();
        let mut count = a.create_label();
        a.mov(ecx, 100).unwrap();
        a.set_label(&mut copy).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(copy).unwrap();
        a.xor(edx, edx).unwrap();
        a.set_label(&mut count).unwrap();
        a.inc(edx).unwrap();
        a.cmp(edx, 50).unwrap();
        a.jl(count).unwrap();
        // Halves 200 pixels from the last down, stepping the index past zero.
        let mut scale = a.create_label();
        a.pxor(mm0, mm0).unwrap();
        a.mov(ecx, 4 * 199).unwrap();
        a.set_label(&mut scale).unwrap();
        a.movd(mm1, dword_ptr(esi + ecx)).unwrap();
        a.punpcklbw(mm1, mm0).unwrap();
        a.psrlw(mm1, 1).unwrap();
        a.packuswb(mm1, mm0).unwrap();
        a.movd(dword_ptr(edi + ecx), mm1).unwrap();
        a.sub(ecx, 4).unwrap();
        a.jns(scale).unwrap();
        a.emms().unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        let mut machine = crate::Executor::new(CODE, 0x3_0000).unwrap();
        machine.protect(CODE, 0x3_0000, READ | WRITE).unwrap();
        machine.write(CODE, &bytes).unwrap();
        machine.protect(CODE, 4096, READ | EXEC).unwrap();
        machine.protect(STOP, 4096, READ | EXEC).unwrap();
        machine.write(STACK, &STOP.to_le_bytes()).unwrap();
        machine.add_region(Region::new(CODE, bytes.len())).unwrap();
        machine.state.eip = CODE;
        machine.state.regs[4] = STACK;
        machine.state.regs[6] = SRC;
        machine.state.regs[7] = DST;
        assert_eq!(
            machine.run(STOP, 1 << 30).unwrap(),
            crate::RunOutcome::ReachedStop
        );
        let report = coverage::report().unwrap();
        coverage::testing::FORCE.set(false);
        let row = |outcome: &str| -> Vec<u64> {
            let line = report
                .lines()
                .find(|line| line.starts_with("0x00050000") && line.contains(outcome))
                .unwrap_or_else(|| panic!("{outcome} in {report}"));
            let fields: Vec<&str> = line.split('\t').collect();
            fields[6..10].iter().map(|n| n.parse().unwrap()).collect()
        };
        // One checked back edge proves the copy; its kernel runs the rest.
        let copied = row("\tkernel copy\t");
        assert_eq!(copied[0] + copied[1], 100, "{report}");
        assert_eq!(copied[2], 1);
        assert_eq!(row("\trejected: nothing proven\t"), [49, 0, 0, 0]);
        let scaled = row("\tkernel batch\t");
        assert_eq!(scaled[0] + scaled[1], 200, "{report}");
        assert_eq!(scaled[2], 1);
    }

    #[test]
    fn divide_uses_distinct_host_widths_without_a_second_remainder_division() {
        for clear_high in [true, false] {
            let mut a = CodeAssembler::new(32).unwrap();
            if clear_high {
                a.xor(edx, edx).unwrap();
            }
            a.div(ecx).unwrap();
            a.ret().unwrap();
            let bytes = a.assemble(0x10000).unwrap();
            let mut machine = crate::Executor::new(0x10000, 4096).unwrap();
            machine.write(0x10000, &bytes).unwrap();
            machine.protect(0x10000, 4096, READ | EXEC).unwrap();
            let compiled = compile(
                &Region::new(0x10000, bytes.len()),
                &mut machine.memory,
                false,
            )
            .unwrap();
            let divides: Vec<_> = compiled
                .disasm
                .lines()
                .filter(|line| line.contains("div"))
                .collect();
            println!("EDX cleared {clear_high}: {divides:?}");
            assert!(compiled.disasm.contains("divl"), "{}", compiled.disasm);
            assert_eq!(
                divides.iter().filter(|line| line.contains("divl")).count(),
                1
            );
            assert!(divides.iter().filter(|line| line.contains("divq")).count() <= 1);
            if !clear_high {
                assert!(compiled.disasm.contains("divq"));
            }
        }
    }
}

/// Writes the vcode of a compiled function when dumping is enabled.
fn dump(
    directory: &Option<std::ffi::OsString>,
    name: &str,
    context: &cranelift_codegen::Context,
) -> Result<(), String> {
    let Some(directory) = directory else {
        return Ok(());
    };
    let directory = std::path::Path::new(directory);
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    if let Some(code) = context.compiled_code().and_then(|code| code.vcode.as_ref()) {
        std::fs::write(directory.join(format!("{name}.vcode")), code).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The AArch64 lowering of a region function, for comparing the code the
/// final target would run. Host helper signatures use the AArch64 C ABI.
#[cfg(feature = "cross-dump")]
fn aarch64_vcode(mut function: ir::Function, opt_level: &str) -> Result<String, String> {
    use cranelift_codegen::settings::{self, Configurable};
    for signature in function.dfg.signatures.values_mut() {
        if signature.call_conv == cranelift_codegen::isa::CallConv::WindowsFastcall {
            signature.call_conv = cranelift_codegen::isa::CallConv::SystemV;
        }
    }
    let mut flags = settings::builder();
    // Cross dumps are an explicit diagnostic, so always verify their IR.
    flags
        .set("enable_verifier", "true")
        .map_err(|e| e.to_string())?;
    flags
        .set("opt_level", opt_level)
        .map_err(|e| e.to_string())?;
    flags
        .set("preserve_frame_pointers", "true")
        .map_err(|e| e.to_string())?;
    #[cfg(feature = "gir-capture")]
    if std::env::var_os("ADV_GIR_NATIVE").is_some() {
        flags
            .set("machine_code_cfg_info", "true")
            .map_err(|e| e.to_string())?;
    }
    let isa = cranelift_codegen::isa::lookup_by_name("aarch64")
        .map_err(|e| e.to_string())?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;
    let mut context = cranelift_codegen::Context::for_function(function);
    context.set_disasm(true);
    let code = context
        .compile(&*isa, &mut Default::default())
        .map_err(|e| format!("{e:?}"))?;
    let vcode = code.vcode.clone().unwrap_or_default();
    #[cfg(feature = "gir-capture")]
    crate::gir_capture::native(&context, "aarch64");
    Ok(vcode)
}

/// Checked iterations a versioned loop runs after a failed proof, or after
/// its kernel stopped inside it, before the region calls the kernel again.
const COOLDOWN: u32 = 63;

fn reg_index(reg: R) -> Option<usize> {
    Some(match reg {
        R::EAX => 0,
        R::ECX => 1,
        R::EDX => 2,
        R::EBX => 3,
        R::ESP => 4,
        R::EBP => 5,
        R::ESI => 6,
        R::EDI => 7,
        _ => return None,
    })
}

/// Integer operations the JIT also emits at 8/16-bit width; see `narrow_alu`.
/// PUSH and POP forms that move a 16-bit word and adjust ESP by two.
fn word_stack(i: &Instruction) -> bool {
    use iced_x86::Code as C;
    matches!(
        i.code(),
        C::Push_r16 | C::Push_rm16 | C::Push_imm16 | C::Pushw_imm8 | C::Pop_r16
    )
}

fn narrow_alu(mnemonic: M) -> bool {
    matches!(
        mnemonic,
        M::Add | M::Sub | M::And | M::Or | M::Xor | M::Cmp | M::Test | M::Inc | M::Dec | M::Neg
    )
}

/// LOCK forms the emitter lowers as ordinary read-modify-writes. They are
/// atomic only while this guest thread owns execution; a parallel snapshot
/// sends them to the interpreter, which quiesces the native workers.
fn lockable(i: &Instruction) -> bool {
    i.op0_kind() == OpKind::Memory
        && matches!(
            i.mnemonic(),
            M::Add
                | M::Adc
                | M::Sub
                | M::Sbb
                | M::And
                | M::Or
                | M::Xor
                | M::Inc
                | M::Dec
                | M::Not
                | M::Neg
                | M::Xchg
                | M::Xadd
                | M::Cmpxchg
                | M::Bts
                | M::Btr
                | M::Btc
        )
}

/// Whether this instruction must be one transaction relative to other guest
/// threads: LOCK, or XCHG's implicit lock on a memory operand.
fn exclusive(i: &Instruction) -> bool {
    i.has_lock_prefix()
        || i.mnemonic() == M::Xchg && (0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory)
}

/// Boundaries required by opaque operations and interpreter continuations.
pub(crate) fn opaque_boundaries(i: &Instruction) -> (bool, bool) {
    let native = supported(i);
    let floating = !native && crate::floating::native_x87(i);
    (
        crate::strings::operation(i).is_some() || string::repeated(i) || floating,
        !native && !floating,
    )
}

pub(crate) fn supported(i: &Instruction) -> bool {
    if crate::strings::operation(i).is_some() || string::supported(i) {
        return true;
    }
    // `rep ret` is the branch-predictor idiom for a plain return.
    let rep_ret = i.mnemonic() == M::Ret && !i.has_repne_prefix();
    if i.is_invalid()
        || (i.has_lock_prefix() && !lockable(i))
        || (i.has_rep_prefix() && !rep_ret)
        || i.has_repne_prefix()
        || !matches!(i.segment_prefix(), R::None | R::DS | R::SS | R::ES | R::FS)
    {
        return false;
    }
    if i.mnemonic() == M::Int && i.immediate8() == 0x2e {
        return true;
    }
    if !matches!(
        i.mnemonic(),
        M::Mov
            | M::Movzx
            | M::Movsx
            | M::Lea
            | M::Add
            | M::Sub
            | M::And
            | M::Or
            | M::Xor
            | M::Cmp
            | M::Test
            | M::Inc
            | M::Dec
            | M::Not
            | M::Neg
            | M::Push
            | M::Pop
            | M::Nop
            | M::Rdtsc
            | M::Jmp
            | M::Call
            | M::Ret
            | M::Je
            | M::Jne
            | M::Ja
            | M::Jae
            | M::Jb
            | M::Jbe
            | M::Jg
            | M::Jge
            | M::Jl
            | M::Jle
            | M::Jo
            | M::Jno
            | M::Js
            | M::Jns
            | M::Jp
            | M::Jnp
            | M::Loop
            | M::Jecxz
            | M::Jcxz
    ) {
        return simd::supported(i)
            || scalar::supported(i)
            || mmx::supported(i)
            || sse::supported(i);
    }
    let narrow = matches!(i.mnemonic(), M::Mov | M::Movzx | M::Movsx)
        || narrow_alu(i.mnemonic())
        || word_stack(i);
    for n in 0..i.op_count() {
        match i.op_kind(n) {
            OpKind::Register
                if reg_index(i.op_register(n)).is_some()
                    || (narrow && (i.op_register(n).is_gpr8() || i.op_register(n).is_gpr16())) => {}
            OpKind::Memory => {
                if i.memory_size().size() != 4
                    && i.mnemonic() != M::Lea
                    && !(narrow && matches!(i.memory_size().size(), 1 | 2))
                {
                    return false;
                }
                if [i.memory_base(), i.memory_index()]
                    .iter()
                    .any(|&r| r != R::None && reg_index(r).is_none())
                {
                    return false;
                }
            }
            OpKind::Immediate32 | OpKind::Immediate8to32 | OpKind::NearBranch32 => {}
            OpKind::NearBranch16 if matches!(i.mnemonic(), M::Jcxz | M::Jecxz) => {}
            OpKind::Immediate16 if i.mnemonic() == M::Ret || narrow => {}
            OpKind::Immediate8 if narrow => {}
            OpKind::Immediate8to16 if narrow_alu(i.mnemonic()) || word_stack(i) => {}
            _ => return false,
        }
    }
    if matches!(i.mnemonic(), M::Movzx | M::Movsx)
        && reg_index(i.op0_register()).is_none()
        && !i.op0_register().is_gpr16()
    {
        return false;
    }
    // POP r/m has special ESP-relative addressing; leave that form to the fallback.
    if i.mnemonic() == M::Pop && i.op0_kind() != OpKind::Register {
        return false;
    }
    // Far and 16-bit CALL/RET/PUSH/POP must not inherit our 32-bit stack semantics.
    match i.mnemonic() {
        M::Ret => matches!(
            i.code(),
            iced_x86::Code::Retnd | iced_x86::Code::Retnd_imm16
        ),
        M::Nop => i.op_count() == 0,
        // ECX address size; CX loops keep the interpreter.
        M::Loop => i.code() == iced_x86::Code::Loop_rel8_32_ECX,
        _ => true,
    }
}

#[derive(Clone, Copy)]
struct GuestPointer {
    address: Value,
    /// Byte offset from `address` for ordinary stores; zero unless proven.
    offset: i32,
    guest: Value,
    shared: Value,
    file: Value,
}

struct Emitter<'a> {
    poll: Option<(&'a safepoints::Plan, usize)>,
    poll_block: u32,
    memory_scratch: Option<ir::StackSlot>,
    invariant_plan: Option<invariants::Plan>,
    invariant_body: bool,
    invariant_gate: Option<(ir::FuncRef, Variable)>,
    invariant_resume: Option<ir::FuncRef>,
    row_hooks: Vec<nest_emit::Hook>,
    nest_ref: ir::FuncRef,
    written: Option<Variable>,
    b: FunctionBuilder<'a>,
    regs: [Variable; 8],
    xmm: [Option<Variable>; 8],
    mmx: [Option<Variable>; 8],
    mmx_mode: Option<Variable>,
    mmx_effects: Option<mmx::Effects>,
    flags: Variable,
    fuel: Variable,
    // Last flag-producing instruction: lhs, rhs, result, kind (0=raw,1=add,2=sub,3=logic).
    // Carry has its own operands/kind because INC/DEC preserve it.
    lazy: [Variable; 7],
    state: Value,
    mem: Value,
    abi_state: [Value; 2],
    perms: Value,
    interrupt: Value,
    context: Value,
    tail_ref: ir::SigRef,
    ptr_ty: ir::Type,
    base: u32,
    len: usize,
    blocks: BTreeMap<u32, ir::Block>,
    /// Versioned loops, their cloned blocks, and the clone being emitted.
    plans: Vec<loops::Plan>,
    fast_blocks: Vec<BTreeMap<u32, ir::Block>>,
    /// Checked iterations left before a versioned loop retries its proof.
    cooldowns: Vec<Variable>,
    /// Forwarded slot values of each loop, valid inside its clone.
    slot_vars: Vec<Vec<Variable>>,
    /// Each loop's validated windows as `(low, span)`: an access at `addr`
    /// fits when `addr - low < span`. Empty when its kernel starts.
    windows: Vec<BTreeMap<(u32, bool), (Variable, Variable)>>,
    fast: Option<usize>,
    /// Finalized kernel addresses, one per plan, for the region to call.
    kernels: Vec<usize>,
    /// Each loop's batch form, and in its kernel the batch being emitted.
    batches: Vec<Option<vector::Batch>>,
    lanes: Option<vector::Lanes>,
    /// With loop coverage on, each loop's counters by header and latch.
    coverage: BTreeMap<(u32, u32), usize>,
    range_ok_ref: ir::FuncRef,
    /// The current instruction's operand address and size once a kernel
    /// validated its windows (`precheck`).
    site: Option<(u32, Value, u32)>,
    /// Registers this body never writes: CpuState already holds their exact
    /// values at every exit. Includes the invariant registers of a kernel.
    /// In a kernel, the host address of each unchanged base register.
    frames: BTreeMap<usize, Variable>,
    /// In a kernel: its loop, the counter value that ends the iterations its
    /// fuel pays for, and the fuel and counter it started with.
    kernel: Option<(usize, Variable, ir::StackSlot)>,
    dispatch: ir::Block,
    shared_exit: ir::Block,
    flag_exits: BTreeMap<(u32, u32), ir::Block>,
    guard_exits: BTreeMap<guards::Key, (Exit, ir::Block)>,
    /// Additional guarded paths pay off after the optimizing tier folds them.
    optimizing: bool,
    pc: u32,
    return_hint: Option<u32>,
    local_return_hint: Option<u32>,
    shadow_ref: Option<ir::FuncRef>,
    shadow_entries: BTreeSet<u32>,
    hot_dispatch: ir::Block,
    hot_exits: bool,
    direct_footers: BTreeMap<u32, ir::Block>,
    write_summary: Option<write_summary::Summary>,
    bridge: Value,
    trap_ref: ir::FuncRef,
    repeat_ref: ir::FuncRef,
    x87_ref: ir::FuncRef,
    /// Guest PC -> address of its NativeX87, owned by the resulting Compiled.
    x87: BTreeMap<u32, (usize, Option<u16>)>,
    x87_native: bool,
    x87_cold: Option<ir::Block>,
    x87_cold_reads: u8,
    /// Other guest threads may run native code at the same time.
    parallel: bool,
    /// Address of the profile's block counter, for Fast code that may tier up.
    heat: Option<usize>,
    /// Indirect call site -> address of its profile target cell.
    call_sites: BTreeMap<u32, usize>,
    /// Inside a resident x87 lane, every guard and shared/file access path of an
    /// integer instruction branches here before the instruction commits; the
    /// lane then materializes its prefix and re-executes canonically.
    lane: Option<ir::Block>,
    atomic_store_ref: ir::FuncRef,
    file_read_ref: ir::FuncRef,
    file_write_ref: ir::FuncRef,
    file_prepare_ref: ir::FuncRef,
    file_prepared_ref: ir::FuncRef,
    timestamp_ref: ir::FuncRef,
    // Optional sampler attribution (compile::diagnostics); `source` is the
    // current tag, restored after fault markers.
    source_map: bool,
    source_origin: u32,
    source: ir::SourceLoc,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_pc: Option<u32>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_mode: Option<Variable>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_entry: u32,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_seeds: Vec<(Value, crate::host_fault::recipe::Input)>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_draft: Option<fault::Pending>,
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fault_return: Option<contract::RecoveryExit>,
}

impl Emitter<'_> {
    fn chain_or_return(
        &mut self,
        pc: Value,
        target_hash: Option<Value>,
        reason: Option<Value>,
        prediction: Option<Value>,
    ) {
        let returned = self.b.create_block();
        if let Some(reason) = reason {
            let candidate = self.b.create_block();
            let dispatch = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, reason, Exit::Dispatch as i64);
            self.b.ins().brif(dispatch, candidate, &[], returned, &[]);
            self.b.switch_to_block(candidate);
        }
        let slots = self.b.ins().load(
            self.ptr_ty,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, slots) as i32,
        );
        let remaining = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, remaining) as i32,
        );
        let stop = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, stop) as i32,
        );
        let fuel = self.b.use_var(self.fuel);
        let none = self.b.ins().icmp_imm(IntCC::Equal, slots, 0);
        let limit = self.b.ins().icmp_imm(IntCC::Equal, remaining, 0);
        let is_stop = self.b.ins().icmp(IntCC::Equal, pc, stop);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, fuel, 0);
        let bad = self.b.ins().bor(none, limit);
        let bad = self.b.ins().bor(bad, is_stop);
        let bad = self.b.ins().bor(bad, empty);
        let prepare = self.b.create_block();
        self.b.ins().brif(bad, returned, &[], prepare, &[]);
        self.b.switch_to_block(prepare);
        // Only a Rust exit publishes the incoming architectural prefix. A
        // successful link carries it directly through the native arguments.
        let pending = self
            .b
            .ins()
            .atomic_load(types::I8, MemFlags::trusted(), self.interrupt);
        let pending = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
        let interrupted = self.b.create_block();
        self.b.set_cold_block(interrupted);
        let search_ready = self.b.create_block();
        self.b
            .ins()
            .brif(pending, interrupted, &[], search_ready, &[]);
        self.b.switch_to_block(interrupted);
        self.save_state(pc);
        let interrupted_reason = self.constant(Exit::Interrupted as u32);
        self.b.ins().return_(&[interrupted_reason]);
        self.b.switch_to_block(search_ready);
        let hit = self.b.create_block();
        self.b.append_block_param(hit, self.ptr_ty);
        if let Some(prediction) = prediction {
            let valid = self.b.ins().icmp_imm(IntCC::NotEqual, prediction, 0);
            let predicted = self.b.create_block();
            let search_only = self.b.create_block();
            self.b.ins().brif(valid, predicted, &[], search_only, &[]);
            self.b.switch_to_block(predicted);
            self.shadow_hit();
            self.b.ins().jump(hit, &[prediction.into()]);
            self.b.switch_to_block(search_only);
        }
        let mask = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, mask) as i32,
        );
        let hashed = if let Some(hash) = target_hash {
            hash
        } else {
            let high = self.b.ins().ushr_imm(pc, 16);
            let mixed = self.b.ins().bxor(pc, high);
            let mixed = self.b.ins().imul_imm(mixed, 0x9e37_79b1_u32 as i64);
            let high = self.b.ins().ushr_imm(mixed, 16);
            self.b.ins().bxor(mixed, high)
        };
        let index = self.b.ins().band(hashed, mask);
        let search = self.b.create_block();
        self.b.append_block_param(search, types::I32);
        self.b.append_block_param(search, types::I32);
        let zero = self.constant(0);
        self.b.ins().jump(search, &[index.into(), zero.into()]);
        self.b.switch_to_block(search);
        let index = self.b.block_params(search)[0];
        let probes = self.b.block_params(search)[1];
        let offset = self.b.ins().uextend(self.ptr_ty, index);
        let offset = self
            .b
            .ins()
            .imul_imm(offset, std::mem::size_of::<Slot>() as i64);
        let address = self.b.ins().iadd(slots, offset);
        let entry_address = self
            .b
            .ins()
            .iadd_imm(address, offset_of!(Slot, entry) as i64);
        let entry = self
            .b
            .ins()
            .atomic_load(self.ptr_ty, MemFlags::trusted(), entry_address);
        let absent = self.b.ins().icmp_imm(IntCC::Equal, entry, 0);
        let occupied = self.b.create_block();
        self.b.ins().brif(absent, returned, &[], occupied, &[]);
        self.b.switch_to_block(occupied);
        let key = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            address,
            offset_of!(Slot, pc) as i32,
        );
        let found = self.b.ins().icmp(IntCC::Equal, pc, key);
        let advance = self.b.create_block();
        self.b.ins().brif(found, hit, &[entry.into()], advance, &[]);
        self.b.switch_to_block(advance);
        let next = self.b.ins().iadd_imm(index, 1);
        let next = self.b.ins().band(next, mask);
        let probes = self.b.ins().iadd_imm(probes, 1);
        let exhausted = self.b.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            probes,
            crate::linking::PROBES as i64,
        );
        self.b.ins().brif(
            exhausted,
            returned,
            &[],
            search,
            &[next.into(), probes.into()],
        );
        self.b.switch_to_block(hit);
        let entry = self.b.block_params(hit)[0];
        let remaining = self.b.ins().iadd_imm(remaining, -1);
        self.b.ins().store(
            MemFlags::trusted(),
            remaining,
            self.context,
            offset_of!(NativeContext, remaining) as i32,
        );
        let linked = self.b.ins().load(
            types::I64,
            MemFlags::trusted(),
            self.context,
            offset_of!(NativeContext, linked_entries) as i32,
        );
        let linked = self.b.ins().iadd_imm(linked, 1);
        self.b.ins().store(
            MemFlags::trusted(),
            linked,
            self.context,
            offset_of!(NativeContext, linked_entries) as i32,
        );
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        if self.fault_mode.is_some() {
            let disabled = self.b.ins().iconst(types::I8, 0);
            self.b.ins().store(
                MemFlags::trusted(),
                disabled,
                self.context,
                offset_of!(NativeContext, fault_reads) as i32,
            );
        }
        self.tail_to(entry, pc);
        self.b.switch_to_block(returned);
        self.save_state(pc);
        let reason = reason.unwrap_or_else(|| self.constant(Exit::Dispatch as u32));
        self.b.ins().return_(&[reason]);
    }
    fn constant(&mut self, value: u32) -> Value {
        self.b.ins().iconst(types::I32, i64::from(value))
    }
    fn exit_const(&mut self, reason: Exit, pc: u32) {
        if reason == Exit::Dispatch && self.hot_exits {
            self.flush_mmx_effects();
            let block = if let Some(&block) = self.direct_footers.get(&pc) {
                Some(block)
            } else if self.direct_footers.len() < 2 {
                let block = self.b.create_block();
                self.direct_footers.insert(pc, block);
                Some(block)
            } else {
                None
            };
            if let Some(block) = block {
                self.b.ins().jump(block, &[]);
            } else {
                let target = self.constant(pc);
                self.exit_predicted(reason, target, None);
            }
            return;
        }
        let pc = self.constant(pc);
        self.exit(reason, pc);
    }
    fn x87_instruction(&mut self, i: &Instruction, operation: usize) {
        let pc = self.constant(i.ip32());
        // MMX aliases require synchronization for correctness. XMM and integer
        // flag/partial-register effects retain the full lane for code quality:
        // keeping their SSA values across a helper can increase hot-path spills.
        let effects = if self.mmx_mode.is_none()
            && self.mmx_effects.is_none()
            && self.mmx.iter().all(Option::is_none)
            && self.xmm.iter().all(Option::is_none)
        {
            crate::floating::x87_effects(i)
                .filter(|e| !e.read_flags && !e.write_flags && e.write_gpr == 0)
        } else {
            None
        };
        if let Some(effects) = &effects {
            for index in 0..8 {
                if effects.read_gpr & (1 << index) != 0 {
                    let value = self.b.use_var(self.regs[index]);
                    self.b
                        .ins()
                        .store(MemFlags::trusted(), value, self.state, (index * 4) as i32);
                }
            }
            if effects.read_flags {
                let flags = self.materialize_flags();
                self.b.ins().store(
                    MemFlags::trusted(),
                    flags,
                    self.state,
                    offset_of!(CpuState, eflags) as i32,
                );
            }
            self.b.ins().store(
                MemFlags::trusted(),
                pc,
                self.state,
                offset_of!(CpuState, eip) as i32,
            );
        } else {
            self.save_state(pc);
        }
        let operation = self.b.ins().iconst(self.ptr_ty, operation as i64);
        let call = self
            .b
            .ins()
            .call(self.x87_ref, &[self.bridge, self.state, operation]);
        let result = self.b.inst_results(call)[0];
        if let Some(effects) = &effects {
            for index in 0..8 {
                if effects.write_gpr & (1 << index) != 0 {
                    let value = self.b.ins().load(
                        types::I32,
                        MemFlags::trusted(),
                        self.state,
                        (index * 4) as i32,
                    );
                    self.b.def_var(self.regs[index], value);
                }
            }
            if effects.write_flags {
                self.reload_flags();
            }
        } else {
            self.reload_state();
        }
        if self.mmx_effects.is_some() {
            // The call committed and refreshed the MMX aliases in CpuState.
            self.begin_mmx_block();
        }
        let done = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let completed =
            self.b
                .ins()
                .icmp_imm(IntCC::Equal, result, i64::from(crate::floating::X87_DONE));
        self.b.ins().brif(completed, done, &[], slow, &[]);
        self.b.switch_to_block(slow);
        let retry = self.b.create_block();
        let read_fault = self.b.create_block();
        let write_fault = self.b.create_block();
        let after = self.b.create_block();
        for block in [retry, read_fault, write_fault, after] {
            self.b.set_cold_block(block);
        }
        let mut switch = Switch::new();
        switch.set_entry(u128::from(crate::floating::X87_FALLBACK), retry);
        switch.set_entry(u128::from(crate::floating::X87_READ_FAULT), read_fault);
        switch.set_entry(u128::from(crate::floating::X87_WRITE_FAULT), write_fault);
        switch.emit(&mut self.b, result, after);
        self.b.switch_to_block(retry);
        self.exit_const(Exit::Fallback, i.ip32());
        self.b.switch_to_block(read_fault);
        self.exit_const(Exit::ReadFault, i.ip32());
        self.b.switch_to_block(write_fault);
        self.exit_const(Exit::WriteFault, i.ip32());
        // Completed with SMC or a pending #MF: let the executor re-check both.
        self.b.switch_to_block(after);
        self.exit_const(Exit::Fallback, i.next_ip32());
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }
    fn exit(&mut self, reason: Exit, pc: Value) {
        self.exit_predicted(reason, pc, None);
    }
    fn exit_predicted(&mut self, reason: Exit, pc: Value, prediction: Option<Value>) {
        if self.lanes_active() {
            self.materialize_lanes();
        }
        self.flush_mmx_effects();
        self.settle_fuel(pc);
        // Every exit publishes architectural state. A scheduler or exception
        // handler can observe EFLAGS even when the next guest instruction
        // would overwrite them, so keep the lazy prefix for save_state.
        if reason == Exit::Dispatch && self.hot_exits {
            if self.shadow_ref.is_some() {
                let prediction = prediction.unwrap_or_else(|| self.b.ins().iconst(self.ptr_ty, 0));
                self.b
                    .ins()
                    .jump(self.hot_dispatch, &[pc.into(), prediction.into()]);
            } else {
                self.b.ins().jump(self.hot_dispatch, &[pc.into()]);
            }
            return;
        }
        let reason = self.constant(reason as u32);
        let block = self.flag_exit();
        if self.shadow_ref.is_some() {
            let prediction = prediction.unwrap_or_else(|| self.b.ins().iconst(self.ptr_ty, 0));
            self.b
                .ins()
                .jump(block, &[reason.into(), pc.into(), prediction.into()]);
        } else {
            self.b.ins().jump(block, &[reason.into(), pc.into()]);
        }
    }
    fn flag_exit(&mut self) -> ir::Block {
        if !self.optimizing {
            return self.shared_exit;
        }
        let kind = self.b.use_var(self.lazy[3]);
        let carry_kind = self.b.use_var(self.lazy[6]);
        let (Some(kind), Some(carry_kind)) = (self.known(kind), self.known(carry_kind)) else {
            return self.shared_exit;
        };
        let recipe = (kind, carry_kind);
        if let Some(&block) = self.flag_exits.get(&recipe) {
            return block;
        }
        if self.flag_exits.len() == 8 {
            return self.shared_exit;
        }
        let block = self.b.create_block();
        self.b.set_cold_block(block);
        self.b.append_block_param(block, types::I32);
        self.b.append_block_param(block, types::I32);
        if self.shadow_ref.is_some() {
            self.b.append_block_param(block, self.ptr_ty);
        }
        self.flag_exits.insert(recipe, block);
        block
    }
    fn publish_cold_exit(
        &mut self,
        pc: Value,
        reason: Value,
        flags: Option<Value>,
        prediction: Option<Value>,
        terminal: bool,
    ) {
        self.save_state_with_flags(pc, flags);
        if let Some(written) = self.written {
            let value = self.b.use_var(written);
            self.b.ins().store(
                MemFlags::trusted(),
                value,
                self.context,
                offset_of!(NativeContext, written) as i32,
            );
        }
        if let Some(entry) = self.invariant_resume {
            // The proof ends at the selected loop boundary. Resume the
            // checked body before any surrounding code can change a base.
            let resume = self.b.create_block();
            let returned = self.b.create_block();
            let dispatch = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, reason, Exit::Dispatch as i64);
            self.b.ins().brif(dispatch, resume, &[], returned, &[]);
            self.b.switch_to_block(resume);
            let address = self.b.ins().func_addr(self.ptr_ty, entry);
            self.tail_to(address, pc);
            self.b.switch_to_block(returned);
            self.b.ins().return_(&[reason]);
        } else if terminal {
            self.b.ins().return_(&[reason]);
        } else {
            self.chain_or_return(pc, None, Some(reason), prediction);
        }
    }
    fn save_state(&mut self, pc: Value) {
        self.save_state_with_flags(pc, None);
    }
    fn save_extended_state(&mut self) {
        self.flush_mmx_effects();
        for (index, var) in self.mmx.iter().enumerate() {
            if let Some(var) = var {
                let value = self.b.use_var(*var);
                let value = self.b.ins().bitcast(
                    types::I64X2,
                    MemFlags::new().with_endianness(ir::Endianness::Little),
                    value,
                );
                let bits = self.b.ins().extractlane(value, 0);
                self.b.ins().store(
                    MemFlags::new(),
                    bits,
                    self.state,
                    (offset_of!(CpuState, mmx) + index * 8) as i32,
                );
            }
        }
        if let Some(var) = self.mmx_mode {
            let value = self.b.use_var(var);
            self.b.ins().store(
                MemFlags::new(),
                value,
                self.state,
                offset_of!(CpuState, mmx_mode) as i32,
            );
        }
        for (index, var) in self.xmm.iter().enumerate() {
            if let Some(var) = var {
                let value = self.b.use_var(*var);
                self.b.ins().store(
                    MemFlags::new(),
                    value,
                    self.state,
                    (offset_of!(CpuState, xmm) + index * 16) as i32,
                );
            }
        }
    }
    fn save_state_with_flags(&mut self, pc: Value, flags: Option<Value>) {
        self.save_extended_state();
        for (i, value) in self.gpr_arguments().into_iter().enumerate() {
            self.b
                .ins()
                .store(MemFlags::trusted(), value, self.state, (i * 16) as i32);
        }
        let flags = flags.unwrap_or_else(|| self.materialize_flags());
        self.b.ins().store(
            MemFlags::trusted(),
            flags,
            self.state,
            offset_of!(CpuState, eflags) as i32,
        );
        let fuel = self.b.use_var(self.fuel);
        self.b.ins().store(
            MemFlags::trusted(),
            fuel,
            self.state,
            offset_of!(CpuState, fuel) as i32,
        );
        self.b.ins().store(
            MemFlags::trusted(),
            pc,
            self.state,
            offset_of!(CpuState, eip) as i32,
        );
    }
    fn save_link_state(&mut self) {
        let Some(summary) = self.write_summary else {
            self.save_extended_state();
            return;
        };
        for index in 0..8 {
            if summary.xmm & (1 << index) != 0 {
                let value = self
                    .b
                    .use_var(self.xmm[index].expect("written XMM was initialized"));
                self.b.ins().store(
                    MemFlags::new(),
                    value,
                    self.state,
                    (offset_of!(CpuState, xmm) + index * 16) as i32,
                );
            }
        }
    }
    fn reload_state(&mut self) {
        for (index, var) in self.mmx.iter().enumerate() {
            if let Some(var) = var {
                let bits = self.b.ins().load(
                    types::I64,
                    MemFlags::new(),
                    self.state,
                    (offset_of!(CpuState, mmx) + index * 8) as i32,
                );
                let vector = self.b.ins().scalar_to_vector(types::I64X2, bits);
                let value = self.b.ins().bitcast(
                    types::I8X16,
                    MemFlags::new().with_endianness(ir::Endianness::Little),
                    vector,
                );
                self.b.def_var(*var, value);
            }
        }
        if let Some(var) = self.mmx_mode {
            let value = self.b.ins().load(
                types::I32,
                MemFlags::new(),
                self.state,
                offset_of!(CpuState, mmx_mode) as i32,
            );
            self.b.def_var(var, value);
        }
        for (i, var) in self.regs.iter().enumerate() {
            let v = self
                .b
                .ins()
                .load(types::I32, MemFlags::trusted(), self.state, (i * 4) as i32);
            self.b.def_var(*var, v);
        }
        for (i, var) in self.xmm.iter().enumerate() {
            if let Some(var) = var {
                let v = self.b.ins().load(
                    types::I8X16,
                    MemFlags::new(),
                    self.state,
                    (offset_of!(CpuState, xmm) + i * 16) as i32,
                );
                self.b.def_var(*var, v);
            }
        }
        self.reload_flags();
        let fuel = self.b.ins().load(
            types::I64,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, fuel) as i32,
        );
        self.b.def_var(self.fuel, fuel);
        // Host/REP helpers can edit canonical x87/MMX state, so restart the
        // current prefix from the freshly reloaded pending mode.
        self.begin_mmx_block();
    }
    fn reload_flags(&mut self) {
        let flags = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, eflags) as i32,
        );
        self.b.def_var(self.flags, flags);
        let zero = self.constant(0);
        for var in self.lazy {
            self.b.def_var(var, zero);
        }
        let carry = self.b.ins().band_imm(flags, 1);
        self.b.def_var(self.lazy[4], carry);
    }
    fn trap_call(&mut self, next: u32) {
        let pc = self.constant(next);
        self.save_state(pc);
        self.shadow_clear();
        let pc = self.constant(self.pc);
        let call = self
            .b
            .ins()
            .call(self.trap_ref, &[self.bridge, self.state, pc]);
        let result = self.b.inst_results(call)[0];
        self.reload_state();
        let done = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let resume = self.b.create_block();
        let park = self.b.create_block();
        self.b.ins().brif(done, resume, &[], park, &[]);
        self.b.switch_to_block(park);
        let pc = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, eip) as i32,
        );
        self.exit(Exit::Trap, pc);
        self.b.switch_to_block(resume);
        self.direct(next);
    }
    /// Starts guest instruction `pc`. Source tags are sampler metadata only.
    fn at(&mut self, pc: u32) {
        self.pc = pc;
        self.site = None;
        self.mark(diagnostics::Part::Body);
    }
    fn mark(&mut self, part: diagnostics::Part) {
        let offset = self.pc.wrapping_sub(self.source_origin);
        self.mark_source(diagnostics::guest_source(offset, part));
    }
    fn mark_source(&mut self, source: ir::SourceLoc) {
        if self.source_map {
            self.source = source;
            self.b.set_srcloc(source);
        }
    }
    fn guard(&mut self, bad: Value, reason: Exit, addr: Option<Value>) {
        if let Some(lane) = self.lane {
            let ok = self.b.create_block();
            self.b.ins().brif(bad, lane, &[], ok, &[]);
            self.b.switch_to_block(ok);
            self.b.seal_block(ok);
            return;
        }
        if self.shared_guard(bad, reason, addr) {
            return;
        }
        let fail = self.b.create_block();
        self.b.set_cold_block(fail);
        let ok = self.b.create_block();
        self.b.ins().brif(bad, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        if let Some(addr) = addr {
            self.b.ins().store(
                MemFlags::trusted(),
                addr,
                self.state,
                offset_of!(CpuState, fault_addr) as i32,
            );
        }
        self.exit_const(reason, self.pc);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
    }
    fn direct(&mut self, pc: u32) {
        if self.row_edge(pc) {
            return;
        }
        if let Some(index) = self.fast
            && self.plans[index].header == pc
            && self.plans[index].latch == self.pc
        {
            self.continue_clone(index);
            return;
        }
        let clone = self
            .fast
            .and_then(|index| self.fast_blocks[index].get(&pc).copied());
        if let Some(block) = clone.or_else(|| self.blocks.get(&pc).copied()) {
            if self
                .poll
                .is_some_and(|(plan, _)| plan.crosses(self.poll_block, pc))
                || pc <= self.pc
            {
                let target = self.constant(pc);
                self.interrupt_at(target, false);
                if self.kernel.is_none()
                    && let Some(&counters) = self.coverage.get(&(pc, self.pc))
                {
                    self.count_one(counters, offset_of!(coverage::Counters, checked));
                }
            }
            self.flush_mmx_effects();
            if pc <= self.pc && self.enter_invariant(pc, block) {
                return;
            }
            let versioned = self
                .fast
                .is_none()
                .then(|| {
                    self.plans
                        .iter()
                        .position(|plan| plan.latch == self.pc && plan.header == pc)
                })
                .flatten();
            match versioned {
                Some(index) => self.enter_clone(index, block),
                None => {
                    self.b.ins().jump(block, &[]);
                }
            }
        } else {
            self.exit_const(Exit::Dispatch, pc);
        }
    }
    /// Prove invariant ranges only after one ordinary iteration. Failure is
    /// not a guest fault and is not retried until the next native invocation.
    fn enter_invariant(&mut self, pc: u32, checked: ir::Block) -> bool {
        let Some((entry, attempted)) = self.invariant_gate else {
            return false;
        };
        let plan = self.invariant_plan.as_ref().unwrap();
        if pc != plan.header || self.pc != plan.latch {
            return false;
        }
        let groups = plan.groups.clone();
        let attempt = self.b.create_block();
        let tried = self.b.use_var(attempted);
        self.b.ins().brif(tried, checked, &[], attempt, &[]);
        self.b.switch_to_block(attempt);
        let one = self.constant(1);
        self.b.def_var(attempted, one);
        for group in groups {
            let sum = self.invariant_sum(&group);
            let sum = self.b.ins().uextend(types::I64, sum);
            let start = self.b.ins().iadd_imm(sum, group.low - i64::from(self.base));
            let end = self
                .b
                .ins()
                .iadd_imm(sum, group.high - i64::from(self.base));
            let len = self.b.ins().iconst(types::I64, self.len as i64);
            let required = self.constant(u32::from(
                (if group.read { READ } else { 0 }) | (if group.write { WRITE } else { 0 }),
            ));
            let forbidden = self.constant(u32::from(
                crate::memory::SHARED | if group.write { EXEC } else { 0 },
            ));
            let call = self.b.ins().call(
                self.range_ok_ref,
                &[self.perms, start, end, len, required, forbidden],
            );
            let valid = self.b.inst_results(call)[0];
            let next = self.b.create_block();
            self.b.ins().brif(valid, next, &[], checked, &[]);
            self.b.switch_to_block(next);
        }
        let target = self.constant(pc);
        self.save_state(target);
        let address = self.b.ins().func_addr(self.ptr_ty, entry);
        self.tail_to(address, target);
        true
    }

    fn invariant_sum(&mut self, group: &invariants::Group) -> Value {
        let base = group.base.map(|index| self.b.use_var(self.regs[index]));
        match group.index {
            Some((index, scale)) => {
                let value = self.b.use_var(self.regs[index]);
                let scaled = self.b.ins().imul_imm(value, i64::from(scale));
                base.map_or(scaled, |base| self.b.ins().iadd(base, scaled))
            }
            None => base.expect("invariant operand has a register base or index"),
        }
    }
    /// Whether the fuel pays for every block of one more iteration of loop
    /// `index`, which its kernel charges as a whole.
    fn funded(&mut self, index: usize) -> Value {
        let blocks = self.plans[index].starts.len() as i64;
        let fuel = self.b.use_var(self.fuel);
        self.b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, fuel, blocks)
    }
    /// A kernel's back edge stays in the clone until the counter reaches the
    /// value its fuel pays for; the region's checked header then resumes.
    fn continue_clone(&mut self, index: usize) {
        let header = self.plans[index].header;
        let checked = self.blocks[&header];
        self.flush_mmx_effects();
        let counter = self.plans[index].counter;
        let (_, stop, _) = self.kernel.expect("clones run in kernels");
        let count = self.b.use_var(self.regs[counter]);
        let stop = self.b.use_var(stop);
        let funded = self.b.ins().icmp(IntCC::NotEqual, count, stop);
        let next = self.b.create_block();
        let refill = self.b.create_block();
        self.b.set_cold_block(refill);
        self.b.ins().brif(funded, next, &[], refill, &[]);
        // The checked header may yield or fault before overwriting state.
        self.b.switch_to_block(refill);
        self.b.ins().jump(checked, &[]);
        self.b.switch_to_block(next);
        let target = self.constant(header);
        self.interrupt_at(target, false);
        self.refresh_clone();
        self.resume_clone(index);
    }
    /// On a kernel's back edge, before it reaches the loop header again.
    fn refresh_clone(&mut self) {
        // Redefining each frame pointer here keeps it a block parameter, which
        // instruction selection cannot split back into base, extended
        // register and displacement: a frame access stays one instruction.
        let frames: Vec<_> = self.frames.iter().map(|(&b, &v)| (b, v)).collect();
        for (base, frame) in frames {
            let address = self.frame(base);
            self.b.def_var(frame, address);
        }
        // Likewise the stop value stays one register the latch compares
        // with, instead of being folded into the counter every iteration.
        let (_, stop, _) = self.kernel.expect("clones run in kernels");
        let value = self.b.use_var(stop);
        let value = self.b.ins().iadd_imm(value, 0);
        self.b.def_var(stop, value);
    }
    /// In a kernel, the register of a 16-bit loop counter that `i`
    /// decrements. The clone runs only while the counter is at least one
    /// before it, so the decrement never borrows from the high half.
    fn counting(&self, i: &Instruction) -> Option<usize> {
        let (index, ..) = self.kernel?;
        self.fast?;
        let plan = &self.plans[index];
        (plan.counter_size == 2 && i.ip32() == plan.counter_step).then_some(plan.counter)
    }
    /// The iteration count in a counter register's value of `size` bytes.
    fn counter_value(&mut self, size: u32, value: Value) -> Value {
        if size == 2 {
            self.b.ins().band_imm(value, 0xffff)
        } else {
            value
        }
    }
    /// How far loop `index`'s counter moves from `from` to reach `to`.
    pub(super) fn counter_distance(&mut self, index: usize, from: Value, to: Value) -> Value {
        let plan = &self.plans[index];
        let (size, step) = (plan.counter_size, plan.steps[plan.counter]);
        let distance = if step > 0 {
            self.b.ins().isub(to, from)
        } else {
            self.b.ins().isub(from, to)
        };
        self.counter_value(size, distance)
    }
    /// The counter steps from `from` to `to`, a value it reaches by them.
    pub(super) fn counter_steps(&mut self, index: usize, from: Value, to: Value) -> Value {
        let magnitude = self.plans[index].steps[self.plans[index].counter].abs();
        let distance = self.counter_distance(index, from, to);
        if magnitude == 1 {
            distance
        } else {
            self.b.ins().udiv_imm(distance, magnitude)
        }
    }
    /// The iterations loop `index` has left, as an `I64`, when its latch
    /// branches back: the counter's steps to its bound, rounded up, and one
    /// more for an inclusive bound.
    pub(super) fn remaining(&mut self, index: usize) -> Value {
        let plan = &self.plans[index];
        let (counter, bound, inclusive) = (plan.counter, plan.bound, plan.inclusive);
        let magnitude = plan.steps[counter].abs();
        let count = self.b.use_var(self.regs[counter]);
        let bound = match bound {
            loops::Bound::Constant(value) => self.constant(value),
            loops::Bound::Register(register) => self.b.use_var(self.regs[register]),
            loops::Bound::Memory(operand) => {
                // enter_clone checked these exact bytes before the proof can
                // read them. The proof excludes every loop write; kernel
                // entry sees the same bound with no intervening guest call.
                let address = self.bound_address(operand);
                let address = self.proven_address(address);
                self.b.ins().load(types::I32, MemFlags::new(), address, 0)
            }
        };
        let distance = self.counter_distance(index, count, bound);
        let distance = self.b.ins().uextend(types::I64, distance);
        let bias = if inclusive { magnitude } else { magnitude - 1 };
        let distance = self.b.ins().iadd_imm(distance, bias);
        if magnitude == 1 {
            distance
        } else {
            self.b.ins().udiv_imm(distance, magnitude)
        }
    }
    /// A kernel's fuel when it leaves at `pc`: every iteration its counter
    /// shows as begun pays for all of the loop's blocks, an exit inside an
    /// iteration included. The region charges the blocks it then runs again.
    fn settle_fuel(&mut self, pc: Value) {
        let Some((index, _, entry)) = self.kernel else {
            return;
        };
        let plan = &self.plans[index];
        let (header, step) = (plan.header, plan.counter_step);
        let counter = plan.counter;
        let blocks = plan.starts.len() as i64;
        let fuel = self.b.ins().stack_load(types::I64, entry, 0);
        let first = self.b.ins().stack_load(types::I32, entry, 8);
        let count = self.b.use_var(self.regs[counter]);
        let done = self.counter_steps(index, first, count);
        let mut done = self.b.ins().uextend(types::I64, done);
        // Inside an iteration, up to the counter's step.
        match self.known(pc) {
            Some(pc) => {
                if header < pc && pc <= step {
                    done = self.b.ins().iadd_imm(done, 1);
                }
            }
            None => {
                let relative = self.b.ins().iadd_imm(pc, -i64::from(header) - 1);
                let inside = self.b.ins().icmp_imm(
                    IntCC::UnsignedLessThan,
                    relative,
                    i64::from(step - header),
                );
                let inside = self.b.ins().uextend(types::I64, inside);
                done = self.b.ins().iadd(done, inside);
            }
        }
        if let Some(counters) = self.plan_counters(index) {
            self.count(counters, offset_of!(coverage::Counters, kernel), done);
        }
        let charge = self.b.ins().imul_imm(done, blocks);
        let fuel = self.b.ins().isub(fuel, charge);
        self.b.def_var(self.fuel, fuel);
    }
    /// The host address of guest register `base`'s value.
    fn frame(&mut self, base: usize) -> Value {
        let value = self.b.use_var(self.regs[base]);
        let biased = self.b.ins().iadd_imm(self.mem, -i64::from(self.base));
        let offset = self.b.ins().uextend(self.ptr_ty, value);
        self.b.ins().iadd(biased, offset)
    }
    /// A checked back edge enters the clone once the remaining iterations'
    /// operand ranges are proven. A failed proof waits `COOLDOWN` iterations
    /// before the next attempt, so unprovable loops stay cheap.
    fn enter_clone(&mut self, index: usize, checked: ir::Block) {
        let cooldown = self.cooldowns[index];
        let wait = self.b.use_var(cooldown);
        let attempt = self.b.create_block();
        let waiting = self.b.create_block();
        self.b.ins().brif(wait, waiting, &[], attempt, &[]);
        self.b.switch_to_block(waiting);
        let left = self.b.ins().iadd_imm(wait, -1);
        self.b.def_var(cooldown, left);
        self.b.ins().jump(checked, &[]);
        self.b.switch_to_block(attempt);
        let funded = self.funded(index);
        let prove = self.b.create_block();
        self.b.ins().brif(funded, prove, &[], checked, &[]);
        self.b.switch_to_block(prove);
        let failed = self.b.create_block();
        self.b.set_cold_block(failed);
        if let loops::Bound::Memory(operand) = self.plans[index].bound {
            // Failure is a refusal to optimize, not an early guest fault.
            // The checked loop must still commit its prefix before CMP faults.
            let address = self.bound_address(operand);
            let address = self.b.ins().uextend(types::I64, address);
            let start = self.b.ins().iadd_imm(address, -i64::from(self.base));
            let end = self.b.ins().iadd_imm(start, 4);
            let len = self.b.ins().iconst(types::I64, self.len as i64);
            let required = self.constant(u32::from(READ));
            let forbidden = self.constant(u32::from(crate::memory::SHARED));
            let call = self.b.ins().call(
                self.range_ok_ref,
                &[self.perms, start, end, len, required, forbidden],
            );
            let valid = self.b.inst_results(call)[0];
            let readable = self.b.create_block();
            self.b.ins().brif(valid, readable, &[], failed, &[]);
            self.b.switch_to_block(readable);
        }
        let proven = self.loop_proof(index);
        let load = self.b.create_block();
        self.b.ins().brif(proven, load, &[], failed, &[]);
        self.b.switch_to_block(load);
        self.b.seal_block(load);
        self.call_kernel(index);
        self.b.switch_to_block(failed);
        if let Some(counters) = self.plan_counters(index) {
            self.count_one(counters, offset_of!(coverage::Counters, failures));
        }
        let wait = self.constant(COOLDOWN);
        self.b.def_var(cooldown, wait);
        self.b.ins().jump(checked, &[]);
    }
    /// Runs loop `index` in its kernel with the state at its header. The kernel
    /// returns `Dispatch` to resume here at the saved PC; any other exit has
    /// already saved the state and leaves the region with the same reason.
    fn call_kernel(&mut self, index: usize) {
        let header = self.constant(self.plans[index].header);
        self.save_state(header);
        let address = self.b.ins().iconst(self.ptr_ty, self.kernels[index] as i64);
        let args = self.tail_arguments(header);
        let call = self.b.ins().call_indirect(self.tail_ref, address, &args);
        let reason = self.b.inst_results(call)[0];
        let resume = self.b.create_block();
        let leave = self.b.create_block();
        self.b.set_cold_block(leave);
        let dispatch = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, reason, Exit::Dispatch as i64);
        self.b.ins().brif(dispatch, resume, &[], leave, &[]);
        self.b.switch_to_block(leave);
        self.b.seal_block(leave);
        self.b.ins().return_(&[reason]);
        self.b.switch_to_block(resume);
        self.b.seal_block(resume);
        // The resumed path leaves through the dispatcher; other edges of the
        // current guest block keep composing the MMX prefix they had.
        let effects = self.mmx_effects.take();
        self.reload_state();
        let pc = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, eip) as i32,
        );
        // A kernel that stopped inside its loop met memory its windows
        // reject; the checked loop runs a while before the next call.
        if let Some(exit) = self
            .row_hooks
            .iter()
            .find(|hook| hook.nest.inner == index)
            .map(|hook| hook.nest.exit)
        {
            if let Some(&block) = self.blocks.get(&exit) {
                let same = self.b.ins().icmp_imm(IntCC::Equal, pc, i64::from(exit));
                let other = self.b.create_block();
                self.b.ins().brif(same, block, &[], other, &[]);
                self.b.switch_to_block(other);
            }
        }
        let (first, last) = (self.plans[index].header, self.plans[index].latch);
        let inside = self.b.ins().iadd_imm(pc, -i64::from(first) - 1);
        let inside =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedLessThan, inside, i64::from(last - first));
        let cooldown = self.cooldowns[index];
        let wait = self.b.use_var(cooldown);
        let rest = self.constant(COOLDOWN);
        let wait = self.b.ins().select(inside, rest, wait);
        self.b.def_var(cooldown, wait);
        if self.shadow_ref.is_some() {
            let absent = self.b.ins().iconst(self.ptr_ty, 0);
            self.b
                .ins()
                .jump(self.dispatch, &[pc.into(), absent.into()]);
        } else {
            self.b.ins().jump(self.dispatch, &[pc.into()]);
        }
        self.mmx_effects = effects;
    }
    /// A kernel's entry: the region proved the loop and saved the state at
    /// its header. Forwarded slots start from memory; windows start empty.
    fn enter_kernel(&mut self, index: usize) {
        // Count at the actual entry, including row-pool calls which bypass
        // the region's ordinary call_kernel path. These are atomic counters.
        if let Some(counters) = self.plan_counters(index) {
            self.count_one(counters, offset_of!(coverage::Counters, calls));
        }
        let plan = &self.plans[index];
        let (counter, step) = (plan.counter, plan.steps[plan.counter]);
        let blocks = plan.starts.len() as i64;
        // The fuel pays for whole iterations: the loop stops early when the
        // counter reaches `stop` and charges what ran when it leaves.
        let fuel = self.b.use_var(self.fuel);
        let first = self.b.use_var(self.regs[counter]);
        let entry = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        self.b.ins().stack_store(fuel, entry, 0);
        self.b.ins().stack_store(first, entry, 8);
        let count = self.remaining(index);
        let affordable = self.b.ins().udiv_imm(fuel, blocks);
        let limit = self.b.ins().umin(affordable, count);
        // Fewer steps than wrap the counter: it meets `stop` only there. A
        // 16-bit counter's high half never changes (`counting`).
        let most = self
            .b
            .ins()
            .iconst(types::I64, u32::MAX as i64 / step.abs());
        let limit = self.b.ins().umin(limit, most);
        let limit = self.b.ins().ireduce(types::I32, limit);
        let advance = self.b.ins().imul_imm(limit, step);
        let stop = self.b.ins().iadd(first, advance);
        let stop_var = self.b.declare_var(types::I32);
        self.b.def_var(stop_var, stop);
        self.kernel = Some((index, stop_var, entry));
        let plan = &self.plans[index];
        let bases: BTreeSet<usize> = plan
            .proven
            .values()
            .filter(|operand| operand.index.is_none())
            .filter_map(|operand| operand.base)
            .filter(|&base| plan.invariant & 1 << base != 0)
            .collect();
        for base in bases {
            let frame = self.b.declare_var(self.ptr_ty);
            let address = self.frame(base);
            self.b.def_var(frame, address);
            self.frames.insert(base, frame);
        }
        let slots = self.plans[index].slots.clone();
        for (slot, (base, displacement)) in slots.into_iter().enumerate() {
            let address = self.b.use_var(self.regs[base]);
            let address = self.b.ins().iadd_imm(address, displacement);
            let address = self.proven_address(address);
            let value = self.b.ins().load(types::I32, MemFlags::new(), address, 0);
            self.b.def_var(self.slot_vars[index][slot], value);
        }
        self.begin_lanes(index);
        self.resume_clone(index);
    }
    /// The bound's exact x86 effective address, including 32-bit wrapping.
    fn bound_address(&mut self, operand: loops::Operand) -> Value {
        let mut address = self.constant(operand.displacement as u32);
        if let Some(base) = operand.base {
            let base = self.b.use_var(self.regs[base]);
            address = self.b.ins().iadd(address, base);
        }
        if let Some((index, scale)) = operand.index {
            let index = self.b.use_var(self.regs[index]);
            let index = self.b.ins().imul_imm(index, i64::from(scale));
            address = self.b.ins().iadd(address, index);
        }
        address
    }
    /// Whether every group of loop `index` stays in ordinary memory for the
    /// iterations left when its latch branches back, at least one.
    fn loop_proof(&mut self, index: usize) -> Value {
        let groups = self.plans[index].groups.clone();
        let count = self.remaining(index);
        let more = self.b.ins().iadd_imm(count, -1);
        let len = self.b.ins().iconst(types::I64, self.len as i64);
        let some = self.b.ins().icmp_imm(IntCC::NotEqual, count, 0);
        let mut ok = self.b.ins().uextend(types::I32, some);
        for group in groups {
            let mut address = self.constant(0);
            if let Some(base) = group.base {
                let value = self.b.use_var(self.regs[base]);
                address = self.b.ins().iadd(address, value);
            }
            if let Some((index, scale)) = group.index {
                let value = self.b.use_var(self.regs[index]);
                let scaled = self.b.ins().imul_imm(value, i64::from(scale));
                address = self.b.ins().iadd(address, scaled);
            }
            let address = self.b.ins().uextend(types::I64, address);
            let base = i64::from(self.base);
            let mut start = self.b.ins().iadd_imm(address, group.low - base);
            let mut end = self.b.ins().iadd_imm(address, group.high - base);
            let span = self.b.ins().imul_imm(more, group.step.abs());
            if group.step >= 0 {
                end = self.b.ins().iadd(end, span);
            } else {
                start = self.b.ins().isub(start, span);
            }
            let mut required = 0;
            let mut forbidden = u32::from(crate::memory::SHARED);
            if group.read {
                required |= u32::from(READ);
            }
            if group.write {
                required |= u32::from(WRITE);
                forbidden |= u32::from(EXEC);
            }
            let required = self.constant(required);
            let forbidden = self.constant(forbidden);
            let call = self.b.ins().call(
                self.range_ok_ref,
                &[self.perms, start, end, len, required, forbidden],
            );
            let result = self.b.inst_results(call)[0];
            ok = self.b.ins().band(ok, result);
        }
        // A speculative trip count is valid only if ALL stores miss its
        // bound, including stores forwarded through a promoted stack slot.
        if let loops::Bound::Memory(operand) = self.plans[index].bound {
            let address = self.bound_address(operand);
            let start = self.b.ins().uextend(types::I64, address);
            let end = self.b.ins().iadd_imm(start, 4);
            let accesses = self.plans[index].accesses.clone();
            for (_, access, _, write) in accesses {
                if !write {
                    continue;
                }
                let (write_start, write_end) = self.access_span(&access, more);
                let below = self
                    .b
                    .ins()
                    .icmp(IntCC::SignedLessThanOrEqual, write_end, start);
                let above = self
                    .b
                    .ins()
                    .icmp(IntCC::SignedLessThanOrEqual, end, write_start);
                let apart = self.b.ins().bor(below, above);
                let apart = self.b.ins().uextend(types::I32, apart);
                ok = self.b.ins().band(ok, apart);
            }
        }
        // Forwarded slots must lie outside every other proven write. Writes
        // through a slot's own base without an index are disjoint by plan.
        let plan = &self.plans[index];
        let (slots, writes) = (plan.slots.clone(), plan.writes.clone());
        for (slot_base, displacement) in slots {
            let slot = self.b.use_var(self.regs[slot_base]);
            let slot = self.b.ins().iadd_imm(slot, displacement);
            let slot = self.b.ins().uextend(types::I64, slot);
            let slot_start = slot;
            let slot_end = self.b.ins().iadd_imm(slot_start, 4);
            for write in &writes {
                if write.base == Some(slot_base) && write.index.is_none() {
                    continue;
                }
                let (start, end) = self.access_span(write, more);
                let below = self
                    .b
                    .ins()
                    .icmp(IntCC::SignedLessThanOrEqual, end, slot_start);
                let above = self
                    .b
                    .ins()
                    .icmp(IntCC::SignedLessThanOrEqual, slot_end, start);
                let apart = self.b.ins().bor(below, above);
                let apart = self.b.ins().uextend(types::I32, apart);
                ok = self.b.ins().band(ok, apart);
            }
        }
        ok
    }
    /// Guest byte range `[start, end)` of a proven operand over the
    /// iterations left: iteration zero's bytes extended by `more` steps.
    fn access_span(&mut self, access: &loops::Access, more: Value) -> (Value, Value) {
        // Match the range proof and x86 effective addresses: base + index *
        // scale wraps at 32 bits before widening. Otherwise a wrapping write
        // can appear above 4 GiB and hide its alias with a forwarded slot.
        let mut address = self.constant(0);
        if let Some(base) = access.base {
            let value = self.b.use_var(self.regs[base]);
            address = self.b.ins().iadd(address, value);
        }
        if let Some((index, scale)) = access.index {
            let value = self.b.use_var(self.regs[index]);
            let scaled = self.b.ins().imul_imm(value, i64::from(scale));
            address = self.b.ins().iadd(address, scaled);
        }
        let address = self.b.ins().uextend(types::I64, address);
        let mut start = self.b.ins().iadd_imm(address, access.low);
        let mut end = self.b.ins().iadd_imm(address, access.high);
        let span = self.b.ins().imul_imm(more, access.step.abs());
        if access.step >= 0 {
            end = self.b.ins().iadd(end, span);
        } else {
            start = self.b.ins().isub(start, span);
        }
        (start, end)
    }
    /// The forwarded slot variable for the current instruction's operand.
    fn slot(&self, ty: ir::Type) -> Option<Variable> {
        let index = self.fast?;
        let slot = *self.plans[index].promoted.get(&self.pc)?;
        (ty == types::I32).then(|| self.slot_vars[index][slot])
    }
    /// The host address of an operand the current loop clone has proven, as a
    /// base and a constant offset. The proof keeps `base + index * scale +
    /// displacement` from wrapping, so the displacement can stay an immediate
    /// of the host access: loop-invariant bases then share one hoisted value
    /// instead of one per displacement.
    fn proven(&mut self, size: usize) -> Option<(Value, i32)> {
        if self.invariant_body
            && let Some(plan) = &self.invariant_plan
            && let Some(site) = plan.sites.get(&self.pc)
            && size as u32 <= site.width
        {
            let displacement = site.displacement;
            let group = plan.groups[site.group].clone();
            let sum = self.invariant_sum(&group);
            let sum = self.b.ins().uextend(self.ptr_ty, sum);
            let biased = self.b.ins().iadd_imm(self.mem, -i64::from(self.base));
            let address = self.b.ins().iadd(biased, sum);
            return Some((address, displacement));
        }
        let plan = &self.plans[self.fast?];
        let operand = *plan.proven.get(&self.pc)?;
        if size as u32 > operand.width {
            return None;
        }
        if matches!(plan.bound, loops::Bound::Memory(bound) if bound == operand) {
            // The dedicated precheck proves the fully wrapped address, not
            // a signed displacement from an unwrapped host base. In
            // particular absolute guest addresses above 2 GiB stay positive.
            let address = self.bound_address(operand);
            return Some((self.proven_address(address), 0));
        }
        if operand.index.is_none()
            && let Some(frame) = operand.base.and_then(|base| self.frames.get(&base))
        {
            let frame = *frame;
            return Some((self.b.use_var(frame), operand.displacement));
        }
        let mut sum = None;
        if let Some(base) = operand.base {
            sum = Some(self.b.use_var(self.regs[base]));
        }
        if let Some((index, scale)) = operand.index {
            let value = self.b.use_var(self.regs[index]);
            let scaled = self.b.ins().imul_imm(value, i64::from(scale));
            sum = Some(match sum {
                Some(sum) => self.b.ins().iadd(sum, scaled),
                None => scaled,
            });
        }
        let biased = self.b.ins().iadd_imm(self.mem, -i64::from(self.base));
        let address = match sum {
            Some(sum) => {
                let sum = self.b.ins().uextend(self.ptr_ty, sum);
                self.b.ins().iadd(biased, sum)
            }
            None => biased,
        };
        Some((address, operand.displacement))
    }
    fn proven_address(&mut self, addr: Value) -> Value {
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let offset = self.b.ins().uextend(self.ptr_ty, offset);
        self.b.ins().iadd(self.mem, offset)
    }
    fn indirect(&mut self, target: Value) {
        self.indirect_predicted(target, None);
    }
    fn indirect_predicted(&mut self, target: Value, prediction: Option<Value>) {
        self.flush_mmx_effects();
        if self.shadow_ref.is_some() {
            let prediction = prediction.unwrap_or_else(|| self.b.ins().iconst(self.ptr_ty, 0));
            self.b
                .ins()
                .jump(self.dispatch, &[target.into(), prediction.into()]);
        } else {
            self.b.ins().jump(self.dispatch, &[target.into()]);
        }
    }
    fn record_indirect(&mut self, target: Value, kind: crate::IndirectBranchKind) {
        // Only committed indirect transfers pay these plain state stores.
        // No allocation, callback, clock read or Rust dispatch is introduced.
        let base = offset_of!(CpuState, last_indirect_branch) as i32;
        let source = self.constant(self.pc);
        let kind = self.constant(kind as u32);
        for (value, offset) in [
            (source, offset_of!(crate::IndirectBranch, source)),
            (target, offset_of!(crate::IndirectBranch, target)),
            (kind, offset_of!(crate::IndirectBranch, kind)),
        ] {
            self.b
                .ins()
                .store(MemFlags::trusted(), value, self.state, base + offset as i32);
        }
    }
    fn interrupt_at(&mut self, pc: Value, respect_stops: bool) {
        self.poll_budget(pc);
        let pending = self
            .b
            .ins()
            .atomic_load(types::I8, MemFlags::trusted(), self.interrupt);
        let mut pending = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
        // A clone checks interrupts after funding its next iteration.
        if self.fast.is_none() {
            let fuel = self.b.use_var(self.fuel);
            let work = self.b.ins().icmp_imm(IntCC::NotEqual, fuel, 0);
            pending = self.b.ins().band(pending, work);
        }
        if respect_stops {
            let stop = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.context,
                offset_of!(NativeContext, stop) as i32,
            );
            let ordinary = self.b.ins().icmp(IntCC::NotEqual, pc, stop);
            pending = self.b.ins().band(pending, ordinary);
        }
        let interrupted = self.b.create_block();
        self.b.set_cold_block(interrupted);
        let continued = self.b.create_block();
        self.b.ins().brif(pending, interrupted, &[], continued, &[]);
        self.b.switch_to_block(interrupted);
        self.exit(Exit::Interrupted, pc);
        self.b.switch_to_block(continued);
    }
    fn address(&mut self, i: &Instruction) -> Value {
        if let Some((pc, addr, _)) = self.site
            && pc == self.pc
        {
            return addr;
        }
        let mut addr = self.constant(i.memory_displacement32());
        if i.memory_base() != R::None {
            let value = self.read_register(i.memory_base());
            addr = self.b.ins().iadd(addr, value);
        }
        if i.memory_index() != R::None {
            let value = self.read_register(i.memory_index());
            let scaled = self
                .b
                .ins()
                .imul_imm(value, i64::from(i.memory_index_scale()));
            addr = self.b.ins().iadd(addr, scaled);
        }
        if i.memory_base().is_gpr16() || i.memory_index().is_gpr16() {
            addr = self.b.ins().band_imm(addr, 0xffff);
        }
        if i.segment_prefix() == R::FS && i.mnemonic() != M::Lea {
            let fs = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.state,
                offset_of!(CpuState, fs_base) as i32,
            );
            addr = self.b.ins().iadd(addr, fs);
        }
        addr
    }
    fn known(&self, value: Value) -> Option<u32> {
        let value = if self.optimizing {
            self.b.func.dfg.resolve_aliases(value)
        } else {
            value
        };
        let inst = self.b.func.dfg.value_def(value).inst()?;
        match self.b.func.dfg.insts[inst] {
            ir::InstructionData::UnaryImm {
                opcode: ir::Opcode::Iconst,
                imm,
            } => Some(imm.bits() as u32),
            _ => None,
        }
    }
    /// Range-checks one access. Returns its window offset and the distinct page
    /// positions to validate; an in-range constant address needs no guard.
    fn access_range(&mut self, addr: Value, size: usize, reason: Exit) -> (Value, Vec<Value>) {
        if let Some(offset) = self.known(addr).map(|addr| addr.wrapping_sub(self.base))
            && offset as usize <= self.len - size
        {
            let end = offset + size as u32 - 1;
            let mut positions = vec![self.constant(offset)];
            if end >> 12 != offset >> 12 {
                positions.push(self.constant(end));
            }
            return (positions[0], positions);
        }
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let bad =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, offset, (self.len - size) as i64);
        self.guard(bad, reason, Some(addr));
        if size == 1 {
            return (offset, vec![offset]);
        }
        let end = self.b.ins().iadd_imm(offset, size as i64 - 1);
        (offset, vec![offset, end])
    }
    /// Loads exact permissions from the pinned native metadata snapshot.
    /// File promotions retain their conservative SHARED classification until
    /// re-entry; permission revocations leave native through the trap epoch.
    /// Equal loads merge; constant in-window positions may leave loops.
    fn permission(&mut self, position: Value) -> Value {
        let mut flags = MemFlags::trusted().with_readonly();
        if self
            .known(position)
            .is_some_and(|position| (position as usize) < self.len)
        {
            flags = flags.with_can_move();
        }
        let page = self.b.ins().ushr_imm(position, 12);
        let page = self.b.ins().uextend(self.ptr_ty, page);
        let at = self.b.ins().iadd(self.perms, page);
        self.b.ins().load(types::I8, flags, at, 0)
    }
    /// Nonzero unless every page grants `permission` and none has a
    /// `forbidden` bit: one branch however many pages an access touches.
    fn denied(&mut self, permissions: &[Value], permission: u8, forbidden: u8) -> Value {
        let mask = if let [p] = permissions {
            self.b.ins().band_imm(*p, i64::from(permission | forbidden))
        } else {
            let all = permissions[1..]
                .iter()
                .fold(permissions[0], |all, &p| self.b.ins().band(all, p));
            let any = permissions[1..]
                .iter()
                .fold(permissions[0], |any, &p| self.b.ins().bor(any, p));
            let granted = self.b.ins().band_imm(all, i64::from(permission));
            let forbidden = self.b.ins().band_imm(any, i64::from(forbidden));
            self.b.ins().bor(granted, forbidden)
        };
        self.b
            .ins()
            .icmp_imm(IntCC::NotEqual, mask, i64::from(permission))
    }
    fn pointer(&mut self, addr: Value, size: usize) -> GuestPointer {
        if let Some((address, offset)) = self.proven(size) {
            let ordinary = self.b.ins().iconst(types::I8, 0);
            return GuestPointer {
                address,
                offset,
                guest: addr,
                shared: ordinary,
                file: ordinary,
            };
        }
        if self.validated(addr, size, false) && self.validated(addr, size, true) {
            let address = self.window_address(addr);
            let ordinary = self.b.ins().iconst(types::I8, 0);
            return GuestPointer {
                address,
                offset: 0,
                guest: addr,
                shared: ordinary,
                file: ordinary,
            };
        }
        self.mark(diagnostics::Part::Check);
        let (offset, positions) = self.access_range(addr, size, Exit::WriteFault);
        // Both metadata addresses are safe after the range guard. Loading them
        // together lets either original SHARED guard enter one cold path, where
        // all permissions are checked before file preparation or SSA mutation.
        let permissions: Vec<_> = positions.iter().map(|&p| self.permission(p)).collect();
        let combined = permissions[1..]
            .iter()
            .fold(permissions[0], |all, &p| self.b.ins().bor(all, p));
        let shared = self
            .b
            .ins()
            .band_imm(combined, i64::from(crate::memory::SHARED));
        let executable = self.b.ins().band_imm(combined, i64::from(EXEC));
        let executable = self.b.ins().icmp_imm(IntCC::NotEqual, executable, 0);
        let no_file = self.b.ins().iconst(types::I8, 0);
        if let Some(lane) = self.lane {
            let bad = self.denied(&permissions, WRITE, crate::memory::SHARED);
            let ok = self.b.create_block();
            self.b.ins().brif(bad, lane, &[], ok, &[]);
            self.b.switch_to_block(ok);
            self.b.seal_block(ok);
            self.guard(executable, Exit::Fallback, None);
            self.mark(diagnostics::Part::Body);
            let host_offset = self.b.ins().uextend(self.ptr_ty, offset);
            let address = self.b.ins().iadd(self.mem, host_offset);
            return GuestPointer {
                address,
                offset: 0,
                guest: addr,
                shared: no_file,
                file: no_file,
            };
        }
        let cold = self.b.create_block();
        self.b.set_cold_block(cold);
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        // The cold path also takes the Fallback exit for executable pages.
        let bad = self.denied(&permissions, WRITE, crate::memory::SHARED | EXEC);
        let ok = self.b.create_block();
        self.b.ins().brif(bad, cold, &[], ok, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
        self.b.ins().jump(done, &[no_file.into()]);
        self.b.switch_to_block(cold);
        self.mark(diagnostics::Part::Slow);
        if self.optimizing {
            // A single fault edge suffices: no access occurs between page
            // checks and both failures report the original operand address.
            let missing = self.denied(&permissions, WRITE, 0);
            self.guard(missing, Exit::WriteFault, Some(addr));
            self.b.set_cold_block(self.b.current_block().unwrap());
        } else {
            for &p in &permissions {
                let access = self.b.ins().band_imm(p, i64::from(WRITE));
                let missing = self.b.ins().icmp_imm(IntCC::Equal, access, 0);
                self.guard(missing, Exit::WriteFault, Some(addr));
                self.b.set_cold_block(self.b.current_block().unwrap());
            }
        }
        self.guard(executable, Exit::Fallback, None);
        self.b.set_cold_block(self.b.current_block().unwrap());
        let file: Vec<_> = permissions
            .iter()
            .map(|&p| {
                let kind = self
                    .b
                    .ins()
                    .band_imm(p, i64::from(crate::memory::SHARED | crate::memory::AUDIO));
                self.b
                    .ins()
                    .icmp_imm(IntCC::Equal, kind, i64::from(crate::memory::SHARED))
            })
            .collect();
        let file = file[1..]
            .iter()
            .fold(file[0], |all, &f| self.b.ins().bor(all, f));
        let prepare = self.b.create_block();
        self.b.set_cold_block(prepare);
        let pcm = self.b.create_block();
        self.b.set_cold_block(pcm);
        self.b.ins().brif(file, prepare, &[], pcm, &[]);
        self.b.switch_to_block(pcm);
        self.b.ins().jump(done, &[no_file.into()]);
        self.b.switch_to_block(prepare);
        let width = self.constant(size as u32);
        let call = self
            .b
            .ins()
            .call(self.file_prepare_ref, &[self.bridge, addr, width]);
        let failed = self.b.inst_results(call)[0];
        self.guard(failed, Exit::WriteFault, Some(addr));
        self.b.set_cold_block(self.b.current_block().unwrap());
        let is_file = self.b.ins().iconst(types::I8, 1);
        self.b.ins().jump(done, &[is_file.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        self.mark(diagnostics::Part::Body);
        let file = self.b.block_params(done)[0];
        let host_offset = self.b.ins().uextend(self.ptr_ty, offset);
        let address = self.b.ins().iadd(self.mem, host_offset);
        GuestPointer {
            address,
            offset: 0,
            guest: addr,
            shared,
            file,
        }
    }
    /// Loads and plain stores can finish their shared access inside the original
    /// page-guard cold path. No classification branch runs after an ordinary
    /// guard succeeds. RMW stores retain pointer()'s early prevalidation because
    /// their flags and value are computed between validation and writeback.
    fn memory_access(&mut self, addr: Value, ty: ir::Type, value: Option<Value>) -> Option<Value> {
        let write = value.is_some();
        let size = ty.bytes() as usize;
        // A forwarded slot loads its value; stores also write memory below.
        if let Some(slot) = self.slot(ty) {
            match value {
                None => return Some(self.b.use_var(slot)),
                Some(value) => self.b.def_var(slot, value),
            }
        }
        if let Some((address, offset)) = self.proven(size) {
            return match value {
                Some(value) => {
                    self.b.ins().store(MemFlags::new(), value, address, offset);
                    None
                }
                None => Some(self.b.ins().load(ty, MemFlags::new(), address, offset)),
            };
        }
        if self.validated(addr, size, write) {
            let address = self.window_address(addr);
            return match value {
                Some(value) => {
                    self.b.ins().store(MemFlags::new(), value, address, 0);
                    None
                }
                None => Some(self.b.ins().load(ty, MemFlags::new(), address, 0)),
            };
        }
        self.checked_memory(addr, ty, value)
    }
    /// In a kernel, validates the windows of the instruction's memory operand
    /// before any of its effects. An access outside ordinary memory leaves
    /// the kernel at the instruction, which the region's checked block then
    /// performs. The loop body thus calls nothing, so its values can stay in
    /// caller-saved registers.
    fn precheck(&mut self, i: &Instruction) {
        let Some(index) = self.fast else {
            return;
        };
        if self.lane.is_some() {
            return;
        }
        let sites: Vec<_> = [false, true]
            .into_iter()
            .filter_map(|write| {
                let window = self.windows[index].get(&(self.pc, write))?;
                Some((write, *window))
            })
            .collect();
        if sites.is_empty() {
            return;
        }
        let addr = self.address(i);
        let size = i.memory_size().size() as u32;
        for (write, (low_var, span_var)) in sites {
            self.mark(diagnostics::Part::Check);
            let low = self.b.use_var(low_var);
            let span = self.b.use_var(span_var);
            let relative = self.b.ins().isub(addr, low);
            let hit = self.b.ins().icmp(IntCC::UnsignedLessThan, relative, span);
            let refill = self.b.create_block();
            self.b.set_cold_block(refill);
            let next = self.b.create_block();
            self.b.ins().brif(hit, next, &[], refill, &[]);
            self.b.switch_to_block(refill);
            self.b.seal_block(refill);
            self.mark(diagnostics::Part::Slow);
            let (low, span) = self.refill_window(addr, size, write);
            self.b.def_var(low_var, low);
            self.b.def_var(span_var, span);
            self.b.ins().jump(next, &[]);
            self.b.switch_to_block(next);
            self.b.seal_block(next);
        }
        self.mark(diagnostics::Part::Body);
        self.site = Some((self.pc, addr, size));
    }
    /// The window around an access of `size` bytes at `addr` that missed its
    /// window: the adjacent pages, up to `WINDOW_PAGES` on each side, that
    /// all grant the access and have no forbidden bit. Leaves the kernel at
    /// the instruction when the access itself is not ordinary memory.
    fn refill_window(&mut self, addr: Value, size: u32, write: bool) -> (Value, Value) {
        let (required, forbidden) = if write {
            (WRITE, crate::memory::SHARED | EXEC)
        } else {
            (READ, crate::memory::SHARED)
        };
        let (mask, required) = (i64::from(required | forbidden), i64::from(required));
        let page = crate::memory::PAGE_SIZE as i64;
        let len = self.len as i64;
        let reach = loops::WINDOW_PAGES;
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let start = self.b.ins().uextend(types::I64, offset);
        let end = self.b.ins().iadd_imm(start, i64::from(size));
        let outside = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, end, len);
        let blocks: [ir::Block; 7] = std::array::from_fn(|_| self.b.create_block());
        let [deopt, inside, down, probe_down, up, probe_up, finish] = blocks;
        for block in blocks {
            self.b.set_cold_block(block);
        }
        self.b.append_block_param(down, types::I64);
        for block in [up, finish] {
            self.b.append_block_param(block, types::I64);
            self.b.append_block_param(block, types::I64);
        }
        self.b.ins().brif(outside, deopt, &[], inside, &[]);
        // The access's own pages, then the limits of the scan around them.
        self.b.switch_to_block(inside);
        self.b.seal_block(inside);
        let first = self.b.ins().ushr_imm(start, 12);
        let last = self.b.ins().iadd_imm(end, -1);
        let last = self.b.ins().ushr_imm(last, 12);
        let first_ok = self.page_grants(first, mask, required);
        let last_ok = self.page_grants(last, mask, required);
        let ok = self.b.ins().band(first_ok, last_ok);
        let near = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, first, reach);
        let zero = self.b.ins().iconst(types::I64, 0);
        let below = self.b.ins().iadd_imm(first, -reach);
        let floor = self.b.ins().select(near, zero, below);
        let above = self.b.ins().iadd_imm(last, reach);
        let top = self.b.ins().iconst(types::I64, (len + page - 1) / page - 1);
        let ceiling = self.b.ins().umin(above, top);
        self.b.ins().brif(ok, down, &[first.into()], deopt, &[]);
        self.b.switch_to_block(deopt);
        self.b.seal_block(deopt);
        self.deopt();
        // Extend downwards while the page below grants the access.
        self.b.switch_to_block(down);
        let low = self.b.block_params(down)[0];
        let at_floor = self.b.ins().icmp(IntCC::Equal, low, floor);
        self.b
            .ins()
            .brif(at_floor, up, &[low.into(), last.into()], probe_down, &[]);
        self.b.switch_to_block(probe_down);
        self.b.seal_block(probe_down);
        let lower = self.b.ins().iadd_imm(low, -1);
        let granted = self.page_grants(lower, mask, required);
        self.b.ins().brif(
            granted,
            down,
            &[lower.into()],
            up,
            &[low.into(), last.into()],
        );
        self.b.seal_block(down);
        // Then upwards.
        self.b.switch_to_block(up);
        let low = self.b.block_params(up)[0];
        let high = self.b.block_params(up)[1];
        let at_ceiling = self.b.ins().icmp(IntCC::Equal, high, ceiling);
        self.b.ins().brif(
            at_ceiling,
            finish,
            &[low.into(), high.into()],
            probe_up,
            &[],
        );
        self.b.switch_to_block(probe_up);
        self.b.seal_block(probe_up);
        let higher = self.b.ins().iadd_imm(high, 1);
        let granted = self.page_grants(higher, mask, required);
        self.b.ins().brif(
            granted,
            up,
            &[low.into(), higher.into()],
            finish,
            &[low.into(), high.into()],
        );
        self.b.seal_block(up);
        self.b.switch_to_block(finish);
        self.b.seal_block(finish);
        let low = self.b.block_params(finish)[0];
        let high = self.b.block_params(finish)[1];
        let low = self.b.ins().ishl_imm(low, 12);
        let high = self.b.ins().iadd_imm(high, 1);
        let high = self.b.ins().ishl_imm(high, 12);
        let len = self.b.ins().iconst(types::I64, len);
        let high = self.b.ins().umin(high, len);
        let span = self.b.ins().isub(high, low);
        let span = self.b.ins().iadd_imm(span, 1 - i64::from(size));
        let span = self.b.ins().ireduce(types::I32, span);
        let low = self.b.ins().ireduce(types::I32, low);
        let low = self.b.ins().iadd_imm(low, i64::from(self.base));
        (low, span)
    }
    /// Leaves a kernel for the region to perform the current instruction,
    /// which may raise: unlike other resuming exits, every register and flag
    /// stays exact.
    fn deopt(&mut self) {
        debug_assert!(!self.hot_exits && self.shadow_ref.is_none());
        self.flush_mmx_effects();
        let reason = self.constant(Exit::Dispatch as u32);
        let pc = self.constant(self.pc);
        self.settle_fuel(pc);
        self.b
            .ins()
            .jump(self.shared_exit, &[reason.into(), pc.into()]);
    }
    /// Whether permission page `page` (an I64 index) has exactly `required`
    /// among the bits of `mask`.
    fn page_grants(&mut self, page: Value, mask: i64, required: i64) -> Value {
        let at = self.b.ins().iadd(self.perms, page);
        let flags = MemFlags::trusted().with_readonly();
        let bits = self.b.ins().uload8(types::I32, flags, at, 0);
        let bits = self.b.ins().band_imm(bits, mask);
        self.b.ins().icmp_imm(IntCC::Equal, bits, required)
    }
    /// Whether `precheck` validated this access of `size` bytes at `addr`
    /// for `write`: the current instruction's operand inside its window.
    fn validated(&self, addr: Value, size: usize, write: bool) -> bool {
        let (Some(index), Some((pc, site, width))) = (self.fast, self.site) else {
            return false;
        };
        self.lane.is_none()
            && pc == self.pc
            && site == addr
            && size as u32 <= width
            && self.windows[index].contains_key(&(pc, write))
    }
    fn window_address(&mut self, addr: Value) -> Value {
        let biased = self.b.ins().iadd_imm(self.mem, -i64::from(self.base));
        let offset = self.b.ins().uextend(self.ptr_ty, addr);
        self.b.ins().iadd(biased, offset)
    }
    /// One conservative metadata test on ordinary dynamic accesses. A miss is
    /// not a fault: the precise path still accepts non-crossing accesses next
    /// to a protected/shared page and preserves every special-page operation.
    fn fast_memory_guard(&mut self, addr: Value, size: usize, write: bool, ordinary: ir::Block) {
        debug_assert!((1..=16).contains(&size));
        self.mark(diagnostics::Part::Check);
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let full = self.base == 0 && self.len as u64 == 1_u64 << 32;
        if !full {
            let bad =
                self.b
                    .ins()
                    .icmp_imm(IntCC::UnsignedGreaterThan, offset, (self.len - size) as i64);
            let reason = if write {
                Exit::WriteFault
            } else {
                Exit::ReadFault
            };
            self.guard(bad, reason, Some(addr));
        }
        let page = self.b.ins().ushr_imm(offset, 12);
        let page = self.b.ins().uextend(self.ptr_ty, page);
        let at = self.b.ins().iadd(self.perms, page);
        let mut flags = MemFlags::trusted().with_readonly();
        // Only full windows make every possible u32 page index dereferenceable
        // before the bounds guard. The final entry has no two-page grants.
        if full {
            flags = flags.with_can_move();
        }
        let metadata = self.b.ins().load(types::I8, flags, at, 0);
        let permission = if write { WRITE } else { READ };
        let pair = if write {
            crate::memory::FAST_WRITE
        } else {
            crate::memory::FAST_READ
        };
        let grants = self.b.ins().band_imm(metadata, i64::from(pair));
        let single = self.b.create_block();
        // Allocation-end pages are common; keep their ordinary proof warm.
        let cold = self.b.create_block();
        self.b.set_cold_block(cold);
        self.b.ins().brif(grants, ordinary, &[], single, &[]);
        self.b.switch_to_block(single);
        self.b.seal_block(single);
        // Stack/heap allocation end pages often have an unmapped neighbor.
        // The same metadata load also grants an entirely in-page access;
        // those ordinary accesses must not pay the exact shared-page checks.
        let mask = permission | crate::memory::SHARED | if write { EXEC } else { 0 };
        let single = self.b.ins().band_imm(metadata, i64::from(mask));
        let missing = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, single, i64::from(permission));
        let within = self.b.ins().band_imm(offset, 4095);
        let crosses =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, within, (4096 - size) as i64);
        let bad = self.b.ins().bor(missing, crosses);
        self.b.ins().brif(bad, cold, &[], ordinary, &[]);
        self.b.switch_to_block(cold);
        self.b.seal_block(cold);
    }

    /// An unproven access through the exact page table.
    fn checked_memory(&mut self, addr: Value, ty: ir::Type, value: Option<Value>) -> Option<Value> {
        let write = value.is_some();
        let size = ty.bytes() as usize;
        self.mark(diagnostics::Part::Check);
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        let hardware = if !write
            && self.lane.is_none()
            && self.fault_pc == Some(self.pc)
            && matches!(ty, types::I8 | types::I16 | types::I32)
        {
            self.capture_hardware_read(addr, size)
        } else {
            None
        };
        let reason = if write {
            Exit::WriteFault
        } else {
            Exit::ReadFault
        };
        // Both grants enter the same ordinary load/store block. Duplicating
        // the access would add result phis and lengthen guest-state liveness.
        let fast = self.optimizing && self.known(addr).is_none() && self.lane.is_none();
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        let fast = fast && self.fault_pc != Some(self.pc);
        let ordinary = fast.then(|| self.b.create_block());
        let fast_address = fast.then(|| self.window_address(addr));
        if let Some(ok) = ordinary {
            self.fast_memory_guard(addr, size, write, ok);
        }
        if fast && !write {
            // The pair/single-page guards already admit every ordinary read.
            // A miss can use the transactional helper directly; rebuilding
            // the exact permission graph only duplicates its validation.
            return Some(self.finish_fast_read(addr, fast_address.unwrap(), ty, ordinary.unwrap()));
        }
        let (offset, positions) = self.access_range(addr, size, reason);
        let address = fast_address.unwrap_or_else(|| {
            let host_offset = self.b.ins().uextend(self.ptr_ty, offset);
            self.b.ins().iadd(self.mem, host_offset)
        });
        if fast {
            self.b.set_cold_block(self.b.current_block().unwrap());
        }
        let atomic = self.lane.unwrap_or_else(|| {
            let block = self.b.create_block();
            self.b.set_cold_block(block);
            block
        });
        let done = self.b.create_block();
        if !write {
            self.b.append_block_param(done, ty);
        }
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        if let Some((enabled, recovery)) = hardware {
            let classify = self.b.create_block();
            let checked = self.b.create_block();
            let raw = self.b.create_block();
            self.b.ins().brif(enabled, classify, &[], checked, &[]);
            self.b.switch_to_block(classify);
            let permissions: Vec<_> = positions.iter().map(|&p| self.permission(p)).collect();
            let combined = permissions[1..]
                .iter()
                .fold(permissions[0], |all, &p| self.b.ins().bor(all, p));
            let shared = self
                .b
                .ins()
                .band_imm(combined, i64::from(crate::memory::SHARED));
            self.b.ins().brif(shared, checked, &[], raw, &[]);
            self.b.switch_to_block(raw);
            recovery.publish(&mut self.b, self.ptr_ty, self.context);
            self.b.set_srcloc(ir::SourceLoc::new(fault::LOAD_SOURCE));
            let result = self.b.ins().load(
                ty,
                MemFlags::new().with_trap_code(Some(ir::TrapCode::user(1).unwrap())),
                address,
                0,
            );
            self.b.set_srcloc(self.source);
            self.b.ins().jump(done, &[result.into()]);
            self.b.switch_to_block(checked);
        }
        let permission = if write { WRITE } else { READ };
        let permissions: Vec<_> = positions.iter().map(|&p| self.permission(p)).collect();
        // The slow path revalidates and takes the Fallback exit for executable
        // pages, so outside lanes one branch covers every page and EXEC.
        let fused = write && self.lane.is_none();
        let forbidden = if fused {
            crate::memory::SHARED | EXEC
        } else {
            crate::memory::SHARED
        };
        let bad = self.denied(&permissions, permission, forbidden);
        let ok = ordinary.unwrap_or_else(|| self.b.create_block());
        self.b.ins().brif(bad, atomic, &[], ok, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
        if write && !fused {
            let combined = permissions[1..]
                .iter()
                .fold(permissions[0], |all, &p| self.b.ins().bor(all, p));
            let executable = self.b.ins().band_imm(combined, i64::from(EXEC));
            let executable = self.b.ins().icmp_imm(IntCC::NotEqual, executable, 0);
            self.guard(executable, Exit::Fallback, None);
        }
        self.mark(diagnostics::Part::Body);
        if let Some(value) = value {
            self.b.ins().store(MemFlags::new(), value, address, 0);
            self.b.ins().jump(done, &[]);
        } else {
            let result = self.b.ins().load(ty, MemFlags::new(), address, 0);
            self.b.ins().jump(done, &[result.into()]);
        }
        if self.lane.is_some() {
            self.b.switch_to_block(done);
            self.b.seal_block(done);
            return (!write).then(|| self.b.block_params(done)[0]);
        }
        self.b.switch_to_block(atomic);
        self.mark(diagnostics::Part::Slow);
        // Either boundary guard can enter here. Revalidate *both* pages before
        // any byte is read/written. Mixed file/AUDIO elements use checked Memory
        // helpers, which keep PCM subranges byte atomic.
        let mut combined = None;
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        let compact = hardware.is_none();
        #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
        let compact = true;
        if compact && write {
            let missing = self.denied(&permissions, permission, 0);
            self.guard(missing, reason, Some(addr));
            self.b.set_cold_block(self.b.current_block().unwrap());
            combined = Some(
                permissions[1..]
                    .iter()
                    .fold(permissions[0], |all, &p| self.b.ins().bor(all, p)),
            );
        }
        for &position in positions.iter().filter(|_| !compact) {
            let p = self.permission(position);
            let access = self.b.ins().band_imm(p, i64::from(permission));
            let missing = self.b.ins().icmp_imm(IntCC::Equal, access, 0);
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            if let Some((_, recovery)) = hardware {
                self.checked_read_guard(missing, addr, recovery);
            } else {
                self.guard(missing, reason, Some(addr));
            }
            #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
            self.guard(missing, reason, Some(addr));
            self.b.set_cold_block(self.b.current_block().unwrap());
            combined = Some(if let Some(old) = combined {
                self.b.ins().bor(old, p)
            } else {
                p
            });
        }
        if write {
            let executable = self.b.ins().band_imm(combined.unwrap(), i64::from(EXEC));
            let executable = self.b.ins().icmp_imm(IntCC::NotEqual, executable, 0);
            self.guard(executable, Exit::Fallback, None);
            self.b.set_cold_block(self.b.current_block().unwrap());
        }
        // The cold helper already preserves byte-atomic PCM subranges and
        // prepares every lazy/COW page before a store commits. Share it with
        // pure PCM accesses instead of emitting a second per-width access
        // graph at every guest operand. Permission and code-write guards above
        // retain the original fault address, PC and restartable SSA prefix.
        self.memory_completion(addr, ty, value, done);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        self.mark(diagnostics::Part::Body);
        (!write).then(|| self.b.block_params(done)[0])
    }
    /// Emit the bounded transactional helper in the current cold block.
    fn memory_completion(
        &mut self,
        addr: Value,
        ty: ir::Type,
        value: Option<Value>,
        done: ir::Block,
    ) {
        let write = value.is_some();
        let reason = if write {
            Exit::WriteFault
        } else {
            Exit::ReadFault
        };
        let slot = self.memory_buffer();
        let buffer = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let width = self.constant(ty.bytes());
        let function = if let Some(value) = value {
            self.b.ins().stack_store(value, slot, 0);
            self.file_write_ref
        } else {
            self.file_read_ref
        };
        let call = self
            .b
            .ins()
            .call(function, &[self.bridge, addr, buffer, width]);
        let failed = self.b.inst_results(call)[0];
        self.guard(failed, reason, Some(addr));
        self.b.set_cold_block(self.b.current_block().unwrap());
        if write {
            self.b.ins().jump(done, &[]);
        } else {
            let result = self.b.ins().stack_load(ty, slot, 0);
            self.b.ins().jump(done, &[result.into()]);
        }
    }
    fn finish_fast_read(
        &mut self,
        addr: Value,
        address: Value,
        ty: ir::Type,
        ordinary: ir::Block,
    ) -> Value {
        let done = self.b.create_block();
        self.b.append_block_param(done, ty);
        self.mark(diagnostics::Part::Slow);
        self.memory_completion(addr, ty, None, done);
        self.b.switch_to_block(ordinary);
        self.b.seal_block(ordinary);
        self.mark(diagnostics::Part::Body);
        let result = self.b.ins().load(ty, MemFlags::new(), address, 0);
        self.b.ins().jump(done, &[result.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        self.b.block_params(done)[0]
    }
    fn direct_load(&mut self, addr: Value, ty: ir::Type) -> Value {
        self.memory_access(addr, ty, None).unwrap()
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fn capture_hardware_read(
        &mut self,
        address: Value,
        width: usize,
    ) -> Option<(Value, contract::RecoveryExit)> {
        let variable = self.fault_mode?;
        let outputs = std::array::from_fn(|index| match index {
            0..=7 => self.b.use_var(self.regs[index]),
            8 => self.b.use_var(self.flags),
            9..=15 => self.b.use_var(self.lazy[index - 9]),
            16 => self.b.use_var(self.fuel),
            _ => address,
        });
        let recovery = contract::RecoveryExit::new(&mut self.b);
        let draft = fault::Draft::capture(
            &mut self.b,
            &self.fault_seeds,
            self.state,
            outputs,
            self.fault_entry,
            self.pc,
            width,
            recovery,
        )?;
        let enabled = self.b.use_var(variable);
        let disabled = self.b.ins().iconst(types::I8, 0);
        self.b.def_var(variable, disabled);
        self.fault_return = Some(recovery);
        self.fault_draft = Some(draft);
        Some((enabled, recovery))
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fn checked_read_guard(&mut self, bad: Value, address: Value, recovery: contract::RecoveryExit) {
        let failed = self.b.create_block();
        self.b.set_cold_block(failed);
        let continued = self.b.create_block();
        self.b.ins().brif(bad, failed, &[], continued, &[]);
        self.b.switch_to_block(failed);
        self.b.ins().store(
            MemFlags::trusted(),
            address,
            self.state,
            offset_of!(CpuState, fault_addr) as i32,
        );
        let pc = self.constant(self.pc);
        self.save_state(pc);
        recovery.jump(&mut self.b);
        self.b.switch_to_block(continued);
    }
    fn direct_store(&mut self, addr: Value, value: Value) {
        let ty = self.b.func.dfg.value_type(value);
        self.memory_access(addr, ty, Some(value));
    }
    /// Synchronous memory helpers borrow at most 16 bytes until they return.
    /// Consume a read immediately into SSA; no buffer pointer escapes or is
    /// live across another helper. One slot therefore serves the whole function.
    fn memory_buffer(&mut self) -> ir::StackSlot {
        if let Some(slot) = self.memory_scratch {
            return slot;
        }
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            16,
            4,
        ));
        self.memory_scratch = Some(slot);
        slot
    }
    fn store_memory(&mut self, pointer: GuestPointer, value: Value) {
        if let Some(slot) = self.slot(self.b.func.dfg.value_type(value)) {
            self.b.def_var(slot, value);
        }
        if self.known(pointer.shared) == Some(0) {
            self.b
                .ins()
                .store(MemFlags::new(), value, pointer.address, pointer.offset);
            return;
        }
        let ordinary = self.b.create_block();
        let atomic = self.b.create_block();
        self.b.set_cold_block(atomic);
        let done = self.b.create_block();
        self.b
            .ins()
            .brif(pointer.shared, atomic, &[], ordinary, &[]);
        self.b.switch_to_block(ordinary);
        self.b
            .ins()
            .store(MemFlags::new(), value, pointer.address, pointer.offset);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(atomic);
        let overlay = self.b.create_block();
        self.b.set_cold_block(overlay);
        let pcm = self.b.create_block();
        self.b.set_cold_block(pcm);
        self.b.ins().brif(pointer.file, overlay, &[], pcm, &[]);
        self.b.switch_to_block(pcm);
        self.store_atomic(pointer.address, value);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(overlay);
        let slot = self.memory_buffer();
        self.b.ins().stack_store(value, slot, 0);
        let buffer = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let width = self.constant(self.b.func.dfg.value_type(value).bytes());
        self.b.ins().call(
            self.file_prepared_ref,
            &[self.bridge, pointer.guest, buffer, width],
        );
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }
    fn store_atomic(&mut self, pointer: Value, value: Value) {
        let ty = self.b.func.dfg.value_type(value);
        let (low, high) = if ty.is_vector() {
            let bits = self.b.ins().bitcast(
                types::I64X2,
                MemFlags::new().with_endianness(ir::Endianness::Little),
                value,
            );
            (
                self.b.ins().extractlane(bits, 0),
                self.b.ins().extractlane(bits, 1),
            )
        } else {
            let low = if ty == types::I64 {
                value
            } else {
                self.b.ins().uextend(types::I64, value)
            };
            (low, self.b.ins().iconst(types::I64, 0))
        };
        let width = self.constant(ty.bytes());
        self.b
            .ins()
            .call(self.atomic_store_ref, &[pointer, low, high, width]);
    }
    fn read(&mut self, i: &Instruction, n: u32) -> Value {
        match i.op_kind(n) {
            OpKind::Register => self.read_register(i.op_register(n)),
            OpKind::Memory => {
                let addr = self.address(i);
                let size = i.memory_size().size();
                let value = self.direct_load(addr, integer_type(size));
                if size == 4 {
                    value
                } else {
                    self.b.ins().uextend(types::I32, value)
                }
            }
            OpKind::Immediate32 => self.constant(i.immediate32()),
            OpKind::Immediate16 => self.constant(u32::from(i.immediate16())),
            OpKind::Immediate8 => self.constant(u32::from(i.immediate8())),
            OpKind::Immediate8to32 => self.constant(i.immediate8to32() as u32),
            OpKind::Immediate8to16 => self.constant(u32::from(i.immediate8to16() as u16)),
            OpKind::NearBranch32 => self.constant(i.near_branch32()),
            _ => unreachable!(),
        }
    }
    /// A general register, zero-extended from its width.
    fn read_register(&mut self, reg: R) -> Value {
        let full = self
            .b
            .use_var(self.regs[reg_index(reg.full_register32()).unwrap()]);
        if reg.size() == 4 {
            full
        } else {
            let value = if matches!(reg, R::AH | R::CH | R::DH | R::BH) {
                self.b.ins().ushr_imm(full, 8)
            } else {
                full
            };
            self.b
                .ins()
                .band_imm(value, if reg.size() == 1 { 255 } else { 65535 })
        }
    }
    fn destination(&mut self, i: &Instruction) -> Option<GuestPointer> {
        if i.op0_kind() == OpKind::Memory {
            let addr = self.address(i);
            Some(self.pointer(addr, i.memory_size().size()))
        } else {
            None
        }
    }
    fn write(&mut self, i: &Instruction, ptr: Option<GuestPointer>, value: Value) {
        if let Some(ptr) = ptr {
            let size = i.memory_size().size();
            let value = if size == 4 {
                value
            } else {
                self.b.ins().ireduce(integer_type(size), value)
            };
            self.store_memory(ptr, value);
        } else {
            self.write_register(i.op0_register(), value);
        }
    }
    /// Writes a general register's width, keeping the rest of its full register.
    fn write_register(&mut self, reg: R, value: Value) {
        let var = self.regs[reg_index(reg.full_register32()).unwrap()];
        let value = if reg.size() == 4 {
            value
        } else {
            let high = matches!(reg, R::AH | R::CH | R::DH | R::BH);
            let mask = if reg.size() == 1 { 255 } else { 65535 };
            let value = self.b.ins().band_imm(value, mask);
            let value = if high {
                self.b.ins().ishl_imm(value, 8)
            } else {
                value
            };
            let old = self.b.use_var(var);
            let old = self
                .b
                .ins()
                .band_imm(old, !(mask << if high { 8 } else { 0 }));
            self.b.ins().bor(old, value)
        };
        self.b.def_var(var, value);
    }
    fn flags(
        &mut self,
        a: Value,
        b: Value,
        result: Value,
        subtract: bool,
        logical: bool,
        keep_cf: bool,
    ) {
        let kind = self.constant(if logical {
            3
        } else if subtract {
            2
        } else {
            1
        });
        for (var, value) in self.lazy[..4].iter().zip([a, b, result, kind]) {
            self.b.def_var(*var, value);
        }
        if !keep_cf {
            let (left, right, kind) = if logical {
                let zero = self.constant(0);
                (zero, zero, zero)
            } else {
                let kind = self.constant(1);
                if subtract {
                    (a, b, kind)
                } else {
                    (result, a, kind)
                }
            };
            for (var, value) in self.lazy[4..].iter().zip([left, right, kind]) {
                self.b.def_var(*var, value);
            }
        }
    }
    /// Compute only the requested flag. Full EFLAGS is reconstructed on exits.
    fn flag(&mut self, bit: u32) -> Value {
        if bit == 0 {
            let a = self.b.use_var(self.lazy[4]);
            let b = self.b.use_var(self.lazy[5]);
            let kind = self.b.use_var(self.lazy[6]);
            if self.optimizing && self.known(kind) == Some(0) {
                return a;
            }
            let known_compare = self.optimizing && self.known(kind) == Some(1);
            let raw = self.b.ins().icmp_imm(IntCC::Equal, kind, 0);
            let c = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
            let c = self.b.ins().uextend(types::I32, c);
            if known_compare {
                return c;
            }
            return self.b.ins().select(raw, a, c);
        }
        let old = self.b.use_var(self.flags);
        let old = self.b.ins().ushr_imm(old, i64::from(bit));
        let old = self.b.ins().band_imm(old, 1);
        let a = self.b.use_var(self.lazy[0]);
        let b = self.b.use_var(self.lazy[1]);
        let result = self.b.use_var(self.lazy[2]);
        let kind = self.b.use_var(self.lazy[3]);
        let value = match bit {
            6 => {
                let z = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
                self.b.ins().uextend(types::I32, z)
            }
            7 => self.b.ins().ushr_imm(result, 31),
            2 => {
                // Narrow operations keep their operands shifted to bit 31.
                let shift = self.b.ins().ushr_imm(kind, 8);
                let low = self.b.ins().ushr(result, shift);
                let byte = self.b.ins().band_imm(low, 255);
                let count = self.b.ins().popcnt(byte);
                let count = self.b.ins().band_imm(count, 1);
                self.b.ins().bxor_imm(count, 1)
            }
            11 if self.carries_overflow(kind) => return b,
            4 | 11 => {
                let ab = self.b.ins().bxor(a, b);
                let base = self.b.ins().band_imm(kind, 3);
                let value = if bit == 4 {
                    let xor = self.b.ins().bxor(ab, result);
                    let shift = self.b.ins().ushr_imm(kind, 8);
                    let xor = self.b.ins().ushr(xor, shift);
                    self.b.ins().ushr_imm(xor, 4)
                } else {
                    let add = self.b.ins().icmp_imm(IntCC::Equal, base, 1);
                    let not = self.b.ins().bnot(ab);
                    let first = self.b.ins().select(add, not, ab);
                    let ar = self.b.ins().bxor(a, result);
                    let of = self.b.ins().band(first, ar);
                    self.b.ins().ushr_imm(of, 31)
                };
                let value = self.b.ins().band_imm(value, 1);
                let logical = self.b.ins().icmp_imm(IntCC::Equal, base, 3);
                let zero = self.constant(0);
                self.b.ins().select(logical, zero, value)
            }
            _ => unreachable!(),
        };
        // Kind bit 4 means a later CF/OF writer replaced OF with b (0 or 1)
        // and folded the old b into a, which leaves AF's a ^ b ^ result intact.
        // The low two bits still say whether the rest are raw (0) or derived.
        let (value, raw) = if bit == 11 {
            let carried = self.b.ins().band_imm(kind, 4);
            let value = self.b.ins().select(carried, b, value);
            (value, self.b.ins().icmp_imm(IntCC::Equal, kind, 0))
        } else {
            let base = self.b.ins().band_imm(kind, 3);
            (value, self.b.ins().icmp_imm(IntCC::Equal, base, 0))
        };
        self.b.ins().select(raw, old, value)
    }
    /// Whether this kind value is statically known to carry OF in b.
    fn carries_overflow(&self, kind: Value) -> bool {
        if let Some(kind) = self.known(kind) {
            return kind & 4 != 0;
        }
        self.b.func.dfg.value_def(kind).inst().is_some_and(|inst| {
            matches!(
                self.b.func.dfg.insts[inst],
                ir::InstructionData::BinaryImm64 {
                    opcode: ir::Opcode::BorImm,
                    imm,
                    ..
                } if imm.bits() & 4 != 0
            )
        })
    }
    fn materialize_flags(&mut self) -> Value {
        let source = self.source;
        self.mark_source(diagnostics::overhead_source(diagnostics::Overhead::Flags));
        if self.optimizing {
            // Speed already folds individual constant recipes. Keeping that
            // shape avoids extending shared-expression live ranges at exits.
            let old = self.b.use_var(self.flags);
            let mut flags = self.b.ins().band_imm(old, !0x8d5_i64);
            for bit in [0, 2, 4, 6, 7, 11] {
                let value = self.flag(bit);
                let value = self.b.ins().ishl_imm(value, i64::from(bit));
                flags = self.b.ins().bor(flags, value);
            }
            self.mark_source(source);
            return flags;
        }
        // Materialize the complete word once. Independent flag consumers still
        // use flag(), but a checkpoint shares the recipe decoding and masks.
        let old = self.b.use_var(self.flags);
        let a = self.b.use_var(self.lazy[0]);
        let b = self.b.use_var(self.lazy[1]);
        let result = self.b.use_var(self.lazy[2]);
        let kind = self.b.use_var(self.lazy[3]);
        let base = self.b.ins().band_imm(kind, 3);
        let shift = self.b.ins().ushr_imm(kind, 8);
        let raw = self.b.ins().icmp_imm(IntCC::Equal, base, 0);
        let logical = self.b.ins().icmp_imm(IntCC::Equal, base, 3);
        let zero = self.constant(0);
        let ab = self.b.ins().bxor(a, b);

        let low = self.b.ins().ushr(result, shift);
        let byte = self.b.ins().band_imm(low, 255);
        let parity = self.b.ins().popcnt(byte);
        let parity = self.b.ins().band_imm(parity, 1);
        let parity = self.b.ins().bxor_imm(parity, 1);
        let parity = self.b.ins().ishl_imm(parity, 2);
        let zf = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let zf = self.b.ins().uextend(types::I32, zf);
        let zf = self.b.ins().ishl_imm(zf, 6);
        let sf = self.b.ins().ushr_imm(result, 24);
        let sf = self.b.ins().band_imm(sf, 0x80);
        let af = self.b.ins().bxor(ab, result);
        let af = self.b.ins().ushr(af, shift);
        let af = self.b.ins().band_imm(af, 0x10);
        let af = self.b.ins().select(logical, zero, af);
        let derived = self.b.ins().bor(parity, zf);
        let derived = self.b.ins().bor(derived, sf);
        let derived = self.b.ins().bor(derived, af);
        let canonical = self.b.ins().band_imm(old, 0xd4);
        let partial = self.b.ins().select(raw, canonical, derived);

        // OF may be replaced by a later rotate/multiply while PF/AF/ZF/SF
        // still refer to the previous recipe (including canonical entry flags).
        let add = self.b.ins().icmp_imm(IntCC::Equal, base, 1);
        let not_ab = self.b.ins().bnot(ab);
        let first = self.b.ins().select(add, not_ab, ab);
        let ar = self.b.ins().bxor(a, result);
        let overflow = self.b.ins().band(first, ar);
        let overflow = self.b.ins().ushr_imm(overflow, 20);
        let overflow = self.b.ins().band_imm(overflow, 0x800);
        let overflow = self.b.ins().select(logical, zero, overflow);
        let carried = self.b.ins().band_imm(kind, 4);
        let carried_value = self.b.ins().ishl_imm(b, 11);
        let overflow = self.b.ins().select(carried, carried_value, overflow);
        let raw_of = self.b.ins().icmp_imm(IntCC::Equal, kind, 0);
        let old_of = self.b.ins().band_imm(old, 0x800);
        let overflow = self.b.ins().select(raw_of, old_of, overflow);
        let carry = self.flag(0);
        let preserved = self.b.ins().band_imm(old, !0x8d5_i64);
        let flags = self.b.ins().bor(preserved, partial);
        let flags = self.b.ins().bor(flags, overflow);
        let flags = self.b.ins().bor(flags, carry);
        self.mark_source(source);
        flags
    }
    /// Whether `result` is the instruction `opcode` applied to `a` and `b`.
    fn computes(&self, result: Value, opcode: ir::Opcode, a: Value, b: Value) -> bool {
        let dfg = &self.b.func.dfg;
        let resolve = |v| dfg.resolve_aliases(v);
        dfg.value_def(resolve(result)).inst().is_some_and(|inst| {
            matches!(
                dfg.insts[inst],
                ir::InstructionData::Binary { opcode: op, args: [x, y] }
                    if op == opcode && resolve(x) == resolve(a) && resolve(y) == resolve(b)
            )
        })
    }
    /// A signed or unsigned order condition as one comparison when the lazy
    /// flags statically come from a 32-bit SUB/CMP/ADD or any logical result.
    fn fused_condition(&mut self, m: M) -> Option<Value> {
        let kind = self.b.use_var(self.lazy[3]);
        let kind = self.known(kind)?;
        let a = self.b.use_var(self.lazy[0]);
        let b = self.b.use_var(self.lazy[1]);
        let result = self.b.use_var(self.lazy[2]);
        let signed = |m| match m {
            M::Jl => Some(IntCC::SignedLessThan),
            M::Jge => Some(IntCC::SignedGreaterThanOrEqual),
            M::Jle => Some(IntCC::SignedLessThanOrEqual),
            M::Jg => Some(IntCC::SignedGreaterThan),
            _ => None,
        };
        match kind {
            // SUB, CMP and DEC: OF and SF order a and b exactly.
            2 if self.computes(result, ir::Opcode::Isub, a, b) => {
                // DEC keeps the old CF, so unsigned orders also need CF's
                // lazy operands to be this subtraction's.
                let carry = [self.lazy[4], self.lazy[5], self.lazy[6]].map(|v| self.b.use_var(v));
                let dfg = &self.b.func.dfg;
                let borrows = dfg.resolve_aliases(carry[0]) == dfg.resolve_aliases(a)
                    && dfg.resolve_aliases(carry[1]) == dfg.resolve_aliases(b)
                    && self.known(carry[2]) == Some(1);
                let cc = signed(m).or(match m {
                    M::Jbe if borrows => Some(IntCC::UnsignedLessThanOrEqual),
                    M::Ja if borrows => Some(IntCC::UnsignedGreaterThan),
                    _ => None,
                })?;
                Some(self.b.ins().icmp(cc, a, b))
            }
            // ADD and INC: SF ^ OF is the sign of the exact sum.
            1 if self.computes(result, ir::Opcode::Iadd, a, b) => {
                // For signed i32 operands, a + b < 0 iff a <= -b - 1.
                // -b - 1 is exactly !b, including b == i32::MIN. Keep this
                // comparison at i32 width instead of sign-extending both
                // operands to compute an i64 sum. Testing the wrapped result
                // itself would lose x86 overflow semantics.
                if matches!(m, M::Jl | M::Jge) {
                    let bound = self.b.ins().bnot(b);
                    let cc = if m == M::Jl {
                        IntCC::SignedLessThanOrEqual
                    } else {
                        IntCC::SignedGreaterThan
                    };
                    return Some(self.b.ins().icmp(cc, a, bound));
                }
                let cc = signed(m)?;
                let a = self.b.ins().sextend(types::I64, a);
                let b = self.b.ins().sextend(types::I64, b);
                let sum = self.b.ins().iadd(a, b);
                Some(self.b.ins().icmp_imm(cc, sum, 0))
            }
            // Logical operations clear OF; narrow ones keep the sign at bit 31.
            _ if kind & 7 == 3 => {
                let cc = signed(m)?;
                Some(self.b.ins().icmp_imm(cc, result, 0))
            }
            _ => None,
        }
    }
    fn condition(&mut self, m: M) -> Value {
        if let Some(fused) = self.fused_condition(m) {
            return fused;
        }
        // Emit just what the condition needs; in particular JZ/JNZ needs no parity,
        // carry or overflow work in an interpreter's loop backedge.
        let (v, is_zero) = match m {
            M::Je | M::Jne => (self.flag(6), m == M::Jne),
            M::Jb | M::Jae => (self.flag(0), m == M::Jae),
            M::Jbe | M::Ja => {
                let c = self.flag(0);
                let z = self.flag(6);
                (self.b.ins().bor(c, z), m == M::Ja)
            }
            M::Jl | M::Jge | M::Jle | M::Jg => {
                let s = self.flag(7);
                let o = self.flag(11);
                let mut so = self.b.ins().bxor(s, o);
                if matches!(m, M::Jle | M::Jg) {
                    let z = self.flag(6);
                    so = self.b.ins().bor(so, z);
                }
                (so, matches!(m, M::Jge | M::Jg))
            }
            M::Jo | M::Jno => (self.flag(11), m == M::Jno),
            M::Js | M::Jns => (self.flag(7), m == M::Jns),
            M::Jp | M::Jnp => (self.flag(2), m == M::Jnp),
            _ => unreachable!(),
        };
        self.b.ins().icmp_imm(
            if is_zero {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            },
            v,
            0,
        )
    }
    fn instruction(&mut self, i: &Instruction) -> bool {
        if let Some(&(operation, opcode)) = self.x87.get(&i.ip32()) {
            self.native_x87_instruction(i, operation, opcode);
            return false;
        }
        if self.parallel && exclusive(i) {
            let always = self.b.ins().iconst(types::I8, 1);
            self.guard(always, Exit::Fallback, None);
        }
        if let Some(operation) = crate::strings::operation(i) {
            let pc = self.constant(i.ip32());
            self.save_state(pc);
            let operation = self.constant(operation);
            let call = self
                .b
                .ins()
                .call(self.repeat_ref, &[self.bridge, self.state, operation]);
            let result = self.b.inst_results(call)[0];
            self.reload_state();
            let done = self.b.create_block();
            let retry = self.b.create_block();
            let fallback = self.b.create_block();
            let budget = self.b.create_block();
            let read_fault = self.b.create_block();
            let write_fault = self.b.create_block();
            self.b.set_cold_block(read_fault);
            self.b.set_cold_block(write_fault);
            let mut switch = Switch::new();
            switch.set_entry(0, done);
            switch.set_entry(u128::from(crate::strings::MORE), retry);
            switch.set_entry(u128::from(crate::strings::BUDGET), budget);
            switch.set_entry(u128::from(crate::strings::READ_FAULT), read_fault);
            switch.set_entry(u128::from(crate::strings::WRITE_FAULT), write_fault);
            switch.emit(&mut self.b, result, fallback);
            self.b.switch_to_block(done);
            self.b.seal_block(done);
            self.direct(i.next_ip32());
            self.b.switch_to_block(retry);
            self.direct(i.ip32());
            self.b.switch_to_block(fallback);
            if string::supported(i) {
                // Bulk guards can decline a range, overlap or host-specific
                // direction. Keep the checked element path native.
                self.string_instruction(i);
            } else {
                self.exit_const(Exit::Fallback, i.ip32());
            }
            self.b.switch_to_block(budget);
            self.exit_const(Exit::Budget, i.ip32());
            self.b.switch_to_block(read_fault);
            self.exit_const(Exit::ReadFault, i.ip32());
            self.b.switch_to_block(write_fault);
            self.exit_const(Exit::WriteFault, i.ip32());
            return true;
        }
        if string::supported(i) {
            return self.string_instruction(i);
        }
        if narrow_alu(i.mnemonic()) {
            let size = if i.op0_kind() == OpKind::Memory {
                i.memory_size().size()
            } else {
                i.op0_register().size()
            };
            if size < 4 {
                self.narrow_alu(i, size);
                return false;
            }
        }
        if mmx::supported(i) {
            self.mmx_instruction(i);
            return false;
        }
        if scalar::supported(i) {
            self.scalar_instruction(i);
            return false;
        }
        if simd::supported(i) {
            self.simd_instruction(i);
            return false;
        }
        if sse::supported(i) {
            self.sse_instruction(i);
            return false;
        }
        match i.mnemonic() {
            M::Int => {
                self.trap_call(i.next_ip32());
                return true;
            }
            M::Nop => {}
            M::Rdtsc => {
                let call = self.b.ins().call(self.timestamp_ref, &[self.bridge]);
                let ticks = self.b.inst_results(call)[0];
                let low = self.b.ins().ireduce(types::I32, ticks);
                let shifted = self.b.ins().ushr_imm(ticks, 32);
                let high = self.b.ins().ireduce(types::I32, shifted);
                self.b.def_var(self.regs[0], low);
                self.b.def_var(self.regs[2], high);
            }
            M::Mov | M::Lea | M::Movzx | M::Movsx => {
                let mut v = if i.mnemonic() == M::Lea {
                    self.address(i)
                } else {
                    self.read(i, 1)
                };
                if i.mnemonic() == M::Movsx {
                    let size = if i.op1_kind() == OpKind::Memory {
                        i.memory_size().size()
                    } else {
                        i.op1_register().size()
                    };
                    let narrow = self.b.ins().ireduce(integer_type(size), v);
                    v = self.b.ins().sextend(types::I32, narrow);
                }
                if i.op0_kind() == OpKind::Memory {
                    let addr = self.address(i);
                    let size = i.memory_size().size();
                    let narrow = if size == 4 {
                        v
                    } else {
                        self.b.ins().ireduce(integer_type(size), v)
                    };
                    self.direct_store(addr, narrow);
                } else {
                    self.write(i, None, v);
                }
            }
            M::Add
            | M::Sub
            | M::And
            | M::Or
            | M::Xor
            | M::Cmp
            | M::Test
            | M::Inc
            | M::Dec
            | M::Not
            | M::Neg => {
                let a = self.read(i, 0);
                let b = if i.op_count() == 2 {
                    self.read(i, 1)
                } else {
                    self.constant(1)
                };
                // Validate the destination before changing flags or registers.
                let ptr = if matches!(i.mnemonic(), M::Cmp | M::Test) {
                    None
                } else {
                    self.destination(i)
                };
                let result = match i.mnemonic() {
                    M::Add | M::Inc => self.b.ins().iadd(a, b),
                    M::Sub | M::Cmp | M::Dec => self.b.ins().isub(a, b),
                    M::And | M::Test => self.b.ins().band(a, b),
                    M::Or => self.b.ins().bor(a, b),
                    M::Xor => self.b.ins().bxor(a, b),
                    M::Not => self.b.ins().bnot(a),
                    M::Neg => self.b.ins().ineg(a),
                    _ => unreachable!(),
                };
                if i.mnemonic() == M::Neg {
                    let zero = self.constant(0);
                    self.flags(zero, a, result, true, false, false);
                } else if i.mnemonic() != M::Not {
                    self.flags(
                        a,
                        b,
                        result,
                        matches!(i.mnemonic(), M::Sub | M::Cmp | M::Dec),
                        matches!(i.mnemonic(), M::And | M::Or | M::Xor | M::Test),
                        matches!(i.mnemonic(), M::Inc | M::Dec),
                    );
                }
                if !matches!(i.mnemonic(), M::Cmp | M::Test) {
                    self.write(i, ptr, result);
                }
            }
            M::Push | M::Call => {
                let v = self.read(i, 0);
                let width = if word_stack(i) { 2 } else { 4 };
                let sp = self.b.use_var(self.regs[4]);
                let sp = self.b.ins().iadd_imm(sp, -(width as i64));
                let ptr = self.pointer(sp, width);
                let pushed = if i.mnemonic() == M::Call {
                    self.constant(i.next_ip32())
                } else if width == 2 {
                    self.b.ins().ireduce(types::I16, v)
                } else {
                    v
                };
                self.store_memory(ptr, pushed);
                self.b.def_var(self.regs[4], sp);
                if i.mnemonic() == M::Call {
                    self.shadow_push(sp, i.next_ip32());
                    if i.op0_kind() == OpKind::NearBranch32 {
                        self.direct(i.near_branch32());
                    } else {
                        self.record_indirect(v, crate::IndirectBranchKind::Call);
                        if let Some(&cell) = self.call_sites.get(&self.pc) {
                            let cell = self.b.ins().iconst(self.ptr_ty, cell as i64);
                            self.b.ins().store(MemFlags::trusted(), v, cell, 0);
                        }
                        self.indirect(v);
                    }
                    return true;
                }
            }
            M::Pop | M::Ret => {
                let sp = self.b.use_var(self.regs[4]);
                let original_sp = sp;
                let width = if word_stack(i) { 2 } else { 4 };
                let value = if width == 2 {
                    let word = self.direct_load(sp, types::I16);
                    self.b.ins().uextend(types::I32, word)
                } else {
                    self.direct_load(sp, types::I32)
                };
                let adjustment = if i.mnemonic() == M::Ret && i.op_count() != 0 {
                    i64::from(i.immediate16())
                } else {
                    0
                };
                let sp = self.b.ins().iadd_imm(sp, width + adjustment);
                self.b.def_var(self.regs[4], sp);
                if i.mnemonic() == M::Ret {
                    self.record_indirect(value, crate::IndirectBranchKind::Return);
                    if let Some(target) = self.return_hint {
                        // A small interpreter helper often has one caller in the region.
                        // Check the actual guest return address before taking this edge.
                        let hit = self
                            .b
                            .ins()
                            .icmp_imm(IntCC::Equal, value, i64::from(target));
                        let yes = self.b.create_block();
                        let miss = self.b.create_block();
                        self.b.ins().brif(hit, yes, &[], miss, &[]);
                        self.b.switch_to_block(yes);
                        if self.local_return_hint.is_none() {
                            self.shadow_prediction(original_sp, value);
                        }
                        self.direct(target);
                        self.b.switch_to_block(miss);
                        let prediction = self.shadow_prediction(original_sp, value);
                        self.indirect_predicted(value, prediction);
                    } else {
                        let prediction = self.shadow_prediction(original_sp, value);
                        self.indirect_predicted(value, prediction);
                    }
                    return true;
                }
                self.write(i, None, value);
            }
            M::Jmp => {
                if i.op0_kind() == OpKind::NearBranch32 {
                    self.direct(i.near_branch32());
                } else {
                    let target = self.read(i, 0);
                    self.record_indirect(target, crate::IndirectBranchKind::Jump);
                    self.indirect(target);
                }
                return true;
            }
            M::Loop | M::Jecxz | M::Jcxz => {
                let count = self.b.use_var(self.regs[1]);
                let condition = if i.mnemonic() == M::Loop {
                    // LOOP decrements ECX without touching flags.
                    let count = self.b.ins().iadd_imm(count, -1);
                    self.b.def_var(self.regs[1], count);
                    self.b.ins().icmp_imm(IntCC::NotEqual, count, 0)
                } else {
                    let count = if i.mnemonic() == M::Jcxz {
                        self.b.ins().band_imm(count, 0xffff)
                    } else {
                        count
                    };
                    self.b.ins().icmp_imm(IntCC::Equal, count, 0)
                };
                let yes = self.b.create_block();
                let no = self.b.create_block();
                self.b.ins().brif(condition, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                self.direct(i.near_branch_target() as u32);
                self.b.switch_to_block(no);
                self.direct(i.next_ip32());
                return true;
            }
            branch => {
                let condition = self.condition(branch);
                let yes = self.b.create_block();
                let no = self.b.create_block();
                self.b.ins().brif(condition, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                self.direct(i.near_branch32());
                self.b.switch_to_block(no);
                self.direct(i.next_ip32());
                return true;
            }
        }
        false
    }
}

fn integer_type(size: usize) -> ir::Type {
    match size {
        1 => types::I8,
        2 => types::I16,
        4 => types::I32,
        8 => types::I64,
        _ => unreachable!(),
    }
}
