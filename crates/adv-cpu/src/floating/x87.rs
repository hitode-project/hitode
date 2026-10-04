use super::softfloat as sf;
use super::*;
use crate::{X87Register as Ext, X87State};
use iced_x86::{Mnemonic as M, OpKind};
const INDEFINITE: Ext = Ext([0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff]);
const IE: u16 = 1;
const DE: u16 = 2;
const C0: u16 = 0x100;
const C1: u16 = 0x200;
const C2: u16 = 0x400;
const C3: u16 = 0x4000;

pub(super) fn supported(i: &Instruction) -> bool {
    matches!(
        i.mnemonic(),
        M::Fld
            | M::Fst
            | M::Fstp
            | M::Fild
            | M::Fist
            | M::Fistp
            | M::Fisttp
            | M::Fbld
            | M::Fbstp
            | M::Fld1
            | M::Fldz
            | M::Fldpi
            | M::Fldl2e
            | M::Fldl2t
            | M::Fldlg2
            | M::Fldln2
            | M::Fadd
            | M::Faddp
            | M::Fiadd
            | M::Fsub
            | M::Fsubp
            | M::Fsubr
            | M::Fsubrp
            | M::Fisub
            | M::Fisubr
            | M::Fmul
            | M::Fmulp
            | M::Fimul
            | M::Fdiv
            | M::Fdivp
            | M::Fdivr
            | M::Fdivrp
            | M::Fidiv
            | M::Fidivr
            | M::Fsqrt
            | M::Frndint
            | M::Fscale
            | M::Fyl2x
            | M::Fyl2xp1
            | M::F2xm1
            | M::Fsin
            | M::Fcos
            | M::Fsincos
            | M::Fptan
            | M::Fpatan
            | M::Fxtract
            | M::Fprem
            | M::Fprem1
            | M::Fabs
            | M::Fchs
            | M::Fxch
            | M::Ffree
            | M::Ffreep
            | M::Fincstp
            | M::Fdecstp
            | M::Fcom
            | M::Fcomp
            | M::Fcompp
            | M::Fucom
            | M::Fucomp
            | M::Fucompp
            | M::Ficom
            | M::Ficomp
            | M::Ftst
            | M::Fcomi
            | M::Fcomip
            | M::Fucomi
            | M::Fucomip
            | M::Fxam
            | M::Fcmovb
            | M::Fcmovbe
            | M::Fcmove
            | M::Fcmovnb
            | M::Fcmovnbe
            | M::Fcmovne
            | M::Fcmovnu
            | M::Fcmovu
            | M::Fldcw
            | M::Fnstcw
            | M::Fstcw
            | M::Fnstsw
            | M::Fstsw
            | M::Fnclex
            | M::Fclex
            | M::Fninit
            | M::Finit
            | M::Fnop
            | M::Wait
            | M::Fldenv
            | M::Fnstenv
            | M::Fstenv
            | M::Fnsave
            | M::Fsave
            | M::Frstor
            | M::Fxsave
            | M::Fxrstor
    )
}
fn exceptions(tx: &mut Transaction, flags: u16) {
    tx.state.x87.status |= flags;
    if flags & !tx.state.x87.control & 0x3f != 0 {
        tx.state.x87.status |= 0x8080;
        tx.x87_exception = true;
    }
}
fn operand(tx: &mut Transaction, logical: usize) -> Ext {
    let slot = tx.state.x87.physical_index(logical);
    if tx.state.x87.physical_tag(slot) == 3 {
        tx.state.x87.status &= !C1;
        exceptions(tx, IE | 0x40);
        return INDEFINITE;
    }
    let v = tx.state.x87.regs[slot];
    let exp = v.sign_exponent() & 0x7fff;
    if exp != 0 && v.significand() >> 63 == 0 {
        exceptions(tx, IE);
        return INDEFINITE;
    }
    if exp == 0 && v.significand() != 0 {
        if v.significand() >> 63 != 0 {
            return Ext::from_parts(v.significand(), v.sign_exponent() | 1);
        }
        exceptions(tx, DE);
    }
    v
}
fn rounded(tx: &mut Transaction) {
    let flags = sf::flags();
    exceptions(tx, flags);
    tx.state.x87.status = (tx.state.x87.status & !C1)
        | if flags & 0x20 != 0 && flags & IE == 0 && sf::rounded_up() {
            C1
        } else {
            0
        };
}
fn set(tx: &mut Transaction, logical: usize, value: Ext) {
    let slot = tx.state.x87.physical_index(logical);
    tx.state.x87.regs[slot] = value;
    tx.state.x87.set_physical_tag(slot, value.tag());
}
fn push(tx: &mut Transaction, mut value: Ext) {
    let slot = tx.state.x87.top().wrapping_sub(1) & 7;
    if tx.state.x87.physical_tag(slot) != 3 {
        exceptions(tx, IE | 0x40);
        tx.state.x87.status |= C1;
        value = INDEFINITE;
    } else {
        tx.state.x87.status &= !C1;
    }
    tx.state.x87.push(value);
}
fn pop(tx: &mut Transaction) {
    let slot = tx.state.x87.top();
    tx.state.x87.set_physical_tag(slot, 3);
    tx.state.x87.set_top(slot + 1);
}
fn st_index(r: R) -> Option<usize> {
    match r {
        R::ST0 => Some(0),
        R::ST1 => Some(1),
        R::ST2 => Some(2),
        R::ST3 => Some(3),
        R::ST4 => Some(4),
        R::ST5 => Some(5),
        R::ST6 => Some(6),
        R::ST7 => Some(7),
        _ => None,
    }
}
fn is_integer(m: M) -> bool {
    matches!(
        m,
        M::Fild
            | M::Fiadd
            | M::Fisub
            | M::Fisubr
            | M::Fimul
            | M::Fidiv
            | M::Fidivr
            | M::Ficom
            | M::Ficomp
    )
}
fn source(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<Ext, Step> {
    if i.op0_kind() == OpKind::Memory {
        let address = tx.address(i)?;
        let width = i.memory_size().size();
        let bytes = tx.read(memory, address, width)?;
        let value = match width {
            10 if !is_integer(i.mnemonic()) => Ext(bytes.as_ref().try_into().unwrap()),
            2 if is_integer(i.mnemonic()) => sf::from(
                i16::from_le_bytes(bytes.as_ref().try_into().unwrap()) as i32 as u32 as u64,
                2,
            ),
            4 if is_integer(i.mnemonic()) => sf::from(
                u32::from_le_bytes(bytes.as_ref().try_into().unwrap()) as u64,
                2,
            ),
            8 if is_integer(i.mnemonic()) => {
                sf::from(u64::from_le_bytes(bytes.as_ref().try_into().unwrap()), 3)
            }
            4 => {
                let bits = u32::from_le_bytes(bytes.as_ref().try_into().unwrap());
                if bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0 {
                    exceptions(tx, DE);
                }
                sf::from(bits as u64, 0)
            }
            8 => {
                let bits = u64::from_le_bytes(bytes.as_ref().try_into().unwrap());
                if bits & 0x7ff0_0000_0000_0000 == 0 && bits & 0x000f_ffff_ffff_ffff != 0 {
                    exceptions(tx, DE);
                }
                sf::from(bits, 1)
            }
            _ => return Err(tx.unsupported(i)),
        };
        exceptions(tx, sf::flags());
        tx.state.x87.dp = address;
        Ok(value)
    } else {
        let index = if i.op_count() == 2 {
            st_index(i.op1_register())
        } else {
            st_index(i.op0_register())
        }
        .unwrap_or(1);
        Ok(operand(tx, index))
    }
}
fn reset(tx: &mut Transaction) {
    let regs = tx.state.x87.regs;
    tx.state.x87 = X87State {
        control: 0x37f,
        regs,
        ..X87State::default()
    };
}
fn metadata(tx: &mut Transaction) {
    tx.state.x87.ip = tx.pc;
    if let Some(opcode) = tx.opcode {
        tx.state.x87.opcode = opcode;
    }
}
pub(super) fn execute(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let m = i.mnemonic();
    let no_wait = matches!(
        m,
        M::Fninit
            | M::Fnclex
            | M::Fnstcw
            | M::Fnstsw
            | M::Fnstenv
            | M::Fnsave
            | M::Fxsave
            | M::Fxrstor
    );
    if !no_wait && tx.state.x87.status & 0x80 != 0 {
        return Err(Step::Trap {
            pc: tx.pc,
            vector: 16,
        });
    }
    sf::configure(tx.state.x87.control);
    match m {
        M::Wait | M::Fnop => {}
        M::Fninit | M::Finit => reset(tx),
        M::Fnclex | M::Fclex => tx.state.x87.status &= !0x80ff,
        M::Fldcw => {
            let addr = tx.address(i)?;
            let cw = u16::from_le_bytes(tx.read(memory, addr, 2)?.as_ref().try_into().unwrap());
            tx.state.x87.set_control(cw);
        }
        M::Fnstcw | M::Fstcw => {
            let addr = tx.address(i)?;
            tx.store(memory, addr, &tx.state.x87.control.to_le_bytes())?;
        }
        M::Fnstsw | M::Fstsw => {
            let value = tx.state.x87.status;
            if i.op0_kind() == OpKind::Register {
                tx.state.regs[0] = (tx.state.regs[0] & 0xffff0000) | value as u32;
            } else {
                let addr = tx.address(i)?;
                tx.store(memory, addr, &value.to_le_bytes())?;
            }
        }
        M::Fldenv | M::Fnstenv | M::Fstenv | M::Fnsave | M::Fsave | M::Frstor => {
            environment(i, tx, memory)?
        }
        M::Fxsave | M::Fxrstor => fx_environment(i, tx, memory)?,
        M::Fld | M::Fild => {
            metadata(tx);
            let value = source(i, tx, memory)?;
            push(tx, value);
        }
        M::Fld1 | M::Fldz | M::Fldpi | M::Fldl2e | M::Fldl2t | M::Fldlg2 | M::Fldln2 => {
            metadata(tx);
            // Intel's constant ROM is rounded according to RC, independently of PC.
            let (mut sig, exp, below) = match m {
                M::Fld1 => (0x8000_0000_0000_0000, 0x3fff, false),
                M::Fldz => (0, 0, false),
                M::Fldpi => (0xc90f_daa2_2168_c235, 0x4000, true),
                M::Fldl2e => (0xb8aa_3b29_5c17_f0bc, 0x3fff, false),
                M::Fldl2t => (0xd49a_784b_cd1b_8afe, 0x4000, false),
                M::Fldlg2 => (0x9a20_9a84_fbcf_f799, 0x3ffd, false),
                _ => (0xb172_17f7_d1cf_79ac, 0x3ffe, true),
            };
            if !matches!(m, M::Fld1 | M::Fldz) {
                let rc = (tx.state.x87.control >> 10) & 3;
                if (rc == 1 || rc == 3) && below {
                    sig -= 1;
                }
                if rc == 2 && !below {
                    sig += 1;
                }
            }
            push(tx, Ext::from_parts(sig, exp));
        }
        M::Fst | M::Fstp | M::Fist | M::Fistp | M::Fisttp => {
            metadata(tx);
            let address = if i.op0_kind() == OpKind::Memory {
                let addr = tx.address(i)?;
                tx.check(memory, addr, i.memory_size().size(), true)?;
                Some(addr)
            } else {
                None
            };
            let value = operand(tx, 0);
            tx.state.x87.status &= !C1;
            if let Some(addr) = address {
                let width = i.memory_size().size();
                let integer = matches!(m, M::Fist | M::Fistp | M::Fisttp);
                let rc = if m == M::Fisttp {
                    1
                } else {
                    sf::rounding(tx.state.x87.control >> 10)
                };
                let mut bytes = [0; 10];
                if !integer && width == 10 {
                    bytes = value.0;
                } else {
                    let kind = if integer {
                        if width == 8 { 3 } else { 2 }
                    } else {
                        if width == 8 { 1 } else { 0 }
                    };
                    let mut bits = sf::to(value, kind, rc);
                    if integer
                        && width == 2
                        && ((bits as u32 as i32) < i16::MIN as i32
                            || (bits as u32 as i32) > i16::MAX as i32)
                    {
                        exceptions(tx, IE);
                        bits = 0x8000;
                        // The invalid 16-bit conversion suppresses precision
                        // reporting from the wider temporary integer result.
                        sf::configure(tx.state.x87.control);
                    }
                    rounded(tx);
                    bytes[..width].copy_from_slice(&bits.to_le_bytes()[..width]);
                }
                tx.store(memory, addr, &bytes[..width])?;
                tx.state.x87.dp = addr;
            } else {
                set(tx, st_index(i.op0_register()).unwrap_or(0), value);
            }
            if matches!(m, M::Fstp | M::Fistp | M::Fisttp) {
                pop(tx);
            }
        }
        M::Fbld | M::Fbstp => bcd(i, tx, memory)?,
        M::Fadd
        | M::Faddp
        | M::Fiadd
        | M::Fsub
        | M::Fsubp
        | M::Fsubr
        | M::Fsubrp
        | M::Fisub
        | M::Fisubr
        | M::Fmul
        | M::Fmulp
        | M::Fimul
        | M::Fdiv
        | M::Fdivp
        | M::Fdivr
        | M::Fdivrp
        | M::Fidiv
        | M::Fidivr => {
            metadata(tx);
            tx.state.x87.status &= !C1;
            let dest = if i.op_count() == 2 {
                st_index(i.op0_register()).unwrap_or(0)
            } else if matches!(
                m,
                M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp
            ) {
                1
            } else {
                0
            };
            let mut a = operand(tx, dest);
            let mut b = source(i, tx, memory)?;
            if matches!(
                m,
                M::Fsubr | M::Fsubrp | M::Fisubr | M::Fdivr | M::Fdivrp | M::Fidivr
            ) {
                std::mem::swap(&mut a, &mut b);
            }
            let op = if matches!(m, M::Fadd | M::Faddp | M::Fiadd) {
                0
            } else if matches!(m, M::Fmul | M::Fmulp | M::Fimul) {
                2
            } else if matches!(
                m,
                M::Fdiv | M::Fdivp | M::Fdivr | M::Fdivrp | M::Fidiv | M::Fidivr
            ) {
                3
            } else {
                1
            };
            let result = sf::binary(a, b, op);
            rounded(tx);
            set(tx, dest, result);
            if matches!(
                m,
                M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp
            ) {
                pop(tx);
            }
        }
        M::Fsqrt | M::Frndint => {
            metadata(tx);
            tx.state.x87.status &= !C1;
            let a = operand(tx, 0);
            let result = sf::unary(a, u8::from(m == M::Frndint));
            rounded(tx);
            set(tx, 0, result);
        }
        M::Fscale => {
            metadata(tx);
            let a = operand(tx, 0);
            let b = operand(tx, 1);
            let (result, flags) = super::transcendental::scale(a, b, tx.state.x87.control);
            exceptions(tx, flags);
            rounded(tx);
            set(tx, 0, result);
        }
        M::Fprem | M::Fprem1 => {
            metadata(tx);
            let a = operand(tx, 0);
            let b = operand(tx, 1);
            let (result, flags, quotient) = remainder(a, b, m == M::Fprem1, tx.state.x87.control);
            exceptions(tx, flags);
            if let Some(quotient) = quotient {
                tx.state.x87.status = (tx.state.x87.status & !(C0 | C1 | C2 | C3))
                    | if quotient & 4 != 0 { C0 } else { 0 }
                    | if quotient & 1 != 0 { C1 } else { 0 }
                    | if quotient & 2 != 0 { C3 } else { 0 };
            } else {
                tx.state.x87.status |= C2;
            }
            set(tx, 0, result);
        }
        M::Fyl2x | M::Fyl2xp1 | M::Fpatan => {
            metadata(tx);
            let x = operand(tx, 0);
            let y = operand(tx, 1);
            let (result, flags) = if m == M::Fpatan {
                super::transcendental::atan2(y, x, tx.state.x87.control)
            } else {
                super::transcendental::log_product(x, y, m == M::Fyl2xp1, tx.state.x87.control)
            };
            exceptions(tx, flags);
            rounded(tx);
            set(tx, 1, result);
            pop(tx);
        }
        M::F2xm1 => {
            metadata(tx);
            let x = operand(tx, 0);
            let Some((result, flags)) =
                super::transcendental::exp2_minus_one(x, tx.state.x87.control)
            else {
                return Err(tx.unsupported(i));
            };
            exceptions(tx, flags);
            rounded(tx);
            set(tx, 0, result);
        }
        M::Fsin | M::Fcos | M::Fsincos | M::Fptan => {
            metadata(tx);
            let x = operand(tx, 0);
            if let Some(((sine, cosine), flags)) =
                super::transcendental::sin_cos(x, tx.state.x87.control)
            {
                exceptions(tx, flags);
                rounded(tx);
                tx.state.x87.status &= !C2;
                if m == M::Fptan {
                    sf::configure(tx.state.x87.control | 0x300);
                    let result = sf::binary(sine, cosine, 3);
                    rounded(tx);
                    set(tx, 0, result);
                    push(tx, Ext::from_parts(1 << 63, 0x3fff));
                } else {
                    set(tx, 0, if m == M::Fcos { cosine } else { sine });
                    if m == M::Fsincos {
                        push(tx, cosine);
                    }
                }
            } else {
                tx.state.x87.status = (tx.state.x87.status & !C1) | C2;
            }
        }
        M::Fxtract => {
            metadata(tx);
            let a = operand(tx, 0);
            let exponent = a.sign_exponent() & 0x7fff;
            let mut significand = a;
            let extracted = if exponent == 0x7fff {
                if a.significand() & !(1 << 63) != 0 {
                    significand = sf::binary(a, a, 0);
                    rounded(tx);
                    significand
                } else {
                    Ext::from_parts(1 << 63, 0x7fff)
                }
            } else if a.significand() == 0 {
                exceptions(tx, 4);
                Ext::from_parts(1 << 63, 0xffff)
            } else {
                let shift = a.significand().leading_zeros();
                let unbiased = if exponent == 0 {
                    1 - 0x3fff - shift as i32
                } else {
                    exponent as i32 - 0x3fff
                };
                significand = Ext::from_parts(
                    a.significand() << shift,
                    (a.sign_exponent() & 0x8000) | 0x3fff,
                );
                sf::from(unbiased as u32 as u64, 2)
            };
            tx.state.x87.status &= !C1;
            set(tx, 0, extracted);
            push(tx, significand);
        }
        M::Fabs | M::Fchs => {
            metadata(tx);
            let value = operand(tx, 0);
            let exp = if m == M::Fabs {
                value.sign_exponent() & 0x7fff
            } else {
                value.sign_exponent() ^ 0x8000
            };
            set(tx, 0, Ext::from_parts(value.significand(), exp));
            tx.state.x87.status &= !C1;
        }
        M::Fxch => {
            metadata(tx);
            let n = st_index(i.op1_register()).unwrap_or(1);
            let a = operand(tx, 0);
            let b = operand(tx, n);
            set(tx, 0, b);
            set(tx, n, a);
            tx.state.x87.status &= !C1;
        }
        M::Ffree | M::Ffreep => {
            let slot = tx
                .state
                .x87
                .physical_index(st_index(i.op0_register()).unwrap_or(0));
            tx.state.x87.set_physical_tag(slot, 3);
            if m == M::Ffreep {
                pop(tx);
            }
        }
        M::Fincstp | M::Fdecstp => {
            let top = tx.state.x87.top();
            tx.state.x87.set_top(if m == M::Fincstp {
                top + 1
            } else {
                top.wrapping_sub(1)
            });
            tx.state.x87.status &= !C1;
        }
        M::Fcmovb
        | M::Fcmovbe
        | M::Fcmove
        | M::Fcmovnb
        | M::Fcmovnbe
        | M::Fcmovne
        | M::Fcmovnu
        | M::Fcmovu => {
            metadata(tx);
            let flags = tx.state.eflags;
            let taken = match m {
                M::Fcmovb => flags & 1 != 0,
                M::Fcmovnb => flags & 1 == 0,
                M::Fcmove => flags & 0x40 != 0,
                M::Fcmovne => flags & 0x40 == 0,
                M::Fcmovbe => flags & 0x41 != 0,
                M::Fcmovnbe => flags & 0x41 == 0,
                M::Fcmovu => flags & 4 != 0,
                _ => flags & 4 == 0,
            };
            let _ = operand(tx, 0);
            let src = operand(tx, st_index(i.op1_register()).unwrap_or(1));
            if taken {
                set(tx, 0, src);
            }
            tx.state.x87.status &= !C1;
        }
        M::Fxam => {
            let slot = tx.state.x87.top();
            let a = tx.state.x87.regs[slot];
            let exp = a.sign_exponent() & 0x7fff;
            let sig = a.significand();
            let cc = if tx.state.x87.physical_tag(slot) == 3 {
                C3 | C0
            } else if exp == 0 && sig == 0 {
                C3
            } else if exp == 0 {
                C3 | C2
            } else if sig >> 63 == 0 {
                0
            } else if exp == 0x7fff {
                if sig == 1 << 63 { C2 | C0 } else { C0 }
            } else {
                C2
            };
            tx.state.x87.status = (tx.state.x87.status & !(C0 | C1 | C2 | C3))
                | cc
                | if a.sign_exponent() & 0x8000 != 0 {
                    C1
                } else {
                    0
                };
        }
        _ => {
            metadata(tx);
            let a = operand(tx, 0);
            let b = if m == M::Ftst {
                Ext::default()
            } else {
                source(i, tx, memory)?
            };
            let quiet = matches!(
                m,
                M::Fucom | M::Fucomp | M::Fucompp | M::Fucomi | M::Fucomip
            );
            let comparison = sf::compare(a, b, !quiet);
            exceptions(tx, sf::flags());
            if matches!(m, M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip) {
                tx.state.eflags = (tx.state.eflags & !0x8d5)
                    | match comparison {
                        0 => 0x40,
                        1 => 1,
                        3 => 0x45,
                        _ => 0,
                    };
            } else {
                tx.state.x87.status = (tx.state.x87.status & !(C0 | C1 | C2 | C3))
                    | match comparison {
                        0 => C3,
                        1 => C0,
                        3 => C0 | C2 | C3,
                        _ => 0,
                    };
            }
            if matches!(
                m,
                M::Fcomp | M::Fucomp | M::Ficomp | M::Fcomip | M::Fucomip | M::Fcompp | M::Fucompp
            ) {
                pop(tx);
            }
            if matches!(m, M::Fcompp | M::Fucompp) {
                pop(tx);
            }
        }
    }
    Ok(())
}

fn bcd(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let address = tx.address(i)?;
    metadata(tx);
    if i.mnemonic() == M::Fbld {
        let bytes = tx.read(memory, address, 10)?;
        let mut value = 0u64;
        let mut multiplier = 1u64;
        let mut invalid = false;
        for &byte in &bytes[..9] {
            for digit in [byte & 15, byte >> 4] {
                invalid |= digit > 9;
                value += digit as u64 * multiplier;
                multiplier *= 10;
            }
        }
        if invalid {
            exceptions(tx, IE);
            push(tx, INDEFINITE);
        } else {
            let signed = if bytes[9] & 0x80 != 0 {
                -(value as i64)
            } else {
                value as i64
            };
            push(tx, sf::from(signed as u64, 3));
        }
    } else {
        tx.check(memory, address, 10, true)?;
        let a = operand(tx, 0);
        let converted = sf::to(a, 3, sf::rounding(tx.state.x87.control >> 10)) as i64;
        exceptions(tx, sf::flags());
        let mut bytes = [0; 10];
        let mut value = converted.unsigned_abs();
        if value >= 1_000_000_000_000_000_000 || sf::flags() & IE != 0 {
            exceptions(tx, IE);
            bytes[7] = 0xc0;
            bytes[8] = 0xff;
            bytes[9] = 0xff;
        } else {
            for byte in &mut bytes[..9] {
                *byte = (value % 10) as u8;
                value /= 10;
                *byte |= ((value % 10) as u8) << 4;
                value /= 10;
            }
            bytes[9] = if a.sign_exponent() & 0x8000 != 0 {
                0x80
            } else {
                0
            };
        }
        tx.store(memory, address, &bytes)?;
        pop(tx);
    }
    tx.state.x87.dp = address;
    Ok(())
}
fn environment(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let address = tx.address(i)?;
    let m = i.mnemonic();
    let total = i.memory_size().size();
    let size = if matches!(total, 14 | 94) { 14 } else { 28 };
    let full = matches!(m, M::Fnsave | M::Fsave | M::Frstor);
    let len = size + if full { 80 } else { 0 };
    if matches!(m, M::Fldenv | M::Frstor) {
        let bytes = tx.read(memory, address, len)?;
        let word = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        tx.state.x87.control = word(0);
        tx.state.x87.status = word(if size == 14 { 2 } else { 4 });
        tx.state.x87.tag = word(if size == 14 { 4 } else { 8 });
        if size == 14 {
            tx.state.x87.ip = word(6) as u32;
            tx.state.x87.cs = word(8);
            tx.state.x87.dp = word(10) as u32;
            tx.state.x87.ds = word(12);
        } else {
            tx.state.x87.ip = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
            tx.state.x87.cs = word(16);
            tx.state.x87.opcode = word(18) & 0x7ff;
            tx.state.x87.dp = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
            tx.state.x87.ds = word(24);
        }
        if full {
            for logical in 0..8 {
                let slot = tx.state.x87.physical_index(logical);
                tx.state.x87.regs[slot] = Ext(bytes
                    [size + logical * 10..size + (logical + 1) * 10]
                    .try_into()
                    .unwrap());
            }
        }
    } else {
        let mut bytes = vec![0xff; len];
        let f = &tx.state.x87;
        for (at, value) in if size == 14 {
            vec![
                (0, f.control),
                (2, f.status),
                (4, f.tag),
                (6, f.ip as u16),
                (8, f.cs),
                (10, f.dp as u16),
                (12, f.ds),
            ]
        } else {
            vec![
                (0, f.control),
                (4, f.status),
                (8, f.tag),
                (16, f.cs),
                (18, f.opcode),
                (24, f.ds),
            ]
        } {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        if size == 28 {
            bytes[12..16].copy_from_slice(&f.ip.to_le_bytes());
            bytes[20..24].copy_from_slice(&f.dp.to_le_bytes());
        }
        if full {
            for logical in 0..8 {
                bytes[size + logical * 10..size + (logical + 1) * 10]
                    .copy_from_slice(&f.regs[f.physical_index(logical)].0);
            }
        }
        tx.store(memory, address, &bytes)?;
        if full {
            reset(tx);
        } else {
            tx.state.x87.control |= 0x3f;
        }
    }
    Ok(())
}
fn fx_environment(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let address = tx.address(i)?;
    if address & 15 != 0 {
        return Err(Step::Trap {
            pc: tx.pc,
            vector: 13,
        });
    }
    if i.mnemonic() == M::Fxrstor {
        // The guest uses the non-64-bit format: XMM0..7 end at byte 287.
        // XMM8..15 and the reserved/software tail are not architectural reads.
        let bytes = tx.read(memory, address, 288)?;
        let mxcsr = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        if mxcsr & !0xffff != 0 {
            return Err(Step::Trap {
                pc: tx.pc,
                vector: 13,
            });
        }
        let word = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        let f = &mut tx.state.x87;
        f.control = word(0);
        f.status = word(2);
        f.opcode = word(6) & 0x7ff;
        f.ip = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        f.cs = word(12);
        f.dp = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        f.ds = word(20);
        for logical in 0..8 {
            let slot = f.physical_index(logical);
            let a = Ext(bytes[32 + logical * 16..42 + logical * 16]
                .try_into()
                .unwrap());
            f.regs[slot] = a;
            f.set_physical_tag(
                slot,
                if bytes[4] & (1 << slot) == 0 {
                    3
                } else {
                    a.tag()
                },
            );
        }
        tx.state.mxcsr = mxcsr;
        for (slot, xmm) in tx.state.xmm.iter_mut().enumerate() {
            xmm.copy_from_slice(&bytes[160 + slot * 16..176 + slot * 16]);
        }
    } else {
        // The non-64-bit format writes x87/MMX, MXCSR and XMM0..7 only.
        // Leave bytes 288..511 untouched, including the software-owned 464..511.
        // No read/permission check of that unused tail is needed to preserve it.
        let mut bytes = [0; 288];
        let f = &tx.state.x87;
        for (at, value) in [
            (0, f.control),
            (2, f.status),
            (6, f.opcode),
            (12, f.cs),
            (20, f.ds),
        ] {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[8..12].copy_from_slice(&f.ip.to_le_bytes());
        bytes[16..20].copy_from_slice(&f.dp.to_le_bytes());
        bytes[24..28].copy_from_slice(&tx.state.mxcsr.to_le_bytes());
        bytes[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
        for slot in 0..8 {
            if f.physical_tag(slot) != 3 {
                bytes[4] |= 1 << slot;
            }
        }
        for logical in 0..8 {
            bytes[32 + logical * 16..42 + logical * 16]
                .copy_from_slice(&f.regs[f.physical_index(logical)].0);
        }
        for (slot, xmm) in tx.state.xmm.iter().enumerate() {
            bytes[160 + slot * 16..176 + slot * 16].copy_from_slice(xmm);
        }
        tx.store(memory, address, &bytes)?;
    }
    Ok(())
}

/// Exact significand division. A complete reduction needs at most 127 numerator
/// bits. Larger exponent differences perform one permitted N=32 partial step,
/// set C2, and can be repeated by the guest until the quotient bits are defined.
fn remainder(a: Ext, b: Ext, nearest: bool, control: u16) -> (Ext, u16, Option<u64>) {
    let ae = a.sign_exponent() & 0x7fff;
    let be = b.sign_exponent() & 0x7fff;
    let an = ae == 0x7fff && a.significand() & !(1 << 63) != 0;
    let bn = be == 0x7fff && b.significand() & !(1 << 63) != 0;
    if an || bn {
        sf::configure(control | 0x300);
        let result = sf::binary(a, b, 0);
        return (result, sf::flags(), Some(0));
    }
    if ae == 0x7fff || b.significand() == 0 {
        return (INDEFINITE, IE, Some(0));
    }
    if a.significand() == 0 || be == 0x7fff {
        return (a, 0, Some(0));
    }
    let normalize = |v: Ext| {
        let shift = v.significand().leading_zeros();
        let exponent = i32::from(v.sign_exponent() & 0x7fff).max(1) - shift as i32;
        (v.significand() << shift, exponent)
    };
    let (asig, aexp) = normalize(a);
    let (bsig, bexp) = normalize(b);
    let difference = aexp - bexp;
    let sign = a.sign_exponent() & 0x8000;
    let (residue, exponent, quotient, reverse, partial) = if difference >= 0 {
        let partial = difference >= 64;
        let shift = if partial { 32 } else { difference as u32 };
        let numerator = (asig as u128) << shift;
        let divisor = bsig as u128;
        let mut quotient = numerator / divisor;
        let mut residue = numerator % divisor;
        let reverse = !partial
            && nearest
            && (residue * 2 > divisor || residue * 2 == divisor && quotient & 1 != 0);
        if reverse {
            residue = divisor - residue;
            quotient += 1;
        }
        (
            residue as u64,
            if partial { aexp - 32 } else { bexp },
            quotient as u64,
            reverse,
            partial,
        )
    } else if nearest && difference == -1 && asig > bsig {
        (
            ((bsig as u128) * 2 - asig as u128) as u64,
            aexp,
            1,
            true,
            false,
        )
    } else {
        return (a, 0, Some(0));
    };
    let result = if residue == 0 {
        Ext::from_parts(0, sign)
    } else {
        let shift = residue.leading_zeros();
        let exp = exponent - shift as i32;
        let normalized = residue << shift;
        Ext::from_parts(
            if exp <= 0 {
                normalized >> (1 - exp)
            } else {
                normalized
            },
            (if reverse { sign ^ 0x8000 } else { sign }) | exp.max(0) as u16,
        )
    };
    // x87 condition codes expose the low quotient magnitude bits, irrespective
    // of operand signs; the remainder's sign is handled separately above.
    (result, 0, if partial { None } else { Some(quotient) })
}
