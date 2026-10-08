// Original host-code recovery experiment, included only by the test build.
mod backend;
mod capabilities;
mod corpus;
mod lower;
mod memory;
mod tests;

use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic as M, OpKind, Register,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// Explicit recovery spills at arbitrary scalar trapping instructions.
    Backend,
    /// Private stable memory: entry proofs, otherwise checked cold exits.
    Proof,
}

#[derive(Clone, Copy, Debug)]
struct State<T> {
    gpr: [T; 8],
    flags: T,
}

#[derive(Clone, Copy, Debug)]
struct Recovery<T> {
    state: State<T>,
    pc: T,
    committed: T,
    address: T,
}

#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Frame {
    gpr: [u32; 8],
    flags: u32,
    pc: u32,
    committed: u32,
    address: u32,
}

impl Frame {
    fn from_cpu(cpu: &crate::CpuState) -> Self {
        Self {
            gpr: cpu.regs,
            flags: cpu.eflags,
            pc: cpu.eip,
            ..Self::default()
        }
    }
    fn restore(&self, cpu: &mut crate::CpuState) {
        cpu.regs = self.gpr;
        cpu.eflags = self.flags;
        cpu.eip = self.pc;
    }
}

const DONE: u32 = 0;
const CHECK: u32 = 1;
const INTERRUPT: u32 = 2;
const BUDGET: u32 = 3;
const HARDWARE: u32 = 0x1279;

#[derive(Clone)]
struct Graph {
    instructions: BTreeMap<u32, Instruction>,
    returns: BTreeSet<u32>,
    start: u32,
    invariant: u8,
}

fn reg(r: Register) -> Option<usize> {
    (Register::EAX..=Register::EDI)
        .contains(&r)
        .then(|| r as usize - Register::EAX as usize)
}

impl Graph {
    fn decode(bytes: &[u8], start: u32) -> Result<Self, String> {
        Self::from_instructions(
            Decoder::with_ip(32, bytes, u64::from(start), DecoderOptions::NONE),
            start,
        )
    }
    fn from_instructions(
        decoded: impl IntoIterator<Item = Instruction>,
        start: u32,
    ) -> Result<Self, String> {
        let mut instructions = BTreeMap::new();
        let mut returns = BTreeSet::new();
        let mut invariant = 0xff;
        let mut factory = iced_x86::InstructionInfoFactory::new();
        for i in decoded {
            if !supported(&i) {
                return Err(format!("unsupported {:?}", i.code()));
            }
            if i.flow_control() == FlowControl::Call {
                returns.insert(i.next_ip32());
            }
            for r in factory.info(&i).used_registers() {
                if matches!(
                    r.access(),
                    iced_x86::OpAccess::Write
                        | iced_x86::OpAccess::CondWrite
                        | iced_x86::OpAccess::ReadWrite
                        | iced_x86::OpAccess::ReadCondWrite
                ) && let Some(n) = reg(r.register().full_register32())
                {
                    invariant &= !(1 << n);
                }
            }
            instructions.insert(i.ip32(), i);
        }
        if instructions.is_empty() || instructions.len() > 256 {
            return Err("region size".into());
        }
        Ok(Self {
            instructions,
            returns,
            start,
            invariant,
        })
    }

    fn invariant_address(&self, i: &Instruction, frame: &Frame) -> Option<u32> {
        if i.op0_kind() != OpKind::Memory && i.op1_kind() != OpKind::Memory {
            return None;
        }
        let component = |r| {
            if r == Register::None {
                Some(0)
            } else {
                let n = reg(r)?;
                (self.invariant & (1 << n) != 0).then_some(frame.gpr[n])
            }
        };
        Some(
            component(i.memory_base())?
                .wrapping_add(component(i.memory_index())?.wrapping_mul(i.memory_index_scale()))
                .wrapping_add(i.memory_displacement32()),
        )
    }
}

fn supported(i: &Instruction) -> bool {
    if i.is_invalid()
        || i.has_lock_prefix()
        || i.has_rep_prefix()
        || i.has_repne_prefix()
        || !matches!(
            i.segment_prefix(),
            Register::None | Register::DS | Register::SS | Register::ES
        )
    {
        return false;
    }
    if (0..i.op_count())
        .any(|n| i.op_kind(n) == OpKind::Register && reg(i.op_register(n)).is_none())
    {
        return false;
    }
    if i.memory_base() != Register::None && reg(i.memory_base()).is_none()
        || i.memory_index() != Register::None && reg(i.memory_index()).is_none()
    {
        return false;
    }
    match i.mnemonic() {
        M::Nop => true,
        M::Mov | M::Add | M::Sub | M::Cmp | M::And | M::Or | M::Xor | M::Test => {
            (0..i.op_count()).all(|n| {
                matches!(
                    i.op_kind(n),
                    OpKind::Register
                        | OpKind::Memory
                        | OpKind::Immediate32
                        | OpKind::Immediate8to32
                )
            }) && (!(0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory)
                || i.memory_size().size() == 4)
        }
        M::Lea => i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Memory,
        M::Inc | M::Dec | M::Not => i.op0_kind() == OpKind::Register,
        M::Push => matches!(
            i.op0_kind(),
            OpKind::Register | OpKind::Immediate32 | OpKind::Immediate8to32
        ),
        M::Pop => i.op0_kind() == OpKind::Register && i.op0_register() != Register::ESP,
        M::Call | M::Jmp => i.op0_kind() == OpKind::NearBranch32,
        M::Ret => i.code() == iced_x86::Code::Retnd,
        _ => {
            i.flow_control() == FlowControl::ConditionalBranch
                && i.op0_kind() == OpKind::NearBranch32
                && i.condition_code() != iced_x86::ConditionCode::None
        }
    }
}
mod measure_g12;
