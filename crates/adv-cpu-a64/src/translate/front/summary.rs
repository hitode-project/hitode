//! Instruction effects shared by the optimizer, split proof and arbiter.

use iced_x86::{
    CodeSize, CpuidFeature, Instruction, InstructionInfoFactory, Mnemonic as M, OpAccess, OpKind,
    Register,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stream(Register, Register, Register, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RegisterUse {
    pub register: Register,
    pub access: OpAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemoryUse {
    pub segment: Register,
    pub base: Register,
    pub index: Register,
    pub scale: u32,
    pub displacement: u32,
    pub width: u32,
    pub address_bits: u8,
    pub access: OpAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Carried {
    pub flags_read: u32,
    pub flags_modified: u32,
    pub conditional_flags: bool,
    pub emms: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Summary {
    pub stream: Option<Stream>,
    pub registers: Box<[RegisterUse]>,
    pub memory: Box<[MemoryUse]>,
    pub stack: Option<i32>,
    pub carried: Option<Carried>,
}

impl Summary {
    pub(super) fn new(instruction: &Instruction, factory: &mut InstructionInfoFactory) -> Self {
        let info = factory.info(instruction);
        Self {
            stream: instruction
                .op_kinds()
                .any(|kind| kind == OpKind::Memory)
                .then(|| {
                    Stream(
                        instruction.memory_segment(),
                        instruction.memory_base(),
                        instruction.memory_index(),
                        instruction.memory_index_scale(),
                    )
                }),
            registers: info
                .used_registers()
                .iter()
                .map(|used| RegisterUse {
                    register: used.register(),
                    access: used.access(),
                })
                .collect(),
            memory: info
                .used_memory()
                .iter()
                .map(|used| MemoryUse {
                    segment: used.segment(),
                    base: used.base(),
                    index: used.index(),
                    scale: used.scale(),
                    displacement: used.displacement() as u32,
                    width: used.memory_size().size() as u32,
                    address_bits: if used.address_size() == CodeSize::Code16 {
                        16
                    } else {
                        32
                    },
                    access: used.access(),
                })
                .collect(),
            stack: stack(instruction),
            carried: carried(instruction),
        }
    }
}

fn carried(ins: &Instruction) -> Option<Carried> {
    let mnemonic = ins.mnemonic();
    if ins.cpuid_features().contains(&CpuidFeature::FPU)
        || matches!(mnemonic, M::Ldmxcsr | M::Stmxcsr | M::Fxrstor | M::Fxsave)
    {
        return None;
    }
    let conditional_flags = matches!(
        mnemonic,
        M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld | M::Shrd
    ) && {
        let last = ins.op_count() - 1;
        ins.op_kind(last) != OpKind::Immediate8 || ins.immediate8() & 31 == 0
    };
    Some(Carried {
        flags_read: ins.rflags_read(),
        flags_modified: ins.rflags_modified(),
        conditional_flags: conditional_flags && ins.rflags_modified() != 0,
        emms: mnemonic == M::Emms,
    })
}

fn stack(ins: &Instruction) -> Option<i32> {
    let mnemonic = ins.mnemonic();
    let adjust = matches!(mnemonic, M::Add | M::Sub)
        && ins.op0_kind() == OpKind::Register
        && ins.op0_register() == Register::ESP
        && !matches!(ins.op1_kind(), OpKind::Register | OpKind::Memory);
    let named = (0..ins.op_count())
        .any(|op| ins.op_kind(op) == OpKind::Register && ins.op_register(op) == Register::ESP);
    let indexed = ins.op_kinds().any(|op| op == OpKind::Memory)
        && (ins.memory_index() == Register::ESP
            || ins.memory_base() == Register::ESP
                && (ins.memory_index() != Register::None || mnemonic == M::Lea));
    if named && !adjust || indexed || matches!(mnemonic, M::Leave | M::Enter) {
        return None;
    }
    let mut stack = ins.stack_pointer_increment();
    if adjust {
        let value = ins.immediate(1) as i32;
        stack += if mnemonic == M::Add {
            value
        } else {
            value.wrapping_neg()
        };
    }
    Some(stack)
}

#[cfg(test)]
pub(crate) fn from_bytes(bytes: &[u8], pc: u32) -> Option<std::sync::Arc<Summary>> {
    let instruction = iced_x86::Decoder::with_ip(32, bytes, u64::from(pc), 0).decode();
    (!instruction.is_invalid()).then(|| {
        std::sync::Arc::new(Summary::new(
            &instruction,
            &mut InstructionInfoFactory::new(),
        ))
    })
}
