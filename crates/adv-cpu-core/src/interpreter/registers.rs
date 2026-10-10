//! Infallible register operations commit directly to canonical state.
//! Memory, traps, REP and all unclassified forms retain the checked transaction.
use super::*;

fn flag(flags: &mut u32, bit: u32, value: bool) {
    *flags = (*flags & !bit) | if value { bit } else { 0 };
}

pub(super) fn szp(flags: &mut u32, value: u32, width: u8) {
    let value = value & mask(width);
    flag(flags, ZF, value == 0);
    flag(flags, SF, value & (1 << (u32::from(width) * 8 - 1)) != 0);
    flag(flags, PF, (value as u8).count_ones().is_multiple_of(2));
}
pub(super) fn arithmetic(
    flags: &mut u32,
    a: u32,
    b: u32,
    carry: u32,
    width: u8,
    subtract: bool,
) -> u32 {
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
    flag(
        flags,
        CF,
        if subtract {
            u64::from(a) < u64::from(b) + u64::from(carry)
        } else {
            u64::from(a) + u64::from(b) + u64::from(carry) > u64::from(mask(width))
        },
    );
    flag(flags, OF, wide < -limit || wide >= limit);
    flag(flags, AF, (a ^ b ^ result) & 0x10 != 0);
    szp(flags, result, width);
    result
}

pub(super) fn binary(flags: &mut u32, kind: M, a: u32, b: u32, width: u8) -> u32 {
    let carry = u32::from(*flags & CF != 0 && matches!(kind, M::Adc | M::Sbb));
    match kind {
        M::Add | M::Adc => arithmetic(flags, a, b, carry, width, false),
        M::Sub | M::Sbb | M::Cmp => arithmetic(flags, a, b, carry, width, true),
        _ => {
            let value = match kind {
                M::And | M::Test => a & b,
                M::Or => a | b,
                M::Xor => a ^ b,
                _ => unreachable!(),
            };
            *flags &= !(CF | OF);
            szp(flags, value, width);
            value
        }
    }
}

pub(super) fn unary(flags: &mut u32, kind: M, value: u32, width: u8) -> u32 {
    match kind {
        M::Not => !value,
        M::Neg => arithmetic(flags, 0, value, 0, width, true),
        _ => {
            let carry = *flags & CF;
            let result = arithmetic(flags, value, 1, 0, width, kind == M::Dec);
            *flags = (*flags & !CF) | carry;
            result
        }
    }
}

