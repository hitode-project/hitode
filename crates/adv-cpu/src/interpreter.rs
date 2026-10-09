//! Bounded predecoded x86-32 integer execution on the shared CPU and memory.
//!
//! Each supported instruction stages its integer state and its memory
//! write. All guest accesses are checked before that write and the CPU commit;
//! faults and unsupported forms therefore leave the instruction restartable.
//! Undefined arithmetic flags are preserved rather than borrowed from the host.
//! GS, segment/control registers and far transfers remain outside the flat model.
use std::collections::{BTreeMap, HashMap};

use iced_x86::{
    Code, ConditionCode, Decoder, DecoderError, DecoderOptions, Instruction, Mnemonic as M, OpKind,
    Register as R,
};

use crate::{CpuState, EXEC, Memory, PAGE_SIZE, READ, WRITE};

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
    pub(crate) pc: u32,
    flags: u32,
    operation: StringOp,
}

impl RepContinuation {
    pub(crate) fn fault_native_compare(state: &mut CpuState, native: u32) {
        Self::commit_native_compare(state, native, true);
        state.eflags = state.rep.take().unwrap().flags;
    }
    /// Called immediately before a native comparison chunk commits its flags.
    /// A later native/interpreted chunk belongs to the same guest instruction.
    pub(crate) fn commit_native_compare(state: &mut CpuState, native: u32, more: bool) {
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
    cache: HashMap<u32, Cached>,
    blocks: HashMap<u32, DecodedBlock>,
    validity: crate::code_validity::CodeValidity<(bool, u32)>,
    event_validity: bool,
    capacity: usize,
    decodes: u64,
    pub(crate) frame_retired: Option<u64>,
    pub(crate) census_enabled: bool,
    pub(crate) census: Option<crate::tier1::census::Entry>,
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
            cache: HashMap::new(),
            blocks: HashMap::new(),
            validity: Default::default(),
            event_validity: crate::code_validity::enabled(),
            capacity,
            decodes: 0,
            frame_retired: None,
            census_enabled: std::env::var_os("ADV_TIER1_CENSUS").is_some_and(|v| v == "1"),
            census: None,
        }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.blocks.clear();
        self.validity.clear();
    }

    fn refresh(&mut self, memory: &mut Memory) {
        if !self.event_validity {
            return;
        }
        for (block, pc) in self.validity.refresh(memory) {
            let valid = if block {
                self.blocks
                    .get(&pc)
                    .is_some_and(|entry| entry.cacheable(memory) && entry.revalidate(memory))
            } else {
                self.cache
                    .get(&pc)
                    .is_some_and(|entry| entry.cacheable(memory) && entry.revalidate(memory))
            };
            if !valid {
                if block {
                    self.blocks.remove(&pc);
                } else {
                    self.cache.remove(&pc);
                }
                self.validity.remove((block, pc));
            }
        }
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
        self.refresh(memory);
        let pc = state.eip;
        let (decoded, form) = if let Some(entry) = self
            .cache
            .get(&pc)
            .filter(|e| self.event_validity || e.valid(memory))
        {
            (
                entry.decoded,
                self.census.as_ref().map(|_| {
                    crate::tier1::census::Form::decode(&entry.bytes[..entry.len as usize])
                }),
            )
        } else {
            self.decodes += 1;
            let entry = fetch(memory, pc);
            let decoded = entry.decoded;
            let form = self
                .census
                .as_ref()
                .map(|_| crate::tier1::census::Form::decode(&entry.bytes[..entry.len as usize]));
            if self.capacity != 0 && (!self.event_validity || entry.cacheable(memory)) {
                if self.cache.len() >= self.capacity {
                    if self.event_validity {
                        self.clear_cache();
                    } else {
                        self.cache.clear();
                    }
                }
                if self.event_validity {
                    self.validity.insert(
                        (false, pc),
                        entry.pages.iter().flatten().map(|page| page.address),
                        memory,
                    );
                }
                self.cache.insert(pc, entry);
            }
            (decoded, form)
        };
        let execution = execute_decoded(decoded, state, memory, max_units, None);
        if let Some(retired) = &mut self.frame_retired {
            *retired += retired_instruction(decoded, execution, state, pc);
        }
        if let (Some(census), Some(form)) = (&mut self.census, form) {
            census.record(form, execution.units);
        }
        execution
    }

    /// Execute one bounded predecoded basic block. This is internal to Executor:
    /// public step/run_chunk retain their single-instruction/REP contract.
    pub(crate) fn run_block(
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
        self.refresh(memory);
        let pc = state.eip;
        let revision = native.map(|(_, revision)| revision);
        if !self.blocks.get(&pc).is_some_and(|block| {
            block.revision == revision && (self.event_validity || block.valid(memory))
        }) {
            let mut block =
                DecodedBlock::fetch(memory, pc, native, &mut self.decodes, self.event_validity);
            if self.capacity == 0 || (self.event_validity && !block.cacheable(memory)) {
                return block.execute_observed(
                    state,
                    memory,
                    max_units,
                    stop,
                    self.census.as_mut(),
                    self.frame_retired.as_mut(),
                );
            }
            // Bound each independent cache in entries. A larger block working set
            // avoids flushing hot floating fallback sequences during discovery.
            if self.blocks.len() >= self.capacity {
                if self.event_validity {
                    self.clear_cache();
                } else {
                    self.blocks.clear();
                }
            }
            if self.event_validity {
                self.validity.insert(
                    (true, pc),
                    block.pages.iter().map(|page| page.address),
                    memory,
                );
            }
            self.blocks.insert(pc, block);
        }
        self.blocks.get_mut(&pc).unwrap().execute_observed(
            state,
            memory,
            max_units,
            stop,
            self.census.as_mut(),
            self.frame_retired.as_mut(),
        )
    }
}

