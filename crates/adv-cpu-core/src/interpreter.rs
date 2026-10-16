//! Bounded predecoded x86-32 integer execution on the shared CPU and memory.
//!
//! Potentially faulting instructions stage integer state and memory writes.
//! Infallible register forms commit directly at the same instruction boundary.
//! Guest accesses are checked before the write and CPU commit, keeping faults
//! and unsupported forms restartable.
//! Undefined arithmetic flags are preserved rather than borrowed from the host.
//! GS, segment/control registers and far transfers remain outside the flat model.
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;

use iced_x86::{
    Code, ConditionCode, Decoder, DecoderError, DecoderOptions, Instruction, Mnemonic as M, OpKind,
    Register as R,
};

use crate::{CpuState, EXEC, Memory, PAGE_SIZE, READ, WRITE};

mod registers;
mod scalar;
mod simd;
mod strings;
#[cfg(test)]
mod window_tests;

const CF: u32 = 1;
const PF: u32 = 1 << 2;
const AF: u32 = 1 << 4;
const ZF: u32 = 1 << 6;
const SF: u32 = 1 << 7;
const OF: u32 = 1 << 11;

/// Outcome of one instruction. Fuel accounting belongs to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Executed,
    /// The owner completes a clock read into EDX:EAX and advances to `next`.
    /// One instruction unit is charged; all architectural state is still intact.
    ReadTimestamp {
        pc: u32,
        next: u32,
    },
    Unsupported {
        pc: u32,
        code: Code,
    },
    FetchFault {
        pc: u32,
        address: u32,
    },
    ReadFault {
        pc: u32,
        address: u32,
    },
    WriteFault {
        pc: u32,
        address: u32,
    },
    InvalidInstruction {
        pc: u32,
    },
    /// `pc` always names the instruction. Software INT/INT3 have committed the
    /// next EIP; divide errors and UD2 retain the faulting EIP and CPU state.
    Trap {
        pc: u32,
        vector: u8,
    },
}

/// Completed instruction/REP-element units. A fault may follow completed REP
/// elements; their memory and index/count progress must not be replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Execution {
    pub step: Step,
    pub units: u32,
}

/// Per-thread REP comparison restart metadata, carried by CpuState snapshots.
/// This is executor bookkeeping, not an architectural register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepContinuation {
    pub pc: u32,
    flags: u32,
    operation: StringOp,
}

impl RepContinuation {
    pub fn fault_native_compare(state: &mut CpuState, native: u32) {
        Self::commit_native_compare(state, native, true);
        state.eflags = state.rep.take().unwrap().flags;
    }
    /// Called immediately before a native comparison chunk commits its flags.
    /// A later native/interpreted chunk belongs to the same guest instruction.
    pub fn commit_native_compare(state: &mut CpuState, native: u32, more: bool) {
        if !more {
            state.rep = None;
            return;
        }
        debug_assert!((6..18).contains(&native));
        let operation = StringOp {
            kind: if native < 12 {
                StringKind::Cmps
            } else {
                StringKind::Scas
            },
            width: match (native - 6) % 3 {
                0 => 1,
                1 => 2,
                _ => 4,
            },
            word: false,
            fs: false,
            repeat: true,
            unequal: (native - 6) % 6 >= 3,
        };
        if !state
            .rep
            .as_ref()
            .is_some_and(|r| r.pc == state.eip && r.operation == operation)
        {
            state.rep = Some(Self {
                pc: state.eip,
                flags: state.eflags,
                operation,
            });
        }
    }
}

