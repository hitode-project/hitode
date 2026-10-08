//! Linux/Android/macOS ARM64 entry/exit, immutable code ownership, and checked continuations.
use super::{BUDGET, DISPATCH, Execution, FALLBACK, LOAD, STORE, Tier1Stats, emit};
use crate::execution_abi::Tier1Frame;
use crate::interpreter::{Execution as Interpreted, Interpreter, Step};
use crate::{CpuState, Memory};
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[cfg(not(target_os = "macos"))]
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

core::arch::global_asm!(
    r#"
.text
.p2align 2
.if {apple}
.global _adv_tier1_enter
_adv_tier1_enter:
.else
.global adv_tier1_enter
.type adv_tier1_enter,%function
adv_tier1_enter:
.endif
    stp x29, x30, [sp, #-96]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x25, x26, [sp, #64]
    stp x27, x28, [sp, #80]
    mov x29, sp
    mov x27, x0
    mov x28, x2
    mov x16, x1
    ldp w19, w20, [x27]
    ldp w21, w22, [x27, #8]
    ldp w23, w24, [x27, #16]
    ldp w25, w26, [x27, #24]
    ldr w8, [x27, #32]
    ldr x7, [x27, #40]
    ldr w6, [x27, #48]
    blr x16
    stp w19, w20, [x27]
    stp w21, w22, [x27, #8]
    stp w23, w24, [x27, #16]
    stp w25, w26, [x27, #24]
    str w8, [x27, #32]
    ldp x19, x20, [sp, #16]
    ldp x21, x22, [sp, #32]
    ldp x23, x24, [sp, #48]
    ldp x25, x26, [sp, #64]
    ldp x27, x28, [sp, #80]
    ldp x29, x30, [sp], #96
    ret
.if !{apple}
.size adv_tier1_enter, .-adv_tier1_enter
.endif
"#,
    apple = const cfg!(target_os = "macos") as u8
);
unsafe extern "C" {
    fn adv_tier1_enter(frame: *mut Tier1Frame, entry: *const u8, window: *mut u8);
    // Android uses the NDK compiler-rt archive already linked by build.rs.
    #[cfg(not(target_os = "macos"))]
    fn __clear_cache(start: *mut core::ffi::c_void, end: *mut core::ffi::c_void);
}

/// The entry frame followed by exclusive helper and architectural-state data.
/// The frame comes first, so the pointer in x27 is also the helpers' context.
#[repr(C)]
pub(super) struct Entry {
    pub(super) frame: Tier1Frame,
    pub(super) memory: *mut Memory,
    pub(super) inline: u64,
    pub(super) state: *mut CpuState,
    pub(super) vector: [u64; 2],
}

/// Checked 1/2/4-byte guest load without leaving native code, zero- or
/// sign-extended by `kind`. The checks are the same as the Tier 0 access;
/// on refusal nothing was read and the instruction reruns there.
extern "C" fn load(entry: *mut Entry, address: u32, kind: u32) -> u64 {
    // SAFETY: called only by emitted code inside `run_block`, which derived
    // `memory` from its exclusive borrow and does not touch it until return.
    let (memory, inline) = unsafe { (&*(*entry).memory, &mut (*entry).inline) };
    let width = (kind & 7) as u8;
    if !crate::interpreter::accessible(memory, address, width, crate::READ) {
        return 1 << 32;
    }
    let Ok(bytes) = memory.read_data(address, width as usize) else {
        return 1 << 32;
    };
    *inline += 1;
    let mut raw = [0; 4];
    raw[..bytes.len()].copy_from_slice(&bytes);
    let value = u32::from_le_bytes(raw);
    let unused = 32 - 8 * u32::from(width);
    u64::from(if kind & emit::SIGNED != 0 {
        ((value << unused) as i32 >> unused) as u32
    } else {
        value
    })
}

/// Checked 1/2/4-byte guest store without leaving native code. A store to
/// executable memory or one that advances the epoch asks the code to leave.
/// CHECK_WRITE probes only validate permission, matching zero-count shifts.
extern "C" fn store(entry: *mut Entry, address: u32, value: u32, kind: u32) -> u32 {
    // SAFETY: as for `load`.
    let (memory, inline) = unsafe { (&mut *(*entry).memory, &mut (*entry).inline) };
    let width = kind & 7;
    // Memory::write is a host write API; guest permissions must be checked
    // before it so immutable code and guards cannot be bypassed.
    if !crate::interpreter::accessible(memory, address, width as u8, crate::WRITE) {
        return 2;
    }
    if kind & emit::CHECK_WRITE != 0 {
        return 0;
    }
    let epoch = memory.epoch;
    if memory
        .write(address, &value.to_le_bytes()[..width as usize])
        .is_err()
    {
        return 2;
    }
    *inline += 1;
    u32::from(
        memory.epoch != epoch
            || memory.executable(address, 1)
            || memory.executable(address.wrapping_add(width - 1), 1),
    )
}

/// Stage a complete PUSHAD/POPAD transaction. No live guest register changes
/// before success; POPAD deliberately does not read the saved ESP slot.
extern "C" fn stack_all(entry: *mut Entry, pop: u32) -> u32 {
    // SAFETY: the emitted caller spilled all GPRs to its private entry frame.
    let entry = unsafe { &mut *entry };
    let memory = unsafe { &mut *entry.memory };
    let mut regs = entry.frame.regs;
    let esp = regs[4];
    if pop != 0 {
        for (offset, index) in [7, 6, 5, 4, 3, 2, 1, 0].into_iter().enumerate() {
            if index == 4 {
                continue;
            }
            let address = esp.wrapping_add(offset as u32 * 4);
            if !crate::interpreter::accessible(memory, address, 4, crate::READ) {
                return 2;
            }
            let Ok(bytes) = memory.read_data(address, 4) else {
                return 2;
            };
            regs[index] = u32::from_le_bytes(bytes.as_ref().try_into().expect("scalar read"));
        }
        regs[4] = esp.wrapping_add(32);
        entry.frame.regs = regs;
        entry.inline += 7;
        return 0;
    }
    let Some(address) = esp.checked_sub(32) else {
        return 2;
    };
    // A single private-range write avoids partially committed pushes. Shared
    // stacks and wrapping ranges retain Tier 0's ordered store transaction.
    if !crate::interpreter::accessible(memory, address, 32, crate::WRITE)
        || memory.shared_range(address, 32)
    {
        return 2;
    }
    let mut bytes = [0; 32];
    for (n, value) in regs.iter().rev().enumerate() {
        bytes[n * 4..n * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    let epoch = memory.epoch;
    if memory.write(address, &bytes).is_err() {
        return 2;
    }
    entry.frame.regs[4] = address;
    entry.inline += 8;
    u32::from(
        memory.epoch != epoch || memory.executable(address, 1) || memory.executable(esp - 1, 1),
    )
}

#[cfg(target_os = "macos")]
use crate::jit_memory_macos::Code;

#[cfg(not(target_os = "macos"))]
struct Code {
    pointer: NonNull<u8>,
    len: usize,
}
#[cfg(not(target_os = "macos"))]
impl Code {
    fn new(words: &[u32]) -> Result<Self, String> {
        let bytes = words
            .len()
            .checked_mul(4)
            .ok_or("Tier 1 code size overflow")?;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err("cannot query Tier 1 host page size".into());
        }
        let len = bytes
            .checked_add(page as usize - 1)
            .ok_or("Tier 1 code size overflow")?
            / (page as usize)
            * (page as usize);
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(format!(
                "allocate Tier 1 code: {}",
                std::io::Error::last_os_error()
            ));
        }
        let code = Self {
            pointer: NonNull::new(raw.cast()).ok_or("null Tier 1 code mapping")?,
            len,
        };
        // SAFETY: private writable allocation; no entry is published yet.
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                code.pointer.as_ptr(),
                bytes,
            );
            // Clean data and invalidate instruction caches while still RW.
            // The serial Rc owner publishes no entry until RX succeeds; code
            // is immutable afterwards and never has a writable executable view.
            __clear_cache(raw, raw.cast::<u8>().add(bytes).cast());
        }
        if unsafe { libc::mprotect(raw, len, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
            return Err(format!(
                "publish Tier 1 code: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(code)
    }
}
#[cfg(not(target_os = "macos"))]
impl Drop for Code {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.pointer.as_ptr().cast(), self.len);
        }
    }
}

#[cfg(test)]
mod publication_tests {
    use super::Code;

    #[test]
    fn published_code_is_rx_and_new_instructions_are_visible() {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        for value in 1..=16u32 {
            // Authored AArch64 MOV W0, #imm; RET. Cross a real host-page boundary
            // so both 4 KiB and 16 KiB devices exercise full mapping protection.
            let mut words = vec![0xd503201f; page / 4 + 1];
            words[0] = 0x52800000 | (value << 5);
            words[1] = 0xd65f03c0;
            let code = Code::new(&words).unwrap();
            assert!(code.len >= std::mem::size_of_val(words.as_slice()));
            #[cfg(not(target_os = "macos"))]
            {
                assert_eq!(code.len, 2 * page);
                let start = code.pointer.as_ptr() as usize;
                let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
                let permissions = maps.lines().find_map(|line| {
                    let mut fields = line.split_whitespace();
                    let (low, high) = fields.next()?.split_once('-')?;
                    let low = usize::from_str_radix(low, 16).ok()?;
                    let high = usize::from_str_radix(high, 16).ok()?;
                    (low <= start && high >= start + code.len).then(|| fields.next().unwrap())
                });
                assert_eq!(permissions, Some("r-xp"));
            }
            // SAFETY: the immutable RX entry obeys the host ABI and returns u32.
            let entry: unsafe extern "C" fn() -> u32 =
                unsafe { std::mem::transmute(code.pointer.as_ptr()) };
            assert_eq!(unsafe { entry() }, value);
        }
    }
}

struct Unit {
    // Field order retires the diagnostic range before Code unmaps its storage.
    _perf_range: Option<crate::perf_map::Registration>,
    code: Code,
    input: Vec<(u32, Vec<u8>)>,
    // Descriptors embedded in native calls share the immutable code lifetime.
    _x87: super::x87::Operations,
    _sse: super::sse::Operations,
}
impl Unit {
    fn valid(&self, memory: &Memory) -> bool {
        self.input.iter().all(|(pc, bytes)| {
            memory.executable(*pc, bytes.len())
                && !memory.shared_range(*pc, bytes.len())
                && memory
                    .read_data(*pc, bytes.len())
                    .is_ok_and(|current| current.as_ref() == bytes)
        })
    }
}

pub(crate) struct Engine {
    entries: BTreeMap<u32, (Rc<Unit>, usize)>,
    // Pin units until retirement, including overwritten entries.
    // This keeps page-rounded accounting exact without scanning on each lookup.
    units: Vec<Rc<Unit>>,
    entry_limit: usize,
    byte_limit: usize,
    epoch: u64,
    native_revision: u64,
    stats: Tier1Stats,
}
impl Engine {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            entries: BTreeMap::new(),
            units: Vec::new(),
            entry_limit: 4096,
            byte_limit: 16 * 1024 * 1024,
            epoch: 0,
            native_revision: 0,
            stats: Tier1Stats::default(),
        })
    }
    pub fn stats(&self) -> Tier1Stats {
        self.stats.clone()
    }
    pub fn record_bridge(&mut self, units: u32) {
        self.stats.bridge_units += u64::from(units);
        self.stats.bridge_entries += u64::from(units != 0);
    }
    fn clear(&mut self) {
        self.entries.clear();
        self.units.clear();
        self.stats.resident_bytes = 0;
    }
    fn retire_published(&mut self, boundaries: &BTreeMap<u32, u32>) {
        // Inspect every pinned CFG, not only the current entry map: a later CFG
        // can overwrite an entry while an older owner still branches through it.
        let retired: BTreeSet<_> = self
            .units
            .iter()
            .filter(|unit| unit.input.iter().any(|(pc, _)| boundaries.contains_key(pc)))
            .map(Rc::as_ptr)
            .collect();
        if retired.is_empty() {
            return;
        }
        self.stats.publication_flushes += u64::from(retired.len() == self.units.len());
        self.stats.cache_evictions += retired.len() as u64;
        self.entries
            .retain(|_, (unit, _)| !retired.contains(&Rc::as_ptr(unit)));
        self.units
            .retain(|unit| !retired.contains(&Rc::as_ptr(unit)));
        self.stats.resident_bytes = self.units.iter().map(|unit| unit.code.len).sum();
    }
    pub fn set_cache_limits(&mut self, entries: usize, bytes: usize) {
        self.entry_limit = entries;
        self.byte_limit = bytes;
        if self.entries.len() > entries || self.stats.resident_bytes > bytes {
            self.stats.cache_evictions += self.units.len() as u64;
            self.clear();
        }
    }
    fn compile(
        &mut self,
        memory: &mut Memory,
        start: u32,
        boundaries: Option<&BTreeMap<u32, u32>>,
    ) -> Result<bool, String> {
        if self.entry_limit == 0 || self.byte_limit == 0 {
            self.stats.cache_rejections += 1;
            return Ok(false);
        }
        let timer = Instant::now();
        let mut queue = VecDeque::from([start]);
        let mut instructions = BTreeMap::<u32, Instruction>::new();
        let mut input = Vec::new();
        while let Some(pc) = queue.pop_front() {
            if instructions.contains_key(&pc)
                || instructions.len() >= 128
                || pc.abs_diff(start) > 4096
                || boundaries.is_some_and(|entries| entries.contains_key(&pc))
            {
                continue;
            }
            let mut bytes = Vec::with_capacity(15);
            for offset in 0..15u32 {
                let Some(at) = pc.checked_add(offset) else {
                    break;
                };
                // An external writer can change shared backing without this
                // owner's epoch. Such code must stay on the Tier 0 path.
                if !memory.executable(at, 1) || memory.shared_range(at, 1) {
                    break;
                }
                memory.watch_code(at, 1);
                let Ok(byte) = memory.read_data(at, 1) else {
                    break;
                };
                bytes.push(byte[0]);
            }
            let mut decoder = Decoder::with_ip(32, &bytes, u64::from(pc), DecoderOptions::NONE);
            let instruction = decoder.decode();
            if instruction.is_invalid() {
                continue;
            }
            bytes.truncate(instruction.len());
            input.push((pc, bytes));
            instructions.insert(pc, instruction);
            if !emit::supported(&instruction) {
                continue;
            }
            if instruction.is_jcc_short_or_near()
                || emit::counter_branch(&instruction).is_some()
                || matches!(instruction.mnemonic(), Mnemonic::Jmp | Mnemonic::Call)
                    && instruction.op0_kind() == OpKind::NearBranch32
            {
                queue.push_back(instruction.near_branch_target() as u32);
            }
            // A call's return point is likely code; bytes after RET or JMP are not.
            if !matches!(
                instruction.flow_control(),
                FlowControl::UnconditionalBranch
                    | FlowControl::IndirectBranch
                    | FlowControl::Return
            ) {
                queue.push_back(instruction.next_ip32());
            }
        }
        if !instructions.contains_key(&start) {
            return Ok(false);
        }
        let host_features = crate::HostFeatures::get();
        let x87 = super::x87::Operations::new(&instructions, &input, host_features);
        let sse = super::sse::Operations::new(&instructions, host_features);
        let mut emitted = emit::Emitter::new(
            emit::Helpers {
                load: load as *const () as u64,
                store: store as *const () as u64,
                stack_all: stack_all as *const () as u64,
                x87: super::x87::call as *const () as u64,
                sse: super::sse::call as *const () as u64,
                vector_load: super::simd::load as *const () as u64,
                vector_store: super::simd::store as *const () as u64,
            },
            host_features,
        );
        for instruction in instructions.values() {
            emitted.instruction(
                instruction,
                x87.get(instruction.ip32()),
                sse.get(instruction.ip32()),
            );
        }
        let emitted = emitted.finish()?;
        let code = Code::new(&emitted.words)?;
        if instructions.len() > self.entry_limit || code.len > self.byte_limit {
            self.stats.cache_rejections += 1;
            return Ok(false);
        }
        if self.entries.len() + instructions.len() > self.entry_limit
            || self.stats.resident_bytes + code.len > self.byte_limit
        {
            self.stats.cache_evictions += self.units.len() as u64;
            self.clear();
        }
        self.stats.resident_bytes += code.len;
        self.stats.compilations += 1;
        self.stats.code_bytes += emitted.words.len() as u64 * 4;
        let unit = Rc::new(Unit {
            _perf_range: crate::perf_map::register(
                code.pointer.as_ptr() as usize,
                emitted.words.len() * 4,
                crate::perf_map::Category::Tier1,
            ),
            code,
            input,
            _x87: x87,
            _sse: sse,
        });
        for pc in instructions.keys() {
            self.entries
                .insert(*pc, (unit.clone(), emitted.entries[pc]));
        }
        self.units.push(unit);
        self.epoch = memory.epoch;
        self.stats.compile_time += timer.elapsed();
        Ok(true)
    }
    pub fn run_block(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        interpreter: &mut Interpreter,
        units: u32,
        stop: u32,
        boundaries: Option<(&BTreeMap<u32, u32>, u64)>,
    ) -> Result<Execution, String> {
        if memory.is_parallel() {
            return Err("Tier 1 requires an exclusive serial execution owner".into());
        }
        // Publications happen only on this serial owner between batches. Old
        // CFGs may cross a newly published entry, so retire conflicting owners
        // before running again. Disjoint CFGs remain valid across publications.
        if let Some((entries, revision)) = boundaries
            && self.native_revision != revision
        {
            self.stats.publication_revisions += 1;
            self.retire_published(entries);
            self.native_revision = revision;
        }
        if self.epoch != memory.epoch
            || self
                .entries
                .get(&state.eip)
                .is_some_and(|(unit, _)| !unit.valid(memory))
        {
            if !self.entries.is_empty() {
                self.stats.invalidations += 1;
            }
            self.clear();
            self.epoch = memory.epoch;
        }
        if !self.entries.contains_key(&state.eip)
            && !self.compile(memory, state.eip, boundaries.map(|(entries, _)| entries))?
        {
            self.stats.compile_misses += 1;
            self.stats.fallback_entries += 1;
            let execution = interpreter.run_chunk(state, memory, units.min(1));
            self.stats.fallback_instructions += u64::from(execution.units);
            return Ok(Execution {
                execution,
                interpreted_units: execution.units,
                interpreted: true,
                last_units: execution.units,
            });
        }
        let (unit, offset) = &self.entries[&state.eip];
        let window = memory.ptr.as_ptr();
        state.commit_mmx();
        let mut entry = Entry {
            frame: Tier1Frame {
                regs: state.regs,
                flags: state.eflags,
                pc: state.eip,
                remaining: u64::from(units),
                stop,
                fs_base: state.fs_base,
                ..Default::default()
            },
            memory: std::ptr::from_mut(memory),
            inline: 0,
            state: std::ptr::from_mut(state),
            vector: [0; 2],
        };
        // SAFETY: this owner pins all code and exclusively owns the CPU/memory.
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        crate::jit_memory_macos::synchronize_execution();
        // Every emitted entry uses the fixed ABI; the access helpers reach
        // memory only through `entry`, and `memory` is unused until return.
        unsafe {
            adv_tier1_enter(
                (&raw mut entry).cast(),
                unit.code.pointer.as_ptr().add(*offset),
                window,
            );
        }
        let frame = entry.frame;
        self.stats.inline_accesses += entry.inline;
        state.regs = frame.regs;
        state.eflags = frame.flags;
        state.eip = frame.pc;
        let completed = units - (frame.remaining as u32);
        self.stats.native_entries += 1;
        self.stats.saves += 1;
        self.stats.restores += 1;
        self.stats.native_instructions += u64::from(completed);
        self.stats.direct_transfers += frame.links;
        let (step, extra, interpreted) = match frame.reason {
            DISPATCH | BUDGET => (Step::Executed, 0, 0),
            FALLBACK => {
                self.stats.fallback_entries += 1;
                let e = interpreter.run_chunk(state, memory, frame.remaining.min(1) as u32);
                self.stats.fallback_instructions += u64::from(e.units);
                (e.step, e.units, e.units)
            }
            LOAD => {
                self.stats.checked_accesses += 1;
                match crate::interpreter::accessible(memory, frame.address, 4, crate::READ)
                    .then(|| memory.read_data(frame.address, 4))
                    .transpose()
                {
                    Ok(Some(bytes)) => {
                        state.regs[frame.operand as usize] = u32::from_le_bytes(
                            bytes.as_ref().try_into().expect("checked scalar size"),
                        );
                        state.eip = frame.value;
                        (Step::Executed, 1, 0)
                    }
                    _ => (
                        Step::ReadFault {
                            pc: frame.pc,
                            address: frame.address,
                        },
                        0,
                        0,
                    ),
                }
            }
            STORE => {
                self.stats.checked_accesses += 1;
                // Memory::write is a host write API; guest permissions must be
                // checked before it so immutable code and guards cannot be bypassed.
                if !crate::interpreter::accessible(memory, frame.address, 4, crate::WRITE) {
                    (
                        Step::WriteFault {
                            pc: frame.pc,
                            address: frame.address,
                        },
                        0,
                        0,
                    )
                } else {
                    match memory.write(frame.address, &frame.value.to_le_bytes()) {
                        Ok(()) => {
                            state.eip = frame.operand;
                            (Step::Executed, 1, 0)
                        }
                        Err(_) => (
                            Step::WriteFault {
                                pc: frame.pc,
                                address: frame.address,
                            },
                            0,
                            0,
                        ),
                    }
                }
            }
            _ => return Err("invalid Tier 1 exit reason".into()),
        };
        Ok(Execution {
            execution: Interpreted {
                step,
                units: completed + extra,
            },
            interpreted_units: interpreted,
            interpreted: frame.reason == FALLBACK,
            last_units: extra,
        })
    }
}
