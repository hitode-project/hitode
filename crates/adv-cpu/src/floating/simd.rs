use super::softfloat as sf;
use super::*;
use crate::X87Register;
use iced_x86::{Mnemonic as M, OpKind};
fn xmm(r: R) -> Option<usize> {
    match r {
        R::XMM0 => Some(0),
        R::XMM1 => Some(1),
        R::XMM2 => Some(2),
        R::XMM3 => Some(3),
        R::XMM4 => Some(4),
        R::XMM5 => Some(5),
        R::XMM6 => Some(6),
        R::XMM7 => Some(7),
        _ => None,
    }
}
fn mmx(r: R) -> Option<usize> {
    match r {
        R::MM0 => Some(0),
        R::MM1 => Some(1),
        R::MM2 => Some(2),
        R::MM3 => Some(3),
        R::MM4 => Some(4),
        R::MM5 => Some(5),
        R::MM6 => Some(6),
        R::MM7 => Some(7),
        _ => None,
    }
}
fn read(
    i: &Instruction,
    n: u32,
    tx: &Transaction,
    memory: &Memory,
    width: usize,
) -> Result<[u8; 16], Step> {
    let mut bytes = [0; 16];
    if i.op_kind(n) == OpKind::Memory {
        let address = tx.address(i)?;
        bytes[..width].copy_from_slice(&tx.read(memory, address, width)?);
    } else if let Some(slot) = xmm(i.op_register(n)) {
        bytes = tx.state.xmm[slot];
    } else if let Some(slot) = mmx(i.op_register(n)) {
        bytes[..8].copy_from_slice(&tx.state.mmx[slot].to_le_bytes());
    } else if let Some(slot) = reg_index(i.op_register(n)) {
        bytes[..4].copy_from_slice(&tx.state.regs[slot].to_le_bytes());
    } else {
        return Err(tx.unsupported(i));
    }
    Ok(bytes)
}
fn write(
    i: &Instruction,
    tx: &mut Transaction,
    memory: &Memory,
    bytes: &[u8],
    width: usize,
) -> Result<(), Step> {
    if i.op0_kind() == OpKind::Memory {
        let address = tx.address(i)?;
        tx.store(memory, address, &bytes[..width])?;
    } else if let Some(slot) = xmm(i.op0_register()) {
        tx.state.xmm[slot][..width].copy_from_slice(&bytes[..width]);
    } else if let Some(slot) = mmx(i.op0_register()) {
        let bits = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        tx.state.mmx[slot] = bits;
        tx.state.x87.regs[slot] = X87Register::from_parts(bits, 0xffff);
    } else if let Some(slot) = reg_index(i.op0_register()) {
        tx.state.regs[slot] = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    } else {
        return Err(tx.unsupported(i));
    }
    Ok(())
}
fn enter_mmx(tx: &mut Transaction) {
    tx.state.x87.set_top(0);
    tx.state.x87.tag = 0;
}
pub(super) fn supported(i: &Instruction) -> bool {
    let m = i.mnemonic();
    matches!(
        m,
        M::Emms
            | M::Femms
            | M::Ldmxcsr
            | M::Stmxcsr
            | M::Rcpps
            | M::Rcpss
            | M::Rsqrtps
            | M::Rsqrtss
            | M::Maskmovq
            | M::Maskmovdqu
            | M::Movnti
            | M::Sfence
            | M::Lfence
            | M::Mfence
            | M::Prefetchnta
            | M::Prefetcht0
            | M::Prefetcht1
            | M::Prefetcht2
            | M::Clflush
            | M::Movaps
            | M::Movapd
            | M::Movups
            | M::Movupd
            | M::Movdqa
            | M::Movdqu
            | M::Movd
            | M::Movq
            | M::Movq2dq
            | M::Movdq2q
            | M::Movntq
            | M::Movntdq
            | M::Movntps
            | M::Movntpd
            | M::Movhlps
            | M::Movlhps
            | M::Movlps
            | M::Movhps
            | M::Movlpd
            | M::Movhpd
            | M::Movmskps
            | M::Movmskpd
            | M::Pmovmskb
            | M::Addss
            | M::Addsd
            | M::Subss
            | M::Subsd
            | M::Mulss
            | M::Mulsd
            | M::Divss
            | M::Divsd
            | M::Sqrtss
            | M::Sqrtsd
            | M::Minss
            | M::Minsd
            | M::Maxss
            | M::Maxsd
            | M::Addps
            | M::Addpd
            | M::Subps
            | M::Subpd
            | M::Mulps
            | M::Mulpd
            | M::Divps
            | M::Divpd
            | M::Sqrtps
            | M::Sqrtpd
            | M::Minps
            | M::Minpd
            | M::Maxps
            | M::Maxpd
            | M::Comiss
            | M::Comisd
            | M::Ucomiss
            | M::Ucomisd
            | M::Cvtsi2ss
            | M::Cvtsi2sd
            | M::Cvtss2si
            | M::Cvtsd2si
            | M::Cvttss2si
            | M::Cvttsd2si
            | M::Cvtss2sd
            | M::Cvtsd2ss
            | M::Cvtps2dq
            | M::Cvttps2dq
            | M::Cvtdq2ps
            | M::Cvtpd2dq
            | M::Cvttpd2dq
            | M::Cvtdq2pd
            | M::Cvtps2pd
            | M::Cvtpd2ps
            | M::Cvtpi2ps
            | M::Cvtpi2pd
            | M::Cvtps2pi
            | M::Cvttps2pi
            | M::Cvtpd2pi
            | M::Cvttpd2pi
            | M::Cmpps
            | M::Cmppd
            | M::Psadbw
            | M::Pmuludq
            | M::Xorps
            | M::Xorpd
            | M::Andps
            | M::Andpd
            | M::Andnps
            | M::Andnpd
            | M::Orps
            | M::Orpd
            | M::Pxor
            | M::Pand
            | M::Pandn
            | M::Por
            | M::Paddb
            | M::Paddw
            | M::Paddd
            | M::Paddq
            | M::Psubb
            | M::Psubw
            | M::Psubd
            | M::Psubq
            | M::Paddusb
            | M::Paddusw
            | M::Psubusb
            | M::Psubusw
            | M::Paddsb
            | M::Paddsw
            | M::Psubsb
            | M::Psubsw
            | M::Pavgb
            | M::Pavgw
            | M::Pcmpeqb
            | M::Pcmpeqw
            | M::Pcmpeqd
            | M::Pcmpgtb
            | M::Pcmpgtw
            | M::Pcmpgtd
            | M::Pmullw
            | M::Pmulhw
            | M::Pmulhuw
            | M::Pmaddwd
            | M::Pminub
            | M::Pmaxub
            | M::Pminsw
            | M::Pmaxsw
            | M::Punpcklbw
            | M::Punpckhbw
            | M::Punpcklwd
            | M::Punpckhwd
            | M::Punpckldq
            | M::Punpckhdq
            | M::Punpcklqdq
            | M::Punpckhqdq
            | M::Unpcklps
            | M::Unpckhps
            | M::Unpcklpd
            | M::Unpckhpd
            | M::Packsswb
            | M::Packssdw
            | M::Packuswb
            | M::Psllw
            | M::Pslld
            | M::Psllq
            | M::Psrlw
            | M::Psrld
            | M::Psrlq
            | M::Psraw
            | M::Psrad
            | M::Pslldq
            | M::Psrldq
            | M::Pshufw
            | M::Pshufd
            | M::Pshuflw
            | M::Pshufhw
            | M::Shufps
            | M::Shufpd
            | M::Pinsrw
            | M::Pextrw
    ) || matches!(m, M::Movss | M::Movsd | M::Cmpss | M::Cmpsd)
        && (i.op0_kind() == OpKind::Memory || xmm(i.op0_register()).is_some())
}
pub(super) fn execute(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let m = i.mnemonic();
    let uses_mmx = matches!(m, M::Emms | M::Femms)
        || (0..i.op_count()).any(|op| mmx(i.op_register(op)).is_some());
    if uses_mmx && tx.state.x87.status & 0x80 != 0 {
        return Err(Step::Trap {
            pc: tx.pc,
            vector: 16,
        });
    }
    if matches!(
        m,
        M::Sfence
            | M::Lfence
            | M::Mfence
            | M::Prefetchnta
            | M::Prefetcht0
            | M::Prefetcht1
            | M::Prefetcht2
    ) {
        // Guest memory operations are synchronous in this flat execution model;
        // prefetch hints do not access memory or generate page faults.
        return Ok(());
    }
    if m == M::Clflush {
        tx.read(memory, tx.address(i)?, 1)?;
        return Ok(());
    }
    if matches!(m, M::Emms | M::Femms) {
        tx.state.x87.tag = 0xffff;
        return Ok(());
    }
    if matches!(m, M::Ldmxcsr | M::Stmxcsr) {
        let address = tx.address(i)?;
        if m == M::Ldmxcsr {
            let value =
                u32::from_le_bytes(tx.read(memory, address, 4)?.as_ref().try_into().unwrap());
            if value & !0xffff != 0 {
                return Err(Step::Trap {
                    pc: tx.pc,
                    vector: 13,
                });
            }
            tx.state.mxcsr = value;
        } else {
            tx.store(memory, address, &tx.state.mxcsr.to_le_bytes())?;
        }
        return Ok(());
    }
    if i.memory_size().size() == 16
        && !matches!(m, M::Movups | M::Movupd | M::Movdqu)
        && (i.op0_kind() == OpKind::Memory || i.op1_kind() == OpKind::Memory)
    {
        let address = tx.address(i)?;
        if address & 15 != 0 {
            return Err(Step::Trap {
                pc: tx.pc,
                vector: 13,
            });
        }
    }
    let is_mmx = (0..i.op_count()).any(|n| mmx(i.op_register(n)).is_some());
    let width = if is_mmx { 8 } else { 16 };
    match m {
        M::Movnti => {
            let source = read(i, 1, tx, memory, 4)?;
            write(i, tx, memory, &source, 4)?;
        }
        M::Maskmovq | M::Maskmovdqu => {
            let bytes = read(i, 1, tx, memory, width)?;
            let masks = read(i, 2, tx, memory, width)?;
            let mask = masks[..width]
                .iter()
                .enumerate()
                .fold(0u16, |mask, (n, byte)| mask | (u16::from(byte >> 7) << n));
            let mut address = tx.state.regs[7];
            if i.op0_kind() == OpKind::MemorySegDI {
                address &= 0xffff;
            }
            if i.segment_prefix() == R::FS {
                address = address.wrapping_add(tx.state.fs_base);
            }
            tx.masked_store(memory, address, &bytes[..width], mask)?;
        }
        M::Rcpps | M::Rcpss | M::Rsqrtps | M::Rsqrtss => {
            approximate(i, tx, memory)?;
        }
        M::Movq2dq | M::Movdq2q => {
            let mut bytes = read(i, 1, tx, memory, 8)?;
            bytes[8..].fill(0);
            write(i, tx, memory, &bytes, if m == M::Movq2dq { 16 } else { 8 })?;
        }
        M::Cvtps2dq
        | M::Cvttps2dq
        | M::Cvtdq2ps
        | M::Cvtpd2dq
        | M::Cvttpd2dq
        | M::Cvtdq2pd
        | M::Cvtps2pd
        | M::Cvtpd2ps
        | M::Cvtpi2ps
        | M::Cvtpi2pd
        | M::Cvtps2pi
        | M::Cvttps2pi
        | M::Cvtpd2pi
        | M::Cvttpd2pi => {
            packed_conversion(i, tx, memory)?;
        }
        M::Movaps
        | M::Movapd
        | M::Movups
        | M::Movupd
        | M::Movdqa
        | M::Movdqu
        | M::Movntdq
        | M::Movntps
        | M::Movntpd
        | M::Movntq => {
            let bytes = read(i, 1, tx, memory, width)?;
            write(i, tx, memory, &bytes, width)?;
        }
        M::Movd | M::Movq => {
            let size = if m == M::Movd { 4 } else { 8 };
            let mut bytes = read(i, 1, tx, memory, size)?;
            if xmm(i.op0_register()).is_some() || mmx(i.op0_register()).is_some() {
                bytes[size..].fill(0);
                write(i, tx, memory, &bytes, width)?;
            } else {
                write(i, tx, memory, &bytes, size)?;
            }
        }
        M::Movss | M::Movsd => {
            let size = if m == M::Movss { 4 } else { 8 };
            let bytes = read(i, 1, tx, memory, size)?;
            if i.op1_kind() == OpKind::Memory && xmm(i.op0_register()).is_some() {
                tx.state.xmm[xmm(i.op0_register()).unwrap()] = [0; 16];
            }
            write(i, tx, memory, &bytes, size)?;
        }
        M::Movhlps | M::Movlhps | M::Movlps | M::Movhps | M::Movlpd | M::Movhpd => {
            let source = read(i, 1, tx, memory, 8)?;
            if i.op0_kind() == OpKind::Memory {
                let slot = xmm(i.op1_register()).ok_or(tx.unsupported(i))?;
                let high = matches!(m, M::Movhps | M::Movhpd);
                let bytes = tx.state.xmm[slot];
                let address = tx.address(i)?;
                tx.store(memory, address, &bytes[if high { 8..16 } else { 0..8 }])?;
            } else {
                let slot = xmm(i.op0_register()).ok_or(tx.unsupported(i))?;
                if matches!(m, M::Movlhps | M::Movhps | M::Movhpd) {
                    tx.state.xmm[slot][8..].copy_from_slice(&source[..8]);
                } else {
                    tx.state.xmm[slot][..8]
                        .copy_from_slice(&source[if m == M::Movhlps { 8..16 } else { 0..8 }]);
                }
            }
        }
        M::Movmskps | M::Movmskpd | M::Pmovmskb => {
            let size = if m == M::Movmskps {
                4
            } else if m == M::Movmskpd {
                8
            } else {
                1
            };
            let bytes = read(i, 1, tx, memory, width)?;
            let mut mask = 0;
            for lane in 0..width / size {
                mask |= ((bytes[(lane + 1) * size - 1] >> 7) as u32) << lane;
            }
            tx.state.regs[reg_index(i.op0_register()).ok_or(tx.unsupported(i))?] = mask;
        }
        _ if float_supported(m) => floating(i, tx, memory)?,
        _ => integer(i, tx, memory, width)?,
    }
    if is_mmx {
        enter_mmx(tx);
    }
    Ok(())
}
fn float_supported(m: M) -> bool {
    matches!(
        m,
        M::Addss
            | M::Addsd
            | M::Subss
            | M::Subsd
            | M::Mulss
            | M::Mulsd
            | M::Divss
            | M::Divsd
            | M::Sqrtss
            | M::Sqrtsd
            | M::Minss
            | M::Minsd
            | M::Maxss
            | M::Maxsd
            | M::Addps
            | M::Addpd
            | M::Subps
            | M::Subpd
            | M::Mulps
            | M::Mulpd
            | M::Divps
            | M::Divpd
            | M::Sqrtps
            | M::Sqrtpd
            | M::Minps
            | M::Minpd
            | M::Maxps
            | M::Maxpd
            | M::Comiss
            | M::Comisd
            | M::Ucomiss
            | M::Ucomisd
            | M::Cvtsi2ss
            | M::Cvtsi2sd
            | M::Cvtss2si
            | M::Cvtsd2si
            | M::Cvttss2si
            | M::Cvttsd2si
            | M::Cvtss2sd
            | M::Cvtsd2ss
            | M::Cmpss
            | M::Cmpsd
            | M::Cmpps
            | M::Cmppd
    )
}
fn bits(bytes: &[u8]) -> u64 {
    let mut out = [0; 8];
    out[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(out)
}
fn nan(a: u64, width: usize) -> bool {
    if width == 4 {
        a & 0x7f80_0000 == 0x7f80_0000 && a & 0x7fffff != 0
    } else {
        a & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && a & 0x000f_ffff_ffff_ffff != 0
    }
}
fn signaling(a: u64, width: usize) -> bool {
    nan(a, width) && a & if width == 4 { 1 << 22 } else { 1 << 51 } == 0
}
fn denormal(a: u64, width: usize) -> bool {
    if width == 4 {
        a & 0x7f80_0000 == 0 && a & 0x7fffff != 0
    } else {
        a & 0x7ff0_0000_0000_0000 == 0 && a & 0x000f_ffff_ffff_ffff != 0
    }
}
fn input(tx: &mut Transaction, mut a: u64, width: usize) -> u64 {
    if denormal(a, width) {
        if tx.state.mxcsr & 0x40 != 0 {
            a &= if width == 4 { 1 << 31 } else { 1 << 63 };
        } else {
            finish(tx, 2);
        }
    }
    a
}
fn finish(tx: &mut Transaction, flags: u16) {
    tx.state.mxcsr |= flags as u32;
    let pending = (flags as u32 & 0x3f) & !(tx.state.mxcsr >> 7);
    if pending != 0 {
        tx.sse_exception = true;
    }
}
fn comparison(a: u64, b: u64, width: usize) -> u8 {
    if nan(a, width) || nan(b, width) {
        3
    } else if width == 4 {
        let (a, b) = (f32::from_bits(a as u32), f32::from_bits(b as u32));
        if a == b {
            0
        } else if a < b {
            1
        } else {
            2
        }
    } else {
        let (a, b) = (f64::from_bits(a), f64::from_bits(b));
        if a == b {
            0
        } else if a < b {
            1
        } else {
            2
        }
    }
}
fn floating(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let m = i.mnemonic();
    let double = matches!(
        m,
        M::Addsd
            | M::Subsd
            | M::Mulsd
            | M::Divsd
            | M::Sqrtsd
            | M::Minsd
            | M::Maxsd
            | M::Addpd
            | M::Subpd
            | M::Mulpd
            | M::Divpd
            | M::Sqrtpd
            | M::Minpd
            | M::Maxpd
            | M::Comisd
            | M::Ucomisd
            | M::Cvtsd2si
            | M::Cvttsd2si
            | M::Cvtsd2ss
            | M::Cvtsi2sd
            | M::Cvtss2sd
            | M::Cmpsd
            | M::Cmppd
    );
    let width = if double { 8 } else { 4 };
    sf::configure_sse(tx.state.mxcsr);
    if matches!(
        m,
        M::Cvtsi2ss
            | M::Cvtsi2sd
            | M::Cvtss2si
            | M::Cvtsd2si
            | M::Cvttss2si
            | M::Cvttsd2si
            | M::Cvtss2sd
            | M::Cvtsd2ss
    ) {
        let from = if matches!(m, M::Cvtsi2ss | M::Cvtsi2sd) {
            2
        } else if matches!(m, M::Cvtss2sd | M::Cvtss2si | M::Cvttss2si) {
            0
        } else {
            1
        };
        let to = if matches!(m, M::Cvtss2si | M::Cvtsd2si | M::Cvttss2si | M::Cvttsd2si) {
            2
        } else if matches!(m, M::Cvtsi2ss | M::Cvtsd2ss) {
            0
        } else {
            1
        };
        let source_width = if from == 1 { 8 } else { 4 };
        let source = read(i, 1, tx, memory, source_width)?;
        let mut a = bits(&source[..source_width]);
        if from != 2 {
            a = input(tx, a, source_width);
        }
        let rc = if matches!(m, M::Cvttss2si | M::Cvttsd2si) {
            1
        } else {
            sf::rounding((tx.state.mxcsr >> 13) as u16)
        };
        let mut result = sf::sse_convert(a, from, to, rc);
        finish(tx, sf::flags());
        let target_width = if to == 1 { 8 } else { 4 };
        if to != 2 && tx.state.mxcsr & 0x8000 != 0 && denormal(result, target_width) {
            finish(tx, 0x30);
            result &= if target_width == 4 { 1 << 31 } else { 1 << 63 };
        }
        write(i, tx, memory, &result.to_le_bytes(), target_width)?;
        return Ok(());
    }
    let packed = matches!(
        m,
        M::Addps
            | M::Addpd
            | M::Subps
            | M::Subpd
            | M::Mulps
            | M::Mulpd
            | M::Divps
            | M::Divpd
            | M::Sqrtps
            | M::Sqrtpd
            | M::Minps
            | M::Minpd
            | M::Maxps
            | M::Maxpd
            | M::Cmpps
            | M::Cmppd
    );
    let count = if packed { 16 / width } else { 1 };
    let source = read(i, 1, tx, memory, if packed { 16 } else { width })?;
    let slot = xmm(i.op0_register()).ok_or(tx.unsupported(i))?;
    let destination = tx.state.xmm[slot];
    for lane in 0..count {
        let offset = lane * width;
        let a = if matches!(m, M::Sqrtss | M::Sqrtsd | M::Sqrtps | M::Sqrtpd) {
            0
        } else {
            input(tx, bits(&destination[offset..offset + width]), width)
        };
        let b = input(tx, bits(&source[offset..offset + width]), width);
        if matches!(m, M::Comiss | M::Comisd | M::Ucomiss | M::Ucomisd) {
            if signaling(a, width)
                || signaling(b, width)
                || matches!(m, M::Comiss | M::Comisd) && (nan(a, width) || nan(b, width))
            {
                finish(tx, 1);
            }
            let c = comparison(a, b, width);
            tx.state.eflags = (tx.state.eflags & !0x8d5)
                | match c {
                    0 => 0x40,
                    1 => 1,
                    3 => 0x45,
                    _ => 0,
                };
            continue;
        }
        let result = if matches!(m, M::Cmpss | M::Cmpsd | M::Cmpps | M::Cmppd) {
            let predicate = i.immediate8() & 7;
            let c = comparison(a, b, width);
            if signaling(a, width)
                || signaling(b, width)
                || matches!(predicate, 1 | 2 | 5 | 6) && c == 3
            {
                finish(tx, 1);
            }
            let yes = match predicate {
                0 => c == 0,
                1 => c == 1,
                2 => c <= 1,
                3 => c == 3,
                4 => c != 0,
                5 => c != 1,
                6 => c > 1,
                _ => c != 3,
            };
            if yes { u64::MAX } else { 0 }
        } else if matches!(
            m,
            M::Minss | M::Minsd | M::Minps | M::Minpd | M::Maxss | M::Maxsd | M::Maxps | M::Maxpd
        ) {
            if signaling(a, width) || signaling(b, width) {
                finish(tx, 1);
            }
            let c = comparison(a, b, width);
            if matches!(m, M::Minss | M::Minsd | M::Minps | M::Minpd) && c == 1
                || matches!(m, M::Maxss | M::Maxsd | M::Maxps | M::Maxpd) && c == 2
            {
                a
            } else {
                b
            }
        } else {
            let op = if matches!(m, M::Addss | M::Addsd | M::Addps | M::Addpd) {
                0
            } else if matches!(m, M::Subss | M::Subsd | M::Subps | M::Subpd) {
                1
            } else if matches!(m, M::Mulss | M::Mulsd | M::Mulps | M::Mulpd) {
                2
            } else if matches!(m, M::Divss | M::Divsd | M::Divps | M::Divpd) {
                3
            } else {
                4
            };
            sf::sse_binary(a, b, op, width as u8)
        };
        finish(tx, sf::flags());
        let mut result = result;
        if tx.state.mxcsr & 0x8000 != 0 && denormal(result, width) {
            finish(tx, 0x30);
            result &= if width == 4 { 1 << 31 } else { 1 << 63 };
        }
        tx.state.xmm[slot][offset..offset + width].copy_from_slice(&result.to_le_bytes()[..width]);
    }
    finish(tx, sf::flags());
    Ok(())
}
fn signed(value: u64, width: usize) -> i64 {
    ((value << (64 - width * 8)) as i64) >> (64 - width * 8)
}
fn packed_conversion(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let m = i.mnemonic();
    let from = match m {
        M::Cvtdq2ps | M::Cvtdq2pd | M::Cvtpi2ps | M::Cvtpi2pd => 2,
        M::Cvtpd2dq | M::Cvttpd2dq | M::Cvtpd2ps | M::Cvtpd2pi | M::Cvttpd2pi => 1,
        _ => 0,
    };
    let to = match m {
        M::Cvtdq2ps | M::Cvtpi2ps | M::Cvtpd2ps => 0,
        M::Cvtdq2pd | M::Cvtpi2pd | M::Cvtps2pd => 1,
        _ => 2,
    };
    let count = if matches!(m, M::Cvtdq2ps | M::Cvtps2dq | M::Cvttps2dq) {
        4
    } else {
        2
    };
    let source_width = if from == 1 { 8 } else { 4 };
    let target_width = if to == 1 { 8 } else { 4 };
    let source = read(i, 1, tx, memory, count * source_width)?;
    let mut out = [0; 16];
    let truncation = matches!(m, M::Cvttps2dq | M::Cvttpd2dq | M::Cvttps2pi | M::Cvttpd2pi);
    let rc = if truncation {
        1
    } else {
        sf::rounding((tx.state.mxcsr >> 13) as u16)
    };
    for lane in 0..count {
        sf::configure_sse(tx.state.mxcsr);
        let offset = lane * source_width;
        let mut a = bits(&source[offset..offset + source_width]);
        if from != 2 {
            a = input(tx, a, source_width);
        }
        let mut result = sf::sse_convert(a, from, to, rc);
        finish(tx, sf::flags());
        if to != 2 && tx.state.mxcsr & 0x8000 != 0 && denormal(result, target_width) {
            finish(tx, 0x30);
            result &= if target_width == 4 { 1 << 31 } else { 1 << 63 };
        }
        let offset = lane * target_width;
        out[offset..offset + target_width].copy_from_slice(&result.to_le_bytes()[..target_width]);
    }
    // CVTPI2PS alone preserves the high 64 bits of the destination XMM register.
    let width = if m == M::Cvtpi2ps || mmx(i.op0_register()).is_some() {
        8
    } else {
        16
    };
    write(i, tx, memory, &out, width)
}
fn approximate(i: &Instruction, tx: &mut Transaction, memory: &Memory) -> Result<(), Step> {
    let m = i.mnemonic();
    let count = if matches!(m, M::Rcpps | M::Rsqrtps) {
        4
    } else {
        1
    };
    let source = read(i, 1, tx, memory, count * 4)?;
    let slot = xmm(i.op0_register()).ok_or(tx.unsupported(i))?;
    for (lane, bytes) in source[..count * 4].as_chunks::<4>().0.iter().enumerate() {
        let a = u32::from_le_bytes(*bytes);
        let sign = a & 0x8000_0000;
        let magnitude = a & 0x7fff_ffff;
        let square_root = matches!(m, M::Rsqrtps | M::Rsqrtss);
        let result = if nan(a as u64, 4) {
            a | 0x0040_0000
        } else if magnitude < 0x0080_0000 {
            sign | 0x7f80_0000
        } else if square_root && sign != 0 {
            0xffc0_0000
        } else if magnitude == 0x7f80_0000 {
            sign
        } else {
            // These ISA approximations ignore RC and never raise exceptions.
            // Rounding an exact SoftFloat reciprocal to 12 significant bits
            // satisfies the SDM relative error bound (1.5 * 2^-12).
            sf::configure_sse(0x1f80);
            let denominator = if square_root {
                sf::sse_binary(0, a as u64, 4, 4)
            } else {
                a as u64
            };
            let exact = sf::sse_binary(0x3f80_0000, denominator, 3, 4) as u32;
            let rounded = exact.wrapping_add(0x800) & !0xfff;
            if rounded & 0x7f80_0000 == 0 {
                sign
            } else {
                rounded
            }
        };
        tx.state.xmm[slot][lane * 4..lane * 4 + 4].copy_from_slice(&result.to_le_bytes());
    }
    Ok(())
}
fn integer(
    i: &Instruction,
    tx: &mut Transaction,
    memory: &Memory,
    width: usize,
) -> Result<(), Step> {
    let m = i.mnemonic();
    if m == M::Pinsrw {
        let mut out = read(i, 0, tx, memory, width)?;
        let lane = i.immediate8() as usize & (width / 2 - 1);
        let source = read(
            i,
            1,
            tx,
            memory,
            if i.op1_kind() == OpKind::Memory { 2 } else { 4 },
        )?;
        out[lane * 2..lane * 2 + 2].copy_from_slice(&source[..2]);
        return write(i, tx, memory, &out, width);
    }
    if m == M::Pextrw {
        let source = read(i, 1, tx, memory, width)?;
        let lane = i.immediate8() as usize & (width / 2 - 1);
        let mut out = [0; 4];
        out[..2].copy_from_slice(&source[lane * 2..lane * 2 + 2]);
        return write(
            i,
            tx,
            memory,
            &out,
            if i.op0_kind() == OpKind::Memory { 2 } else { 4 },
        );
    }
    let left = read(i, 0, tx, memory, width)?;
    let right = if i.op1_kind() == OpKind::Immediate8 {
        [0; 16]
    } else {
        read(i, 1, tx, memory, width)?
    };
    let mut out = [0; 16];
    if matches!(
        m,
        M::Xorps
            | M::Xorpd
            | M::Pxor
            | M::Andps
            | M::Andpd
            | M::Pand
            | M::Andnps
            | M::Andnpd
            | M::Pandn
            | M::Orps
            | M::Orpd
            | M::Por
    ) {
        for n in 0..width {
            out[n] = match m {
                M::Xorps | M::Xorpd | M::Pxor => left[n] ^ right[n],
                M::Andps | M::Andpd | M::Pand => left[n] & right[n],
                M::Andnps | M::Andnpd | M::Pandn => !left[n] & right[n],
                _ => left[n] | right[n],
            };
        }
    } else if matches!(
        m,
        M::Punpcklbw
            | M::Punpckhbw
            | M::Punpcklwd
            | M::Punpckhwd
            | M::Punpckldq
            | M::Punpckhdq
            | M::Punpcklqdq
            | M::Punpckhqdq
            | M::Unpcklps
            | M::Unpckhps
            | M::Unpcklpd
            | M::Unpckhpd
    ) {
        let size = match m {
            M::Punpcklbw | M::Punpckhbw => 1,
            M::Punpcklwd | M::Punpckhwd => 2,
            M::Punpckldq | M::Punpckhdq | M::Unpcklps | M::Unpckhps => 4,
            _ => 8,
        };
        let high = matches!(
            m,
            M::Punpckhbw | M::Punpckhwd | M::Punpckhdq | M::Punpckhqdq | M::Unpckhps | M::Unpckhpd
        );
        let base = if high { width / 2 } else { 0 };
        for n in 0..width / (2 * size) {
            out[n * 2 * size..n * 2 * size + size]
                .copy_from_slice(&left[base + n * size..base + (n + 1) * size]);
            out[n * 2 * size + size..(n + 1) * 2 * size]
                .copy_from_slice(&right[base + n * size..base + (n + 1) * size]);
        }
    } else if matches!(m, M::Packsswb | M::Packssdw | M::Packuswb) {
        let size = if m == M::Packssdw { 2 } else { 1 };
        for (n, chunk) in left[..width]
            .chunks_exact(size * 2)
            .chain(right[..width].chunks_exact(size * 2))
            .enumerate()
        {
            let value = signed(bits(chunk), size * 2);
            let value = if m == M::Packuswb {
                value.clamp(0, 255)
            } else if size == 1 {
                value.clamp(-128, 127)
            } else {
                value.clamp(-32768, 32767)
            };
            out[n * size..(n + 1) * size].copy_from_slice(&value.to_le_bytes()[..size]);
        }
    } else if matches!(m, M::Pshufw | M::Pshufd | M::Pshuflw | M::Pshufhw) {
        let source = read(i, 1, tx, memory, width)?;
        out = source;
        let size = if m == M::Pshufd { 4 } else { 2 };
        let base = if m == M::Pshufhw { 8 } else { 0 };
        let imm = i.immediate8();
        for lane in 0..4 {
            let index = ((imm >> (lane * 2)) & 3) as usize;
            out[base + lane * size..base + (lane + 1) * size]
                .copy_from_slice(&source[base + index * size..base + (index + 1) * size]);
        }
    } else if matches!(m, M::Shufps | M::Shufpd) {
        let imm = i.immediate8();
        if m == M::Shufps {
            for n in 0..4 {
                let source = if n < 2 { &left } else { &right };
                let index = ((imm >> (n * 2)) & 3) as usize;
                out[n * 4..n * 4 + 4].copy_from_slice(&source[index * 4..index * 4 + 4]);
            }
        } else {
            out[..8].copy_from_slice(&left[(imm as usize & 1) * 8..(imm as usize & 1) * 8 + 8]);
            out[8..].copy_from_slice(
                &right[((imm as usize >> 1) & 1) * 8..((imm as usize >> 1) & 1) * 8 + 8],
            );
        }
    } else if matches!(m, M::Pslldq | M::Psrldq) {
        let count = (i.immediate8() as usize).min(16);
        if m == M::Pslldq {
            out[count..].copy_from_slice(&left[..16 - count]);
        } else {
            out[..16 - count].copy_from_slice(&left[count..]);
        }
    } else if m == M::Psadbw {
        for offset in (0..width).step_by(8) {
            let sum: u64 = left[offset..offset + 8]
                .iter()
                .zip(&right[offset..offset + 8])
                .map(|(&a, &b)| u64::from(a.abs_diff(b)))
                .sum();
            out[offset..offset + 8].copy_from_slice(&sum.to_le_bytes());
        }
    } else if m == M::Pmuludq {
        for offset in (0..width).step_by(8) {
            let product = bits(&left[offset..offset + 4]) * bits(&right[offset..offset + 4]);
            out[offset..offset + 8].copy_from_slice(&product.to_le_bytes());
        }
    } else if m == M::Pinsrw {
        out = left;
        let lane = i.immediate8() as usize & (width / 2 - 1);
        let source = read(
            i,
            1,
            tx,
            memory,
            if i.op1_kind() == OpKind::Memory { 2 } else { 4 },
        )?;
        out[lane * 2..lane * 2 + 2].copy_from_slice(&source[..2]);
    } else if m == M::Pextrw {
        let source = read(i, 1, tx, memory, width)?;
        let lane = i.immediate8() as usize & (width / 2 - 1);
        let mut out = [0; 4];
        out[..2].copy_from_slice(&source[lane * 2..lane * 2 + 2]);
        return write(
            i,
            tx,
            memory,
            &out,
            if i.op0_kind() == OpKind::Memory { 2 } else { 4 },
        );
    } else {
        let size = match m {
            M::Paddb
            | M::Psubb
            | M::Paddusb
            | M::Psubusb
            | M::Paddsb
            | M::Psubsb
            | M::Pavgb
            | M::Pcmpeqb
            | M::Pcmpgtb
            | M::Pminub
            | M::Pmaxub => 1,
            M::Paddd | M::Psubd | M::Pcmpeqd | M::Pcmpgtd | M::Pslld | M::Psrld | M::Psrad => 4,
            M::Paddq | M::Psubq | M::Psllq | M::Psrlq => 8,
            _ => 2,
        };
        let lane_bits = size * 8;
        let max = if size == 8 {
            u64::MAX
        } else {
            (1u64 << lane_bits) - 1
        };
        for lane in 0..width / size {
            let n = lane * size;
            let (a, b) = (bits(&left[n..n + size]), bits(&right[n..n + size]));
            let (sa, sb) = (signed(a, size), signed(b, size));
            let value = match m {
                M::Paddb | M::Paddw | M::Paddd | M::Paddq => a.wrapping_add(b),
                M::Psubb | M::Psubw | M::Psubd | M::Psubq => a.wrapping_sub(b),
                M::Paddusb | M::Paddusw => (a + b).min(max),
                M::Psubusb | M::Psubusw => a.saturating_sub(b),
                M::Paddsb | M::Paddsw => (sa + sb)
                    .clamp(-(1i64 << (lane_bits - 1)), (1i64 << (lane_bits - 1)) - 1)
                    as u64,
                M::Psubsb | M::Psubsw => (sa - sb)
                    .clamp(-(1i64 << (lane_bits - 1)), (1i64 << (lane_bits - 1)) - 1)
                    as u64,
                M::Pavgb | M::Pavgw => (a + b + 1) >> 1,
                M::Pcmpeqb | M::Pcmpeqw | M::Pcmpeqd => {
                    if a == b {
                        max
                    } else {
                        0
                    }
                }
                M::Pcmpgtb | M::Pcmpgtw | M::Pcmpgtd => {
                    if sa > sb {
                        max
                    } else {
                        0
                    }
                }
                M::Pmullw => a * b,
                M::Pmulhw => ((sa * sb) >> 16) as u64,
                M::Pmulhuw => (a * b) >> 16,
                M::Pminub => a.min(b),
                M::Pmaxub => a.max(b),
                M::Pminsw => sa.min(sb) as u64,
                M::Pmaxsw => sa.max(sb) as u64,
                M::Psllw
                | M::Pslld
                | M::Psllq
                | M::Psrlw
                | M::Psrld
                | M::Psrlq
                | M::Psraw
                | M::Psrad => {
                    let count = if i.op1_kind() == OpKind::Immediate8 {
                        i.immediate8() as u64
                    } else {
                        bits(&right[..8])
                    };
                    if matches!(m, M::Psraw | M::Psrad) {
                        (sa >> count.min((lane_bits - 1) as u64)) as u64
                    } else if count >= lane_bits as u64 {
                        0
                    } else if matches!(m, M::Psllw | M::Pslld | M::Psllq) {
                        a << count
                    } else {
                        a >> count
                    }
                }
                M::Pmaddwd => {
                    if lane % 2 != 0 {
                        continue;
                    }
                    let next = n + 2;
                    let sa2 = signed(bits(&left[next..next + 2]), 2);
                    let sb2 = signed(bits(&right[next..next + 2]), 2);
                    let value = (sa * sb + sa2 * sb2) as i32;
                    out[n..n + 4].copy_from_slice(&value.to_le_bytes());
                    continue;
                }
                _ => return Err(tx.unsupported(i)),
            };
            out[n..n + size].copy_from_slice(&value.to_le_bytes()[..size]);
        }
    }
    write(i, tx, memory, &out, width)
}
