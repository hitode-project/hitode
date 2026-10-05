//! Transactional x87, SSE and packed-integer execution on canonical CPU state.
use crate::{CpuState, Memory, PAGE_SIZE, READ, WRITE, interpreter::Step};
use iced_x86::{Instruction, Mnemonic as M, Register as R};
pub(crate) mod fast;
#[cfg(all(test, target_arch = "x86_64"))]
mod native_x87;
mod simd;
mod softfloat;
mod transcendental;
mod x87;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    X87,
    Simd,
}

pub(crate) fn operation(i: &Instruction) -> Option<Operation> {
    if i.has_lock_prefix() || i.segment_prefix() == R::GS {
        return None;
    }
    if x87::supported(i) {
        Some(Operation::X87)
    } else if simd::supported(i) {
        Some(Operation::Simd)
    } else {
        None
    }
}

/// One x87 instruction executed from generated code without leaving the region.
/// Generated code holds a pointer to it; the owning `Compiled` keeps it alive.
pub(crate) struct NativeX87 {
    pub instruction: Instruction,
    pub opcode: Option<u16>,
    /// Set when the instruction has a host-double fast path.
    pub fast: Option<fast::Fast>,
}

/// Integer effects of an audited x87 helper. The canonical x87/MMX image,
/// memory/fault address and EIP may change; XMM, segment bases and fuel do not.
/// Memory writes remain transactional; faults retry at the saved EIP, while
/// SMC/pending #MF exits materialize the completed instruction's full state.
#[derive(Default)]
pub(crate) struct X87Effects {
    pub read_gpr: u8,
    pub write_gpr: u8,
    pub read_flags: bool,
    pub write_flags: bool,
}

pub(crate) fn x87_effects(i: &Instruction) -> Option<X87Effects> {
    x87::helper_effects(i)
}

/// The instruction completed; generated code continues at the next instruction.
pub(crate) const X87_DONE: u32 = 0;
/// Nothing changed; the instruction is retried by the interpreter.
pub(crate) const X87_FALLBACK: u32 = 1;
pub(crate) const X87_READ_FAULT: u32 = 2;
pub(crate) const X87_WRITE_FAULT: u32 = 3;
/// The instruction completed, but it wrote executable memory or left an
/// unmasked exception pending. Generated code must return to the executor.
pub(crate) const X87_EXIT: u32 = 4;

pub(crate) fn native_x87(i: &Instruction) -> bool {
    // FXSAVE/FXRSTOR also own XMM and MXCSR state.
    operation(i) == Some(Operation::X87) && !matches!(i.mnemonic(), M::Fxsave | M::Fxrstor)
}

/// Called only by native code with the Executor's live bridge and CpuState.
/// GPRs/flags need only be current where X87Effects declares an input; x87/MMX
/// and EIP must be canonical. Generated exits materialize all remaining state.
/// Runs the interpreter's own x87 transaction, so results are bit-identical.
pub(crate) unsafe extern "C" fn x87_call(
    bridge: *mut std::ffi::c_void,
    state: *mut CpuState,
    operation: *const NativeX87,
) -> u32 {
    // SAFETY: same ABI/lifetime contract as strings::repeat; the instruction is
    // owned by the Compiled that is executing.
    let (bridge, state, operation) = unsafe {
        (
            &mut *bridge.cast::<crate::trap::Bridge<'_>>(),
            &mut *state,
            &*operation,
        )
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the bridge's memory is the executor's exclusively borrowed Memory.
        let memory = unsafe { &mut *bridge.memory };
        if let Some(fast) = operation.fast
            && let Some(result) = fast::execute(operation, fast, state, memory)
        {
            return result;
        }
        let epoch = memory.epoch;
        match execute(
            &operation.instruction,
            Operation::X87,
            operation.opcode,
            state,
            memory,
            true,
        ) {
            Step::Executed if memory.epoch != epoch || state.x87.status & 0x80 != 0 => X87_EXIT,
            Step::Executed => X87_DONE,
            Step::ReadFault { address, .. } => {
                state.fault_addr = address;
                X87_READ_FAULT
            }
            Step::WriteFault { address, .. } => {
                state.fault_addr = address;
                X87_WRITE_FAULT
            }
            _ => X87_FALLBACK,
        }
    }))
    .unwrap_or(X87_FALLBACK)
}

