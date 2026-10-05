use std::collections::{BTreeMap, BTreeSet};
use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use iced_x86::{
    Decoder, DecoderError, DecoderOptions, FlowControl, Instruction, Mnemonic as M, OpKind,
    Register as R,
};

use crate::linking::{NativeContext, Slot, Table};
use crate::{CompileStrategy, CpuState, EXEC, Exit, Memory, READ, Region, WRITE};
mod diagnostics;
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
mod fault;
mod mmx;
mod scalar;
mod shadow;
mod simd;
mod write_summary;
pub(crate) mod x87;

type Entry = unsafe extern "C" fn(*mut NativeContext, *mut u8) -> u32;

/// Borrowed controls for one bounded native entry and its linked regions.
pub(crate) struct NativeRun<'a> {
    pub links: Option<&'a Table>,
    pub stop: u32,
    pub interrupt: &'a std::sync::atomic::AtomicBool,
    pub shadow: Option<&'a mut crate::linking::shadow::Stack>,
}

pub(crate) struct Compiled {
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
    pub code_size: usize,
    /// x87 instructions called from generated code by address.
    _x87: Box<[crate::floating::NativeX87]>,
    /// Owner-side recency sampled at Rust entries, never read by generated code.
    // Controller-only metadata; native descriptors never reference this cell.
    pub cache_stamp: std::cell::Cell<u64>,
    #[cfg(all(test, target_arch = "x86_64"))]
    disasm: String,
}

impl Drop for Compiled {
    fn drop(&mut self) {
        // SAFETY: the entry pointer is private, run borrows self, and no generated
        // code can still be executing while this exclusive drop runs.
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        self.unwind.clear();
        unsafe { self.module.take().unwrap().free_memory() };
    }
}

impl Compiled {
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
        let mut context = NativeContext {
            state,
            perms: memory.perms.as_ptr(),
            bridge: (bridge as *mut crate::trap::Bridge<'_>).cast(),
            interrupt: controls.interrupt,
            slots,
            mask,
            remaining: crate::linking::CHAIN_LIMIT,
            stop: controls.stop,
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            fault_reads: 0,
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
        let result = unsafe { (self.entry)(&mut context, memory.ptr.as_ptr()) };
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
        let result = unsafe {
            crate::host_fault::with_native_session(state, &[region], window, guest_base, |state| {
                context.state = state;
                context.fault_reads = 1;
                (self.entry)(context, pointer)
            })
        };
        match result {
            Ok((result, _)) => Some(result),
            Err(_) => {
                context.fault_reads = 0;
                None
            }
        }
    }
}

/// An immutable, Send compilation input. No guest allocation is borrowed or
/// retained: only bounded instruction bytes and their page stamps are copied.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    parallel: bool,
    pub region: Box<Region>,
    base: u32,
    len: usize,
    bytes: Vec<u8>,
    pages: Vec<PageStamp>,
    // Published reachable regions compare only their decoded instruction bytes.
    // A pending snapshot still stamps the full bounded window conservatively.
    covered: Option<Vec<(usize, usize)>>,
    pub(crate) shadow_returns: bool,
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

    /// Slice an immutable input without consulting active guest memory. Keep
    /// tail/negative-page stamps and the optional hardware-read proof capability.
    pub(crate) fn fragment(&self, region: Region) -> Result<Self, String> {
        let offset = region
            .start
            .checked_sub(self.region.start)
            .ok_or("fragment starts before its snapshot")? as usize;
        if region.len == 0 || offset + region.len > self.bytes.len() {
            return Err("fragment extends beyond its snapshot".into());
        }
        let end =
            (offset + region.len + if region.complete_tail { 14 } else { 0 }).min(self.bytes.len());
        let first = (region.start - self.base) as usize / 4096;
        let last = ((self.region.start - self.base) as usize + end) / 4096;
        Ok(Self {
            parallel: self.parallel,
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
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            native_fault_reads: self.native_fault_reads,
        })
    }

    pub fn capture(region: &Region, memory: &Memory) -> Result<Self, String> {
        if region.len == 0 || region.len > 16 * 1024 || !memory.executable(region.start, region.len)
        {
            return Err("region must contain 1..16384 executable bytes".into());
        }
        let mut bytes = memory.read_data(region.start, region.len)?.to_vec();
        let first = (region.start - memory.base) as usize / 4096;
        let last = ((region.start - memory.base) as usize + region.len - 1) / 4096;
        let mut pages: BTreeSet<usize> = (first..=last).collect();
        if region.complete_tail {
            for offset in region.len..region.len + 14 {
                let Some(address) = u64::from(region.start)
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
            if let Some(address) = u64::from(region.start)
                .checked_add(bytes.len() as u64)
                .and_then(|address| u32::try_from(address).ok())
                && let Ok(range) = memory.range(address, 1)
            {
                pages.insert(range.start / 4096);
            }
        }
        Ok(Self {
            parallel: memory.is_parallel(),
            region: Box::new(region.clone()),
            base: memory.base,
            len: memory.len,
            bytes,
            covered: None,
            shadow_returns: false,
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
                        .read_data(self.region.start, self.bytes.len())
                        .is_ok_and(|bytes| bytes == self.bytes)
                },
                |ranges| {
                    ranges.iter().all(|&(offset, len)| {
                        memory
                            .read_data(self.region.start + offset as u32, len)
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
                            (instruction.ip32() - self.region.start) as usize,
                            instruction.len(),
                        )
                    })
                    .collect(),
            );
            input.pages.retain(|stamp| pages.contains(&stamp.page));
        }
        input
    }