fn execute_decoded(
    decoded: Decoded,
    state: &mut CpuState,
    memory: &mut Memory,
    max_units: u32,
    deferred_mmx: Option<&mut bool>,
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
                step: result,
                units: 0,
            };
        }
        Decoded::Extended(i, operation, opcode) => {
            // Extended interpreter transactions remain conservative until their
            // full memory protocol is independently covered in parallel tests.
            memory.quiesce_workers();
            let x87 = operation == crate::floating::Operation::X87;
            let defer = x87 && deferred_mmx.is_some();
            if let Some(dirty) = deferred_mmx.as_deref()
                && *dirty
                && !x87
            {
                // A following SIMD/MMX instruction can read the physical x87
                // aliases. Materialize the completed x87 prefix before it runs.
                state.refresh_mmx();
            }
            let step = crate::floating::execute(&i, operation, opcode, state, memory, !defer);
            let completed = step == Step::Executed;
            if let Some(dirty) = deferred_mmx {
                if x87 {
                    *dirty |= completed;
                } else {
                    *dirty = false;
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
    if instruction.exclusive {
        // LOCK and implicit atomic RMW execute as one transaction relative to
        // native workers. Ordinary scalar instructions use Memory's audited
        // foreign loads/stores and do not stop unrelated native execution.
        memory.quiesce_workers();
    }
    if let Action::String(operation) = instruction.action {
        return run_string(
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
    Stop(Step),
}

fn retired_instruction(decoded: Decoded, execution: Execution, state: &CpuState, pc: u32) -> u64 {
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
}

#[derive(Clone, Copy)]
struct Page {
    address: u32,
    permissions: u8,
    mapped: bool,
}

impl Page {
    fn at(memory: &Memory, address: u32) -> Self {
        let address = address & !(PAGE_SIZE as u32 - 1);
        let index = memory.range(address, 1).ok().map(|r| r.start / PAGE_SIZE);
        Self {
            address,
            permissions: index.map_or(0, |p| memory.perms[p]),
            mapped: index.is_some_and(|p| memory.mapped[p]),
        }
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
    fn valid(&self, memory: &mut Memory) -> bool {
        for page in self.pages.iter().flatten() {
            memory.watch_code(page.address, PAGE_SIZE);
        }
        self.revalidate(memory)
    }

    fn cacheable(&self, memory: &Memory) -> bool {
        self.pages
            .iter()
            .flatten()
            .all(|page| !memory.externally_mutable_code(page.address, PAGE_SIZE))
    }

    fn revalidate(&self, memory: &Memory) -> bool {
        self.pages.iter().flatten().all(|p| p.matches(memory))
            && memory
                .read_data(self.pc, self.len as usize)
                .is_ok_and(|bytes| bytes == &self.bytes[..self.len as usize])
    }
}

fn fetch(memory: &mut Memory, pc: u32) -> Cached {
    memory.watch_code(pc, 15);
    let mut entry = Cached {
        decoded: Decoded::Stop(Step::InvalidInstruction { pc }),
        pc,
        bytes: [0; 15],
        len: 0,
        pages: [None; 2],
    };
    let mut denied = None;
    for offset in 0..15u32 {
        let Some(address) = pc.checked_add(offset) else {
            denied = Some(pc.wrapping_add(offset));
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
        match memory.read_data(address, 1) {
            Ok(byte) => {
                entry.bytes[offset as usize] = byte[0];
                entry.len += 1;
            }
            Err(_) => {
                denied = Some(address);
                break;
            }
        }
    }
    let mut decoder = Decoder::with_ip(
        32,
        &entry.bytes[..entry.len as usize],
        u64::from(pc),
        DecoderOptions::NONE,
    );
    let instruction = decoder.decode();
    entry.decoded = if instruction.is_invalid() {
        if decoder.last_error() == DecoderError::NoMoreBytes
            && let Some(address) = denied
        {
            Decoded::Stop(Step::FetchFault { pc, address })
        } else {
            Decoded::Stop(Step::InvalidInstruction { pc })
        }
    } else if let Some(action) = predecode(&instruction) {
        Decoded::Ready(Predecoded {
            action,
            next: instruction.next_ip32(),
            exclusive: instruction.has_lock_prefix()
                || instruction.mnemonic() == M::Cpuid
                || instruction.mnemonic() == M::Xchg
                    && (instruction.op0_kind() == OpKind::Memory
                        || instruction.op1_kind() == OpKind::Memory),
        })
    } else if let Some(operation) = crate::floating::operation(&instruction) {
        // Preserve the original prefix/opcode scan in the decoded entry. Its
        // code bytes share the same validation as the instruction itself.
        let opcode = entry.bytes[..instruction.len()]
            .windows(2)
            .find(|bytes| (0xd8..=0xdf).contains(&bytes[0]))
            .map(|bytes| ((bytes[0] as u16 & 7) << 8) | bytes[1] as u16);
        Decoded::Extended(instruction, operation, opcode)
    } else {
        Decoded::Stop(Step::Unsupported {
            pc,
            code: instruction.code(),
        })
    };
    entry
}

const BLOCK_INSTRUCTIONS: usize = 64;
const BLOCK_BYTES: usize = 512;

/// A contiguous basic block, including its terminating instruction. Page
/// subscriptions retain negative fetch dependencies and allocation ownership;
/// bytes are compared only after a relevant event, before executing any prefix.
struct DecodedBlock {
    pc: u32,
    instructions: Vec<(u32, Decoded)>,
    bytes: Vec<u8>,
    pages: Vec<Page>,
    revision: Option<u64>,
    forms: Option<Vec<crate::tier1::census::Form>>,
}

impl DecodedBlock {
    fn valid(&self, memory: &mut Memory) -> bool {
        for page in &self.pages {
            memory.watch_code(page.address, PAGE_SIZE);
        }
        self.revalidate(memory)
    }

    fn cacheable(&self, memory: &Memory) -> bool {
        self.pages
            .iter()
            .all(|page| !memory.externally_mutable_code(page.address, PAGE_SIZE))
    }

    fn revalidate(&self, memory: &Memory) -> bool {
        self.pages.iter().all(|page| page.matches(memory))
            && memory
                .read_data(self.pc, self.bytes.len())
                .is_ok_and(|bytes| bytes == self.bytes)
    }

    fn fetch(
        memory: &mut Memory,
        start: u32,
        native: Option<(&BTreeMap<u32, u32>, u64)>,
        decodes: &mut u64,
        event_validity: bool,
    ) -> Self {
        let mut block = Self {
            pc: start,
            instructions: Vec::new(),
            bytes: Vec::new(),
            pages: Vec::new(),
            revision: native.map(|(_, revision)| revision),
            forms: None,
        };
        let mut pc = start;
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
            // Aliased writers need not advance this owner's epoch. Keep these
            // instructions uncached and return after one instruction so a store
            // through another view cannot leave a stale decoded suffix.
            let volatile = event_validity && !entry.cacheable(memory);
            if volatile && !block.instructions.is_empty() {
                break;
            }
            let (next, terminates) = match entry.decoded {
                Decoded::Ready(instruction) => (
                    instruction.next,
                    matches!(
                        instruction.action,
                        Action::Branch(..)
                            | Action::Loop(..)
                            | Action::Call(..)
                            | Action::Ret(..)
                            | Action::Int(..)
                            | Action::Ud2
                            | Action::Privileged
                            | Action::String(..)
                            | Action::PopFlags(..)
                            | Action::Rdtsc
                    ),
                ),
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
            block.instructions.push((pc, entry.decoded));
            if terminates || next <= pc || volatile {
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
        census: Option<&mut crate::tier1::census::Entry>,
        retired: Option<&mut u64>,
    ) -> (Execution, u32) {
        if census.is_some() {
            self.forms.get_or_insert_with(|| {
                self.instructions
                    .iter()
                    .map(|(pc, _)| {
                        let offset = pc.wrapping_sub(self.pc) as usize;
                        crate::tier1::census::Form::decode(&self.bytes[offset..])
                    })
                    .collect()
            });
        }
        // Specialize the ordinary block loop: disabled diagnostics must not
        // add a retirement branch to every interpreted instruction.
        match (census, retired) {
            (Some(census), Some(retired)) => {
                let forms = self.forms.as_ref().unwrap().as_slice();
                self.execute(
                    state,
                    memory,
                    max_units,
                    stop,
                    |n, units| census.record(forms[n], units),
                    |d, e, s, pc| *retired += retired_instruction(d, e, s, pc),
                )
            }
            (Some(census), None) => {
                let forms = self.forms.as_ref().unwrap().as_slice();
                self.execute(
                    state,
                    memory,
                    max_units,
                    stop,
                    |n, units| census.record(forms[n], units),
                    |_, _, _, _| {},
                )
            }
            (None, Some(retired)) => self.execute(
                state,
                memory,
                max_units,
                stop,
                |_, _| {},
                |d, e, s, pc| *retired += retired_instruction(d, e, s, pc),
            ),
            (None, None) => {
                self.execute(state, memory, max_units, stop, |_, _| {}, |_, _, _, _| {})
            }
        }
    }

    fn execute(
        &self,
        state: &mut CpuState,
        memory: &mut Memory,
        max_units: u32,
        stop: u32,
        mut observe: impl FnMut(usize, u32),
        mut retire: impl FnMut(Decoded, Execution, &CpuState, u32),
    ) -> (Execution, u32) {
        let epoch = memory.epoch;
        let mut result = Execution {
            step: Step::Executed,
            units: 0,
        };
        let mut last_units = 0;
        let mut deferred_mmx = false;
        for (n, &(pc, decoded)) in self.instructions.iter().enumerate() {
            if result.units == max_units || state.eip != pc || pc == stop {
                break;
            }
            let execution = execute_decoded(
                decoded,
                state,
                memory,
                max_units - result.units,
                Some(&mut deferred_mmx),
            );
            observe(n, execution.units);
            retire(decoded, execution, state, pc);
            result.units += execution.units;
            result.step = execution.step;
            last_units = execution.units;
            // Stores can patch this block's next instruction. A flags change can
            // enable single stepping, and floating exceptions remain restartable.
            if execution.step != Step::Executed
                || memory.epoch != epoch
                || state.eflags & 0x100 != 0
            {
                break;
            }
        }
        if deferred_mmx {
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
        let mut offset = self.displacement;
        if let Some(r) = self.base {
            offset = offset.wrapping_add(r.read(&tx.regs));
        }
        if let Some(r) = self.index {
            offset = offset.wrapping_add(r.read(&tx.regs).wrapping_mul(u32::from(self.scale)));
        }
        if self.word {
            offset &= 0xffff;
        }
        offset.wrapping_add(if self.fs { tx.fs_base } else { 0 })
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

fn run_string(
    state: &mut CpuState,
    memory: &mut Memory,
    operation: StringOp,
    next: u32,
    max_units: u32,
    debug: bool,
) -> Execution {
    use StringKind::*;
    let pc = state.eip;
    let index_width = if operation.word { 2 } else { 4 };
    let counter = Reg {
        index: 1,
        shift: 0,
        width: index_width,
    };
    if operation.repeat && counter.read(&state.regs) == 0 {
        state.eip = next;
        state.rep = None;
        return Execution {
            step: if debug {
                Step::Trap { pc, vector: 1 }
            } else {
                Step::Executed
            },
            units: 1,
        };
    }
    let comparison = matches!(operation.kind, Cmps | Scas);
    let had_continuation = state
        .rep
        .as_ref()
        .is_some_and(|r| r.pc == pc && r.operation == operation);
    let original_flags = if operation.repeat && comparison {
        if !had_continuation {
            state.rep = Some(RepContinuation {
                pc,
                flags: state.eflags,
                operation,
            });
        }
        state.rep.as_ref().unwrap().flags
    } else {
        state.rep = None;
        state.eflags
    };
    let mut units = 0;
    while units < if debug { 1 } else { max_units } {
        let mut tx = Transaction {
            regs: state.regs,
            flags: state.eflags,
            eip: pc,
            fs_base: state.fs_base,
            segments: state.segments,
            pc,
            write: None,
            more_writes: Vec::new(),
            indirect_branch: None,
        };
        let source_index = Reg {
            index: 6,
            shift: 0,
            width: index_width,
        };
        let dest_index = Reg {
            index: 7,
            shift: 0,
            width: index_width,
        };
        let source =
            source_index
                .read(&tx.regs)
                .wrapping_add(if operation.fs { state.fs_base } else { 0 });
        let destination = dest_index.read(&tx.regs);
        let accumulator = Reg {
            index: 0,
            shift: 0,
            width: operation.width,
        };
        let element = (|| -> Result<(), Step> {
            match operation.kind {
                Movs => {
                    let value = tx.read_mem(memory, source, operation.width)?;
                    tx.store(
                        memory,
                        Transaction::stack_operand_width(destination, operation.width),
                        value,
                    )?;
                }
                Stos => {
                    let value = accumulator.read(&tx.regs);
                    tx.store(
                        memory,
                        Transaction::stack_operand_width(destination, operation.width),
                        value,
                    )?;
                }
                Lods => {
                    let value = tx.read_mem(memory, source, operation.width)?;
                    accumulator.write(&mut tx.regs, value);
                }
                Cmps => {
                    let a = tx.read_mem(memory, source, operation.width)?;
                    let b = tx.read_mem(memory, destination, operation.width)?;
                    tx.arithmetic(a, b, 0, operation.width, true);
                }
                Scas => {
                    let a = accumulator.read(&tx.regs);
                    let b = tx.read_mem(memory, destination, operation.width)?;
                    tx.arithmetic(a, b, 0, operation.width, true);
                }
            }
            Ok(())
        })();
        if let Err(step) = element {
            if operation.repeat && comparison {
                state.eflags = original_flags;
                // The fault exposes the architectural restart context. A handler
                // may change EFLAGS before restarting the remaining elements;
                // that context is the next instruction's input, not this origin.
                state.rep = None;
            }
            return Execution { step, units };
        }
        let delta = if state.eflags & (1 << 10) != 0 {
            0u32.wrapping_sub(u32::from(operation.width))
        } else {
            u32::from(operation.width)
        };
        if matches!(operation.kind, Movs | Cmps | Lods) {
            let index = source_index.read(&tx.regs).wrapping_add(delta);
            source_index.write(&mut tx.regs, index);
        }
        if operation.kind != Lods {
            let index = dest_index.read(&tx.regs).wrapping_add(delta);
            dest_index.write(&mut tx.regs, index);
        }
        if operation.repeat {
            let count = counter.read(&tx.regs).wrapping_sub(1);
            counter.write(&mut tx.regs, count);
        }
        if let Some((address, width, value)) = tx.write {
            // This element's complete range and permissions have been checked.
            memory
                .write(address, &value.to_le_bytes()[..width as usize])
                .expect("validated REP element write");
        }
        state.regs = tx.regs;
        state.eflags = tx.flags;
        units += 1;
        let more = operation.repeat
            && counter.read(&state.regs) != 0
            && (!comparison || (state.eflags & ZF == 0) == operation.unequal);
        if !more {
            state.eip = next;
            state.rep = None;
        }
        if debug {
            // Unlike a scheduler chunk, a delivered debug exception exposes a
            // restart context that its handler is allowed to change.
            state.rep = None;
            return Execution {
                step: Step::Trap { pc, vector: 1 },
                units,
            };
        }
        if !more {
            break;
        }
    }
    Execution {
        step: Step::Executed,
        units,
    }
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

pub(crate) fn accessible(memory: &Memory, address: u32, width: u8, permission: u8) -> bool {
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
        let value = value & mask(width);
        self.flag(ZF, value == 0);
        self.flag(SF, value & (1 << (u32::from(width) * 8 - 1)) != 0);
        self.flag(PF, (value as u8).count_ones().is_multiple_of(2));
    }
    fn arithmetic(&mut self, a: u32, b: u32, carry: u32, width: u8, subtract: bool) -> u32 {
        let a = a & mask(width);
        let b = b & mask(width);
        let result = if subtract {
            a.wrapping_sub(b).wrapping_sub(carry)
        } else {
            a.wrapping_add(b).wrapping_add(carry)
        } & mask(width);
        let wide = if subtract {
            signed(a, width) - signed(b, width) - i64::from(carry)
        } else {
            signed(a, width) + signed(b, width) + i64::from(carry)
        };
        let limit = 1i64 << (u32::from(width) * 8 - 1);
        self.flag(
            CF,
            if subtract {
                u64::from(a) < u64::from(b) + u64::from(carry)
            } else {
                u64::from(a) + u64::from(b) + u64::from(carry) > u64::from(mask(width))
            },
        );
        self.flag(OF, wide < -limit || wide >= limit);
        self.flag(AF, (a ^ b ^ result) & 0x10 != 0);
        self.szp(result, width);
        result
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
                let carry = u32::from(self.flags & CF != 0 && matches!(kind, M::Adc | M::Sbb));
                let value = match kind {
                    M::Add | M::Adc => self.arithmetic(a, b, carry, width, false),
                    M::Sub | M::Sbb | M::Cmp => self.arithmetic(a, b, carry, width, true),
                    _ => {
                        let value = match kind {
                            M::And | M::Test => a & b,
                            M::Or => a | b,
                            M::Xor => a ^ b,
                            _ => unreachable!(),
                        };
                        self.flags &= !(CF | OF);
                        self.szp(value, width);
                        value
                    }
                };
                if !matches!(kind, M::Cmp | M::Test) {
                    self.store(memory, dst, value)?;
                }
            }
            Action::Unary(kind, dst) => {
                let value = self.read(memory, dst)?;
                let width = dst.width().unwrap();
                let result = match kind {
                    M::Not => !value,
                    M::Neg => self.arithmetic(0, value, 0, width, true),
                    _ => {
                        let carry = self.flags & CF;
                        let result = self.arithmetic(value, 1, 0, width, kind == M::Dec);
                        self.flags = (self.flags & !CF) | carry;
                        result
                    }
                };
                self.store(memory, dst, result)?;
            }
            Action::Shift(kind, dst, count) => {
                let value = self.read(memory, dst)?;
                let count = self.read(memory, count)? & 31;
                self.check_write(memory, dst)?;
                if count != 0 {
                    let result = self.shift(kind, value, count, dst.width().unwrap());
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
        let product = if signed_form {
            (signed(a, width) * signed(b, width)) as u64
        } else {
            u64::from(a) * u64::from(b)
        };
        let bits = u32::from(width) * 8;
        let low = product as u32 & mask(width);
        let overflow = if signed_form {
            product as i64 != signed(low, width)
        } else {
            product >> bits != 0
        };
        self.flag(CF, overflow);
        self.flag(OF, overflow);
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
    fn shift(&mut self, kind: M, value: u32, count: u32, width: u8) -> u32 {
        let bits = u32::from(width) * 8;
        let value = value & mask(width);
        let sign = value & (1 << (bits - 1)) != 0;
        let old_cf = self.flags & CF != 0;
        match kind {
            M::Shl => {
                if count <= bits {
                    self.flag(CF, value & (1 << (bits - count)) != 0);
                }
                let result = (u64::from(value) << count) as u32 & mask(width);
                if count == 1 {
                    self.flag(
                        OF,
                        (result & (1 << (bits - 1)) != 0) != (self.flags & CF != 0),
                    );
                }
                self.szp(result, width);
                result
            }
            M::Shr => {
                if count <= bits {
                    self.flag(CF, value & (1 << (count - 1)) != 0);
                }
                let result = value >> count;
                if count == 1 {
                    self.flag(OF, sign);
                }
                self.szp(result, width);
                result
            }
            M::Sar => {
                self.flag(
                    CF,
                    if count >= bits {
                        sign
                    } else {
                        value & (1 << (count - 1)) != 0
                    },
                );
                let result = (signed(value, width) >> count) as u32 & mask(width);
                if count == 1 {
                    self.flag(OF, false);
                }
                self.szp(result, width);
                result
            }
            M::Rol | M::Ror => {
                let n = count % bits;
                let result = if n == 0 {
                    value
                } else if kind == M::Rol {
                    (value << n) | (value >> (bits - n))
                } else {
                    (value >> n) | (value << (bits - n))
                } & mask(width);
                self.flag(
                    CF,
                    if kind == M::Rol {
                        result & 1 != 0
                    } else {
                        result & (1 << (bits - 1)) != 0
                    },
                );
                if count == 1 {
                    self.flag(
                        OF,
                        if kind == M::Rol {
                            (result & (1 << (bits - 1)) != 0) != (self.flags & CF != 0)
                        } else {
                            (result >> (bits - 1)) ^ (result >> (bits - 2)) & 1 != 0
                        },
                    );
                }
                result
            }
            M::Rcl | M::Rcr => {
                let n = count % (bits + 1);
                if n == 0 {
                    return value;
                }
                let extended = (u64::from(old_cf) << bits) | u64::from(value);
                let all = (1u64 << (bits + 1)) - 1;
                let extended = if kind == M::Rcl {
                    ((extended << n) | (extended >> (bits + 1 - n))) & all
                } else {
                    ((extended >> n) | (extended << (bits + 1 - n))) & all
                };
                let result = extended as u32 & mask(width);
                self.flag(CF, extended & (1u64 << bits) != 0);
                if count == 1 {
                    self.flag(
                        OF,
                        if kind == M::Rcl {
                            (result & (1 << (bits - 1)) != 0) != (self.flags & CF != 0)
                        } else {
                            ((result >> (bits - 1)) ^ (result >> (bits - 2))) & 1 != 0
                        },
                    );
                }
                result
            }
            _ => unreachable!(),
        }
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
    fn event_blocks_keep_same_page_data_and_detect_consumed_smc_events() {
        let mut memory = Memory::new(0x10000, 3 * PAGE_SIZE).unwrap();
        memory
            .protect(0x10000, 3 * PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        for pc in [0x10000, 0x11000] {
            memory.write(pc, &[0xb8, 7, 0, 0, 0, 0xeb, 0xf9]).unwrap();
        }
        let mut interpreter = Interpreter::new();
        interpreter.event_validity = true;
        let mut state = CpuState::default();
        for pc in [0x10000, 0x11000] {
            state.eip = pc;
            assert_eq!(
                interpreter
                    .run_block(&mut state, &mut memory, 64, 0, None)
                    .0
                    .units,
                2
            );
        }
        let decodes = interpreter.decode_count();
        let other = interpreter.blocks[&0x11000].instructions.as_ptr();
        memory.write(0x10080, &[42]).unwrap();
        memory.dirty_pages.clear(); // Tier 2 may consume this before Tier 0 runs.
        state.eip = 0x10000;
        interpreter.run_block(&mut state, &mut memory, 64, 0, None);
        assert_eq!(interpreter.decode_count(), decodes);
        memory.write(0x10001, &[9]).unwrap();
        memory.dirty_pages.clear();
        state.eip = 0x10000;
        interpreter.run_block(&mut state, &mut memory, 64, 0, None);
        assert_eq!(state.regs[0], 9);
        assert_eq!(interpreter.decode_count(), decodes + 2);
        assert_eq!(interpreter.blocks[&0x11000].instructions.as_ptr(), other);
        state.eip = 0x10000;
        memory.protect(0x10000, PAGE_SIZE, READ).unwrap();
        let before = state.clone();
        let result = interpreter
            .run_block(&mut state, &mut memory, 64, 0, None)
            .0;
        assert_eq!(result.units, 0);
        assert_eq!(
            result.step,
            Step::FetchFault {
                pc: 0x10000,
                address: 0x10000
            }
        );
        assert_eq!(state, before);
    }

    #[test]
    fn event_blocks_exit_after_self_modifying_prefix() {
        let mut memory = Memory::new(0x10000, PAGE_SIZE).unwrap();
        memory
            .protect(0x10000, PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        // MOV byte [next immediate],9; MOV EAX,7; JMP header.
        memory
            .write(
                0x10000,
                &[0xc6, 0x05, 8, 0, 1, 0, 9, 0xb8, 7, 0, 0, 0, 0xeb, 0xf2],
            )
            .unwrap();
        let mut interpreter = Interpreter::new();
        interpreter.event_validity = true;
        let mut state = CpuState {
            eip: 0x10000,
            ..Default::default()
        };
        assert_eq!(
            interpreter
                .run_block(&mut state, &mut memory, 64, 0, None)
                .0
                .units,
            1
        );
        assert_eq!(state.eip, 0x10007);
        assert_eq!(state.regs[0], 0);
        interpreter.run_block(&mut state, &mut memory, 64, 0, None);
        assert_eq!(state.regs[0], 9);
    }

    #[test]
    fn event_shared_code_is_uncached_without_owner_epoch_events() {
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        let storage = memory.map_shared_audio(0x10000, PAGE_SIZE).unwrap();
        storage.store_bytes(0, &[0xb8, 7, 0, 0, 0, 0x43]);
        memory
            .protect(0x10000, PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        let mut interpreter = Interpreter::new();
        interpreter.event_validity = true;
        let mut state = CpuState {
            eip: 0x10000,
            ..Default::default()
        };
        assert_eq!(
            interpreter
                .run_block(&mut state, &mut memory, 64, 0, None)
                .0
                .units,
            1
        );
        assert!(interpreter.blocks.is_empty());
        let epoch = memory.epoch;
        storage.store_bytes(1, &[9]);
        assert_eq!(
            memory.epoch, epoch,
            "aliased data writes have no code event"
        );
        state.eip = 0x10000;
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(state.regs[0], 9);
        assert!(interpreter.cache.is_empty());
        assert!(interpreter.blocks.is_empty());
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
        let (execution, _) = interpreter.run_block(&mut state(), &mut third, 64, 0, None);
        assert_eq!(execution.units, 3);
        let decodes = interpreter.decode_count();
        let mut fourth = memory();
        let mut fourth_state = state();
        let (execution, _) = interpreter.run_block(&mut fourth_state, &mut fourth, 64, 0, None);
        assert_eq!(execution.units, 3);
        assert_eq!(fourth_state.eip, CODE);
        assert_eq!(
            interpreter.decode_count(),
            decodes,
            "reuse the full decoded block"
        );
        watched(&mut fourth);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn scalar_interpreter_does_not_park_native_peers_but_atomic_operations_do() {
        use std::sync::{Arc, atomic::Ordering};
        let mut memory = crate::Memory::new(0x1000, 0x2000).unwrap();
        memory
            .protect(0x1000, 0x2000, crate::READ | crate::WRITE)
            .unwrap();
        // inc eax; mov [0x2000],eax; lock inc [0x2000]; xchg eax,[0x2000]
        let code = [
            0x40, 0xa3, 0, 0x20, 0, 0, 0xf0, 0xff, 0x05, 0, 0x20, 0, 0, 0x87, 0x05, 0, 0x20, 0, 0,
        ];
        memory.write(0x1000, &code).unwrap();
        memory
            .protect(0x1000, 4096, crate::READ | crate::EXEC)
            .unwrap();
        let fence = Arc::new(crate::parallel_memory::Fence::default());
        memory.parallel = Some(fence.clone());
        let mut state = crate::CpuState {
            eip: 0x1000,
            ..Default::default()
        };
        let mut interpreter = super::Interpreter::new();
        let permit = fence.enter();
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            super::Step::Executed
        );
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            super::Step::Executed
        );
        assert!(!fence.stop.load(Ordering::Acquire));
        assert!(!fence.is_idle());
        drop(permit);
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            super::Step::Executed
        );
        assert!(fence.stop.load(Ordering::Acquire));
        fence.stop.store(false, Ordering::Release);
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            super::Step::Executed
        );
        assert!(fence.stop.load(Ordering::Acquire));
        assert_eq!(state.regs[0], 2);
        assert_eq!(&*memory.read_data(0x2000, 4).unwrap(), &1u32.to_le_bytes());
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