pub(crate) fn execute(
    i: &Instruction,
    operation: Operation,
    opcode: Option<u16>,
    state: &mut CpuState,
    memory: &mut Memory,
    sync_mmx: bool,
) -> Step {
    let x87 = operation == Operation::X87;
    let pc = state.eip;
    let snapshot = Snapshot::capture(state, x87 && i.mnemonic() != M::Fxrstor);
    let mut tx = Transaction {
        state,
        pc,
        opcode,
        write: None,
        x87_exception: false,
        sse_exception: false,
    };
    tx.state.commit_mmx();
    let result = if x87 {
        x87::execute(i, &mut tx, memory)
    } else {
        simd::execute(i, &mut tx, memory)
    };
    if let Err(result) = result {
        snapshot.restore(tx.state);
        return result;
    }
    if tx.sse_exception {
        let mxcsr = tx.state.mxcsr;
        snapshot.restore(tx.state);
        tx.state.mxcsr = mxcsr;
        return Step::Trap { pc, vector: 19 };
    }
    if tx.x87_exception {
        // x87 arithmetic records an unmasked exception and suppresses the
        // result/pop/store. The next waiting instruction delivers #MF.
        let pending = tx.state.x87.clone();
        snapshot.restore(tx.state);
        tx.state.commit_mmx();
        tx.state.x87.status = (tx.state.x87.status & !0x82ff) | (pending.status & 0x82ff);
        tx.state.x87.ip = pending.ip;
        tx.state.x87.dp = pending.dp;
        tx.state.x87.cs = pending.cs;
        tx.state.x87.ds = pending.ds;
        tx.state.x87.opcode = pending.opcode;
        tx.state.eip = i.next_ip32();
        return Step::Executed;
    }
    if let Some(write) = tx.write {
        let bytes = if let Some(bytes) = write.large.as_deref() {
            bytes
        } else {
            &write.small[..write.len]
        };
        if let Some(mask) = write.mask {
            for (offset, byte) in bytes.iter().enumerate() {
                if mask & (1 << offset) != 0 {
                    // Every selected byte was checked before any store. Masked
                    // bytes have no memory access, including across page edges.
                    if memory
                        .write(write.address + offset as u32, &[*byte])
                        .is_err()
                    {
                        snapshot.restore(tx.state);
                        return Step::WriteFault {
                            pc,
                            address: write.address + offset as u32,
                        };
                    }
                }
            }
        } else if memory.write(write.address, bytes).is_err() {
            snapshot.restore(tx.state);
            return Step::WriteFault {
                pc,
                address: write.address,
            };
        }
    }
    tx.state.eip = i.next_ip32();
    if sync_mmx {
        tx.state.refresh_mmx();
    }
    Step::Executed
}

/// Ordinary x87 instructions only edit their own state, AX and EFLAGS. Keep
/// those fields restartable without copying XMM, MMX, segment or REP state for
/// every decoder operation. FXRSTOR and SIMD retain a complete rollback image.
#[allow(clippy::large_enum_variant)] // Inline rollback avoids allocations in both executor paths.
enum Snapshot {
    X87 {
        floating: crate::X87State,
        eax: u32,
        flags: u32,
        mmx_mode: u32,
    },
    Full(CpuState),
}

impl Snapshot {
    fn capture(state: &CpuState, x87: bool) -> Self {
        if x87 {
            Self::X87 {
                floating: state.x87.clone(),
                eax: state.regs[0],
                flags: state.eflags,
                mmx_mode: state.mmx_mode,
            }
        } else {
            Self::Full(state.clone())
        }
    }

    fn restore(self, state: &mut CpuState) {
        match self {
            Self::X87 {
                floating,
                eax,
                flags,
                mmx_mode,
            } => {
                state.x87 = floating;
                state.regs[0] = eax;
                state.eflags = flags;
                state.mmx_mode = mmx_mode;
            }
            Self::Full(original) => *state = original,
        }
    }
}

struct PendingWrite {
    address: u32,
    len: usize,
    small: [u8; 16],
    large: Option<Vec<u8>>,
    mask: Option<u16>,
}
struct Transaction<'a> {
    state: &'a mut CpuState,
    pc: u32,
    opcode: Option<u16>,
    write: Option<PendingWrite>,
    x87_exception: bool,
    sse_exception: bool,
}

