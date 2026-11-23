//! Transactional x87, SSE and packed-integer execution on canonical CPU state.
use crate::{CpuState, Memory, PAGE_SIZE, READ, WRITE, X87State, interpreter::Step};
use iced_x86::{Instruction, Mnemonic as M, Register as R};
pub mod fast;

pub mod packed;
mod simd;
pub mod softfloat;
mod transcendental;
mod x87;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    X87,
    Simd,
}

pub fn operation(i: &Instruction) -> Option<Operation> {
    if i.has_lock_prefix() {
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
pub struct NativeX87 {
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
pub struct X87Effects {
    pub read_gpr: u8,
    pub write_gpr: u8,
    pub read_flags: bool,
    pub write_flags: bool,
}

pub fn x87_effects(i: &Instruction) -> Option<X87Effects> {
    x87::helper_effects(i)
}

pub fn execute(
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
    state.commit_mmx();
    let mut tx = Transaction::new(Registers::cpu(state), pc, opcode);
    if let Err(result) = tx.execute(i, operation, Operands::Guest(memory)) {
        snapshot.restore(state);
        return result;
    }
    if tx.sse_exception {
        let mxcsr = *tx.state.mxcsr;
        snapshot.restore(state);
        state.mxcsr = mxcsr;
        return Step::Trap { pc, vector: 19 };
    }
    if tx.x87_exception {
        let pending = tx.state.x87.clone();
        let dp = tx.dp;
        snapshot.restore(state);
        state.commit_mmx();
        commit_x87_exception(&mut state.x87, &pending, dp);
        state.eip = i.next_ip32();
        return Step::Executed;
    }
    if let Some(write) = tx.write {
        let bytes = write.bytes();
        if let Some(mask) = write.mask {
            for (offset, byte) in bytes.iter().enumerate() {
                if mask & (1 << offset) != 0 {
                    // Every selected byte was checked before any store. Masked
                    // bytes have no memory access, including across page edges.
                    if memory
                        .write(write.address + offset as u32, &[*byte])
                        .is_err()
                    {
                        snapshot.restore(state);
                        return Step::WriteFault {
                            pc,
                            address: write.address + offset as u32,
                        };
                    }
                }
            }
        } else if memory.write(write.address, bytes).is_err() {
            snapshot.restore(state);
            return Step::WriteFault {
                pc,
                address: write.address,
            };
        }
    }
    state.eip = i.next_ip32();
    if sync_mmx {
        state.refresh_mmx();
    }
    Step::Executed
}

/// An instruction decoded once for evaluation on supplied operands.
#[derive(Clone, Copy)]
pub struct ValueInstruction {
    instruction: Instruction,
    operation: Operation,
    opcode: Option<u16>,
}

impl ValueInstruction {
    pub fn new(instruction: Instruction, opcode: Option<u16>) -> Option<Self> {
        Some(Self {
            operation: operation(&instruction)?,
            instruction,
            opcode,
        })
    }

    /// The state and staged access used by an x87 value instruction.
    pub fn x87_operands(&self) -> Option<X87Operands> {
        if self.operation != Operation::X87 {
            return None;
        }
        let i = &self.instruction;
        let bytes = i.memory_size().size() as u16;
        let mnemonic = i.mnemonic();
        let access = if !i.op_kinds().any(|kind| kind == iced_x86::OpKind::Memory) {
            None
        } else if matches!(
            mnemonic,
            M::Fst
                | M::Fstp
                | M::Fist
                | M::Fistp
                | M::Fisttp
                | M::Fbstp
                | M::Fstcw
                | M::Fnstcw
                | M::Fstsw
                | M::Fnstsw
                | M::Fstenv
                | M::Fnstenv
                | M::Fsave
                | M::Fnsave
                | M::Fxsave
        ) {
            Some(ValueAccess::Write(bytes))
        } else {
            Some(ValueAccess::Read(bytes))
        };
        Some(X87Operands {
            access,
            flags: x87_effects(i).unwrap_or_default(),
            waits: x87::waits(mnemonic),
            save_fx: mnemonic == M::Fxsave,
            restore_fx: mnemonic == M::Fxrstor,
        })
    }

    pub fn decoded_form(&self) -> u16 {
        self.instruction.code() as u16
    }

    /// Evaluate with canonical x87 aliases and the operands required by this
    /// instruction. The caller commits the returned destinations and store.
    pub fn evaluate(&self, frame: &mut ValueFrame) -> ValueOutcome {
        let pc = self.instruction.ip32();
        let before = (self.operation == Operation::X87).then(|| frame.x87.clone());
        let operands = Operands::Loaded {
            address: frame.address,
            bytes: &frame.operand,
        };
        let mut tx = Transaction::new(
            Registers {
                regs: &mut frame.regs,
                eflags: &mut frame.eflags,
                xmm: &mut frame.xmm,
                mmx: &mut frame.mmx,
                x87: &mut frame.x87,
                mxcsr: &mut frame.mxcsr,
                fs_base: 0,
            },
            pc,
            self.opcode,
        );
        match tx.execute(&self.instruction, self.operation, operands) {
            Ok(()) => (),
            Err(Step::Trap { vector, .. }) => return ValueOutcome::Exception(vector),
            Err(Step::Unsupported { .. }) => return ValueOutcome::Unsupported,
            Err(step) => unreachable!("prepared value instruction returned {step:?}"),
        }
        if tx.sse_exception {
            return ValueOutcome::Exception(19);
        }
        if tx.x87_exception {
            let pending = std::mem::replace(tx.state.x87, before.unwrap());
            commit_x87_exception(tx.state.x87, &pending, tx.dp);
            return ValueOutcome::Complete;
        }
        if let Some(write) = tx.write {
            let bytes = write.bytes();
            frame.operand[..bytes.len()].copy_from_slice(bytes);
            return ValueOutcome::Store {
                len: bytes.len() as u16,
                mask: write.mask,
            };
        }
        ValueOutcome::Complete
    }
}

#[derive(Clone, Copy)]
pub enum ValueAccess {
    Read(u16),
    Write(u16),
}

pub struct X87Operands {
    pub access: Option<ValueAccess>,
    pub flags: X87Effects,
    pub waits: bool,
    pub save_fx: bool,
    pub restore_fx: bool,
}

/// Register operands and a prechecked memory operand. A store returns its
/// bytes in `operand`. This image has no execution, scheduling or paging state.
#[derive(Debug)]
#[repr(C, align(16))]
pub struct ValueFrame {
    pub operand: [u8; 512],
    pub xmm: [[u8; 16]; 8],
    pub regs: [u32; 8],
    pub eflags: u32,
    pub mmx: [u64; 8],
    pub x87: X87State,
    pub mxcsr: u32,
    pub address: u32,
}

impl Default for ValueFrame {
    fn default() -> Self {
        Self {
            regs: [0; 8],
            eflags: 0,
            xmm: [[0; 16]; 8],
            mmx: [0; 8],
            x87: X87State::default(),
            mxcsr: 0x1f80,
            address: 0,
            operand: [0; 512],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueOutcome {
    Complete,
    Store {
        len: u16,
        mask: Option<u16>,
    },
    /// Only the committed exception state (MXCSR for #XM) is returned.
    Exception(u8),
    Unsupported,
}

fn commit_x87_exception(x87: &mut X87State, pending: &X87State, dp: Option<u32>) {
    // Unmasked x87 arithmetic suppresses the result/pop/store. The next
    // waiting instruction raises #MF from this committed status and metadata.
    x87.status = (x87.status & !0x82ff) | (pending.status & 0x82ff);
    x87.ip = pending.ip;
    if let Some(dp) = dp {
        x87.dp = dp;
    }
    x87.cs = pending.cs;
    x87.ds = pending.ds;
    x87.opcode = pending.opcode;
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

pub fn check_access(
    memory: &Memory,
    pc: u32,
    address: u32,
    len: usize,
    write: bool,
) -> Result<(), Step> {
    let fault = |address| {
        if write {
            Step::WriteFault { pc, address }
        } else {
            Step::ReadFault { pc, address }
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

struct PendingWrite {
    address: u32,
    len: usize,
    small: [u8; 16],
    large: Option<Vec<u8>>,
    mask: Option<u16>,
}
impl PendingWrite {
    fn bytes(&self) -> &[u8] {
        self.large
            .as_deref()
            .unwrap_or(&self.small[..self.len.min(16)])
    }
}

struct Registers<'a> {
    regs: &'a mut [u32; 8],
    eflags: &'a mut u32,
    xmm: &'a mut [[u8; 16]; 8],
    mmx: &'a mut [u64; 8],
    x87: &'a mut X87State,
    mxcsr: &'a mut u32,
    fs_base: u32,
}

impl<'a> Registers<'a> {
    fn cpu(state: &'a mut CpuState) -> Self {
        Self {
            regs: &mut state.regs,
            eflags: &mut state.eflags,
            xmm: &mut state.xmm,
            mmx: &mut state.mmx,
            x87: &mut state.x87,
            mxcsr: &mut state.mxcsr,
            fs_base: state.fs_base,
        }
    }
}

#[derive(Clone, Copy)]
enum Operands<'a> {
    Guest(&'a Memory),
    Loaded { address: u32, bytes: &'a [u8] },
}

struct Transaction<'a> {
    state: Registers<'a>,
    pc: u32,
    opcode: Option<u16>,
    write: Option<PendingWrite>,
    /// The memory operand an unmasked x87 exception records (EXEC-CORE-8).
    dp: Option<u32>,
    x87_exception: bool,
    sse_exception: bool,
}

impl<'a> Transaction<'a> {
    fn new(state: Registers<'a>, pc: u32, opcode: Option<u16>) -> Self {
        Self {
            state,
            pc,
            opcode,
            write: None,
            dp: None,
            x87_exception: false,
            sse_exception: false,
        }
    }

    fn execute(
        &mut self,
        i: &Instruction,
        operation: Operation,
        operands: Operands<'_>,
    ) -> Result<(), Step> {
        match operation {
            Operation::X87 => x87::execute(i, self, operands),
            Operation::Simd => simd::execute(i, self, operands),
        }
    }

    fn address(&self, i: &Instruction, operands: Operands<'_>) -> Result<u32, Step> {
        if let Operands::Loaded { address, .. } = operands {
            return Ok(address);
        }
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

    fn check(
        &self,
        operands: Operands<'_>,
        address: u32,
        len: usize,
        write: bool,
    ) -> Result<(), Step> {
        if let Operands::Guest(memory) = operands {
            check_access(memory, self.pc, address, len, write)?;
        }
        Ok(())
    }

    fn read<'input>(
        &self,
        operands: Operands<'input>,
        address: u32,
        len: usize,
    ) -> Result<std::borrow::Cow<'input, [u8]>, Step> {
        match operands {
            Operands::Guest(memory) => {
                check_access(memory, self.pc, address, len, false)?;
                memory.read_data(address, len).map_err(|_| Step::ReadFault {
                    pc: self.pc,
                    address,
                })
            }
            Operands::Loaded { bytes, .. } => Ok(std::borrow::Cow::Borrowed(&bytes[..len])),
        }
    }

    fn store(&mut self, operands: Operands<'_>, address: u32, bytes: &[u8]) -> Result<(), Step> {
        self.check(operands, address, bytes.len(), true)?;
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
        operands: Operands<'_>,
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
                self.check(operands, at, 1, true)?;
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

/// Execute a register-only x87 operation using the interpreter's arithmetic.
///
/// # Safety
/// `state` is the live exclusive CPU image; `code` names a transcendental form.
/// Only x87 state and its MMX aliases are read or written.
pub unsafe extern "C" fn x87_leaf(state: *mut CpuState, code: u32, pc: u32, opcode: u32) -> u32 {
    // SAFETY: the translated run owns this CPU image for the call.
    let state = unsafe { &mut *state };
    let before = state.x87.clone();
    let mut instruction = Instruction::default();
    instruction.set_code(iced_x86::Code::try_from(code as usize).unwrap());
    let mut tx = Transaction::new(Registers::cpu(state), pc, Some(opcode as u16));
    if x87::transcendental(&instruction, &mut tx).is_err() {
        state.x87 = before;
        return 1;
    }
    if tx.x87_exception {
        let pending = std::mem::replace(tx.state.x87, before);
        commit_x87_exception(tx.state.x87, &pending, None);
    }
    state.refresh_mmx();
    0
}

#[cfg(test)]
mod leaf_tests {
    use super::*;
    use crate::X87Register as Ext;
    use iced_x86::Code;

    // spec: EXEC-CORE-8
    #[test]
    fn the_data_pointer_records_only_unmasked_exceptions() {
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory
            .protect(0x10000, 4096, crate::memory::READ | crate::memory::WRITE)
            .unwrap();
        memory.write(0x10000, &1.0f64.to_le_bytes()).unwrap();
        memory.write(0x10008, &0.0f64.to_le_bytes()).unwrap();
        let mut state = CpuState {
            eip: 0x8000,
            ..Default::default()
        };
        // Zero divide unmasked.
        state.x87.control = 0x37b;
        state.x87.dp = 0xdead;
        let mut step = |bytes: &[u8], state: &mut CpuState| {
            let i = iced_x86::Decoder::with_ip(32, bytes, u64::from(state.eip), 0).decode();
            execute(&i, Operation::X87, None, state, &mut memory, true)
        };
        // fld qword [0x10000]
        assert_eq!(
            step(&[0xdd, 0x05, 0, 0, 0x01, 0], &mut state),
            Step::Executed
        );
        assert_eq!(state.x87.dp, 0xdead);
        // fdiv qword [0x10008]
        assert_eq!(
            step(&[0xdc, 0x35, 0x08, 0, 0x01, 0], &mut state),
            Step::Executed
        );
        assert_ne!(state.x87.status & 0x80, 0, "the exception is pending");
        assert_eq!(state.x87.dp, 0x10008);
    }

    // spec: A64F-FP-8
    #[test]
    fn x87_leaf_matches_transactions_for_rounding_precision_and_exceptions() {
        let values = [
            Ext::from_parts(0, 0),
            Ext::from_parts(0, 0x8000),
            Ext::from_parts(0xc000000000000000, 0x3ffd),
            Ext::from_parts(0xa000000000000000, 0xc000),
            Ext::from_parts(0x8000000000000001, 0x3fff),
            Ext::from_parts(0x8000000000000000, 0x7fff),
            Ext::from_parts(0x8000000000000000, 0xffff),
            Ext::from_parts(0xc000000000000011, 0x7fff),
            Ext::from_parts(0x8000000000000001, 0x7fff),
            Ext::from_parts(1, 0),
            Ext::from_parts(0x8000000000000000, 0),
            Ext::from_parts(1, 0x4000),
            Ext::from_parts(0x8000000000000000, 1),
            Ext::from_parts(0x8000000000000000, 0x7ffe),
        ];
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        for code in [
            Code::Fsin,
            Code::Fcos,
            Code::Fsincos,
            Code::Fptan,
            Code::Fpatan,
            Code::F2xm1,
            Code::Fyl2x,
            Code::Fyl2xp1,
            Code::Fscale,
            Code::Fprem,
            Code::Fprem1,
        ] {
            let mut instruction = Instruction::default();
            instruction.set_code(code);
            instruction.set_next_ip(0x10002);
            for (n, &value) in values.iter().enumerate() {
                for rc in 0..4 {
                    for pc in [0, 0x200, 0x300] {
                        for masks in [0x3f, 0x1f, 0x3e, 0x3d, 0] {
                            for empty in [false, true] {
                                let mut state = CpuState {
                                    eip: 0x10000,
                                    ..Default::default()
                                };
                                state.x87.control = 0x40 | pc | rc << 10 | masks;
                                state.x87.set_top(5);
                                state.x87.regs[5] = value;
                                state.x87.regs[6] = values[(n + 3) % values.len()];
                                state
                                    .x87
                                    .set_physical_tag(5, if empty { 3 } else { value.tag() });
                                state.x87.set_physical_tag(6, state.x87.regs[6].tag());
                                state.refresh_mmx();
                                let mut expected = state.clone();
                                let outcome = execute(
                                    &instruction,
                                    Operation::X87,
                                    Some(0x123),
                                    &mut expected,
                                    &mut memory,
                                    true,
                                );
                                let mut actual = state.clone();
                                // SAFETY: this is an exclusive CPU image and an audited register form.
                                let status =
                                    unsafe { x87_leaf(&mut actual, code as u32, state.eip, 0x123) };
                                if outcome == Step::Executed {
                                    assert_eq!(status, 0);
                                    assert_eq!(
                                        actual.x87, expected.x87,
                                        "{code:?} {value:?} control={:#x}",
                                        state.x87.control
                                    );
                                    assert_eq!(actual.mmx, expected.mmx);
                                } else {
                                    assert_eq!(status, 1);
                                    assert_eq!(actual.x87, state.x87);
                                }
                                assert_eq!(actual.regs, state.regs);
                                assert_eq!(actual.eflags, state.eflags);
                                assert_eq!(actual.eip, state.eip);
                            }
                        }
                    }
                }
            }
        }
    }
}