pub(super) fn shift(flags: &mut u32, kind: M, value: u32, count: u32, width: u8) -> u32 {
    let bits = u32::from(width) * 8;
    let value = value & mask(width);
    let sign = value & (1 << (bits - 1)) != 0;
    let old_cf = *flags & CF != 0;
    match kind {
        M::Shl => {
            if count <= bits {
                flag(flags, CF, value & (1 << (bits - count)) != 0);
            }
            let result = (u64::from(value) << count) as u32 & mask(width);
            if count == 1 {
                flag(
                    flags,
                    OF,
                    (result & (1 << (bits - 1)) != 0) != (*flags & CF != 0),
                );
            }
            szp(flags, result, width);
            result
        }
        M::Shr => {
            if count <= bits {
                flag(flags, CF, value & (1 << (count - 1)) != 0);
            }
            let result = value >> count;
            if count == 1 {
                flag(flags, OF, sign);
            }
            szp(flags, result, width);
            result
        }
        M::Sar => {
            flag(
                flags,
                CF,
                if count >= bits {
                    sign
                } else {
                    value & (1 << (count - 1)) != 0
                },
            );
            let result = (signed(value, width) >> count) as u32 & mask(width);
            if count == 1 {
                flag(flags, OF, false);
            }
            szp(flags, result, width);
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
            flag(
                flags,
                CF,
                if kind == M::Rol {
                    result & 1 != 0
                } else {
                    result & (1 << (bits - 1)) != 0
                },
            );
            if count == 1 {
                flag(
                    flags,
                    OF,
                    if kind == M::Rol {
                        (result & (1 << (bits - 1)) != 0) != (*flags & CF != 0)
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
            flag(flags, CF, extended & (1u64 << bits) != 0);
            if count == 1 {
                flag(
                    flags,
                    OF,
                    if kind == M::Rcl {
                        (result & (1 << (bits - 1)) != 0) != (*flags & CF != 0)
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

pub(super) fn product(flags: &mut u32, a: u32, b: u32, width: u8, signed_form: bool) -> u64 {
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
    flag(flags, CF, overflow);
    flag(flags, OF, overflow);
    product
}

fn read(operand: Operand, regs: &[u32; 8]) -> u32 {
    match operand {
        Operand::Register(r) => r.read(regs),
        Operand::Immediate(value) => value,
        Operand::Memory(..) => unreachable!("register-only classification"),
    }
}

/// Return false without changing state for any potentially faulting form.
/// Fetch validity, TF and fuel stay with the ordinary instruction boundary.
pub(super) fn eligible(action: Action) -> bool {
    use Operand::{Immediate, Register};
    matches!(
        action,
        Action::Nop
            | Action::Mov(Register(_), Register(_) | Immediate(_), _)
            | Action::Lea(..)
            | Action::Binary(_, Register(_), Register(_) | Immediate(_))
            | Action::Unary(_, Register(_))
            | Action::Set(Register(_), _)
            | Action::Branch(Immediate(_), _)
    )
}

/// The eligibility bit is cached during predecode so other forms never need
/// this second dispatch. Keep a defensive rejection before any state mutation.
pub(super) fn execute(instruction: &Predecoded, state: &mut CpuState) -> bool {
    use Operand::{Immediate, Register};
    let mut next = instruction.next;
    match instruction.action {
        Action::Nop => {}
        Action::Mov(Register(dst), src @ (Register(_) | Immediate(_)), extend) => {
            let value = read(src, &state.regs);
            let value = if extend {
                signed(value, src.width().unwrap()) as u32
            } else {
                value
            };
            dst.write(&mut state.regs, value);
        }
        Action::Lea(dst, address) => {
            let value = address.resolve_regs(&state.regs, state.fs_base);
            dst.write(&mut state.regs, value);
        }
        Action::Binary(kind, Register(dst), src @ (Register(_) | Immediate(_))) => {
            let a = dst.read(&state.regs);
            let b = read(src, &state.regs);
            let value = binary(&mut state.eflags, kind, a, b, dst.width);
            if !matches!(kind, M::Cmp | M::Test) {
                dst.write(&mut state.regs, value);
            }
        }
        Action::Unary(kind, Register(dst)) => {
            let value = dst.read(&state.regs);
            let result = unary(&mut state.eflags, kind, value, dst.width);
            dst.write(&mut state.regs, result);
        }
        Action::Set(Register(dst), condition) => {
            dst.write(
                &mut state.regs,
                u32::from(condition_true(condition, state.eflags)),
            );
        }
        Action::Branch(Immediate(target), condition) => {
            if condition == ConditionCode::None || condition_true(condition, state.eflags) {
                next = target;
            }
        }
        _ => return false,
    }
    state.eip = next;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    #[test]
    fn direct_register_forms_match_transactions_at_every_boundary() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        a.mov(al, ah).unwrap();
        a.mov(ah, al).unwrap();
        a.mov(ax, bx).unwrap();
        a.mov(eax, ecx).unwrap();
        a.mov(ah, 0x81).unwrap();
        a.mov(ax, 0xff01).unwrap();
        a.mov(eax, 0x80000001u32).unwrap();
        a.movzx(ebx, ah).unwrap();
        a.movsx(edx, ax).unwrap();
        a.lea(eax, ptr(edx + esi * 4 - 17)).unwrap();
        a.lea(ax, ptr(bx + si)).unwrap();
        macro_rules! binary {
            ($op:ident) => {
                a.$op(al, ah).unwrap();
                a.$op(ah, al).unwrap();
                a.$op(ax, bx).unwrap();
                a.$op(eax, ecx).unwrap();
                a.$op(al, 0xff).unwrap();
                a.$op(ax, 0xffff).unwrap();
                a.$op(eax, -1).unwrap();
            };
        }
        binary!(add);
        binary!(adc);
        binary!(sub);
        binary!(sbb);
        binary!(and);
        binary!(or);
        binary!(xor);
        binary!(cmp);
        binary!(test);
        macro_rules! unary {
            ($op:ident) => {
                a.$op(al).unwrap();
                a.$op(ah).unwrap();
                a.$op(ax).unwrap();
                a.$op(eax).unwrap();
            };
        }
        unary!(inc);
        unary!(dec);
        unary!(neg);
        unary!(not);
        macro_rules! conditional {
            ($jump:ident, $set:ident) => {
                a.$jump(0x18000u64).unwrap();
                a.$set(ah).unwrap();
            };
        }
        conditional!(jo, seto);
        conditional!(jno, setno);
        conditional!(jb, setb);
        conditional!(jae, setae);
        conditional!(je, sete);
        conditional!(jne, setne);
        conditional!(jbe, setbe);
        conditional!(ja, seta);
        conditional!(js, sets);
        conditional!(jns, setns);
        conditional!(jp, setp);
        conditional!(jnp, setnp);
        conditional!(jl, setl);
        conditional!(jge, setge);
        conditional!(jle, setle);
        conditional!(jg, setg);
        a.jmp(0x18000u64).unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let memory = Memory::new(0x10000, PAGE_SIZE).unwrap();
        let values = [
            0,
            1,
            0x7f,
            0x80,
            0xff,
            0x7fff,
            0x8000,
            0xffff,
            0x7fffffff,
            0x80000000,
            u32::MAX,
            0x93ad812f,
        ];
        for i in Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE) {
            let instruction = Predecoded {
                action: predecode(&i).unwrap(),
                next: i.next_ip32(),
                exclusive: false,
                register_only: eligible(predecode(&i).unwrap()),
                checked_scalar: false,
            };
            assert!(instruction.register_only, "{:?}", i.code());
            for &value in &values {
                // Exercise every defined arithmetic flag input combination,
                // including CF preservation by INC/DEC and undefined AF by TEST.
                for pattern in 0u32..64 {
                    let flags = [CF, PF, AF, ZF, SF, OF]
                        .iter()
                        .enumerate()
                        .fold(0x202, |f, (n, bit)| {
                            f | if pattern & (1 << n) != 0 { *bit } else { 0 }
                        });
                    let mut state = CpuState {
                        regs: std::array::from_fn(|n| value.rotate_right(n as u32 * 3)),
                        eflags: flags,
                        eip: i.ip32(),
                        fuel: 123,
                        fs_base: 0x13000,
                        ..Default::default()
                    };
                    let mut expected = state.clone();
                    let mut tx = Transaction {
                        regs: state.regs,
                        flags,
                        eip: instruction.next,
                        fs_base: state.fs_base,
                        segments: state.segments,
                        pc: state.eip,
                        write: None,
                        more_writes: Vec::new(),
                        indirect_branch: None,
                    };
                    assert_eq!(tx.execute(instruction.action, &memory), Ok(None));
                    assert!(tx.write.is_none() && tx.more_writes.is_empty());
                    expected.regs = tx.regs;
                    expected.eflags = tx.flags;
                    expected.eip = tx.eip;
                    assert!(execute(&instruction, &mut state), "{:?}", i.code());
                    assert_eq!(
                        state,
                        expected,
                        "{:?}, value={value}, flags={flags}",
                        i.code()
                    );
                }
            }
        }
    }

    #[test]
    fn direct_prefix_materializes_flags_before_cross_page_fault_and_tf() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.add(eax, -1).unwrap();
        a.mov(ebx, dword_ptr(0x21ffe)).unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut memory = Memory::new(0x10000, 0x20000).unwrap();
        memory.protect(0x10000, 0x20000, READ | WRITE).unwrap();
        memory.write(0x10000, &bytes).unwrap();
        memory.protect(0x10000, PAGE_SIZE, READ | EXEC).unwrap();
        memory.write(0x21ffe, &123u32.to_le_bytes()).unwrap();
        memory.protect(0x22000, PAGE_SIZE, 0).unwrap();
        let mut interpreter = Interpreter::new();
        let mut state = CpuState {
            eip: 0x10000,
            eflags: 0x202,
            ..Default::default()
        };
        state.regs[0] = 1;
        state.regs[3] = 99;
        let result = interpreter.run_block(&mut state, &mut memory, 2, 0, None).0;
        assert_eq!(result.units, 1);
        assert!(matches!(
            result.step,
            Step::ReadFault {
                address: 0x21ffe,
                ..
            }
        ));
        assert_eq!(state.regs[0], 0);
        assert_eq!(state.regs[3], 99);
        assert_eq!(state.eflags, 0x202 | CF | AF | PF | ZF);
        let fault_pc = state.eip;
        memory.protect(0x22000, PAGE_SIZE, READ).unwrap();
        assert_eq!(
            interpreter
                .run_block(&mut state, &mut memory, 1, 0, None)
                .0
                .units,
            1
        );
        assert_eq!(state.regs[3], 123);
        assert_eq!(state.eflags, 0x202 | CF | AF | PF | ZF);
        // Reuse the original block, but require a trap at its first boundary.
        state.eip = 0x10000;
        state.regs[0] = 1;
        state.regs[3] = 99;
        state.eflags = 0x302;
        let result = interpreter.run_block(&mut state, &mut memory, 2, 0, None).0;
        assert_eq!(
            result,
            Execution {
                step: Step::Trap {
                    pc: 0x10000,
                    vector: 1
                },
                units: 1
            }
        );
        assert_eq!(state.eip, fault_pc);
        assert_eq!(state.regs[3], 99);
        assert_eq!(state.eflags, 0x302 | CF | AF | PF | ZF);
    }

    #[test]
    fn direct_prefix_survives_partial_rep_and_cached_code_replacement() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.add(eax, -1).unwrap();
        a.rep().movsd().unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut memory = Memory::new(0x10000, 0x20000).unwrap();
        memory.protect(0x10000, 0x20000, READ | WRITE).unwrap();
        memory.write(0x10000, &bytes).unwrap();
        memory.protect(0x10000, PAGE_SIZE, READ | EXEC).unwrap();
        memory.write(0x21ffc, &123u32.to_le_bytes()).unwrap();
        memory.write(0x22000, &[7; 8]).unwrap();
        memory.protect(0x22000, PAGE_SIZE, 0).unwrap();
        let mut interpreter = Interpreter::new();
        let mut state = CpuState {
            eip: 0x10000,
            eflags: 0x202,
            ..Default::default()
        };
        state.regs[0] = 1;
        state.regs[1] = 3;
        state.regs[6] = 0x21ffc;
        state.regs[7] = 0x24000;
        let result = interpreter.run_block(&mut state, &mut memory, 4, 0, None).0;
        assert_eq!(result.units, 2);
        assert!(matches!(
            result.step,
            Step::ReadFault {
                address: 0x22000,
                ..
            }
        ));
        assert_eq!(state.eflags, 0x202 | CF | AF | PF | ZF);
        assert_eq!(state.regs[1], 2);
        assert_eq!(state.regs[6], 0x22000);
        assert_eq!(state.regs[7], 0x24004);
        memory.write(0x21ffc, &456u32.to_le_bytes()).unwrap();
        memory.protect(0x22000, PAGE_SIZE, READ).unwrap();
        assert_eq!(
            interpreter
                .run_block(&mut state, &mut memory, 2, 0, None)
                .0
                .units,
            2
        );
        assert_eq!(
            memory.read_data(0x24000, 4).unwrap().as_ref(),
            &123u32.to_le_bytes()
        );
        assert_eq!(memory.read_data(0x24004, 8).unwrap().as_ref(), &[7; 8]);
        assert_eq!(state.eflags, 0x202 | CF | AF | PF | ZF);
        let decodes = interpreter.decode_count();
        let mut replacement = CodeAssembler::new(32).unwrap();
        replacement.add(eax, 9).unwrap();
        replacement.rep().movsd().unwrap();
        memory
            .write(0x10000, &replacement.assemble(0x10000).unwrap())
            .unwrap();
        state.eip = 0x10000;
        assert_eq!(
            interpreter
                .run_block(&mut state, &mut memory, 1, 0, None)
                .0
                .units,
            1
        );
        assert_eq!(state.regs[0], 9);
        assert!(interpreter.decode_count() > decodes);
    }
}
