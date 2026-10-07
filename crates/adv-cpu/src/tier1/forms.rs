//! Platform-independent instruction-form guards shared by Tier 1 and diagnostics.
use iced_x86::{Code, ConditionCode as C, Instruction, Mnemonic as M, OpKind, Register};

pub(super) fn register(r: Register) -> Option<u8> {
    use Register::*;
    Some(match r {
        EAX => 19,
        ECX => 20,
        EDX => 21,
        EBX => 22,
        ESP => 23,
        EBP => 24,
        ESI => 25,
        EDI => 26,
        _ => return Option::None,
    })
}
pub(super) fn register_part(r: Register) -> Option<(u8, u32, u32)> {
    let full = register(r.full_register32())?;
    let shift = u32::from(matches!(
        r,
        Register::AH | Register::CH | Register::DH | Register::BH
    )) * 8;
    let bits = r.size() as u32 * 8;
    matches!(bits, 8 | 16 | 32).then_some((full, shift, bits))
}
pub(super) fn setcc(m: M) -> bool {
    matches!(
        m,
        M::Seto
            | M::Setno
            | M::Setb
            | M::Setae
            | M::Sete
            | M::Setne
            | M::Setbe
            | M::Seta
            | M::Sets
            | M::Setns
            | M::Setp
            | M::Setnp
            | M::Setl
            | M::Setge
            | M::Setle
            | M::Setg
    )
}
pub(super) fn cmovcc(m: M) -> bool {
    matches!(
        m,
        M::Cmovo
            | M::Cmovno
            | M::Cmovb
            | M::Cmovae
            | M::Cmove
            | M::Cmovne
            | M::Cmovbe
            | M::Cmova
            | M::Cmovs
            | M::Cmovns
            | M::Cmovp
            | M::Cmovnp
            | M::Cmovl
            | M::Cmovge
            | M::Cmovle
            | M::Cmovg
    )
}
pub(super) fn shift(m: M) -> bool {
    matches!(m, M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror)
}
pub(super) fn double_shift(i: &Instruction) -> bool {
    matches!(i.mnemonic(), M::Shld | M::Shrd)
        && i.op1_kind() == OpKind::Register
        && register_part(i.op1_register()).is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
        && (i.op2_kind() == OpKind::Immediate8
            || i.op2_kind() == OpKind::Register && i.op2_register() == Register::CL)
}
pub(super) fn shift_count(i: &Instruction) -> bool {
    i.op1_kind() == OpKind::Immediate8
        || i.op1_kind() == OpKind::Register && i.op1_register() == Register::CL
}
pub(super) fn imm(i: &Instruction, op: u32) -> Option<u32> {
    matches!(i.op_kind(op), OpKind::Immediate32 | OpKind::Immediate8to32)
        .then(|| i.immediate(op) as u32)
}
pub(super) fn source(i: &Instruction, op: u32) -> bool {
    i.op_kind(op) == OpKind::Register && register(i.op_register(op)).is_some()
        || imm(i, op).is_some()
}
pub(super) fn scalar32(i: &Instruction, op: u32) -> bool {
    i.op_kind(op) == OpKind::Register && register(i.op_register(op)).is_some()
        || i.op_kind(op) == OpKind::Memory && i.memory_size().size() == 4 && address(i)
}
pub(super) fn rmw_source(i: &Instruction) -> bool {
    i.op1_kind() == OpKind::Register && register(i.op1_register().full_register32()).is_some()
        || matches!(
            i.op1_kind(),
            OpKind::Immediate8
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
        )
}
pub(super) fn rmw(i: &Instruction) -> bool {
    i.op0_kind() == OpKind::Memory
        && matches!(i.memory_size().size(), 1 | 2 | 4)
        && address(i)
        && match i.mnemonic() {
            M::Add | M::Adc | M::Sub | M::Sbb | M::And | M::Or | M::Xor => rmw_source(i),
            M::Inc | M::Dec | M::Not | M::Neg => true,
            m if shift(m) => shift_count(i),
            M::Shld | M::Shrd => i.memory_size().size() >= 2 && double_shift(i),
            _ => false,
        }
}
pub(super) fn address(i: &Instruction) -> bool {
    // Word addressing has different wrapping and remains in Tier 0.
    (i.memory_base() == Register::None || register(i.memory_base()).is_some())
        && (i.memory_index() == Register::None || register(i.memory_index()).is_some())
        && i.memory_displ_size() != 2
        && i.segment_prefix() != Register::GS
}

/// (width, reads ESI, writes EDI). Match codes rather than the SIMD MOVSD name.
pub(super) fn string(i: &Instruction) -> Option<(u32, bool, bool)> {
    let form = match i.code() {
        Code::Movsb_m8_m8 => (1, true, true),
        Code::Movsw_m16_m16 => (2, true, true),
        Code::Movsd_m32_m32 => (4, true, true),
        Code::Stosb_m8_AL => (1, false, true),
        Code::Stosw_m16_AX => (2, false, true),
        Code::Stosd_m32_EAX => (4, false, true),
        Code::Lodsb_AL_m8 => (1, true, false),
        Code::Lodsw_AX_m16 => (2, true, false),
        Code::Lodsd_EAX_m32 => (4, true, false),
        _ => return None,
    };
    ((0..i.op_count()).all(|n| {
        matches!(
            i.op_kind(n),
            OpKind::Register | OpKind::MemorySegESI | OpKind::MemoryESEDI
        )
    }) && i.segment_prefix() != Register::GS)
        .then_some(form)
}