    fn page_index(&self, address: u32) -> Option<usize> {
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

fn record_tail_dependencies(
    region: &Region,
    snapshot: &Snapshot,
    bytes: &[u8],
    offset: usize,
    decoder: &Decoder<'_>,
    instruction: &Instruction,
    pages: &mut BTreeSet<usize>,
) {
    let start = (region.start - snapshot.base) as usize;
    let read_start = (instruction.ip32() - snapshot.base) as usize;
    let read_end = start + offset + decoder.position();
    pages.extend(read_start / 4096..=(read_end - 1) / 4096);
    if region.complete_tail && decoder.last_error() == DecoderError::NoMoreBytes {
        // The next page's mapping/EXEC permission can make a previously
        // truncated instruction compilable, even if none of its bytes
        // could be read. Keep that negative dependency in the cache.
        if let Some(address) = u64::from(region.start)
            .checked_add(bytes.len() as u64)
            .and_then(|address| u32::try_from(address).ok())
            && let Some(page) = snapshot.page_index(address)
        {
            pages.insert(page);
        }
    }
}

pub(crate) fn compile(
    region: &Region,
    memory: &Memory,
    shadow_returns: bool,
) -> Result<Compiled, String> {
    let mut input = Snapshot::capture(region, memory)?;
    input.shadow_returns = shadow_returns;
    compile_snapshot(&input)
}

pub(crate) fn compile_snapshot(snapshot: &Snapshot) -> Result<Compiled, String> {
    let region = &snapshot.region;
    let bytes = &snapshot.bytes;
    let first_page = (region.start - snapshot.base) as usize / 4096;
    let mut pages: BTreeSet<usize> = if region.reachable {
        BTreeSet::new()
    } else {
        (first_page..=((region.start - snapshot.base) as usize + region.len - 1) / 4096).collect()
    };
    let mut instructions = BTreeMap::new();
    if !region.reachable {
        let mut decoder =
            Decoder::with_ip(32, bytes, u64::from(region.start), DecoderOptions::NONE);
        while decoder.can_decode() && decoder.ip() < u64::from(region.start) + region.len as u64 {
            let instruction = decoder.decode();
            record_tail_dependencies(
                region,
                snapshot,
                bytes,
                0,
                &decoder,
                &instruction,
                &mut pages,
            );
            instructions.insert(instruction.ip32(), instruction);
        }
    }
    // Entry points can follow padding/data or overlap an earlier linear decode.
    // Decode direct targets independently, as the CPU would at the actual target PC.
    let mut pending: Vec<u32> = region.entries.clone();
    if region.reachable && pending.is_empty() {
        pending.push(region.start);
    }
    if !region.reachable {
        pending.extend(
            instructions
                .values()
                .filter(|i| i.op0_kind() == OpKind::NearBranch32)
                .map(Instruction::near_branch32),
        );
    }
    while let Some(target) = pending.pop() {
        if instructions.contains_key(&target) || region.frontier.contains(&target) {
            continue;
        }
        let Some(offset) = target
            .checked_sub(region.start)
            .map(|n| n as usize)
            .filter(|&n| n < region.len)
        else {
            continue;
        };
        let mut decoder = Decoder::with_ip(
            32,
            &bytes[offset..],
            u64::from(target),
            DecoderOptions::NONE,
        );
        while decoder.can_decode()
            && decoder.ip() < u64::from(region.start) + region.len as u64
            && !instructions.contains_key(&(decoder.ip() as u32))
            && !region.frontier.contains(&(decoder.ip() as u32))
        {
            let i = decoder.decode();
            record_tail_dependencies(region, snapshot, bytes, offset, &decoder, &i, &mut pages);
            if i.op0_kind() == OpKind::NearBranch32 {
                pending.push(i.near_branch32());
            }
            instructions.insert(i.ip32(), i);
            if region.reachable
                && (i.is_invalid()
                    || matches!(
                        i.flow_control(),
                        FlowControl::UnconditionalBranch
                            | FlowControl::IndirectBranch
                            | FlowControl::Return
                            | FlowControl::Exception
                    ))
            {
                break;
            }
        }
    }
    // x87 stays in the region: guarded scalar lowering handles the exact subset,
    // and the remaining cases call the interpreter's transaction in place.
    let x87: Box<[crate::floating::NativeX87]> = instructions
        .values()
        .filter(|i| !supported(i) && crate::floating::native_x87(i))
        .map(|i| {
            // The snapshot bytes begin at the region start.
            let start = (i.ip32() - region.start) as usize;
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
    let mut entries = BTreeSet::from([region.start]);
    for instruction in instructions.values() {
        if crate::strings::operation(instruction).is_some()
            || (!supported(instruction) && crate::floating::native_x87(instruction))
        {
            entries.insert(instruction.ip32());
            entries.insert(instruction.next_ip32());
        }
        if instruction.flow_control() != FlowControl::Next {
            entries.insert(instruction.next_ip32());
        }
        if matches!(instruction.op0_kind(), OpKind::NearBranch32) {
            entries.insert(instruction.near_branch32());
        }
    }
    for &entry in &region.entries {
        if !instructions.contains_key(&entry) {
            return Err(format!("entry {entry:#x} is not an instruction boundary"));
        }
        entries.insert(entry);
    }
    entries.retain(|pc| instructions.contains_key(pc));
    let returns: BTreeSet<_> = instructions
        .values()
        .filter(|i| {
            matches!(
                i.flow_control(),
                FlowControl::Call | FlowControl::IndirectCall
            )
        })
        .map(Instruction::next_ip32)
        .filter(|pc| entries.contains(pc))
        .collect();
    let return_hint = if returns.len() == 1 {
        returns.first().copied()
    } else {
        None
    };
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let fault_pc = snapshot
        .native_fault_reads
        .then(|| fault::candidate(region.start, &instructions))
        .flatten();

    let opt_level = match if snapshot.parallel {
        CompileStrategy::Fast
    } else {
        region.compile_strategy
    } {
        CompileStrategy::Speed => "speed",
        CompileStrategy::Fast => "none",
    };
    let mut builder = JITBuilder::with_flags(
        &[
            ("opt_level", opt_level),
            ("preserve_frame_pointers", "true"),
        ],
        default_libcall_names(),
    )
    .map_err(|e| e.to_string())?;
    builder.symbol("adv_trap_call", crate::trap::trap_call as *const u8);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    builder.memory_provider(Box::new(crate::jit_memory::WindowsMemory::default()));
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
    builder.symbol(
        "adv_atomic_load_vector",
        crate::atomic_memory::load_vector as *const u8,
    );
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
    ctx.func.signature = module.make_signature();
    ctx.func.signature.call_conv = cranelift_codegen::isa::CallConv::Tail;
    ctx.func.signature.params.extend([AbiParam::new(ptr_ty); 2]);
    ctx.func.signature.returns.push(AbiParam::new(types::I32));
    let tail_signature = ctx.func.signature.clone();
    let id = module
        .declare_function("guest_region", Linkage::Local, &ctx.func.signature)
        .map_err(|e| e.to_string())?;
    let shadow_ref = snapshot
        .shadow_returns
        .then(|| module.declare_func_in_func(id, &mut ctx.func));
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut trap_signature = module.make_signature();
    trap_signature.params.extend([AbiParam::new(ptr_ty); 2]);
    trap_signature.params.push(AbiParam::new(types::I32));
    trap_signature.returns.push(AbiParam::new(types::I32));
    let host_id = module
        .declare_function("adv_trap_call", Linkage::Import, &trap_signature)
        .map_err(|e| e.to_string())?;
    let trap_ref = module.declare_func_in_func(host_id, &mut ctx.func);
    let mut repeat_signature = module.make_signature();
    repeat_signature.params.extend([AbiParam::new(ptr_ty); 2]);
    repeat_signature.returns.push(AbiParam::new(types::I32));
    repeat_signature.params.push(AbiParam::new(types::I32));
    let repeat_id = module
        .declare_function("adv_repeat", Linkage::Import, &repeat_signature)
        .map_err(|e| e.to_string())?;
    let repeat_ref = module.declare_func_in_func(repeat_id, &mut ctx.func);
    let mut x87_signature = module.make_signature();
    x87_signature.params.extend([AbiParam::new(ptr_ty); 3]);
    x87_signature.returns.push(AbiParam::new(types::I32));
    let x87_id = module
        .declare_function("adv_x87", Linkage::Import, &x87_signature)
        .map_err(|e| e.to_string())?;
    let x87_ref = module.declare_func_in_func(x87_id, &mut ctx.func);
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
    let atomic_store_ref = module.declare_func_in_func(atomic_store_id, &mut ctx.func);
    let mut atomic_load_signature = module.make_signature();
    atomic_load_signature
        .params
        .extend([AbiParam::new(ptr_ty); 2]);
    let atomic_load_id = module
        .declare_function(
            "adv_atomic_load_vector",
            Linkage::Import,
            &atomic_load_signature,
        )
        .map_err(|e| e.to_string())?;
    let atomic_load_ref = module.declare_func_in_func(atomic_load_id, &mut ctx.func);
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
    let file_read_ref = module.declare_func_in_func(file_read_id, &mut ctx.func);
    let file_write_id = module
        .declare_function("adv_file_write", Linkage::Import, &file_signature)
        .map_err(|e| e.to_string())?;
    let file_write_ref = module.declare_func_in_func(file_write_id, &mut ctx.func);
    file_signature.returns.clear();
    let file_prepared_id = module
        .declare_function("adv_file_write_prepared", Linkage::Import, &file_signature)
        .map_err(|e| e.to_string())?;
    let file_prepared_ref = module.declare_func_in_func(file_prepared_id, &mut ctx.func);
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
    let file_prepare_ref = module.declare_func_in_func(prepare_id, &mut ctx.func);
    let mut timestamp_signature = module.make_signature();
    timestamp_signature.params.push(AbiParam::new(ptr_ty));
    timestamp_signature.returns.push(AbiParam::new(types::I64));
    let timestamp_id = module
        .declare_function("adv_read_timestamp", Linkage::Import, &timestamp_signature)
        .map_err(|e| e.to_string())?;
    let timestamp_ref = module.declare_func_in_func(timestamp_id, &mut ctx.func);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let mut fault_draft = None;
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    if fault_pc.is_some() {
        ctx.func.collect_debug_info();
    }
    // Fault recipes read their own source locations; keep them untagged.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let source_map = diagnostics::instruction_map_enabled() && fault_pc.is_none();
    #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
    let source_map = diagnostics::instruction_map_enabled();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
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
        let tail_ref = b.import_signature(tail_signature);
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
        for instruction in instructions.values().filter(|i| supported(i)) {
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
        let has_mmx = instructions
            .values()
            .any(|i| supported(i) && mmx::supported(i));
        for instruction in instructions
            .values()
            .filter(|i| supported(i) && mmx::supported(i))
        {
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
        for var in lazy {
            b.def_var(var, zero);
        }
        let fuel = b.declare_var(types::I64);
        for (i, reg) in regs.iter().enumerate() {
            let v = b
                .ins()
                .load(types::I32, MemFlags::trusted(), state, (i * 4) as i32);
            b.def_var(*reg, v);
        }
        let f = b.ins().load(
            types::I32,
            MemFlags::trusted(),
            state,
            offset_of!(CpuState, eflags) as i32,
        );
        b.def_var(flags, f);
        let carry = b.ins().band_imm(f, 1);
        b.def_var(lazy[4], carry);
        let f = b.ins().load(
            types::I64,
            MemFlags::trusted(),
            state,
            offset_of!(CpuState, fuel) as i32,
        );
        b.def_var(fuel, f);
        let pc = b.ins().load(
            types::I32,
            MemFlags::trusted(),
            state,
            offset_of!(CpuState, eip) as i32,
        );
        let blocks: BTreeMap<_, _> = entries.iter().map(|&pc| (pc, b.create_block())).collect();
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
        let write_summary = write_summary::Summary::for_region(&instructions);
        let hot_exits = write_summary.is_some() && write_summary::crosses_regions(&instructions);
        if !hot_exits {
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
            b,
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
            perms,
            interrupt,
            context: params[0],
            tail_ref,
            ptr_ty,
            base: snapshot.base,
            len: snapshot.len,
            blocks,
            dispatch,
            shared_exit,
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
            x87: x87
                .iter()
                .map(|op| (op.instruction.ip32(), (op as *const _ as usize, op.opcode)))
                .collect(),
            atomic_store_ref,
            atomic_load_ref,
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
        // The initial entry carries canonical flags, whereas runtime indirect
        // transfers carry the guest instruction's lazy flags. Do not merge the
        // two paths before a normal exit: that would force every return through
        // a generic flags materialization.
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
        for (&start, &block) in &e.blocks.clone() {
            if x87_segments.interiors.contains(&start) {
                e.b.set_cold_block(block);
            }
            e.b.switch_to_block(block);
            e.pc = start;
            e.mark_source(diagnostics::overhead_source(diagnostics::Overhead::Budget));
            e.begin_mmx_block();
            let budget = e.b.use_var(e.fuel);
            #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
            if start == region.start && fault_pc.is_some() {
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
            let zero = e.b.ins().icmp_imm(IntCC::Equal, budget, 0);
            e.guard(zero, Exit::Budget, None);
            let remaining = e.b.ins().iadd_imm(budget, -1);
            e.b.def_var(e.fuel, remaining);
            let mut pc = start;
            loop {
                if pc != start && e.blocks.contains_key(&pc) {
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
                } else if e.instruction(instruction) {
                    break;
                }
                pc = instruction.next_ip32();
            }
        }
        e.emit_canonical_x87_recovery();
        e.mark_source(diagnostics::overhead_source(
            diagnostics::Overhead::Transfer,
        ));
        // Runtime indirect predecessors each carry their own mode. No
        // individual guest block's recipe dominates this block.
        e.mmx_effects = None;
        e.b.switch_to_block(dispatch);
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
        // SSA variables merge each predecessor's state here. Keep one writeback
        // and one lazy-flags materialization instead of duplicating them at every
        // memory/budget guard. Fault addresses are still stored at the fault site.
        e.b.switch_to_block(shared_exit);
        e.mark_source(diagnostics::overhead_source(diagnostics::Overhead::Exit));
        // Every predecessor has materialized its own MMX prefix. The last
        // emitted guest block's compile-time recipe does not describe this merge.
        e.mmx_effects = None;
        let reason = e.b.block_params(shared_exit)[0];
        let pc = e.b.block_params(shared_exit)[1];
        e.save_state(pc);
        if hot_exits {
            e.b.ins().return_(&[reason]);
        } else {
            let prediction = shadow_ref.map(|_| e.b.block_params(shared_exit)[2]);
            e.chain_or_return(pc, None, Some(reason), prediction);
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
            e.save_hot_state(pc);
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
            e.save_hot_state(pc);
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
            e.b.switch_to_block(block);
            e.b.set_srcloc(ir::SourceLoc::new(fault::RETURN_SOURCE));
            let reason = e.constant(Exit::ReadFault as u32);
            e.b.ins().return_(&[reason]);
            e.b.set_srcloc(ir::SourceLoc::default());
            fault_draft = e.fault_draft.take();
        }
        e.b.seal_all_blocks();
        e.b.finalize();
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let fault_draft = fault_draft.and_then(|pending| pending.resolve(&mut ctx.func));
    module
        .define_function(id, &mut ctx)
        .map_err(|e| format!("{e:?}"))?;
    if let Some(directory) = dump_dir {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        if let Some(code) = ctx.compiled_code().and_then(|code| code.vcode.as_ref()) {
            std::fs::write(
                directory.join(format!("region-{:08x}.vcode", region.start)),
                code,
            )
            .map_err(|e| e.to_string())?;
        }
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
    let tail_entry = module.get_finalized_function(id) as usize;
    // Optional native-PC attribution for external sampling. Addresses are
    // process-local; this does not enable execution profiling or disable links.
    let recorded =
        diagnostics::record_body(region, opt_level, tail_entry, code_size, instructions.len());
    if let Some(ranges) = source_ranges {
        diagnostics::record_instructions(region, recorded, tail_entry, ranges, &instructions);
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
        let args = b.block_params(entry).to_vec();
        let address = b.ins().iconst(ptr_ty, tail_entry as i64);
        let call = b.ins().call_indirect(callee, address, &args);
        let result = b.inst_results(call)[0];
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize();
    }
    module
        .define_function(wrapper_id, &mut wrapper)
        .map_err(|e| format!("{e:?}"))?;
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
    let entry = unsafe {
        std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(wrapper_id))
    };
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let mut unwind = Vec::new();
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    for (address, length, info) in [
        (
            tail_entry,
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
    entries.retain(|pc| {
        supported(&instructions[pc]) || x87.iter().any(|op| op.instruction.ip32() == *pc)
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
    Ok(Compiled {
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
        code_size,
        _x87: x87,
        cache_stamp: std::cell::Cell::new(0),
        #[cfg(all(test, target_arch = "x86_64"))]
        disasm,
    })
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn snapshots_copy_only_the_region_and_tail_and_keep_old_versions() {
        let mut memory = Memory::new(0x10000, 0x10000).unwrap();
        memory
            .protect(0x10000, 0x10000, READ | WRITE | EXEC)
            .unwrap();
        memory.write(0x10000, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
        let mut region = Region::new(0x10000, 6);
        let strict = Snapshot::capture(&region, &memory).unwrap();
        assert_eq!(strict.bytes.len(), 6);
        region.complete_tail = true;
        let old = Snapshot::capture(&region, &memory).unwrap();
        assert_eq!(old.bytes.len(), 20);
        assert!(old.valid(&memory));
        // The compiler receives only the immutable old bytes, not this write.
        memory.write(0x10001, &73u32.to_le_bytes()).unwrap();
        assert!(!old.valid(&memory));
        assert_eq!(old.bytes[1], 1);
        let old_result = compile_snapshot(&old).unwrap();
        assert!(!old_result.valid(&memory));
        let fresh = Snapshot::capture(&region, &memory).unwrap();
        assert!(compile_snapshot(&fresh).unwrap().valid(&memory));
        memory.protect(0x10000, 4096, READ).unwrap();
        assert!(!fresh.valid(&memory));
        assert!(Snapshot::capture(&region, &memory).is_err());
    }

    #[test]
    fn unmapped_tail_is_a_negative_dependency_of_the_compiled_result() {
        let mut memory = Memory::reserve(0x10000, 8192).unwrap();
        memory.map(0x10000, 4096, READ | EXEC).unwrap();
        memory.write(0x10fff, &[0xb8]).unwrap();
        let mut region = Region::new(0x10fff, 1);
        region.complete_tail = true;
        let old = Snapshot::capture(&region, &memory).unwrap();
        assert_eq!(old.bytes, [0xb8]);
        assert_eq!(old.pages.len(), 2);
        let compiled = compile_snapshot(&old).unwrap();
        assert!(compiled.valid(&memory));
        memory.map(0x11000, 4096, READ | EXEC).unwrap();
        memory.write(0x11000, &[91, 0, 0, 0]).unwrap();
        assert!(!old.valid(&memory));
        assert!(!compiled.valid(&memory));
        let fresh = Snapshot::capture(&region, &memory).unwrap();
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
        let old = Snapshot::capture(&region, &memory).unwrap();
        // A malformed prefix sequence must not ask for an uncaptured version.
        compile_snapshot(&old).unwrap();
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    #[test]
    fn first_scalar_load_has_a_verified_machine_fault_site_without_hot_checkpoints() {
        let mut memory = Memory::reserve(0, 1usize << 32).unwrap();
        memory.map(0x10000, 4096, READ | EXEC).unwrap();
        memory.write(0x10000, &[0x8b, 0x06, 0xc3]).unwrap();
        memory.enable_native_protection().unwrap();
        let compiled = compile(&Region::new(0x10000, 3), &memory, false).unwrap();
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
            let compiled = compile(&region, &memory, false).unwrap();
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
        let mut compiled = compile(&Region::new(0x10000, 2), &memory, false).unwrap();
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
            let compiled =
                compile(&Region::new(0x10000, bytes.len()), machine.memory(), false).unwrap();
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
fn narrow_alu(mnemonic: M) -> bool {
    matches!(
        mnemonic,
        M::Add | M::Sub | M::And | M::Or | M::Xor | M::Cmp | M::Test | M::Inc | M::Dec
    )
}

fn supported(i: &Instruction) -> bool {
    if crate::strings::operation(i).is_some() {
        return true;
    }
    if i.is_invalid()
        || i.has_lock_prefix()
        || i.has_rep_prefix()
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
    ) {
        return simd::supported(i) || scalar::supported(i) || mmx::supported(i);
    }
    let narrow = matches!(i.mnemonic(), M::Mov | M::Movzx | M::Movsx) || narrow_alu(i.mnemonic());
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
            OpKind::Immediate16 if i.mnemonic() == M::Ret || narrow => {}
            OpKind::Immediate8 if narrow => {}
            OpKind::Immediate8to16 if narrow_alu(i.mnemonic()) => {}
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
        _ => true,
    }
}

#[derive(Clone, Copy)]
struct GuestPointer {
    address: Value,
    guest: Value,
    shared: Value,
    file: Value,
}

struct Emitter<'a> {
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
    perms: Value,
    interrupt: Value,
    context: Value,
    tail_ref: ir::SigRef,
    ptr_ty: ir::Type,
    base: u32,
    len: usize,
    blocks: BTreeMap<u32, ir::Block>,
    dispatch: ir::Block,
    shared_exit: ir::Block,
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
    atomic_store_ref: ir::FuncRef,
    atomic_load_ref: ir::FuncRef,
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
    fault_return: Option<ir::Block>,
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
        // State was saved before reaching the chain candidate. Return directly
        // so an interrupt never charges the next region or grows the Rust stack.
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
        self.b
            .ins()
            .return_call_indirect(self.tail_ref, entry, &[self.context, self.mem]);
        self.b.switch_to_block(returned);
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
    }
    fn exit(&mut self, reason: Exit, pc: Value) {
        self.exit_predicted(reason, pc, None);
    }
    fn exit_predicted(&mut self, reason: Exit, pc: Value, prediction: Option<Value>) {
        self.flush_mmx_effects();
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
        if self.shadow_ref.is_some() {
            let prediction = prediction.unwrap_or_else(|| self.b.ins().iconst(self.ptr_ty, 0));
            self.b.ins().jump(
                self.shared_exit,
                &[reason.into(), pc.into(), prediction.into()],
            );
        } else {
            self.b
                .ins()
                .jump(self.shared_exit, &[reason.into(), pc.into()]);
        }
    }
    fn save_state(&mut self, pc: Value) {
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
        for (i, reg) in self.regs.iter().enumerate() {
            let v = self.b.use_var(*reg);
            self.b
                .ins()
                .store(MemFlags::trusted(), v, self.state, (i * 4) as i32);
        }
        let flags = self.materialize_flags();
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
    fn save_hot_state(&mut self, pc: Value) {
        let Some(summary) = self.write_summary else {
            self.save_state(pc);
            return;
        };
        for index in 0..8 {
            if summary.gpr & (1 << index) != 0 {
                let value = self.b.use_var(self.regs[index]);
                self.b
                    .ins()
                    .store(MemFlags::trusted(), value, self.state, (index * 4) as i32);
            }
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
        if summary.flags {
            let flags = self.materialize_flags();
            self.b.ins().store(
                MemFlags::trusted(),
                flags,
                self.state,
                offset_of!(CpuState, eflags) as i32,
            );
        }
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
    }
    fn direct(&mut self, pc: u32) {
        if let Some(&block) = self.blocks.get(&pc) {
            if pc <= self.pc {
                let target = self.constant(pc);
                self.interrupt_at(target, false);
            }
            self.flush_mmx_effects();
            self.b.ins().jump(block, &[]);
        } else {
            self.exit_const(Exit::Dispatch, pc);
        }
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
        let pending = self
            .b
            .ins()
            .atomic_load(types::I8, MemFlags::trusted(), self.interrupt);
        let pending = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
        let fuel = self.b.use_var(self.fuel);
        let work = self.b.ins().icmp_imm(IntCC::NotEqual, fuel, 0);
        let mut pending = self.b.ins().band(pending, work);
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
        let mut addr = self.constant(i.memory_displacement32());
        if let Some(reg) = reg_index(i.memory_base()) {
            let value = self.b.use_var(self.regs[reg]);
            addr = self.b.ins().iadd(addr, value);
        }
        if let Some(reg) = reg_index(i.memory_index()) {
            let value = self.b.use_var(self.regs[reg]);
            let scaled = self
                .b
                .ins()
                .imul_imm(value, i64::from(i.memory_index_scale()));
            addr = self.b.ins().iadd(addr, scaled);
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
    fn pointer(&mut self, addr: Value, size: usize) -> GuestPointer {
        self.mark(diagnostics::Part::Check);
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let bad =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, offset, (self.len - size) as i64);
        self.guard(bad, Exit::WriteFault, Some(addr));
        let end = self.b.ins().iadd_imm(offset, size as i64 - 1);
        // Both metadata addresses are safe after the range guard. Loading them
        // together lets either original SHARED guard enter one cold path, where
        // all permissions are checked before file preparation or SSA mutation.
        let permissions = [offset, end].map(|position| {
            let page = self.b.ins().ushr_imm(position, 12);
            let page = self.b.ins().uextend(self.ptr_ty, page);
            let at = self.b.ins().iadd(self.perms, page);
            self.b.ins().load(types::I8, MemFlags::trusted(), at, 0)
        });
        let combined = self.b.ins().bor(permissions[0], permissions[1]);
        let shared = self
            .b
            .ins()
            .band_imm(combined, i64::from(crate::memory::SHARED));
        let executable = self.b.ins().band_imm(combined, i64::from(EXEC));
        let executable = self.b.ins().icmp_imm(IntCC::NotEqual, executable, 0);
        let no_file = self.b.ins().iconst(types::I8, 0);
        let cold = self.b.create_block();
        self.b.set_cold_block(cold);
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        for p in permissions {
            let mask = self
                .b
                .ins()
                .band_imm(p, i64::from(WRITE | crate::memory::SHARED));
            let bad = self
                .b
                .ins()
                .icmp_imm(IntCC::NotEqual, mask, i64::from(WRITE));
            let ok = self.b.create_block();
            self.b.ins().brif(bad, cold, &[], ok, &[]);
            self.b.switch_to_block(ok);
        }
        self.guard(executable, Exit::Fallback, None);
        self.b.ins().jump(done, &[no_file.into()]);
        self.b.switch_to_block(cold);
        self.mark(diagnostics::Part::Slow);
        for p in permissions {
            let access = self.b.ins().band_imm(p, i64::from(WRITE));
            let missing = self.b.ins().icmp_imm(IntCC::Equal, access, 0);
            self.guard(missing, Exit::WriteFault, Some(addr));
            self.b.set_cold_block(self.b.current_block().unwrap());
        }
        self.guard(executable, Exit::Fallback, None);
        self.b.set_cold_block(self.b.current_block().unwrap());
        let file = permissions.map(|p| {
            let kind = self
                .b
                .ins()
                .band_imm(p, i64::from(crate::memory::SHARED | crate::memory::AUDIO));
            self.b
                .ins()
                .icmp_imm(IntCC::Equal, kind, i64::from(crate::memory::SHARED))
        });
        let file = self.b.ins().bor(file[0], file[1]);
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
        self.mark(diagnostics::Part::Body);
        let file = self.b.block_params(done)[0];
        let host_offset = self.b.ins().uextend(self.ptr_ty, offset);
        let address = self.b.ins().iadd(self.mem, host_offset);
        GuestPointer {
            address,
            guest: addr,
            shared,
            file,
        }
    }
    /// Loads and plain stores can finish their shared access inside the original
    /// page-guard cold path. No classification branch runs after an ordinary
    /// guard succeeds. RMW stores retain pointer()'s early prevalidation because
    /// their flags and value are computed between validation and writeback.
    fn direct_memory(&mut self, addr: Value, ty: ir::Type, value: Option<Value>) -> Option<Value> {
        let write = value.is_some();
        let size = ty.bytes() as usize;
        self.mark(diagnostics::Part::Check);
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        let hardware = if !write
            && self.fault_pc == Some(self.pc)
            && matches!(ty, types::I8 | types::I16 | types::I32)
        {
            self.capture_hardware_read(addr, size)
        } else {
            None
        };
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
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
        let end = self.b.ins().iadd_imm(offset, size as i64 - 1);
        let host_offset = self.b.ins().uextend(self.ptr_ty, offset);
        let address = self.b.ins().iadd(self.mem, host_offset);
        let atomic = self.b.create_block();
        self.b.set_cold_block(atomic);
        let done = self.b.create_block();
        if !write {
            self.b.append_block_param(done, ty);
        }
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        if let Some((enabled, _)) = hardware {
            let classify = self.b.create_block();
            let checked = self.b.create_block();
            let raw = self.b.create_block();
            self.b.ins().brif(enabled, classify, &[], checked, &[]);
            self.b.switch_to_block(classify);
            let permissions = [offset, end].map(|position| {
                let page = self.b.ins().ushr_imm(position, 12);
                let page = self.b.ins().uextend(self.ptr_ty, page);
                let at = self.b.ins().iadd(self.perms, page);
                self.b.ins().load(types::I8, MemFlags::trusted(), at, 0)
            });
            let combined = self.b.ins().bor(permissions[0], permissions[1]);
            let shared = self
                .b
                .ins()
                .band_imm(combined, i64::from(crate::memory::SHARED));
            self.b.ins().brif(shared, checked, &[], raw, &[]);
            self.b.switch_to_block(raw);
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
        let mut combined = None;
        for position in [offset, end] {
            let page = self.b.ins().ushr_imm(position, 12);
            let page = self.b.ins().uextend(self.ptr_ty, page);
            let at = self.b.ins().iadd(self.perms, page);
            let p = self.b.ins().load(types::I8, MemFlags::trusted(), at, 0);
            let mask = self
                .b
                .ins()
                .band_imm(p, i64::from(permission | crate::memory::SHARED));
            let bad = self
                .b
                .ins()
                .icmp_imm(IntCC::NotEqual, mask, i64::from(permission));
            let ok = self.b.create_block();
            self.b.ins().brif(bad, atomic, &[], ok, &[]);
            self.b.switch_to_block(ok);
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
        }
        self.mark(diagnostics::Part::Body);
        if let Some(value) = value {
            self.b.ins().store(MemFlags::new(), value, address, 0);
            self.b.ins().jump(done, &[]);
        } else {
            let result = self.b.ins().load(ty, MemFlags::new(), address, 0);
            self.b.ins().jump(done, &[result.into()]);
        }
        self.b.switch_to_block(atomic);
        self.mark(diagnostics::Part::Slow);
        // Either boundary guard can enter here. Revalidate *both* pages before
        // any byte is read/written. Mixed file/AUDIO elements use checked Memory
        // helpers, which keep PCM subranges byte atomic.
        let mut combined = None;
        for position in [offset, end] {
            let page = self.b.ins().ushr_imm(position, 12);
            let page = self.b.ins().uextend(self.ptr_ty, page);
            let at = self.b.ins().iadd(self.perms, page);
            let p = self.b.ins().load(types::I8, MemFlags::trusted(), at, 0);
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
        let file = [offset, end].map(|position| {
            let page = self.b.ins().ushr_imm(position, 12);
            let page = self.b.ins().uextend(self.ptr_ty, page);
            let at = self.b.ins().iadd(self.perms, page);
            let p = self.b.ins().load(types::I8, MemFlags::trusted(), at, 0);
            let kind = self
                .b
                .ins()
                .band_imm(p, i64::from(crate::memory::SHARED | crate::memory::AUDIO));
            self.b
                .ins()
                .icmp_imm(IntCC::Equal, kind, i64::from(crate::memory::SHARED))
        });
        let file = self.b.ins().bor(file[0], file[1]);
        let overlay = self.b.create_block();
        self.b.set_cold_block(overlay);
        let pcm = self.b.create_block();
        self.b.set_cold_block(pcm);
        self.b.ins().brif(file, overlay, &[], pcm, &[]);
        self.b.switch_to_block(pcm);
        if let Some(value) = value {
            self.store_atomic(address, value);
            self.b.ins().jump(done, &[]);
        } else {
            let result = self.load_atomic(address, ty);
            self.b.ins().jump(done, &[result.into()]);
        }
        self.b.switch_to_block(overlay);
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            16,
            4,
        ));
        let buffer = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let width = self.constant(size as u32);
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
        self.b.switch_to_block(done);
        self.mark(diagnostics::Part::Body);
        (!write).then(|| self.b.block_params(done)[0])
    }
    fn direct_load(&mut self, addr: Value, ty: ir::Type) -> Value {
        self.direct_memory(addr, ty, None).unwrap()
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fn capture_hardware_read(
        &mut self,
        address: Value,
        width: usize,
    ) -> Option<(Value, ir::Block)> {
        let variable = self.fault_mode?;
        let outputs = std::array::from_fn(|index| match index {
            0..=7 => self.b.use_var(self.regs[index]),
            8 => self.b.use_var(self.flags),
            9..=15 => self.b.use_var(self.lazy[index - 9]),
            16 => self.b.use_var(self.fuel),
            _ => address,
        });
        let draft = fault::Draft::capture(
            &mut self.b,
            &self.fault_seeds,
            self.state,
            outputs,
            self.fault_entry,
            self.pc,
            width,
        )?;
        let enabled = self.b.use_var(variable);
        let disabled = self.b.ins().iconst(types::I8, 0);
        self.b.def_var(variable, disabled);
        let recovery = self.b.create_block();
        self.b.set_cold_block(recovery);
        self.fault_return = Some(recovery);
        self.fault_draft = Some(draft);
        Some((enabled, recovery))
    }
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    fn checked_read_guard(&mut self, bad: Value, address: Value, recovery: ir::Block) {
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
        self.b.ins().jump(recovery, &[]);
        self.b.switch_to_block(continued);
    }
    fn direct_store(&mut self, addr: Value, value: Value) {
        let ty = self.b.func.dfg.value_type(value);
        self.direct_memory(addr, ty, Some(value));
    }
    fn load_atomic(&mut self, pointer: Value, ty: ir::Type) -> Value {
        if ty.is_vector() {
            // Sixteen inlined byte loads keep many GPR values live and make
            // register allocation spill the ordinary copy loop. A bounded
            // Relaxed helper loads into a private stack buffer instead.
            let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                16,
                4,
            ));
            let destination = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
            self.b
                .ins()
                .call(self.atomic_load_ref, &[pointer, destination]);
            return self.b.ins().stack_load(ty, slot, 0);
        }
        let mut value = self.b.ins().iconst(ty, 0);
        for index in 0..ty.bytes() {
            let address = self.b.ins().iadd_imm(pointer, i64::from(index));
            // Cranelift currently exposes SeqCst atomic loads only. They are
            // stronger than the protocol's Relaxed loads, with the same byte
            // width. The compiler cannot combine them into an ordinary wide load.
            let byte = self
                .b
                .ins()
                .atomic_load(types::I8, MemFlags::trusted(), address);
            if ty == types::I8 {
                value = byte;
            } else {
                let byte = self.b.ins().uextend(ty, byte);
                let byte = self.b.ins().ishl_imm(byte, i64::from(index * 8));
                value = self.b.ins().bor(value, byte);
            }
        }
        value
    }
    fn store_memory(&mut self, pointer: GuestPointer, value: Value) {
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
            .store(MemFlags::new(), value, pointer.address, 0);
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
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            16,
            4,
        ));
        self.b.ins().stack_store(value, slot, 0);
        let buffer = self.b.ins().stack_addr(self.ptr_ty, slot, 0);
        let width = self.constant(self.b.func.dfg.value_type(value).bytes());
        self.b.ins().call(
            self.file_prepared_ref,
            &[self.bridge, pointer.guest, buffer, width],
        );
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
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
            OpKind::Register => {
                let reg = i.op_register(n);
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
            let reg = i.op0_register();
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
            let raw = self.b.ins().icmp_imm(IntCC::Equal, kind, 0);
            let c = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
            let c = self.b.ins().uextend(types::I32, c);
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
            4 | 11 => {
                let ab = self.b.ins().bxor(a, b);
                let base = self.b.ins().band_imm(kind, 255);
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
        let raw = self.b.ins().icmp_imm(IntCC::Equal, kind, 0);
        self.b.ins().select(raw, old, value)
    }
    fn materialize_flags(&mut self) -> Value {
        let old = self.b.use_var(self.flags);
        let mut flags = self.b.ins().band_imm(old, !0x8d5_i64);
        for bit in [0, 2, 4, 6, 7, 11] {
            let value = self.flag(bit);
            let value = self.b.ins().ishl_imm(value, i64::from(bit));
            flags = self.b.ins().bor(flags, value);
        }
        flags
    }
    fn condition(&mut self, m: M) -> Value {
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
            self.direct(i.next_ip32());
            self.b.switch_to_block(retry);
            self.direct(i.ip32());
            self.b.switch_to_block(fallback);
            self.exit_const(Exit::Fallback, i.ip32());
            self.b.switch_to_block(budget);
            self.exit_const(Exit::Budget, i.ip32());
            self.b.switch_to_block(read_fault);
            self.exit_const(Exit::ReadFault, i.ip32());
            self.b.switch_to_block(write_fault);
            self.exit_const(Exit::WriteFault, i.ip32());
            return true;
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
                let sp = self.b.use_var(self.regs[4]);
                let sp = self.b.ins().iadd_imm(sp, -4);
                let ptr = self.pointer(sp, 4);
                let pushed = if i.mnemonic() == M::Call {
                    self.constant(i.next_ip32())
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
                        self.indirect(v);
                    }
                    return true;
                }
            }
            M::Pop | M::Ret => {
                let sp = self.b.use_var(self.regs[4]);
                let original_sp = sp;
                let value = self.direct_load(sp, types::I32);
                let adjustment = if i.mnemonic() == M::Ret && i.op_count() != 0 {
                    i64::from(i.immediate16())
                } else {
                    0
                };
                let sp = self.b.ins().iadd_imm(sp, 4 + adjustment);
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