impl Transaction<'_> {
    fn address(&self, i: &Instruction) -> Result<u32, Step> {
        let register = |r| {
            reg_index(r)
                .map(|slot| self.state.regs[slot] & if r.size() == 2 { 0xffff } else { u32::MAX })
        };
        let mut address = i.memory_displacement32();
        if i.memory_base() != R::None {
            address = address.wrapping_add(register(i.memory_base()).ok_or(Step::Unsupported {
                pc: self.pc,
                code: i.code(),
            })?);
        }
        if i.memory_index() != R::None {
            address = address.wrapping_add(
                register(i.memory_index())
                    .ok_or(Step::Unsupported {
                        pc: self.pc,
                        code: i.code(),
                    })?
                    .wrapping_mul(i.memory_index_scale()),
            );
        }
        if i.memory_base().size() == 2 || i.memory_index().size() == 2 {
            address &= 0xffff;
        }
        if i.segment_prefix() == R::FS {
            address = address.wrapping_add(self.state.fs_base);
        }
        Ok(address)
    }
    fn check(&self, memory: &Memory, address: u32, len: usize, write: bool) -> Result<(), Step> {
        let fault = |address| {
            if write {
                Step::WriteFault {
                    pc: self.pc,
                    address,
                }
            } else {
                Step::ReadFault {
                    pc: self.pc,
                    address,
                }
            }
        };
        if u64::from(address) + len as u64 > 1 << 32 {
            return Err(fault(address));
        }
        let mut offset = 0;
        while offset < len {
            let at = address + offset as u32;
            let range = memory.committed(at, 1).map_err(|_| fault(at))?;
            if memory.perms[range.start / PAGE_SIZE] & if write { WRITE } else { READ } == 0 {
                return Err(fault(at));
            }
            offset += (PAGE_SIZE - at as usize % PAGE_SIZE).min(len - offset);
        }
        Ok(())
    }
    fn read<'a>(
        &self,
        memory: &'a Memory,
        address: u32,
        len: usize,
    ) -> Result<std::borrow::Cow<'a, [u8]>, Step> {
        self.check(memory, address, len, false)?;
        memory.read_data(address, len).map_err(|_| Step::ReadFault {
            pc: self.pc,
            address,
        })
    }
    fn store(&mut self, memory: &Memory, address: u32, bytes: &[u8]) -> Result<(), Step> {
        self.check(memory, address, bytes.len(), true)?;
        let mut small = [0; 16];
        let large = if bytes.len() <= 16 {
            small[..bytes.len()].copy_from_slice(bytes);
            None
        } else {
            Some(bytes.to_vec())
        };
        self.write = Some(PendingWrite {
            address,
            len: bytes.len(),
            small,
            large,
            mask: None,
        });
        Ok(())
    }
    fn masked_store(
        &mut self,
        memory: &Memory,
        address: u32,
        bytes: &[u8],
        mask: u16,
    ) -> Result<(), Step> {
        for offset in 0..bytes.len() {
            if mask & (1 << offset) != 0 {
                let at = address.checked_add(offset as u32).ok_or(Step::WriteFault {
                    pc: self.pc,
                    address,
                })?;
                self.check(memory, at, 1, true)?;
            }
        }
        let mut small = [0; 16];
        small[..bytes.len()].copy_from_slice(bytes);
        self.write = Some(PendingWrite {
            address,
            len: bytes.len(),
            small,
            large: None,
            mask: Some(mask),
        });
        Ok(())
    }
    fn unsupported(&self, i: &Instruction) -> Step {
        Step::Unsupported {
            pc: self.pc,
            code: i.code(),
        }
    }
}
fn reg_index(r: R) -> Option<usize> {
    Some(match r {
        R::EAX | R::AX | R::AL | R::AH => 0,
        R::ECX | R::CX | R::CL | R::CH => 1,
        R::EDX | R::DX | R::DL | R::DH => 2,
        R::EBX | R::BX | R::BL | R::BH => 3,
        R::ESP | R::SP => 4,
        R::EBP | R::BP => 5,
        R::ESI | R::SI => 6,
        R::EDI | R::DI => 7,
        _ => return None,
    })
}
