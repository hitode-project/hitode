//! Access analysis of quick-decoded operands uses canonical instruction
//! forms. Only operand fields are needed here, so no guest bytes are decoded.

use adv_cpu_core::quick::{Alu, Mem, Narrow8or16, QuickOp, ShiftKind, Src};
use iced_x86::{Code, CodeSize, ConditionCode, Instruction, OpKind, Register};

fn gpr(index: u8, width: u8) -> Register {
    let first = match width {
        1 => Register::AL,
        2 => Register::AX,
        _ => Register::EAX,
    };
    Register::try_from(first as usize + usize::from(index)).unwrap()
}

fn operand(inst: &mut Instruction, index: u32, src: Src, width: u8) {
    match src {
        Src::Reg(reg) => inst.set_op_register(index, gpr(reg, width)),
        Src::Imm(value) => {
            inst.set_op_kind(index, OpKind::Immediate32);
            inst.set_immediate32(value);
        }
        Src::Mem(Mem {
            base,
            index: at,
            scale,
            disp,
            ..
        }) => {
            inst.set_op_kind(index, OpKind::Memory);
            inst.set_memory_base(base.map_or(Register::None, |reg| gpr(reg, 4)));
            inst.set_memory_index(at.map_or(Register::None, |reg| gpr(reg, 4)));
            inst.set_memory_index_scale(scale);
            inst.set_memory_displacement32(disp);
            inst.set_memory_displ_size(4);
        }
    }
}

fn pair(inst: &mut Instruction, code: Code, dst: Src, src: Src) {
    inst.set_code(code);
    operand(inst, 0, dst, 4);
    operand(inst, 1, src, 4);
}

fn branch(inst: &mut Instruction, code: Code, target: u32) {
    inst.set_code(code);
    inst.set_op0_kind(OpKind::NearBranch32);
    inst.set_near_branch32(target);
}

const JCC: [Code; 16] = {
    use Code::*;
    [
        Jo_rel32_32,
        Jno_rel32_32,
        Jb_rel32_32,
        Jae_rel32_32,
        Je_rel32_32,
        Jne_rel32_32,
        Jbe_rel32_32,
        Ja_rel32_32,
        Js_rel32_32,
        Jns_rel32_32,
        Jp_rel32_32,
        Jnp_rel32_32,
        Jl_rel32_32,
        Jge_rel32_32,
        Jle_rel32_32,
        Jg_rel32_32,
    ]
};

const CMOV: [Code; 16] = {
    use Code::*;
    [
        Cmovo_r32_rm32,
        Cmovno_r32_rm32,
        Cmovb_r32_rm32,
        Cmovae_r32_rm32,
        Cmove_r32_rm32,
        Cmovne_r32_rm32,
        Cmovbe_r32_rm32,
        Cmova_r32_rm32,
        Cmovs_r32_rm32,
        Cmovns_r32_rm32,
        Cmovp_r32_rm32,
        Cmovnp_r32_rm32,
        Cmovl_r32_rm32,
        Cmovge_r32_rm32,
        Cmovle_r32_rm32,
        Cmovg_r32_rm32,
    ]
};

const SET: [Code; 16] = {
    use Code::*;
    [
        Seto_rm8, Setno_rm8, Setb_rm8, Setae_rm8, Sete_rm8, Setne_rm8, Setbe_rm8, Seta_rm8,
        Sets_rm8, Setns_rm8, Setp_rm8, Setnp_rm8, Setl_rm8, Setge_rm8, Setle_rm8, Setg_rm8,
    ]
};

fn condition(codes: &[Code; 16], cc: ConditionCode) -> Code {
    codes[cc as usize - ConditionCode::o as usize]
}

fn binary(codes: [Code; 3], inst: &mut Instruction, dst: Src, src: Src) {
    pair(
        inst,
        match (dst, src) {
            (_, Src::Imm(_)) => codes[2],
            (_, Src::Mem(_)) => codes[1],
            _ => codes[0],
        },
        dst,
        src,
    );
}