pub(super) fn counter_branch(i: &Instruction) -> Option<bool> {
    Some(match i.code() {
        Code::Loop_rel8_16_CX
        | Code::Loop_rel8_32_CX
        | Code::Loope_rel8_16_CX
        | Code::Loope_rel8_32_CX
        | Code::Loopne_rel8_16_CX
        | Code::Loopne_rel8_32_CX
        | Code::Jcxz_rel8_16
        | Code::Jcxz_rel8_32 => true,
        Code::Loop_rel8_16_ECX
        | Code::Loop_rel8_32_ECX
        | Code::Loope_rel8_16_ECX
        | Code::Loope_rel8_32_ECX
        | Code::Loopne_rel8_16_ECX
        | Code::Loopne_rel8_32_ECX
        | Code::Jecxz_rel8_16
        | Code::Jecxz_rel8_32 => false,
        _ => return None,
    })
}

pub(crate) fn supported(i: &Instruction) -> bool {
    if i.has_lock_prefix() {
        return false;
    }
    if string(i).is_some() {
        return true;
    }
    if i.has_rep_prefix() || i.has_repne_prefix() {
        return false;
    }
    if rmw(i) {
        return true;
    }
    if counter_branch(i).is_some() {
        return true;
    }
    let dst = i.op0_kind() == OpKind::Register && register(i.op0_register()).is_some();
    match i.mnemonic() {
        M::Nop => true,
        M::Mov => {
            dst && source(i, 1)
                || dst
                    && i.op1_kind() == OpKind::Memory
                    && i.memory_size().size() == 4
                    && address(i)
                || i.op0_kind() == OpKind::Memory
                    && i.memory_size().size() == 4
                    && source(i, 1)
                    && address(i)
        }
        M::Lea => dst && i.op1_kind() == OpKind::Memory && address(i),
        M::Movzx | M::Movsx => {
            dst && i.op1_kind() == OpKind::Memory
                && matches!(i.memory_size().size(), 1 | 2)
                && address(i)
                || i.op0_kind() == OpKind::Register
                    && register_part(i.op0_register())
                        .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                    && i.op1_kind() == OpKind::Register
                    && register_part(i.op1_register())
                        .is_some_and(|(_, _, bits)| matches!(bits, 8 | 16))
        }
        M::Imul => {
            i.op_count() == 1 && scalar32(i, 0)
                || dst
                    && matches!(i.op_count(), 2 | 3)
                    && (i.op1_kind() == OpKind::Register && register(i.op1_register()).is_some()
                        || i.op1_kind() == OpKind::Memory
                            && i.memory_size().size() == 4
                            && address(i))
                    && (i.op_count() == 2 || imm(i, 2).is_some())
        }
        M::Mul | M::Div | M::Idiv => scalar32(i, 0),
        M::Bt | M::Bts | M::Btr | M::Btc => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && (i.op1_kind() == OpKind::Immediate8
                    || i.op1_kind() == OpKind::Register
                        && register_part(i.op1_register()).is_some())
        }
        M::Bsf | M::Bsr => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && (i.op1_kind() == OpKind::Register && register_part(i.op1_register()).is_some()
                    || i.op1_kind() == OpKind::Memory && address(i))
        }
        M::Bswap => dst,
        M::Cdq => true,
        M::Pushad | M::Popad => true,
        M::Shld | M::Shrd => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && double_shift(i)
        }
        M::Xchg => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register()).is_some()
                && i.op1_kind() == OpKind::Register
                && register_part(i.op1_register()).is_some()
        }
        M::Add | M::Adc | M::Sub | M::Sbb | M::Cmp | M::And | M::Or | M::Xor | M::Test => {
            let memory =
                |op| i.op_kind(op) == OpKind::Memory && i.memory_size().size() == 4 && address(i);
            dst && (source(i, 1) || memory(1))
                || !matches!(i.mnemonic(), M::Adc | M::Sbb)
                    && i.op0_kind() == OpKind::Register
                    && register_part(i.op0_register()).is_some_and(|(_, _, bits)| bits < 32)
                    && rmw_source(i)
                // Memory destinations that do not write use the same ALU.
                || matches!(i.mnemonic(), M::Cmp | M::Test) && memory(0) && source(i, 1)
        }
        M::Push => i.stack_pointer_increment() == -4 && source(i, 0),
        M::Pop => dst && i.stack_pointer_increment() == 4,
        M::Call | M::Jmp => {
            i.op0_kind() == OpKind::NearBranch32
                || dst
                || i.op0_kind() == OpKind::Memory && i.memory_size().size() == 4 && address(i)
        }
        M::Ret => i.op_count() == 0 || i.op0_kind() == OpKind::Immediate16,
        M::Inc | M::Dec | M::Neg | M::Not => {
            i.op0_kind() == OpKind::Register && register_part(i.op0_register()).is_some()
        }
        m if shift(m) => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register()).is_some()
                && shift_count(i)
        }
        _ if setcc(i.mnemonic()) => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register()).is_some_and(|(_, _, bits)| bits == 8)
                || i.op0_kind() == OpKind::Memory && address(i)
        }
        _ if cmovcc(i.mnemonic()) => {
            i.op0_kind() == OpKind::Register
                && register_part(i.op0_register())
                    .is_some_and(|(_, _, bits)| matches!(bits, 16 | 32))
                && (i.op1_kind() == OpKind::Register && register_part(i.op1_register()).is_some()
                    || i.op1_kind() == OpKind::Memory && address(i))
        }
        _ => {
            i.condition_code() != C::None
                && i.is_jcc_short_or_near()
                && i.op0_kind() == OpKind::NearBranch32
        }
    }
}
