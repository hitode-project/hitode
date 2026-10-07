//! Linux ARM64 entry/exit, immutable code ownership, and checked continuations.
use super::{BUDGET, DISPATCH, Execution, FALLBACK, LOAD, STORE, Tier1Stats, emit};
use crate::execution_abi::Tier1Frame;
use crate::interpreter::{Execution as Interpreted, Interpreter, Step};
use crate::{CpuState, Memory};
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic};
use std::collections::{BTreeMap, VecDeque};
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

core::arch::global_asm!(
    r#"
.text
.p2align 2
.global adv_tier1_enter
.type adv_tier1_enter,%function
adv_tier1_enter:
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
.size adv_tier1_enter, .-adv_tier1_enter
"#
);
unsafe extern "C" {
    fn adv_tier1_enter(frame: *mut Tier1Frame, entry: *const u8, window: *mut u8);
    fn __clear_cache(start: *mut core::ffi::c_void, end: *mut core::ffi::c_void);
}

struct Code {
    pointer: NonNull<u8>,
    len: usize,
}
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
        }
        if unsafe { libc::mprotect(raw, len, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
            return Err(format!(
                "publish Tier 1 code: {}",
                std::io::Error::last_os_error()
            ));
        }
        unsafe {
            __clear_cache(raw, raw.cast::<u8>().add(bytes).cast());
        }
        Ok(code)
    }
}
impl Drop for Code {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.pointer.as_ptr().cast(), self.len);
        }
    }
}
struct Unit {
    code: Code,
    input: Vec<(u32, Vec<u8>)>,
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
    epoch: u64,
    stats: Tier1Stats,
}
impl Engine {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            entries: BTreeMap::new(),
            epoch: 0,
            stats: Tier1Stats::default(),
        })
    }
    pub fn stats(&self) -> Tier1Stats {
        self.stats.clone()
    }
    fn compile(&mut self, memory: &mut Memory, start: u32) -> Result<bool, String> {
        let timer = Instant::now();
        let mut queue = VecDeque::from([start]);
        let mut instructions = BTreeMap::<u32, Instruction>::new();
        let mut input = Vec::new();
        while let Some(pc) = queue.pop_front() {
            if instructions.contains_key(&pc)
                || instructions.len() >= 128
                || pc.abs_diff(start) > 4096
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
            if instruction.is_jcc_short_or_near() || instruction.mnemonic() == Mnemonic::Jmp {
                queue.push_back(instruction.near_branch32());
            }
            if instruction.flow_control() != FlowControl::UnconditionalBranch {
                queue.push_back(instruction.next_ip32());
            }
        }
        if !instructions.contains_key(&start) {
            return Ok(false);
        }
        let mut emitted = emit::Emitter::new();
        for instruction in instructions.values() {
            emitted.instruction(instruction);
        }
        let emitted = emitted.finish()?;
        let code = Code::new(&emitted.words)?;
        if self.entries.len() + instructions.len() > 4096 {
            self.entries.clear();
        }
        self.stats.compilations += 1;
        self.stats.code_bytes += emitted.words.len() as u64 * 4;
        let unit = Rc::new(Unit { code, input });
        for pc in instructions.keys() {
            self.entries
                .insert(*pc, (unit.clone(), emitted.entries[pc]));
        }
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
    ) -> Result<Execution, String> {
        if memory.is_parallel() {
            return Err("Tier 1 requires an exclusive serial execution owner".into());
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
            self.entries.clear();
            self.epoch = memory.epoch;
        }
        if !self.entries.contains_key(&state.eip) && !self.compile(memory, state.eip)? {
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
        let mut frame = Tier1Frame {
            regs: state.regs,
            flags: state.eflags,
            pc: state.eip,
            remaining: u64::from(units),
            stop,
            fs_base: state.fs_base,
            ..Default::default()
        };
        // SAFETY: this owner pins all code and exclusively owns the CPU/memory.
        // Every emitted entry uses the fixed ABI; helpers run only after return.
        unsafe {
            adv_tier1_enter(
                &mut frame,
                unit.code.pointer.as_ptr().add(*offset),
                memory.ptr.as_ptr(),
            );
        }
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