pub(super) fn instruction(op: QuickOp, pc: u32) -> Instruction {
    use Code::*;
    use Src::{Imm, Reg};

    let mut inst = Instruction::default();
    inst.set_ip(u64::from(pc));
    inst.set_code_size(CodeSize::Code32);
    match op {
        QuickOp::Nop => inst.set_code(Nopd),
        QuickOp::NopOperand(src, word) => {
            inst.set_code(if word { Nop_rm16 } else { Nop_rm32 });
            operand(&mut inst, 0, src, if word { 2 } else { 4 });
        }
        QuickOp::Pause => inst.set_code(Pause),
        QuickOp::Mov(dst, src) => binary(
            [Mov_rm32_r32, Mov_r32_rm32, Mov_rm32_imm32],
            &mut inst,
            dst,
            src,
        ),
        QuickOp::Lea(dst, mem) => pair(&mut inst, Lea_r32_m, Reg(dst), Src::Mem(mem)),
        QuickOp::Alu(kind, dst, src) => {
            let codes = match kind {
                Alu::Add => [Add_rm32_r32, Add_r32_rm32, Add_rm32_imm32],
                Alu::Sub => [Sub_rm32_r32, Sub_r32_rm32, Sub_rm32_imm32],
                Alu::And => [And_rm32_r32, And_r32_rm32, And_rm32_imm32],
                Alu::Or => [Or_rm32_r32, Or_r32_rm32, Or_rm32_imm32],
                Alu::Xor => [Xor_rm32_r32, Xor_r32_rm32, Xor_rm32_imm32],
                Alu::Cmp => [Cmp_rm32_r32, Cmp_r32_rm32, Cmp_rm32_imm32],
                Alu::Test => [Test_rm32_r32, Test_rm32_r32, Test_rm32_imm32],
            };
            binary(codes, &mut inst, dst, src);
        }
        QuickOp::Carry(subtract, dst, src) => binary(
            if subtract {
                [Sbb_rm32_r32, Sbb_r32_rm32, Sbb_rm32_imm32]
            } else {
                [Adc_rm32_r32, Adc_r32_rm32, Adc_rm32_imm32]
            },
            &mut inst,
            Reg(dst),
            src,
        ),
        QuickOp::Step(inc, dst) => {
            inst.set_code(if inc { Inc_rm32 } else { Dec_rm32 });
            operand(&mut inst, 0, Reg(dst), 4);
        }
        QuickOp::Neg(dst) | QuickOp::Not(dst) => {
            inst.set_code(if matches!(op, QuickOp::Neg(_)) {
                Neg_rm32
            } else {
                Not_rm32
            });
            operand(&mut inst, 0, Reg(dst), 4);
        }
        QuickOp::Extend(dst, src, signed) => {
            let (src, word) = match src {
                Narrow8or16::Byte(reg) => (Reg(reg), false),
                Narrow8or16::Word(reg) => (Reg(reg), true),
                Narrow8or16::Mem(mem, word) => (Src::Mem(mem), word),
            };
            inst.set_code(match (signed, word) {
                (false, false) => Movzx_r32_rm8,
                (false, true) => Movzx_r32_rm16,
                (true, false) => Movsx_r32_rm8,
                (true, true) => Movsx_r32_rm16,
            });
            operand(&mut inst, 0, Reg(dst), 4);
            operand(&mut inst, 1, src, if word { 2 } else { 1 });
        }
        QuickOp::Cdq => inst.set_code(Cdq),
        QuickOp::Push(src) => {
            inst.set_code(if matches!(src, Imm(_)) {
                Pushd_imm32
            } else {
                Push_rm32
            });
            operand(&mut inst, 0, src, 4);
        }
        QuickOp::Pop(dst) => {
            inst.set_code(Pop_rm32);
            operand(&mut inst, 0, Reg(dst), 4);
        }
        QuickOp::Leave => inst.set_code(Leaved),
        QuickOp::Jmp(target) => branch(&mut inst, Jmp_rel32_32, target),
        QuickOp::Call(target) => branch(&mut inst, Call_rel32_32, target),
        QuickOp::Jcc(cc, target) => branch(&mut inst, condition(&JCC, cc), target),
        QuickOp::Ret(Some(bytes)) => {
            inst.set_code(Retnd_imm16);
            inst.set_op0_kind(OpKind::Immediate16);
            inst.set_immediate16(bytes);
        }
        QuickOp::Ret(None) => inst.set_code(Retnd),
        QuickOp::JmpIndirect(src) | QuickOp::CallIndirect(src) => {
            inst.set_code(if matches!(op, QuickOp::JmpIndirect(_)) {
                Jmp_rm32
            } else {
                Call_rm32
            });
            operand(&mut inst, 0, src, 4);
        }
        QuickOp::Setcc(cc, dst) => {
            inst.set_code(condition(&SET, cc));
            operand(&mut inst, 0, Reg(dst), 1);
        }
        QuickOp::Cmov(cc, dst, src) => pair(&mut inst, condition(&CMOV, cc), Reg(dst), src),
        QuickOp::Shift(kind, dst, count) => {
            let (cl, imm) = match kind {
                ShiftKind::Shl => (Shl_rm32_CL, Shl_rm32_imm8),
                ShiftKind::Shr => (Shr_rm32_CL, Shr_rm32_imm8),
                ShiftKind::Sar => (Sar_rm32_CL, Sar_rm32_imm8),
                ShiftKind::Rol => (Rol_rm32_CL, Rol_rm32_imm8),
                ShiftKind::Ror => (Ror_rm32_CL, Ror_rm32_imm8),
            };
            inst.set_code(if count.is_none() { cl } else { imm });
            operand(&mut inst, 0, dst, 4);
            if let Some(count) = count {
                inst.set_op1_kind(OpKind::Immediate8);
                inst.set_immediate8(count as u8);
            } else {
                inst.set_op1_register(Register::CL);
            }
        }
        QuickOp::Imul(dst, src, imm) => {
            let code = if imm.is_some() {
                Imul_r32_rm32_imm32
            } else {
                Imul_r32_rm32
            };
            pair(&mut inst, code, Reg(dst), src);
            if let Some(imm) = imm {
                operand(&mut inst, 2, Imm(imm), 4);
            }
        }
    }
    inst
}