/// Integer interpreter with a bounded cache shared by all entries in a memory.
pub struct Interpreter {
    cache: FxHashMap<u32, Cached>,
    blocks: FxHashMap<u32, DecodedBlock>,
    capacity: usize,
    decodes: u64,
    pub frame_retired: Option<u64>,
    /// Discovery and tracing observe every transfer before windows may loop.
    pub observe_transfers: bool,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Self::with_capacity(4096)
    }

    /// Zero disables caching. On saturation the cache is cleared as a batch.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: FxHashMap::default(),
            blocks: FxHashMap::default(),
            capacity,
            decodes: 0,
            frame_retired: None,
            observe_transfers: false,
        }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.blocks.clear();
    }

    /// Number of decodes, including invalid, unsupported and inaccessible code.
    pub fn decode_count(&self) -> u64 {
        self.decodes
    }

    pub fn cached_instructions(&self) -> usize {
        self.cache.len()
    }

    pub fn step(&mut self, state: &mut CpuState, memory: &mut Memory) -> Step {
        self.run_chunk(state, memory, 1).step
    }

    pub fn run_chunk(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        max_units: u32,
    ) -> Execution {
        if max_units == 0 {
            return Execution {
                step: Step::Executed,
                units: 0,
            };
        }
        let pc = state.eip;
        let decoded = if let Some(entry) = self
            .cache
            .get_mut(&pc)
            .and_then(|e| e.valid(memory).then_some(e))
        {
            entry.decoded
        } else {
            self.decodes += 1;
            let entry = fetch(memory, pc);
            let decoded = entry.decoded;
            if self.capacity != 0 {
                if self.cache.len() >= self.capacity {
                    self.cache.clear();
                }
                self.cache.insert(pc, entry);
            }
            decoded
        };
        let execution = execute_decoded(&decoded, state, memory, max_units, None);
        if let Some(retired) = &mut self.frame_retired {
            *retired += retired_instruction(&decoded, execution, state, pc);
        }
        execution
    }

    /// Execute a bounded predecoded window. This is internal to Executor:
    /// public step/run_chunk retain their single-instruction/REP contract.
    pub fn run_block(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        max_units: u32,
        stop: u32,
        native: Option<(&BTreeMap<u32, u32>, u64)>,
    ) -> (Execution, u32) {
        if max_units == 0 || state.eip == stop {
            return (
                Execution {
                    step: Step::Executed,
                    units: 0,
                },
                0,
            );
        }
        let pc = state.eip;
        let revision = native.map(|(_, revision)| revision);
        if let Some(block) = self
            .blocks
            .get_mut(&pc)
            .filter(|block| block.revision == revision)
            .and_then(|block| block.valid(memory).then_some(block))
        {
            return block.execute_observed(
                state,
                memory,
                max_units,
                stop,
                self.frame_retired.as_mut(),
                self.observe_transfers,
            );
        }
        let mut block = DecodedBlock::fetch(memory, pc, native, &mut self.decodes);
        if self.capacity == 0 {
            return block.execute_observed(
                state,
                memory,
                max_units,
                stop,
                self.frame_retired.as_mut(),
                self.observe_transfers,
            );
        }
        // Bound each independent cache in entries. A larger block working set
        // avoids flushing hot floating fallback sequences during discovery.
        if self.blocks.len() >= self.capacity {
            self.blocks.clear();
        }
        self.blocks.insert(pc, block);
        self.blocks.get_mut(&pc).unwrap().execute_observed(
            state,
            memory,
            max_units,
            stop,
            self.frame_retired.as_mut(),
            self.observe_transfers,
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MmxCache {
    Unknown,
    Coherent,
    Deferred,
}

fn execute_decoded(
    decoded: &Decoded,
    state: &mut CpuState,
    memory: &mut Memory,
    max_units: u32,
    deferred_mmx: Option<&mut MmxCache>,
) -> Execution {
    let pc = state.eip;
    if state.rep.as_ref().is_some_and(|r| r.pc != pc) {
        state.rep = None;
    }
    let debug = state.eflags & (1 << 8) != 0;
    // RF supplied by an architectural continuation is consumed when the next
    // decoded instruction starts. A fetch/decode stop has not started it yet.
    if !matches!(decoded, Decoded::Stop(_)) {
        state.eflags &= !(1 << 16);
    }
    let instruction = match decoded {
        Decoded::Ready(i) => i,
        Decoded::Stop(result) => {
            return Execution {
                step: *result,
                units: 0,
            };
        }
        Decoded::Simd(plan) => {
            let mut coherent = false;
            if let Some(cache) = deferred_mmx.as_deref()
                && *cache == MmxCache::Deferred
            {
                state.refresh_mmx();
                coherent = true;
            }
            coherent |= deferred_mmx
                .as_deref()
                .is_some_and(|c| *c == MmxCache::Coherent);
            let step = plan.execute(state, memory, coherent);
            if let Some(cache) = deferred_mmx {
                if coherent || step == Step::Executed {
                    *cache = MmxCache::Coherent;
                }
            }
            let completed = step == Step::Executed;
            return Execution {
                step: if completed && debug {
                    Step::Trap { pc, vector: 1 }
                } else {
                    step
                },
                units: u32::from(completed),
            };
        }
        Decoded::Extended(i, operation, opcode) => {
            let x87 = *operation == crate::floating::Operation::X87;
            let defer = x87 && deferred_mmx.is_some();
            if let Some(dirty) = deferred_mmx.as_deref()
                && *dirty == MmxCache::Deferred
                && !x87
            {
                // A following SIMD/MMX instruction can read the physical x87
                // aliases. Materialize the completed x87 prefix before it runs.
                state.refresh_mmx();
            }
            let step = crate::floating::execute(i, *operation, *opcode, state, memory, !defer);
            let completed = step == Step::Executed;
            if let Some(cache) = deferred_mmx {
                if completed {
                    *cache = if x87 {
                        MmxCache::Deferred
                    } else {
                        MmxCache::Coherent
                    };
                } else if !x87 && *cache == MmxCache::Deferred {
                    // The completed x87 prefix was materialized before the
                    // faulting operation; its aliases remain canonical.
                    *cache = MmxCache::Coherent;
                }
            }
            return Execution {
                step: if completed && debug {
                    Step::Trap { pc, vector: 1 }
                } else {
                    step
                },
                units: u32::from(completed),
            };
        }
    };
    if let Action::String(operation) = instruction.action {
        return strings::execute::<true>(
            state,
            memory,
            operation,
            instruction.next,
            max_units.min(256),
            debug,
        );
    }
    state.rep = None;
    if let Action::Rdtsc = instruction.action {
        return Execution {
            step: Step::ReadTimestamp {
                pc,
                next: instruction.next,
            },
            units: 1,
        };
    }
    if instruction.register_only && registers::execute(instruction, state)
        || instruction.checked_scalar && scalar::execute(instruction, state, memory).is_some()
    {
        return Execution {
            step: if debug {
                Step::Trap { pc, vector: 1 }
            } else {
                Step::Executed
            },
            units: 1,
        };
    }
    if let Action::CompareExchange8(address) = instruction.action {
        let address = address.resolve_regs(&state.regs, state.fs_base);
        let result = (|| {
            for permission in [READ, WRITE] {
                if !accessible(memory, address, 8, permission) {
                    let address = fault_address(memory, address, permission);
                    return Err(if permission == READ {
                        Step::ReadFault { pc, address }
                    } else {
                        Step::WriteFault { pc, address }
                    });
                }
            }
            let bytes = memory
                .read_data(address, 8)
                .map_err(|_| Step::ReadFault { pc, address })?;
            let old = u64::from_le_bytes(bytes.as_ref().try_into().unwrap());
            let equal = old == u64::from(state.regs[0]) | (u64::from(state.regs[2]) << 32);
            let value = if equal {
                u64::from(state.regs[3]) | (u64::from(state.regs[1]) << 32)
            } else {
                old
            };
            // The failed comparison is still a write access. Commit all eight
            // bytes before publishing registers or ZF, including COW failures.
            memory
                .write(address, &value.to_le_bytes())
                .map_err(|_| Step::WriteFault { pc, address })?;
            if !equal {
                state.regs[0] = old as u32;
                state.regs[2] = (old >> 32) as u32;
            }
            state.eflags = (state.eflags & !ZF) | if equal { ZF } else { 0 };
            state.eip = instruction.next;
            Ok(if debug {
                Step::Trap { pc, vector: 1 }
            } else {
                Step::Executed
            })
        })();
        return Execution {
            units: u32::from(result.is_ok()),
            step: result.unwrap_or_else(|step| step),
        };
    }
    let mut tx = Transaction {
        regs: state.regs,
        flags: state.eflags,
        eip: instruction.next,
        fs_base: state.fs_base,
        segments: state.segments,
        pc,
        write: None,
        more_writes: Vec::new(),
        indirect_branch: None,
    };
    let trap = match tx.execute(instruction.action, memory) {
        Ok(trap) => trap,
        Err(result) => {
            return Execution {
                step: result,
                units: 0,
            };
        }
    };
    for (address, width, value) in tx.write.into_iter().chain(tx.more_writes) {
        // Permissions, mapping and the complete byte range were validated.
        // The exclusive borrow prevents their changing between check/write.
        if memory
            .write(address, &value.to_le_bytes()[..width as usize])
            .is_err()
        {
            return Execution {
                step: Step::WriteFault { pc, address },
                units: 0,
            };
        }
    }
    state.regs = tx.regs;
    state.eflags = tx.flags;
    state.eip = tx.eip;
    if let Some(branch) = tx.indirect_branch {
        state.last_indirect_branch = branch;
    }
    Execution {
        step: trap.map_or_else(
            || {
                if debug {
                    Step::Trap { pc, vector: 1 }
                } else {
                    Step::Executed
                }
            },
            |vector| Step::Trap { pc, vector },
        ),
        units: 1,
    }
}

#[derive(Clone, Copy)]
enum Decoded {
    Ready(Predecoded),
    Extended(Instruction, crate::floating::Operation, Option<u16>),
    Simd(simd::Plan),
    Stop(Step),
}

fn retired_instruction(decoded: &Decoded, execution: Execution, state: &CpuState, pc: u32) -> u64 {
    if execution.units == 0 {
        return 0;
    }
    // REP elements are work units. The instruction retires once, at completion.
    if matches!(
        decoded,
        Decoded::Ready(Predecoded {
            action: Action::String(_),
            ..
        })
    ) {
        u64::from(
            state.eip != pc
                && matches!(
                    execution.step,
                    Step::Executed | Step::Trap { vector: 1, .. }
                ),
        )
    } else {
        1
    }
}

#[derive(Clone, Copy)]
struct Predecoded {
    action: Action,
    next: u32,
    exclusive: bool,
    register_only: bool,
    checked_scalar: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Page {
    address: u32,
    permissions: u8,
    mapped: bool,
    /// Every write to a watched code page bumps its version.
    version: u64,
}

impl Page {
    fn at(memory: &Memory, address: u32) -> Self {
        let address = address & !(PAGE_SIZE as u32 - 1);
        let index = memory.range(address, 1).ok().map(|r| r.start / PAGE_SIZE);
        Self {
            address,
            permissions: index.map_or(0, |p| memory.perms[p]),
            mapped: index.is_some_and(|p| memory.mapped[p]),
            version: index.map_or(0, |p| memory.versions[p]),
        }
    }
    /// Nothing about the page changed since it was stamped. Another view
    /// writes shared pages without bumping versions: compare their bytes.
    fn current(self, memory: &Memory) -> bool {
        self.permissions & crate::memory::SHARED == 0 && self == Self::at(memory, self.address)
    }
    fn matches(self, memory: &Memory) -> bool {
        let current = Self::at(memory, self.address);
        // Byte equality is checked by the cache owner. A version-only change
        // (e.g. data elsewhere on this executable page) must not force redecoding.
        self.permissions == current.permissions && self.mapped == current.mapped
    }
}

struct Cached {
    decoded: Decoded,
    pc: u32,
    bytes: [u8; 15],
    len: u8,
    pages: [Option<Page>; 2],
}

impl Cached {
    /// Unchanged page stamps need no byte compare: versions are private to
    /// one window, so the fetch already watched these bytes in this one. A
    /// changed or foreign stamp watches them here, then compares and restamps.
    fn valid(&mut self, memory: &mut Memory) -> bool {
        if self.pages.iter().flatten().all(|page| page.current(memory)) {
            return true;
        }
        memory.watch_code(self.pc, self.len as usize);
        if !self.revalidate(memory) {
            return false;
        }
        for page in self.pages.iter_mut().flatten() {
            *page = Page::at(memory, page.address);
        }
        true
    }

    fn revalidate(&self, memory: &Memory) -> bool {
        self.pages.iter().flatten().all(|p| p.matches(memory))
            && memory.matches(self.pc, &self.bytes[..self.len as usize])
    }
}

fn fetch_bytes<const BULK: bool>(memory: &mut Memory, pc: u32) -> (Cached, Option<u32>) {
    let mut entry = Cached {
        decoded: Decoded::Stop(Step::InvalidInstruction { pc }),
        pc,
        bytes: [0; 15],
        len: 0,
        pages: [None; 2],
    };
    let mut denied = None;
    while entry.len < 15 {
        let offset = usize::from(entry.len);
        let Some(address) = pc.checked_add(offset as u32) else {
            denied = Some(pc.wrapping_add(offset as u32));
            break;
        };
        let page = Page::at(memory, address);
        if entry.pages[0].is_none() {
            entry.pages[0] = Some(page);
        } else if entry.pages[0].unwrap().address != page.address {
            entry.pages[1] = Some(page);
        }
        if !page.mapped || page.permissions & EXEC == 0 {
            denied = Some(address);
            break;
        }
        // Preserve the existing bytewise reads of concurrently shared code.
        // Ordinary ranges require only one mapping/host-protection check per
        // page, including execute-only pages (data READ is not required here).
        let mut len = if BULK && page.permissions & crate::memory::SHARED == 0 {
            (15 - offset).min(PAGE_SIZE - address as usize % PAGE_SIZE)
        } else {
            1
        };
        if memory
            .read_into(address, &mut entry.bytes[offset..offset + len])
            .is_err()
        {
            // Retain exact prefix/fault behavior for a partially available
            // range or a host access failure, without assuming page alignment
            // of the owning Memory's end.
            if len == 1
                || memory
                    .read_into(address, &mut entry.bytes[offset..offset + 1])
                    .is_err()
            {
                denied = Some(address);
                break;
            }
            len = 1;
        }
        entry.len += len as u8;
    }
    (entry, denied)
}

fn fetch(memory: &mut Memory, pc: u32) -> Cached {
    let (mut entry, denied) = fetch_bytes::<true>(memory, pc);
    let mut decoder = Decoder::with_ip(
        32,
        &entry.bytes[..entry.len as usize],
        u64::from(pc),
        DecoderOptions::NONE,
    );
    let instruction = decoder.decode();
    // A decoded instruction depends on its own bytes; a failed decode on every
    // byte it saw.
    if !instruction.is_invalid() {
        entry.len = instruction.len() as u8;
    }
    memory.watch_code(pc, entry.len as usize);
    entry.decoded = if instruction.is_invalid() {
        if decoder.last_error() == DecoderError::NoMoreBytes
            && let Some(address) = denied
        {
            Decoded::Stop(Step::FetchFault { pc, address })
        } else {
            Decoded::Stop(Step::InvalidInstruction { pc })
        }
    } else {
        decode(&instruction, &entry.bytes[..instruction.len()])
    };
    entry
}

/// How the interpreter runs a valid `instruction` encoded as `bytes`.
fn decode(instruction: &Instruction, bytes: &[u8]) -> Decoded {
    if let Some(action) = predecode(instruction) {
        Decoded::Ready(Predecoded {
            action,
            next: instruction.next_ip32(),
            register_only: registers::eligible(action),
            checked_scalar: !instruction.has_lock_prefix() && scalar::eligible(action),
            exclusive: instruction.has_lock_prefix()
                || instruction.mnemonic() == M::Cpuid
                || instruction.mnemonic() == M::Xchg
                    && (instruction.op0_kind() == OpKind::Memory
                        || instruction.op1_kind() == OpKind::Memory),
        })
    } else if let Some(plan) = simd::Plan::decode(instruction) {
        Decoded::Simd(plan)
    } else if let Some(operation) = crate::floating::operation(instruction) {
        // Preserve the original prefix/opcode scan in the decoded entry. Its
        // code bytes share the same validation as the instruction itself.
        Decoded::Extended(*instruction, operation, x87_opcode(bytes))
    } else {
        Decoded::Stop(Step::Unsupported {
            pc: instruction.ip32(),
            code: instruction.code(),
        })
    }
}

/// The x87 opcode of an instruction's bytes: the first byte pair whose
/// first byte is D8 to DF.
fn x87_opcode(bytes: &[u8]) -> Option<u16> {
    bytes
        .windows(2)
        .find(|bytes| (0xd8..=0xdf).contains(&bytes[0]))
        .map(|bytes| ((bytes[0] as u16 & 7) << 8) | bytes[1] as u16)
}

/// One instruction translated code runs through the interpreter without
/// leaving, decoded when it is translated.
pub struct Prepared {
    decoded: Decoded,
    /// The host-double path of an x87 instruction.
    x87: Option<crate::floating::NativeX87>,
}

impl Prepared {
    /// `instruction`, encoded as `bytes`, unless the interpreter would stop
    /// at it.
    pub fn new(instruction: &Instruction, bytes: &[u8]) -> Option<Self> {
        let decoded = decode(instruction, bytes);
        let x87 = match decoded {
            Decoded::Stop(_) => return None,
            Decoded::Extended(i, crate::floating::Operation::X87, opcode) => {
                Some(crate::floating::NativeX87 {
                    instruction: i,
                    opcode,
                    fast: crate::floating::fast::classify(&i),
                })
            }
            _ => None,
        };
        Some(Self { decoded, x87 })
    }

    /// The x87 opcode (FOP) the instruction records, if it records one.
    pub fn x87_opcode(&self) -> Option<u16> {
        self.x87.as_ref().and_then(|op| op.opcode)
    }

    /// Runs the instruction at `state.eip` as `Interpreter::run_chunk`
    /// would, a REP for at most `max_units` elements.
    pub fn execute(&self, state: &mut CpuState, memory: &mut Memory, max_units: u32) -> Execution {
        if let Some(op) = &self.x87
            && state.eflags & (1 << 8) == 0
        {
            let result = match op.fast {
                Some(fast) => crate::floating::fast::execute(op, fast, state, memory),
                None => crate::floating::fast::execute_other(op, state, memory),
            };
            if result.is_some() {
                state.eflags &= !(1 << 16);
                return Execution {
                    step: Step::Executed,
                    units: 1,
                };
            }
        }
        execute_decoded(&self.decoded, state, memory, max_units, None)
    }
}

const BLOCK_INSTRUCTIONS: usize = 64;
const BLOCK_BYTES: usize = 512;

/// A contiguous decode window with local branch targets indexed by byte offset.
/// Validate bytes and all page dependencies at each entry. A store changing the
/// code epoch ends execution before the cached suffix can run.
struct DecodedBlock {
    pc: u32,
    instructions: Vec<(u32, Decoded)>,
    offsets: Vec<u8>,
    native_start: bool,
    local_branches: bool,
    bytes: Vec<u8>,
    pages: Vec<Page>,
    revision: Option<u64>,
}

impl DecodedBlock {
    /// As `Cached::valid`: watch and compare bytes only after a page changed.
    fn valid(&mut self, memory: &mut Memory) -> bool {
        if self.pages.iter().all(|page| page.current(memory)) {
            return true;
        }
        memory.watch_code(self.pc, self.bytes.len());
        if !self.revalidate(memory) {
            return false;
        }
        for page in &mut self.pages {
            *page = Page::at(memory, page.address);
        }
        true
    }

    fn revalidate(&self, memory: &Memory) -> bool {
        self.pages.iter().all(|page| page.matches(memory)) && memory.matches(self.pc, &self.bytes)
    }

    fn fetch(
        memory: &mut Memory,
        start: u32,
        native: Option<(&BTreeMap<u32, u32>, u64)>,
        decodes: &mut u64,
    ) -> Self {
        let mut block = Self {
            pc: start,
            instructions: Vec::new(),
            offsets: Vec::new(),
            native_start: native.is_some_and(|(entries, _)| entries.contains_key(&start)),
            local_branches: true,
            bytes: Vec::new(),
            pages: Vec::new(),
            revision: native.map(|(_, revision)| revision),
        };
        let mut pc = start;
        let mut furthest_target = start;
        while block.instructions.len() < BLOCK_INSTRUCTIONS && block.bytes.len() < BLOCK_BYTES {
            // Published entries already exclude forms unsupported by native code.
            // Check all forms, including SSE/MMX, only while building the block.
            // x87 fallbacks remain contiguous because they have no native entry.
            if !block.instructions.is_empty()
                && native.is_some_and(|(entries, _)| entries.contains_key(&pc))
            {
                break;
            }
            *decodes += 1;
            let entry = fetch(memory, pc);
            let shared = entry
                .pages
                .iter()
                .flatten()
                .any(|p| p.permissions & crate::memory::SHARED != 0);
            // Another view may change shared code without an owner epoch. Keep
            // its original transfer boundaries; never cache it in a local loop.
            if shared && block.local_branches && !block.instructions.is_empty() {
                break;
            }
            block.local_branches &= !shared;
            if let Decoded::Ready(instruction) = entry.decoded
                && let Action::Call(Operand::Immediate(target))
                | Action::Branch(Operand::Immediate(target), _) = instruction.action
                && target > pc
                && target.wrapping_sub(start) < BLOCK_BYTES as u32
            {
                furthest_target = furthest_target.max(target);
            }
            let (next, terminates) = match entry.decoded {
                Decoded::Ready(instruction) => (
                    instruction.next,
                    matches!(
                        instruction.action,
                        Action::Int(..)
                            | Action::Ud2
                            | Action::Privileged
                            | Action::String(..)
                            | Action::PopFlags(..)
                            | Action::Rdtsc
                    ) || match instruction.action {
                        Action::Call(Operand::Immediate(target)) => {
                            target.wrapping_sub(start) >= BLOCK_BYTES as u32
                        }
                        Action::Call(_) => true,
                        // A forward local target can follow an earlier return
                        // or backedge. Decode through it, but do not scan past
                        // the final terminator of this bounded static frontier.
                        Action::Ret(_) => pc >= furthest_target,
                        Action::Branch(Operand::Immediate(target), ConditionCode::None)
                            if target <= pc =>
                        {
                            pc >= furthest_target
                        }
                        _ => false,
                    } || !block.local_branches && instruction.action.is_transfer(),
                ),
                Decoded::Simd(plan) => (plan.next, false),
                Decoded::Extended(instruction, _, _) => (
                    instruction.next_ip32(),
                    instruction.flow_control() != iced_x86::FlowControl::Next,
                ),
                Decoded::Stop(_) => (pc, true),
            };
            let len = if next > pc {
                (next - pc) as usize
            } else {
                entry.len as usize
            };
            if !block.instructions.is_empty() && block.bytes.len() + len > BLOCK_BYTES {
                break;
            }
            block.bytes.extend_from_slice(&entry.bytes[..len]);
            for page in entry.pages.into_iter().flatten() {
                if !block.pages.iter().any(|old| old.address == page.address) {
                    block.pages.push(page);
                }
            }
            block.offsets.resize(block.bytes.len(), u8::MAX);
            if let Some(offset) = block.offsets.get_mut(pc.wrapping_sub(start) as usize) {
                *offset = block.instructions.len() as u8;
            }
            block.instructions.push((pc, entry.decoded));
            if terminates || next <= pc {
                break;
            }
            pc = next;
        }
        block
    }

    fn execute_observed(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        max_units: u32,
        stop: u32,
        retired: Option<&mut u64>,
        observe_transfers: bool,
    ) -> (Execution, u32) {
        if observe_transfers || !self.local_branches {
            self.execute_mode::<true>(state, memory, max_units, stop, retired)
        } else {
            self.execute_mode::<false>(state, memory, max_units, stop, retired)
        }
    }

    fn execute_mode<const OBSERVE: bool>(
        &self,
        state: &mut CpuState,
        memory: &mut Memory,
        max_units: u32,
        stop: u32,
        retired: Option<&mut u64>,
    ) -> (Execution, u32) {
        // Specialize the ordinary block loop: disabled diagnostics must not
        // add a retirement branch to every interpreted instruction.
        match retired {
            Some(retired) => {
                self.execute::<OBSERVE>(state, memory, max_units, stop, |d, e, s, pc| {
                    *retired += retired_instruction(d, e, s, pc)
                })
            }
            None => self.execute::<OBSERVE>(state, memory, max_units, stop, |_, _, _, _| {}),
        }
    }

    fn execute<const OBSERVE: bool>(
        &self,
        state: &mut CpuState,
        memory: &mut Memory,
        max_units: u32,
        stop: u32,
        mut retire: impl FnMut(&Decoded, Execution, &CpuState, u32),
    ) -> (Execution, u32) {
        let epoch = memory.epoch;
        let mut result = Execution {
            step: Step::Executed,
            units: 0,
        };
        let mut last_units = 0;
        let mut deferred_mmx = MmxCache::Unknown;
        let mut index = 0;
        while let Some((pc, decoded)) = self.instructions.get(index) {
            let pc = *pc;
            if result.units == max_units || state.eip != pc || pc == stop {
                break;
            }
            let execution = if let Decoded::Ready(instruction) = decoded
                && !instruction.exclusive
                && state.rep.is_none()
                && state.eflags & 0x100 == 0
                && (instruction.register_only && registers::execute(instruction, state)
                    || instruction.checked_scalar
                        && scalar::execute(instruction, state, memory).is_some())
            {
                Execution {
                    step: Step::Executed,
                    units: 1,
                }
            } else {
                execute_decoded(
                    decoded,
                    state,
                    memory,
                    max_units - result.units,
                    Some(&mut deferred_mmx),
                )
            };
            retire(decoded, execution, state, pc);
            result.units += execution.units;
            result.step = execution.step;
            last_units = execution.units;
            // Stores can patch this block's next instruction. A flags change can
            // enable single stepping, and floating exceptions remain restartable.
            if execution.step != Step::Executed
                || memory.epoch != epoch
                || state.eflags & 0x100 != 0
                // Leave room for a full straight-line segment. Prefer a branch
                // boundary for the owner's next interrupt/deadline poll, while
                // retaining the hard instruction/REP allowance above.
                || (OBSERVE || result.units >= max_units.saturating_sub(BLOCK_INSTRUCTIONS as u32))
                    && matches!(decoded, Decoded::Ready(i) if i.action.is_transfer())
            {
                break;
            }
            if self
                .instructions
                .get(index + 1)
                .is_some_and(|&(next, _)| state.eip == next)
            {
                index += 1;
            } else {
                // Transfers may stay only at decoded instruction boundaries;
                // REP, published native entries and owner operations still exit.
                if !matches!(decoded, Decoded::Ready(i) if i.action.is_transfer())
                    || !self.local_branches
                    || self.native_start && state.eip == self.pc
                {
                    break;
                }
                let Some(&next) = self
                    .offsets
                    .get(state.eip.wrapping_sub(self.pc) as usize)
                    .filter(|&&next| next != u8::MAX)
                else {
                    break;
                };
                index = usize::from(next);
            }
        }
        if deferred_mmx == MmxCache::Deferred {
            // Every public/context/native boundary still exposes a coherent
            // MMX cache; only consecutive x87 instructions skip redundant work.
            state.refresh_mmx();
        }
        (result, last_units)
    }
}

#[derive(Clone, Copy)]
struct Reg {
    index: u8,
    shift: u8,
    width: u8,
}

impl Reg {
    fn decode(r: R) -> Option<Self> {
        let number = r as u32;
        let (index, shift, width) = if (R::EAX as u32..=R::EDI as u32).contains(&number) {
            (number - R::EAX as u32, 0, 4)
        } else if (R::AX as u32..=R::DI as u32).contains(&number) {
            (number - R::AX as u32, 0, 2)
        } else if (R::AL as u32..=R::BL as u32).contains(&number) {
            (number - R::AL as u32, 0, 1)
        } else if (R::AH as u32..=R::BH as u32).contains(&number) {
            (number - R::AH as u32, 8, 1)
        } else {
            return None;
        };
        Some(Self {
            index: index as u8,
            shift,
            width,
        })
    }
    fn read(self, regs: &[u32; 8]) -> u32 {
        (regs[self.index as usize] >> self.shift) & mask(self.width)
    }
    fn write(self, regs: &mut [u32; 8], value: u32) {
        let mask = mask(self.width) << self.shift;
        let slot = &mut regs[self.index as usize];
        *slot = (*slot & !mask) | ((value << self.shift) & mask);
    }
}

#[derive(Clone, Copy)]
struct Address {
    base: Option<Reg>,
    index: Option<Reg>,
    scale: u8,
    displacement: u32,
    word: bool,
    fs: bool,
}

impl Address {
    fn decode(i: &Instruction, lea: bool) -> Option<Self> {
        let base = if i.memory_base() == R::None {
            None
        } else {
            Some(Reg::decode(i.memory_base())?)
        };
        let index = if i.memory_index() == R::None {
            None
        } else {
            Some(Reg::decode(i.memory_index())?)
        };
        // XLAT's implicit address is the sole byte-indexed form: iced supplies
        // BX/EBX + unsigned AL. The usual address-width wrap still precedes FS.
        if base.is_some_and(|r| r.width == 1)
            || (index.is_some_and(|r| r.width == 1)
                && !(i.code() == Code::Xlat_m8 && i.memory_index() == R::AL))
        {
            return None;
        }
        if !lea && !matches!(i.memory_segment(), R::DS | R::SS | R::ES | R::CS | R::FS) {
            return None;
        }
        // In a displacement-only address the decoder retains the address width
        // in the encoded displacement size (16 or 32 bits).
        let word = base
            .or(index)
            .map_or(i.memory_displ_size() == 2, |r| r.width == 2);
        Some(Self {
            base,
            index,
            scale: i.memory_index_scale() as u8,
            displacement: i.memory_displacement32(),
            word,
            fs: !lea && i.memory_segment() == R::FS,
        })
    }
    fn resolve(self, tx: &Transaction) -> u32 {
        self.resolve_regs(&tx.regs, tx.fs_base)
    }
    fn resolve_regs(self, regs: &[u32; 8], fs_base: u32) -> u32 {
        let mut offset = self.displacement;
        if let Some(r) = self.base {
            offset = offset.wrapping_add(r.read(regs));
        }
        if let Some(r) = self.index {
            offset = offset.wrapping_add(r.read(regs).wrapping_mul(u32::from(self.scale)));
        }
        if self.word {
            offset &= 0xffff;
        }
        offset.wrapping_add(if self.fs { fs_base } else { 0 })
    }
}

#[derive(Clone, Copy)]
enum Operand {
    Register(Reg),
    Memory(Address, u8),
    Immediate(u32),
}

impl Operand {
    fn decode(i: &Instruction, n: u32) -> Option<Self> {
        Some(match i.op_kind(n) {
            OpKind::Register => Self::Register(Reg::decode(i.op_register(n))?),
            OpKind::Memory => {
                let width = i.memory_size().size();
                if !matches!(width, 1 | 2 | 4) {
                    return None;
                }
                Self::Memory(Address::decode(i, false)?, width as u8)
            }
            OpKind::Immediate8 => Self::Immediate(u32::from(i.immediate8())),
            OpKind::Immediate16 => Self::Immediate(u32::from(i.immediate16())),
            OpKind::Immediate32 => Self::Immediate(i.immediate32()),
            OpKind::Immediate8to16 => Self::Immediate(i.immediate8to16() as u32),
            OpKind::Immediate8to32 => Self::Immediate(i.immediate8to32() as u32),
            OpKind::NearBranch32 => Self::Immediate(i.near_branch32()),
            OpKind::NearBranch16 => Self::Immediate(u32::from(i.near_branch16())),
            _ => return None,
        })
    }
    fn width(self) -> Option<u8> {
        match self {
            Self::Register(r) => Some(r.width),
            Self::Memory(_, width) => Some(width),
            Self::Immediate(_) => None,
        }
    }
    fn writable(self) -> bool {
        !matches!(self, Self::Immediate(_))
    }
}

#[derive(Clone, Copy)]
enum Action {
    Nop,
    Cpuid,
    Rdtsc,
    ReadSegment(Operand, usize),
    StoreDescriptorTable(Address, bool),
    StoreLdt(Operand),
    String(StringOp),
    Mov(Operand, Operand, bool),
    Lea(Reg, Address),
    Binary(M, Operand, Operand),
    Unary(M, Operand),
    Shift(M, Operand, Operand),
    DoubleShift(M, Operand, Operand, Operand),
    Bit(M, Operand, Operand, bool),
    Scan(M, Operand, Operand),
    Bswap(Reg),
    Xchg(Operand, Operand),
    Xadd(Operand, Operand),
    CompareExchange(Operand, Operand),
    CompareExchange8(Address),
    Multiply(Operand, Option<Operand>, Option<Operand>, bool),
    Divide(Operand, bool),
    Branch(Operand, ConditionCode),
    Set(Operand, ConditionCode),
    Cmov(Operand, Operand, ConditionCode),
    Push(Operand, u8),
    Pop(Operand, u8),
    PushAll(u8),
    PopAll(u8),
    PushFlags(u8),
    PopFlags(u8),
    Loop(M, Operand, u8),
    Call(Operand),
    Ret(u16),
    Leave(u8),
    Sign(M),
    Flags(M),
    Int(u8),
    Ud2,
    Privileged,
}

impl Action {
    fn is_transfer(self) -> bool {
        matches!(
            self,
            Self::Branch(..) | Self::Loop(..) | Self::Call(..) | Self::Ret(..)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringKind {
    Movs,
    Stos,
    Cmps,
    Scas,
    Lods,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StringOp {
    kind: StringKind,
    width: u8,
    word: bool,
    fs: bool,
    repeat: bool,
    unequal: bool,
}

fn string_operation(i: &Instruction) -> Option<StringOp> {
    use StringKind::*;
    let (kind, width) = match i.code() {
        Code::Movsb_m8_m8 => (Movs, 1),
        Code::Movsw_m16_m16 => (Movs, 2),
        Code::Movsd_m32_m32 => (Movs, 4),
        Code::Stosb_m8_AL => (Stos, 1),
        Code::Stosw_m16_AX => (Stos, 2),
        Code::Stosd_m32_EAX => (Stos, 4),
        Code::Cmpsb_m8_m8 => (Cmps, 1),
        Code::Cmpsw_m16_m16 => (Cmps, 2),
        Code::Cmpsd_m32_m32 => (Cmps, 4),
        Code::Scasb_AL_m8 => (Scas, 1),
        Code::Scasw_AX_m16 => (Scas, 2),
        Code::Scasd_EAX_m32 => (Scas, 4),
        Code::Lodsb_AL_m8 => (Lods, 1),
        Code::Lodsw_AX_m16 => (Lods, 2),
        Code::Lodsd_EAX_m32 => (Lods, 4),
        _ => return None,
    };
    if i.has_lock_prefix()
        || !matches!(
            i.segment_prefix(),
            R::None | R::CS | R::DS | R::ES | R::SS | R::FS
        )
    {
        return None;
    }
    let word =
        (0..i.op_count()).any(|n| matches!(i.op_kind(n), OpKind::MemorySegSI | OpKind::MemoryESDI));
    Some(StringOp {
        kind,
        width,
        word,
        fs: i.segment_prefix() == R::FS,
        repeat: i.has_rep_prefix() || i.has_repne_prefix(),
        unequal: i.has_repne_prefix(),
    })
}

fn same_width(dst: Operand, src: Operand) -> bool {
    src.width().is_none_or(|w| dst.width() == Some(w))
}

fn predecode(i: &Instruction) -> Option<Action> {
    if let Some(operation) = string_operation(i) {
        return Some(Action::String(operation));
    }
    if i.has_lock_prefix()
        && !(i.op0_kind() == OpKind::Memory
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
                    | M::Cmpxchg8b
                    | M::Bts
                    | M::Btr
                    | M::Btc
            ))
    {
        return None;
    }
    let op = |n| Operand::decode(i, n);
    let pair = || {
        if i.op_count() != 2 {
            return None;
        }
        let (dst, src) = (op(0)?, op(1)?);
        (dst.writable() && same_width(dst, src)).then_some((dst, src))
    };
    Some(match i.mnemonic() {
        M::Nop | M::Pause => Action::Nop,
        M::Cpuid => Action::Cpuid,
        M::Rdtsc => Action::Rdtsc,
        M::Xlatb => Action::Mov(Operand::Register(Reg::decode(R::AL)?), op(0)?, false),
        M::Sgdt | M::Sidt if i.op0_kind() == OpKind::Memory => {
            Action::StoreDescriptorTable(Address::decode(i, false)?, i.mnemonic() == M::Sidt)
        }
        M::Mov
            if matches!(
                i.op1_register(),
                R::ES | R::CS | R::SS | R::DS | R::FS | R::GS
            ) =>
        {
            let index = match i.op1_register() {
                R::ES => 0,
                R::CS => 1,
                R::SS => 2,
                R::DS => 3,
                R::FS => 4,
                _ => 5,
            };
            Action::ReadSegment(op(0)?, index)
        }
        M::Mov => {
            let (dst, src) = pair()?;
            Action::Mov(dst, src, false)
        }
        M::Movsx | M::Movzx => {
            let (dst, src) = (op(0)?, op(1)?);
            // 66 0F B7/BF word->word forms preserve the destination's upper
            // half and flags, as observed on 386EX and current WoW64 hardware.
            if !matches!(dst, Operand::Register(_)) || src.width()? > dst.width()? {
                return None;
            }
            Action::Mov(dst, src, i.mnemonic() == M::Movsx)
        }
        M::Lea => {
            if i.op1_kind() != OpKind::Memory {
                return None;
            }
            Action::Lea(Reg::decode(i.op0_register())?, Address::decode(i, true)?)
        }
        M::Add | M::Adc | M::Sub | M::Sbb | M::And | M::Or | M::Xor | M::Cmp | M::Test => {
            let (dst, src) = pair()?;
            Action::Binary(i.mnemonic(), dst, src)
        }
        M::Inc | M::Dec | M::Not | M::Neg => {
            let dst = op(0)?;
            if !dst.writable() {
                return None;
            }
            Action::Unary(i.mnemonic(), dst)
        }
        M::Sal | M::Shl | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr => {
            let (dst, count) = (op(0)?, op(1)?);
            if !dst.writable()
                || !(count.width().is_none()
                    || matches!(
                        count,
                        Operand::Register(Reg {
                            index: 1,
                            shift: 0,
                            width: 1
                        })
                    ))
            {
                return None;
            }
            // Iced distinguishes the group-2 /6 SAL encoding from /4 SHL.
            // They share the same widths, count masking and flag semantics.
            let kind = if i.mnemonic() == M::Sal {
                M::Shl
            } else {
                i.mnemonic()
            };
            Action::Shift(kind, dst, count)
        }
        M::Xchg => {
            let (a, b) = pair()?;
            if !b.writable() {
                return None;
            }
            Action::Xchg(a, b)
        }
        M::Cmpxchg8b if i.op0_kind() == OpKind::Memory => {
            Action::CompareExchange8(Address::decode(i, false)?)
        }
        M::Xadd | M::Cmpxchg => {
            let (dst, src) = pair()?;
            if !matches!(src, Operand::Register(_)) {
                return None;
            }
            if i.mnemonic() == M::Xadd {
                Action::Xadd(dst, src)
            } else {
                Action::CompareExchange(dst, src)
            }
        }
        M::Shld | M::Shrd => {
            let (dst, src, count) = (op(0)?, op(1)?, op(2)?);
            if !dst.writable() || !matches!(src, Operand::Register(_)) || !same_width(dst, src) {
                return None;
            }
            Action::DoubleShift(i.mnemonic(), dst, src, count)
        }
        M::Bt | M::Bts | M::Btr | M::Btc => {
            let (dst, index) = (op(0)?, op(1)?);
            if !dst.writable() {
                return None;
            }
            Action::Bit(i.mnemonic(), dst, index, i.op1_kind() == OpKind::Immediate8)
        }
        M::Bsf | M::Bsr => {
            let (dst, src) = pair()?;
            if !matches!(dst, Operand::Register(_)) {
                return None;
            }
            Action::Scan(i.mnemonic(), dst, src)
        }
        M::Bswap => {
            let reg = Reg::decode(i.op0_register())?;
            if !matches!(reg.width, 2 | 4) {
                return None;
            }
            Action::Bswap(reg)
        }
        M::Mul | M::Imul => {
            if i.op_count() == 1 {
                Action::Multiply(op(0)?, None, None, i.mnemonic() == M::Imul)
            } else if i.mnemonic() == M::Imul && matches!(i.op_count(), 2 | 3) {
                let (dst, src) = (op(0)?, op(1)?);
                if !matches!(dst, Operand::Register(_))
                    || !same_width(dst, src)
                    || dst.width()? == 1
                {
                    return None;
                }
                Action::Multiply(
                    dst,
                    Some(src),
                    if i.op_count() == 3 {
                        Some(op(2)?)
                    } else {
                        None
                    },
                    true,
                )
            } else {
                return None;
            }
        }
        M::Div | M::Idiv => Action::Divide(op(0)?, i.mnemonic() == M::Idiv),
        M::Jmp => {
            if !matches!(
                i.code(),
                Code::Jmp_rel8_32 | Code::Jmp_rel32_32 | Code::Jmp_rm32
            ) {
                return None;
            }
            Action::Branch(op(0)?, ConditionCode::None)
        }
        M::Je
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
        | M::Jnp => Action::Branch(op(0)?, i.condition_code()),
        M::Sete
        | M::Setne
        | M::Seta
        | M::Setae
        | M::Setb
        | M::Setbe
        | M::Setg
        | M::Setge
        | M::Setl
        | M::Setle
        | M::Seto
        | M::Setno
        | M::Sets
        | M::Setns
        | M::Setp
        | M::Setnp => Action::Set(op(0)?, i.condition_code()),
        M::Cmove
        | M::Cmovne
        | M::Cmova
        | M::Cmovae
        | M::Cmovb
        | M::Cmovbe
        | M::Cmovg
        | M::Cmovge
        | M::Cmovl
        | M::Cmovle
        | M::Cmovo
        | M::Cmovno
        | M::Cmovs
        | M::Cmovns
        | M::Cmovp
        | M::Cmovnp => {
            let (dst, src) = pair()?;
            if !matches!(dst, Operand::Register(_)) {
                return None;
            }
            Action::Cmov(dst, src, i.condition_code())
        }
        M::Push
            if matches!(
                i.code(),
                Code::Push_r16
                    | Code::Push_rm16
                    | Code::Push_imm16
                    | Code::Pushw_imm8
                    | Code::Push_r32
                    | Code::Push_rm32
                    | Code::Pushd_imm32
                    | Code::Pushd_imm8
            ) =>
        {
            Action::Push(
                op(0)?,
                if matches!(
                    i.code(),
                    Code::Push_r16 | Code::Push_rm16 | Code::Push_imm16 | Code::Pushw_imm8
                ) {
                    2
                } else {
                    4
                },
            )
        }
        M::Pop
            if matches!(
                i.code(),
                Code::Pop_r16 | Code::Pop_rm16 | Code::Pop_r32 | Code::Pop_rm32
            ) =>
        {
            Action::Pop(
                op(0)?,
                if matches!(i.code(), Code::Pop_r16 | Code::Pop_rm16) {
                    2
                } else {
                    4
                },
            )
        }
        M::Pushad => Action::PushAll(4),
        M::Pusha => Action::PushAll(2),
        M::Popad => Action::PopAll(4),
        M::Popa => Action::PopAll(2),
        M::Pushfd => Action::PushFlags(4),
        M::Pushf => Action::PushFlags(2),
        M::Popfd => Action::PopFlags(4),
        M::Popf => Action::PopFlags(2),
        M::Loop | M::Loope | M::Loopne | M::Jcxz | M::Jecxz => {
            let word = matches!(
                i.code(),
                Code::Loop_rel8_16_CX
                    | Code::Loop_rel8_32_CX
                    | Code::Loope_rel8_16_CX
                    | Code::Loope_rel8_32_CX
                    | Code::Loopne_rel8_16_CX
                    | Code::Loopne_rel8_32_CX
                    | Code::Jcxz_rel8_16
                    | Code::Jcxz_rel8_32
            );
            Action::Loop(i.mnemonic(), op(0)?, if word { 2 } else { 4 })
        }
        M::Call if matches!(i.code(), Code::Call_rel32_32 | Code::Call_rm32) => {
            Action::Call(op(0)?)
        }
        M::Ret if matches!(i.code(), Code::Retnd | Code::Retnd_imm16) => {
            Action::Ret(if i.op_count() == 0 {
                0
            } else {
                i.immediate16()
            })
        }
        M::Leave if matches!(i.code(), Code::Leaved | Code::Leavew) => {
            Action::Leave(if i.code() == Code::Leavew { 2 } else { 4 })
        }
        M::Cdq | M::Cwd | M::Cbw | M::Cwde => Action::Sign(i.mnemonic()),
        M::Clc | M::Stc | M::Cmc | M::Cld | M::Std | M::Lahf | M::Sahf => {
            Action::Flags(i.mnemonic())
        }
        M::Int => Action::Int(i.immediate8()),
        M::Int3 => Action::Int(3),
        M::Sldt => Action::StoreLdt(op(0)?),
        M::Ud2 => Action::Ud2,
        // The flat Windows profile is CPL3 with IOPL0: neither instruction
        // changes IF or retires before delivering the general protection fault.
        M::Cli | M::Sti => Action::Privileged,
        _ => return None,
    })
}

struct Transaction {
    regs: [u32; 8],
    flags: u32,
    eip: u32,
    fs_base: u32,
    segments: crate::SegmentState,
    pc: u32,
    write: Option<(u32, u8, u32)>,
    more_writes: Vec<(u32, u8, u32)>,
    indirect_branch: Option<crate::IndirectBranch>,
}

/// The address a memory fault reports for an access starting at `start`.
/// Hardware names the first inaccessible byte (measured on Intel): an access
/// that starts on a permitted page and crosses into a denied one faults at
/// the page boundary, with nothing written. No access spans more than two
/// pages, so the start and its neighbouring page decide it without the width.
pub fn fault_address(memory: &Memory, start: u32, permission: u8) -> u32 {
    let next = (start | (PAGE_SIZE as u32 - 1)).wrapping_add(1);
    if !(start as usize).is_multiple_of(PAGE_SIZE)
        && accessible(memory, start, 1, permission)
        && !accessible(memory, next, 1, permission)
    {
        next
    } else {
        start
    }
}

pub fn accessible(memory: &Memory, address: u32, width: u8, permission: u8) -> bool {
    memory
        .committed(address, width as usize)
        .is_ok_and(|range| {
            memory.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
                .iter()
                .all(|p| p & permission != 0)
        })
}

impl Transaction {
    fn read_mem(&self, memory: &Memory, address: u32, width: u8) -> Result<u32, Step> {
        if !accessible(memory, address, width, READ) {
            return Err(Step::ReadFault {
                pc: self.pc,
                address,
            });
        }
        let mut bytes = [0; 4];
        bytes[..width as usize].copy_from_slice(
            &memory
                .read_data(address, width as usize)
                .map_err(|_| Step::ReadFault {
                    pc: self.pc,
                    address,
                })?,
        );
        Ok(u32::from_le_bytes(bytes))
    }
    fn read(&self, memory: &Memory, operand: Operand) -> Result<u32, Step> {
        match operand {
            Operand::Immediate(value) => Ok(value),
            Operand::Register(r) => Ok(r.read(&self.regs)),
            Operand::Memory(a, width) => self.read_mem(memory, a.resolve(self), width),
        }
    }
    fn check_write(&self, memory: &Memory, operand: Operand) -> Result<(), Step> {
        if let Operand::Memory(a, width) = operand {
            let address = a.resolve(self);
            if !accessible(memory, address, width, WRITE) {
                return Err(Step::WriteFault {
                    pc: self.pc,
                    address,
                });
            }
        }
        Ok(())
    }
    fn store(&mut self, memory: &Memory, operand: Operand, value: u32) -> Result<(), Step> {
        self.check_write(memory, operand)?;
        match operand {
            Operand::Register(r) => r.write(&mut self.regs, value),
            Operand::Memory(a, width) => {
                let write = (a.resolve(self), width, value);
                if self.write.is_none() {
                    self.write = Some(write);
                } else {
                    self.more_writes.push(write);
                }
            }
            Operand::Immediate(_) => unreachable!("validated writable operand"),
        }
        Ok(())
    }
    fn stack_operand(address: u32) -> Operand {
        Self::stack_operand_width(address, 4)
    }
    fn stack_operand_width(address: u32, width: u8) -> Operand {
        Operand::Memory(
            Address {
                base: None,
                index: None,
                scale: 1,
                displacement: address,
                word: false,
                fs: false,
            },
            width,
        )
    }
    fn flag(&mut self, bit: u32, value: bool) {
        self.flags = (self.flags & !bit) | if value { bit } else { 0 };
    }
    fn szp(&mut self, value: u32, width: u8) {
        registers::szp(&mut self.flags, value, width);
    }
    fn arithmetic(&mut self, a: u32, b: u32, carry: u32, width: u8, subtract: bool) -> u32 {
        registers::arithmetic(&mut self.flags, a, b, carry, width, subtract)
    }
    fn execute(&mut self, action: Action, memory: &Memory) -> Result<Option<u8>, Step> {
        match action {
            Action::Rdtsc => unreachable!("clock reads complete at the owner boundary"),
            Action::String(_) => unreachable!("REP progress commits per element"),
            Action::Nop => {}
            Action::Cpuid => {
                let [eax, ebx, ecx, edx] = crate::cpu_profile::cpuid(self.regs[0], self.regs[1]);
                self.regs[0] = eax;
                self.regs[3] = ebx;
                self.regs[1] = ecx;
                self.regs[2] = edx;
            }
            Action::ReadSegment(dst, index) => {
                self.store(memory, dst, u32::from(self.segments.selectors[index]))?;
            }
            Action::StoreLdt(dst) => {
                // UMIP is absent from the virtual CPU profile. SLDT is a
                // read-only query: register width follows the operand while
                // memory always receives the two-byte selector.
                self.store(memory, dst, u32::from(self.segments.ldt_selector))?;
            }
            Action::StoreDescriptorTable(address, interrupt) => {
                // On the virtual post-286 CPU, both operand sizes store all
                // six bytes. Address-size wrapping and FS still apply normally.
                let (base, limit) = if interrupt {
                    (self.segments.idt_base, self.segments.idt_limit)
                } else {
                    (self.segments.gdt_base, self.segments.gdt_limit)
                };
                let address = address.resolve(self);
                if u64::from(address) + 6 > 1 << 32 {
                    return Err(Step::WriteFault {
                        pc: self.pc,
                        address,
                    });
                }
                self.store(
                    memory,
                    Self::stack_operand_width(address, 2),
                    u32::from(limit),
                )?;
                self.store(memory, Self::stack_operand(address + 2), base)?;
            }
            Action::Mov(dst, src, sign_extend) => {
                let value = self.read(memory, src)?;
                let value = if sign_extend {
                    signed(value, src.width().unwrap()) as u32
                } else {
                    value
                };
                self.store(memory, dst, value)?;
            }
            Action::Lea(dst, a) => {
                let address = a.resolve(self);
                dst.write(&mut self.regs, address);
            }
            Action::Binary(kind, dst, src) => {
                let (a, b) = (self.read(memory, dst)?, self.read(memory, src)?);
                let width = dst.width().unwrap();
                let value = registers::binary(&mut self.flags, kind, a, b, width);
                if !matches!(kind, M::Cmp | M::Test) {
                    self.store(memory, dst, value)?;
                }
            }
            Action::Unary(kind, dst) => {
                let value = self.read(memory, dst)?;
                let width = dst.width().unwrap();
                let result = registers::unary(&mut self.flags, kind, value, width);
                self.store(memory, dst, result)?;
            }
            Action::Shift(kind, dst, count) => {
                let value = self.read(memory, dst)?;
                let count = self.read(memory, count)? & 31;
                self.check_write(memory, dst)?;
                if count != 0 {
                    let result =
                        registers::shift(&mut self.flags, kind, value, count, dst.width().unwrap());
                    self.store(memory, dst, result)?;
                }
            }
            Action::DoubleShift(kind, dst, src, count) => {
                let value = self.read(memory, dst)?;
                let source = self.read(memory, src)?;
                let count = self.read(memory, count)? & 31;
                self.check_write(memory, dst)?;
                if count != 0 {
                    let width = dst.width().unwrap();
                    let bits = u32::from(width) * 8;
                    let concat = if kind == M::Shld {
                        (u64::from(value) << bits) | u64::from(source)
                    } else {
                        (u64::from(source) << bits) | u64::from(value)
                    };
                    // 16-bit counts 17..31 are architecturally undefined. Use
                    // the conventional 32-bit concatenation rotation, including
                    // the original destination, rather than host shift UB.
                    let result = if width == 2 && kind == M::Shld {
                        (concat as u32).rotate_left(count) >> 16
                    } else if width == 2 {
                        (concat as u32).rotate_right(count)
                    } else if kind == M::Shld {
                        ((concat << count) >> bits) as u32
                    } else {
                        (concat >> count) as u32
                    } & mask(width);
                    self.flag(
                        CF,
                        concat
                            & (1u64
                                << if kind == M::Shld {
                                    bits * 2 - count
                                } else {
                                    count - 1
                                })
                            != 0,
                    );
                    if count == 1 {
                        self.flag(OF, ((value ^ result) >> (bits - 1)) & 1 != 0);
                    }
                    self.szp(result, width);
                    self.store(memory, dst, result)?;
                }
            }
            Action::Bit(kind, dst, index, immediate) => {
                let width = dst.width().unwrap();
                let bits = i64::from(width) * 8;
                let index = self.read(memory, index)?;
                let bit = index & (bits as u32 - 1);
                let dst = match dst {
                    Operand::Memory(address, width) if !immediate => {
                        let linear = address.resolve(self).wrapping_add(
                            (signed(index, width).div_euclid(bits) * i64::from(width)) as u32,
                        );
                        Operand::Memory(
                            Address {
                                base: None,
                                index: None,
                                scale: 1,
                                displacement: linear,
                                word: false,
                                fs: false,
                            },
                            width,
                        )
                    }
                    other => other,
                };
                let value = self.read(memory, dst)?;
                self.flag(CF, value & (1 << bit) != 0);
                match kind {
                    M::Bts => self.store(memory, dst, value | (1 << bit))?,
                    M::Btr => self.store(memory, dst, value & !(1 << bit))?,
                    M::Btc => self.store(memory, dst, value ^ (1 << bit))?,
                    _ => {}
                }
            }
            Action::Scan(kind, dst, src) => {
                let value = self.read(memory, src)?;
                self.flag(ZF, value == 0);
                if value != 0 {
                    self.store(
                        memory,
                        dst,
                        if kind == M::Bsf {
                            value.trailing_zeros()
                        } else {
                            31 - value.leading_zeros()
                        },
                    )?;
                }
            }
            Action::Bswap(reg) => {
                // 66 0F C8+r has an architecturally undefined result, not a
                // specified 16-bit byte swap. Our guest compatibility policy
                // follows observed Intel execution: clear the low word,
                // preserve the upper word and all flags. Do not delegate this
                // undefined host instruction on x64 or invent REV16 on ARM.
                let value = if reg.width == 2 {
                    0
                } else {
                    reg.read(&self.regs).swap_bytes()
                };
                reg.write(&mut self.regs, value);
            }
            Action::Xchg(a, b) => {
                let av = self.read(memory, a)?;
                let bv = self.read(memory, b)?;
                // Resolve any memory operand before ESP or another address
                // register is exchanged. x86 has at most one memory operand.
                let a = freeze(a, self);
                let b = freeze(b, self);
                self.check_write(memory, a)?;
                self.check_write(memory, b)?;
                self.store(memory, a, bv)?;
                self.store(memory, b, av)?;
            }
            Action::Xadd(dst, src) => {
                let a = self.read(memory, dst)?;
                let b = self.read(memory, src)?;
                let dst = freeze(dst, self);
                let value = self.arithmetic(a, b, 0, dst.width().unwrap(), false);
                self.store(memory, src, a)?;
                self.store(memory, dst, value)?;
            }
            Action::CompareExchange8(_) => unreachable!("handled before scalar transactions"),
            Action::CompareExchange(dst, src) => {
                let value = self.read(memory, dst)?;
                let source = self.read(memory, src)?;
                let dst = freeze(dst, self);
                self.check_write(memory, dst)?;
                let width = dst.width().unwrap();
                let accumulator = Reg {
                    index: 0,
                    shift: 0,
                    width,
                };
                let old = accumulator.read(&self.regs);
                self.arithmetic(old, value, 0, width, true);
                if old == value {
                    self.store(memory, dst, source)?;
                } else {
                    if matches!(dst, Operand::Memory(..)) {
                        self.store(memory, dst, value)?;
                    }
                    accumulator.write(&mut self.regs, value);
                }
            }
            Action::Multiply(dst, src, immediate, signed) => {
                self.multiply(memory, dst, src, immediate, signed)?
            }
            Action::Divide(src, signed) => self.divide(memory, src, signed)?,
            Action::Branch(target, condition) => {
                if condition == ConditionCode::None || condition_true(condition, self.flags) {
                    self.eip = self.read(memory, target)?;
                    if !matches!(target, Operand::Immediate(_)) {
                        self.indirect_branch = Some(crate::IndirectBranch {
                            source: self.pc,
                            target: self.eip,
                            kind: crate::IndirectBranchKind::Jump,
                        });
                    }
                }
            }
            Action::Set(dst, condition) => self.store(
                memory,
                dst,
                u32::from(condition_true(condition, self.flags)),
            )?,
            Action::Cmov(dst, src, condition) => {
                // CMOV reads its source even when the condition is false.
                let value = self.read(memory, src)?;
                if condition_true(condition, self.flags) {
                    self.store(memory, dst, value)?;
                }
            }
            Action::Push(src, width) => {
                let value = self.read(memory, src)?;
                let address = self.regs[4].wrapping_sub(u32::from(width));
                self.store(memory, Self::stack_operand_width(address, width), value)?;
                self.regs[4] = address;
            }
            Action::Pop(dst, width) => {
                let value = self.read_mem(memory, self.regs[4], width)?;
                // POP [esp] resolves its destination after the stack increment;
                // POP ESP overwrites that increment with the popped value.
                self.regs[4] = self.regs[4].wrapping_add(u32::from(width));
                self.store(memory, dst, value)?;
            }
            Action::PushAll(width) => {
                let original = self.regs;
                for value in original {
                    self.regs[4] = self.regs[4].wrapping_sub(u32::from(width));
                    self.store(
                        memory,
                        Self::stack_operand_width(self.regs[4], width),
                        value,
                    )?;
                }
            }
            Action::PopAll(width) => {
                let esp = self.regs[4];
                for (offset, index) in [7u8, 6, 5, 4, 3, 2, 1, 0].into_iter().enumerate() {
                    if index != 4 {
                        let value = self.read_mem(
                            memory,
                            esp.wrapping_add(offset as u32 * u32::from(width)),
                            width,
                        )?;
                        Reg {
                            index,
                            shift: 0,
                            width,
                        }
                        .write(&mut self.regs, value);
                    }
                }
                self.regs[4] = esp.wrapping_add(8 * u32::from(width));
            }
            Action::PushFlags(width) => {
                self.regs[4] = self.regs[4].wrapping_sub(u32::from(width));
                self.store(
                    memory,
                    Self::stack_operand_width(self.regs[4], width),
                    self.flags & !0x30000,
                )?;
            }
            Action::PopFlags(width) => {
                let value = self.read_mem(memory, self.regs[4], width)?;
                // The flat Win32 guest runs at CPL3: POPF cannot alter IF/IOPL.
                let writable = 0x24_4dd5 & mask(width);
                self.flags = (self.flags & !writable) | (value & writable) | 2;
                self.regs[4] = self.regs[4].wrapping_add(u32::from(width));
            }
            Action::Loop(kind, target, width) => {
                let counter = Reg {
                    index: 1,
                    shift: 0,
                    width,
                };
                let value = if matches!(kind, M::Jcxz | M::Jecxz) {
                    counter.read(&self.regs)
                } else {
                    let value = counter.read(&self.regs).wrapping_sub(1) & mask(width);
                    counter.write(&mut self.regs, value);
                    value
                };
                let taken = match kind {
                    M::Jcxz | M::Jecxz => value == 0,
                    M::Loope => value != 0 && self.flags & ZF != 0,
                    M::Loopne => value != 0 && self.flags & ZF == 0,
                    _ => value != 0,
                };
                if taken {
                    self.eip = self.read(memory, target)?;
                }
            }
            Action::Call(operand) => {
                let target = self.read(memory, operand)?;
                let address = self.regs[4].wrapping_sub(4);
                self.store(memory, Self::stack_operand(address), self.eip)?;
                self.regs[4] = address;
                self.eip = target;
                if !matches!(operand, Operand::Immediate(_)) {
                    self.indirect_branch = Some(crate::IndirectBranch {
                        source: self.pc,
                        target,
                        kind: crate::IndirectBranchKind::Call,
                    });
                }
            }
            Action::Ret(bytes) => {
                let target = self.read_mem(memory, self.regs[4], 4)?;
                self.regs[4] = self.regs[4].wrapping_add(4 + u32::from(bytes));
                self.eip = target;
                self.indirect_branch = Some(crate::IndirectBranch {
                    source: self.pc,
                    target,
                    kind: crate::IndirectBranchKind::Return,
                });
            }
            Action::Leave(width) => {
                let address = self.regs[5];
                let frame = self.read_mem(memory, address, width)?;
                self.regs[4] = address.wrapping_add(u32::from(width));
                Reg {
                    index: 5,
                    shift: 0,
                    width,
                }
                .write(&mut self.regs, frame);
            }
            Action::Sign(kind) => match kind {
                M::Cdq => self.regs[2] = (self.regs[0] as i32 >> 31) as u32,
                M::Cwd => {
                    self.regs[2] = (self.regs[2] & 0xffff_0000)
                        | if self.regs[0] & 0x8000 != 0 {
                            0xffff
                        } else {
                            0
                        }
                }
                M::Cbw => {
                    self.regs[0] =
                        (self.regs[0] & 0xffff_0000) | (self.regs[0] as i8 as i16 as u16 as u32)
                }
                M::Cwde => self.regs[0] = self.regs[0] as i16 as i32 as u32,
                _ => unreachable!(),
            },
            Action::Flags(kind) => match kind {
                M::Clc => self.flag(CF, false),
                M::Stc => self.flag(CF, true),
                M::Cmc => self.flags ^= CF,
                M::Cld => self.flag(1 << 10, false),
                M::Std => self.flag(1 << 10, true),
                M::Lahf => {
                    self.regs[0] = (self.regs[0] & !0xff00)
                        | ((self.flags & (CF | PF | AF | ZF | SF) | 2) << 8)
                }
                M::Sahf => {
                    self.flags = (self.flags & !(CF | PF | AF | ZF | SF))
                        | ((self.regs[0] >> 8) & (CF | PF | AF | ZF | SF))
                }
                _ => unreachable!(),
            },
            Action::Int(vector) => return Ok(Some(vector)),
            Action::Ud2 => {
                return Err(Step::Trap {
                    pc: self.pc,
                    vector: 6,
                });
            }
            Action::Privileged => {
                return Err(Step::Trap {
                    pc: self.pc,
                    vector: 13,
                });
            }
        }
        Ok(None)
    }
    fn multiply(
        &mut self,
        memory: &Memory,
        dst: Operand,
        src: Option<Operand>,
        immediate: Option<Operand>,
        signed_form: bool,
    ) -> Result<(), Step> {
        let width = dst.width().unwrap();
        let (a, b) = if let Some(src) = src {
            if let Some(imm) = immediate {
                (self.read(memory, src)?, self.read(memory, imm)?)
            } else {
                (self.read(memory, dst)?, self.read(memory, src)?)
            }
        } else {
            (self.regs[0] & mask(width), self.read(memory, dst)?)
        };
        let product = registers::product(&mut self.flags, a, b, width, signed_form);
        let bits = u32::from(width) * 8;
        let low = product as u32 & mask(width);
        if src.is_some() {
            self.store(memory, dst, low)?;
        } else if width == 1 {
            self.regs[0] = (self.regs[0] & !0xffff) | (product as u32 & 0xffff);
        } else {
            let lo = Reg {
                index: 0,
                shift: 0,
                width,
            };
            let hi = Reg {
                index: 2,
                shift: 0,
                width,
            };
            lo.write(&mut self.regs, low);
            hi.write(&mut self.regs, (product >> bits) as u32);
        }
        Ok(())
    }
    fn divide(&mut self, memory: &Memory, src: Operand, signed_form: bool) -> Result<(), Step> {
        let divisor = self.read(memory, src)?;
        let width = src.width().unwrap();
        let bits = u32::from(width) * 8;
        let dividend = if width == 1 {
            u64::from(self.regs[0] & 0xffff)
        } else {
            (u64::from(self.regs[2] & mask(width)) << bits) | u64::from(self.regs[0] & mask(width))
        };
        let trap = || Step::Trap {
            pc: self.pc,
            vector: 0,
        };
        if divisor == 0 {
            return Err(trap());
        }
        let (quotient, remainder) = if signed_form {
            // i128 includes every signed 64-bit dividend divided by -1; neither
            // division nor negation can overflow or panic in the host.
            let dividend = if width == 4 {
                dividend as i64 as i128
            } else {
                let sign = 1u64 << (bits * 2 - 1);
                if dividend & sign != 0 {
                    i128::from(dividend) - (1i128 << (bits * 2))
                } else {
                    i128::from(dividend)
                }
            };
            let divisor = i128::from(signed(divisor, width));
            let q = dividend / divisor;
            let r = dividend % divisor;
            let bound = 1i128 << (bits - 1);
            if q < -bound || q >= bound {
                return Err(trap());
            }
            (q as u32, r as u32)
        } else {
            let q = dividend / u64::from(divisor);
            if q > u64::from(mask(width)) {
                return Err(trap());
            }
            (q as u32, (dividend % u64::from(divisor)) as u32)
        };
        if width == 1 {
            self.regs[0] = (self.regs[0] & !0xffff) | (quotient & 0xff) | ((remainder & 0xff) << 8);
        } else {
            Reg {
                index: 0,
                shift: 0,
                width,
            }
            .write(&mut self.regs, quotient);
            Reg {
                index: 2,
                shift: 0,
                width,
            }
            .write(&mut self.regs, remainder);
        }
        Ok(())
    }
}

fn freeze(operand: Operand, tx: &Transaction) -> Operand {
    match operand {
        Operand::Memory(a, width) => Operand::Memory(
            Address {
                base: None,
                index: None,
                scale: 1,
                displacement: a.resolve(tx),
                word: false,
                fs: false,
            },
            width,
        ),
        _ => operand,
    }
}

fn mask(width: u8) -> u32 {
    match width {
        1 => 0xff,
        2 => 0xffff,
        4 => u32::MAX,
        _ => unreachable!(),
    }
}

fn signed(value: u32, width: u8) -> i64 {
    match width {
        1 => i64::from(value as i8),
        2 => i64::from(value as i16),
        4 => i64::from(value as i32),
        _ => unreachable!(),
    }
}

fn condition_true(condition: ConditionCode, flags: u32) -> bool {
    use ConditionCode::*;
    let carry = flags & CF != 0;
    let zero = flags & ZF != 0;
    let sign = flags & SF != 0;
    let overflow = flags & OF != 0;
    match condition {
        None => true,
        o => overflow,
        no => !overflow,
        b => carry,
        ae => !carry,
        e => zero,
        ne => !zero,
        be => carry || zero,
        a => !carry && !zero,
        s => sign,
        ns => !sign,
        p => flags & PF != 0,
        np => flags & PF == 0,
        l => sign != overflow,
        ge => sign == overflow,
        le => zero || sign != overflow,
        g => !zero && sign == overflow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: A64-ISA-5
    #[test]
    fn cmpxchg8b_commits_only_zf_and_checks_the_whole_write_even_on_mismatch() {
        for locked in [false, true] {
            for equal in [false, true] {
                for writable in [false, true] {
                    let mut memory = Memory::new(0x1000, 0x4000).unwrap();
                    memory.protect(0x1000, 0x4000, READ | WRITE | EXEC).unwrap();
                    let mut code = vec![0x0f, 0xc7, 0x0e]; // cmpxchg8b [esi]
                    if locked {
                        code.insert(0, 0xf0);
                    }
                    memory.write(0x1000, &code).unwrap();
                    memory
                        .write(0x2ffc, &0x1122334455667788u64.to_le_bytes())
                        .unwrap();
                    if !writable {
                        memory.protect(0x3000, PAGE_SIZE, READ).unwrap();
                    }
                    let mut state = CpuState {
                        eip: 0x1000,
                        eflags: 0xad7,
                        regs: [
                            0x55667788,
                            0xaabbccdd,
                            if equal { 0x11223344 } else { 0 },
                            0x98765432,
                            0,
                            0,
                            0x2ffc,
                            0,
                        ],
                        ..Default::default()
                    };
                    let before = state.clone();
                    let result = Interpreter::new().run_chunk(&mut state, &mut memory, 1);
                    let bytes = memory.read_data(0x2ffc, 8).unwrap();
                    if !writable {
                        assert_eq!(
                            result.step,
                            Step::WriteFault {
                                pc: 0x1000,
                                address: 0x3000
                            }
                        );
                        assert_eq!(state.regs, before.regs);
                        assert_eq!(state.eflags, before.eflags);
                        assert_eq!(state.eip, before.eip);
                        assert_eq!(bytes.as_ref(), 0x1122334455667788u64.to_le_bytes());
                    } else {
                        assert_eq!(result.step, Step::Executed);
                        assert_eq!(
                            state.eflags,
                            (before.eflags & !ZF) | if equal { ZF } else { 0 }
                        );
                        assert_eq!(state.regs[0], 0x55667788);
                        assert_eq!(state.regs[2], 0x11223344);
                        assert_eq!(
                            bytes.as_ref(),
                            if equal {
                                0xaabbccdd98765432u64
                            } else {
                                0x1122334455667788
                            }
                            .to_le_bytes()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn frame_retirement_counts_rep_once_and_keeps_fault_prefix() {
        let mut interpreter = Interpreter::new();
        interpreter.frame_retired = Some(0);
        let mut state = CpuState {
            eip: 0x1000,
            ..Default::default()
        };
        let mut memory = Memory::new(0x1000, 0x3000).unwrap();
        memory.protect(0x1000, 0x3000, READ | WRITE | EXEC).unwrap();
        // Authored REP MOVSB, followed by a precise out-of-window load.
        memory
            .write(0x1000, &[0xf3, 0xa4, 0xa1, 0, 0, 9, 0])
            .unwrap();
        state.regs[1] = 300;
        state.regs[6] = 0x2000;
        state.regs[7] = 0x3000;
        assert_eq!(
            interpreter.run_chunk(&mut state, &mut memory, 256).units,
            256
        );
        assert_eq!(interpreter.frame_retired, Some(0));
        assert_eq!(
            interpreter.run_chunk(&mut state, &mut memory, 256).units,
            44
        );
        assert_eq!(interpreter.frame_retired, Some(1));
        assert!(matches!(
            interpreter.run_chunk(&mut state, &mut memory, 256).step,
            Step::ReadFault { .. }
        ));
        assert_eq!(interpreter.frame_retired, Some(1));
        interpreter.frame_retired = None;
        state.eip = 0x1000;
        state.regs[1] = 0;
        interpreter.run_chunk(&mut state, &mut memory, 256);
        assert_eq!(interpreter.frame_retired, None);
    }

    #[test]
    fn first_fetch_and_cross_memory_cache_hits_watch_all_code_pages() {
        const CODE: u32 = 0x10ffc;
        fn memory() -> Memory {
            let mut memory = Memory::new(0x10000, 3 * PAGE_SIZE).unwrap();
            memory
                .protect(0x10000, 3 * PAGE_SIZE, READ | WRITE | EXEC)
                .unwrap();
            // MOV crosses the page boundary; the block returns to its header.
            memory
                .write(CODE, &[0xb8, 7, 0, 0, 0, 0x43, 0xeb, 0xf8])
                .unwrap();
            memory
        }
        fn state() -> CpuState {
            let mut state = CpuState::default();
            state.eip = CODE;
            state
        }
        fn watched(memory: &mut Memory) {
            let permissions = memory.native_permissions();
            assert_ne!(permissions[0] & EXEC, 0);
            assert_ne!(permissions[1] & EXEC, 0);
            assert_eq!(permissions[2] & EXEC, 0);
        }
        let mut interpreter = Interpreter::new();
        let mut first = memory();
        let versions = first.versions.clone();
        let mut first_state = state();
        assert_eq!(
            interpreter.step(&mut first_state, &mut first),
            Step::Executed
        );
        assert_eq!(first_state.regs[0], 7);
        assert_eq!(first.versions, versions);
        watched(&mut first);
        let decodes = interpreter.decode_count();
        let mut second = memory();
        let mut second_state = state();
        assert_eq!(
            interpreter.step(&mut second_state, &mut second),
            Step::Executed
        );
        assert_eq!(
            interpreter.decode_count(),
            decodes,
            "reuse the decoded instruction"
        );
        watched(&mut second);
        let epoch = second.epoch;
        second.write(0x12000, &[9]).unwrap();
        assert_eq!(second.epoch, epoch);
        second.write(CODE + 1, &[8]).unwrap();
        assert_ne!(second.epoch, epoch);
        second_state.eip = CODE;
        assert_eq!(
            interpreter.step(&mut second_state, &mut second),
            Step::Executed
        );
        assert_eq!(second_state.regs[0], 8);
        assert_eq!(interpreter.decode_count(), decodes + 1);

        let mut third = memory();
        let (execution, _) = interpreter.run_block(&mut state(), &mut third, 3, 0, None);
        assert_eq!(execution.units, 3);
        let decodes = interpreter.decode_count();
        let mut fourth = memory();
        let mut fourth_state = state();
        let (execution, _) = interpreter.run_block(&mut fourth_state, &mut fourth, 3, 0, None);
        assert_eq!(execution.units, 3);
        assert_eq!(fourth_state.eip, CODE);
        assert_eq!(
            interpreter.decode_count(),
            decodes,
            "reuse the full decoded block"
        );
        watched(&mut fourth);
        // Data stored beside the block is not code: its page keeps the version.
        let version = fourth.versions[1];
        fourth.write(0x11800, &[1]).unwrap();
        assert_eq!(fourth.versions[1], version);
        // The same byte stored over code bumps the page; intact bytes restamp.
        fourth.write(CODE + 5, &[0x43]).unwrap();
        assert_ne!(fourth.versions[1], version);
        let (execution, _) = interpreter.run_block(&mut fourth_state, &mut fourth, 64, 0, None);
        assert_eq!(execution.units, 3);
        assert_eq!(interpreter.decode_count(), decodes, "data is not code");
        fourth.write(CODE + 1, &[9]).unwrap();
        let (execution, _) = interpreter.run_block(&mut fourth_state, &mut fourth, 64, 0, None);
        assert_eq!(execution.units, 3);
        assert_eq!(fourth_state.regs[0], 9);
        assert_eq!(
            interpreter.decode_count(),
            decodes + 3,
            "patched block redecodes"
        );
    }

    #[test]
    fn stable_native_revision_reuses_blocks_and_publication_rebuilds_the_boundary() {
        let code = 0x10000;
        let mut memory = Memory::new(code, PAGE_SIZE).unwrap();
        memory.protect(code, PAGE_SIZE, READ | EXEC).unwrap();
        // FLD1; FADD ST0,ST0; PXOR XMM0,XMM0; INT 2E.
        memory
            .write(
                code,
                &[0xd9, 0xe8, 0xd8, 0xc0, 0x66, 0x0f, 0xef, 0xc0, 0xcd, 0x2e],
            )
            .unwrap();
        let mut native = BTreeMap::new();
        let mut interpreter = Interpreter::new();
        let make = || CpuState {
            eip: code,
            xmm: [[0x5a; 16]; 8],
            ..CpuState::default()
        };
        let mut state = make();
        let (result, _) =
            interpreter.run_block(&mut state, &mut memory, 256, code + 4, Some((&native, 0)));
        assert_eq!(result.units, 2);
        let decodes = interpreter.decode_count();
        state = make();
        let (result, _) =
            interpreter.run_block(&mut state, &mut memory, 256, code + 4, Some((&native, 0)));
        assert_eq!(result.units, 2);
        assert_eq!(interpreter.decode_count(), decodes);
        native.insert(code + 4, code + 4);
        state = make();
        let (result, last_units) =
            interpreter.run_block(&mut state, &mut memory, 256, code + 32, Some((&native, 1)));
        assert_eq!(result.step, Step::Executed);
        assert_eq!(result.units, 2);
        assert_eq!(last_units, 1);
        assert_eq!(state.eip, code + 4);
        assert_eq!(state.xmm[0], [0x5a; 16]);
        let decodes = interpreter.decode_count();
        for _ in 0..100 {
            state = make();
            let (result, _) =
                interpreter.run_block(&mut state, &mut memory, 256, code + 32, Some((&native, 1)));
            assert_eq!(result.units, 2);
        }
        assert_eq!(interpreter.decode_count(), decodes);
    }

    #[test]
    fn unmap_remap_and_missing_tail_page_invalidate_cached_code_and_faults() {
        let code = 0x10000;
        let mut memory = Memory::reserve(code, PAGE_SIZE * 2).unwrap();
        memory.map(code, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
        memory.write(code, &[0xb8, 1, 0, 0, 0]).unwrap();
        let mut state = CpuState {
            eip: code,
            ..CpuState::default()
        };
        let mut interpreter = Interpreter::new();
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(state.regs[0], 1);
        state.eip = code;
        memory.unmap(code, PAGE_SIZE).unwrap();
        let before = state.clone();
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            Step::FetchFault {
                pc: code,
                address: code
            }
        );
        assert_eq!(state, before);
        memory.map(code, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
        memory.write(code, &[0xb8, 73, 0, 0, 0]).unwrap();
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(state.regs[0], 73);
        let tail = code + PAGE_SIZE as u32 - 2;
        memory.write(tail, &[0xb8, 91]).unwrap();
        state.eip = tail;
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            Step::FetchFault {
                pc: tail,
                address: tail + 2
            }
        );
        let decodes = interpreter.decode_count();
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            Step::FetchFault {
                pc: tail,
                address: tail + 2
            }
        );
        assert_eq!(
            interpreter.decode_count(),
            decodes,
            "cache the negative dependency"
        );
        memory
            .map(code + PAGE_SIZE as u32, PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        memory.write(tail + 2, &[0, 0, 0]).unwrap();
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(state.regs[0], 91);
    }

    #[test]
    fn malformed_full_instruction_and_truncated_fetch_preserve_state_and_cache_is_bounded() {
        let code = 0x10000;
        let mut memory = Memory::new(code, PAGE_SIZE).unwrap();
        memory.protect(code, PAGE_SIZE, EXEC).unwrap();
        memory.write(code, &[0x66; 15]).unwrap();
        let mut interpreter = Interpreter::with_capacity(2);
        let mut state = CpuState {
            eip: code,
            ..CpuState::default()
        };
        let before = state.clone();
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            Step::InvalidInstruction { pc: code }
        );
        assert_eq!(state, before);
        let tail = code + PAGE_SIZE as u32 - 2;
        memory.write(tail, &[0xb8, 91]).unwrap();
        state.eip = tail;
        let before = state.clone();
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            Step::FetchFault {
                pc: tail,
                address: tail + 2
            }
        );
        assert_eq!(state, before);
        memory.write(code, &[0x90; 8]).unwrap();
        for offset in 0..8 {
            state.eip = code + offset;
            assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
            assert!(interpreter.cached_instructions() <= 2);
        }
        let mut uncached = Interpreter::with_capacity(0);
        state.eip = code;
        assert_eq!(uncached.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(uncached.cached_instructions(), 0);
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::*;
    use iced_x86::code_asm::*;

    #[test]
    fn bulk_fetch_matches_bytewise_prefixes_permissions_and_wraparound() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.inc(eax).unwrap();
        let first = a.assemble(0x10000).unwrap();
        let mut a = CodeAssembler::new(32).unwrap();
        a.inc(ecx).unwrap();
        let second = a.assemble(0x11000).unwrap();
        assert_eq!((first.len(), second.len()), (1, 1));
        for base in [0x10000, 0xffffe000] {
            for permission in [0, READ, EXEC, READ | EXEC] {
                for mapped in [false, true] {
                    let mut memory = Memory::reserve(base, 2 * PAGE_SIZE).unwrap();
                    memory.map(base, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
                    memory.write(base, &first.repeat(PAGE_SIZE)).unwrap();
                    if mapped {
                        memory
                            .map(base + PAGE_SIZE as u32, PAGE_SIZE, READ | WRITE)
                            .unwrap();
                        memory
                            .write(base + PAGE_SIZE as u32, &second.repeat(PAGE_SIZE))
                            .unwrap();
                        memory
                            .protect(base + PAGE_SIZE as u32, PAGE_SIZE, permission)
                            .unwrap();
                    }
                    for offset in [0, 4080, 4081, 4082, 4094, 4095, 4096, 8180, 8190, 8191] {
                        let pc = base + offset;
                        let (bulk, bulk_fault) = fetch_bytes::<true>(&mut memory, pc);
                        let (bytes, byte_fault) = fetch_bytes::<false>(&mut memory, pc);
                        assert_eq!(bulk.bytes, bytes.bytes);
                        assert_eq!(bulk.len, bytes.len);
                        assert_eq!(bulk_fault, byte_fault);
                        for (a, b) in bulk.pages.into_iter().zip(bytes.pages) {
                            assert_eq!(
                                a.map(|p| (p.address, p.permissions, p.mapped)),
                                b.map(|p| (p.address, p.permissions, p.mapped))
                            );
                        }
                    }
                }
            }
        }
        let mut memory = Memory::reserve(0x10000, PAGE_SIZE).unwrap();
        let shared = memory.map_shared_audio(0x10000, PAGE_SIZE).unwrap();
        memory
            .protect(0x10000, PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        memory.write(0x10000, &first.repeat(15)).unwrap();
        let (bulk, denied) = fetch_bytes::<true>(&mut memory, 0x10000);
        let (bytes, other) = fetch_bytes::<false>(&mut memory, 0x10000);
        assert_eq!(bulk.bytes, bytes.bytes);
        assert_eq!(denied, other);
        drop(shared);
    }
}

#[cfg(test)]
mod fault_address_tests {
    use super::fault_address;
    use crate::{Memory, READ, WRITE};

    // spec: A64-MEM-5
    #[test]
    fn page_split_faults_name_the_first_inaccessible_byte() {
        let mut memory = Memory::new(0x10000, 0x3000).unwrap();
        memory.protect(0x10000, 0x1000, READ | WRITE).unwrap();
        memory.protect(0x11000, 0x1000, READ).unwrap();
        memory.protect(0x12000, 0x1000, 0).unwrap();
        // Crossing from a writable page into a read-only one.
        assert_eq!(fault_address(&memory, 0x10ff8, WRITE), 0x11000);
        // Starting on the denied page itself.
        assert_eq!(fault_address(&memory, 0x11004, WRITE), 0x11004);
        assert_eq!(fault_address(&memory, 0x11000, WRITE), 0x11000);
        // Reads cross from the read-only page into the inaccessible one.
        assert_eq!(fault_address(&memory, 0x11ffe, READ), 0x12000);
        // Both pages permitted: the fault had another cause; keep the start.
        assert_eq!(fault_address(&memory, 0x10ffc, READ), 0x10ffc);
        // Leaving the window counts as inaccessible.
        assert_eq!(fault_address(&memory, 0x12ffc, READ), 0x12ffc);
    }
}
