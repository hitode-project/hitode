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
    memory: Operands<'_>,
    width: usize,
) -> Result<[u8; 16], Step> {
    let mut bytes = [0; 16];
    if i.op_kind(n) == OpKind::Memory {
        let address = tx.address(i, memory)?;
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
    memory: Operands<'_>,
    bytes: &[u8],
    width: usize,
) -> Result<(), Step> {
    if i.op0_kind() == OpKind::Memory {
        let address = tx.address(i, memory)?;
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
pub(super) fn execute(
    i: &Instruction,
    tx: &mut Transaction,
    memory: Operands<'_>,
) -> Result<(), Step> {
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
        tx.read(memory, tx.address(i, memory)?, 1)?;
        return Ok(());
    }
    if matches!(m, M::Emms | M::Femms) {
        tx.state.x87.tag = 0xffff;
        return Ok(());
    }
    if matches!(m, M::Ldmxcsr | M::Stmxcsr) {
        let address = tx.address(i, memory)?;
        if m == M::Ldmxcsr {
            let value =
                u32::from_le_bytes(tx.read(memory, address, 4)?.as_ref().try_into().unwrap());
            if value & !0xffff != 0 {
                return Err(Step::Trap {
                    pc: tx.pc,
                    vector: 13,
                });
            }
            *tx.state.mxcsr = value;
        } else {
            tx.store(memory, address, &tx.state.mxcsr.to_le_bytes())?;
        }
        return Ok(());
    }
    if matches!(memory, Operands::Guest(_))
        && i.memory_size().size() == 16
        && !matches!(m, M::Movups | M::Movupd | M::Movdqu)
        && (i.op0_kind() == OpKind::Memory || i.op1_kind() == OpKind::Memory)
    {
        let address = tx.address(i, memory)?;
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
                let address = tx.address(i, memory)?;
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
    // Fixed-width lanes avoid a variable-size memcpy for every packed element.
    // Keep the fallback for callers that need a partial, zero-extended value.
    match bytes.len() {
        1 => u64::from(bytes[0]),
        2 => u64::from(u16::from_le_bytes(bytes.try_into().unwrap())),
        4 => u64::from(u32::from_le_bytes(bytes.try_into().unwrap())),
        8 => u64::from_le_bytes(bytes.try_into().unwrap()),
        _ => {
            let mut out = [0; 8];
            out[..bytes.len()].copy_from_slice(bytes);
            u64::from_le_bytes(out)
        }
    }
}

#[cfg(test)]
#[test]
fn packed_lane_reads_preserve_width_endianness_and_unaligned_input() {
    for len in 0..=8 {
        for offset in 0..16 {
            for seed in [0u8, 1, 0x7f, 0x80, 0xff] {
                let bytes: [u8; 24] = std::array::from_fn(|n| seed.wrapping_add((n * 37) as u8));
                let lane = &bytes[offset..offset + len];
                let expected = lane
                    .iter()
                    .enumerate()
                    .fold(0u64, |v, (n, b)| v | (u64::from(*b) << (n * 8)));
                assert_eq!(bits(lane), expected);
            }
        }
    }
}

#[cfg(test)]
#[test]
fn sse_minmax_qnan_raises_ie_and_float_to_int_skips_de() {
    use crate::{CpuState, Memory, READ, WRITE};
    use iced_x86::{Decoder, DecoderOptions};
    let run = |bytes: &[u8], mxcsr: u32, xmm0: [u8; 16], xmm1: [u8; 16]| {
        let mut decoder = Decoder::with_ip(32, bytes, 0x1000, DecoderOptions::NONE);
        let insn = decoder.decode();
        let op = operation(&insn).unwrap();
        let mut state = CpuState {
            eip: 0x1000,
            mxcsr,
            ..CpuState::default()
        };
        state.xmm[0] = xmm0;
        state.xmm[1] = xmm1;
        let mut mem = Memory::new(0x1000, 0x2000).unwrap();
        mem.protect(0x1000, 0x2000, READ | WRITE).unwrap();
        let step = super::execute(&insn, op, None, &mut state, &mut mem, false);
        (step, state)
    };

    // Quiet NaN in MIN*/MAX* raises IE (bit 0) and selects the second operand.
    let mut q32 = [0u8; 16];
    let mut one32 = [0u8; 16];
    for lane in 0..4 {
        q32[lane * 4..lane * 4 + 4].copy_from_slice(&0x7fc0_0001u32.to_le_bytes());
        one32[lane * 4..lane * 4 + 4].copy_from_slice(&1.0f32.to_le_bytes());
    }
    let mut q64 = [0u8; 16];
    let mut one64 = [0u8; 16];
    for lane in 0..2 {
        q64[lane * 8..lane * 8 + 8].copy_from_slice(&0x7ff8_0000_0000_0001u64.to_le_bytes());
        one64[lane * 8..lane * 8 + 8].copy_from_slice(&1.0f64.to_le_bytes());
    }
    for (bytes, a, b, width) in [
        (&[0xf3, 0x0f, 0x5d, 0xc1][..], q32, one32, 4), // minss
        (&[0xf3, 0x0f, 0x5f, 0xc1][..], q32, one32, 4), // maxss
        (&[0x0f, 0x5d, 0xc1][..], q32, one32, 16),      // minps
        (&[0x0f, 0x5f, 0xc1][..], q32, one32, 16),      // maxps
        (&[0xf2, 0x0f, 0x5d, 0xc1][..], q64, one64, 8), // minsd
        (&[0xf2, 0x0f, 0x5f, 0xc1][..], q64, one64, 8), // maxsd
        (&[0x66, 0x0f, 0x5d, 0xc1][..], q64, one64, 16), // minpd
        (&[0x66, 0x0f, 0x5f, 0xc1][..], q64, one64, 16), // maxpd
    ] {
        let (step, st) = run(bytes, 0x1f80, a, b);
        assert_eq!(step, Step::Executed);
        assert_eq!(st.mxcsr & 0x3f, 0x01);
        let mut want = a;
        want[..width].copy_from_slice(&b[..width]);
        assert_eq!(st.xmm[0], want);
    }

    // Float-to-int conversions never raise DE: DAZ=0 sets PE=1 (no #XM when DM=0),
    // DAZ=1 flushes subnormals to 0 (PE=0, DE=0).
    let mut sub32 = [0u8; 16];
    for lane in 0..4 {
        sub32[lane * 4..lane * 4 + 4].copy_from_slice(&1u32.to_le_bytes());
    }
    let mut sub64 = [0u8; 16];
    for lane in 0..2 {
        sub64[lane * 8..lane * 8 + 8].copy_from_slice(&1u64.to_le_bytes());
    }
    for (bytes, src) in [
        (&[0xf3, 0x0f, 0x5b, 0xc1][..], sub32), // cvttps2dq
        (&[0x66, 0x0f, 0x5b, 0xc1][..], sub32), // cvtps2dq
        (&[0x66, 0x0f, 0xe6, 0xc1][..], sub64), // cvttpd2dq
        (&[0xf2, 0x0f, 0xe6, 0xc1][..], sub64), // cvtpd2dq
        (&[0xf3, 0x0f, 0x2c, 0xc1][..], sub32), // cvttss2si
        (&[0xf3, 0x0f, 0x2d, 0xc1][..], sub32), // cvtss2si
        (&[0xf2, 0x0f, 0x2c, 0xc1][..], sub64), // cvttsd2si
        (&[0xf2, 0x0f, 0x2d, 0xc1][..], sub64), // cvtsd2si
    ] {
        let (step, st) = run(bytes, 0x1e80, [0; 16], src); // DAZ=0, DM=0
        assert_eq!(step, Step::Executed);
        assert_eq!(st.mxcsr & 0x3f, 0x20);
        let (step, st) = run(bytes, 0x1ec0, [0; 16], src); // DAZ=1, DM=0
        assert_eq!(step, Step::Executed);
        assert_eq!(st.mxcsr & 0x3f, 0x00);
    }

    // Intel SDM Vol. 1 §11.5.3.2: unmasked pre-computation IE takes priority
    // over post-computation PE across packed lanes. On [1.5, qNaN, ...], an
    // unmasked IE commits only IE (0x01) before #XM and preserves dst; when
    // IE is masked (IM=1), both IE and PE (0x21) commit.
    let mut mixed32 = [0u8; 16];
    mixed32[0..4].copy_from_slice(&1.5f32.to_le_bytes());
    mixed32[4..8].copy_from_slice(&0x7fc0_0000u32.to_le_bytes());
    let mut mixed64 = [0u8; 16];
    mixed64[0..8].copy_from_slice(&1.5f64.to_le_bytes());
    mixed64[8..16].copy_from_slice(&0x7ff8_0000_0000_0000u64.to_le_bytes());
    let sentinel = [0xa5u8; 16];
    for (bytes, src) in [
        (&[0xf3, 0x0f, 0x5b, 0xc1][..], mixed32), // cvttps2dq
        (&[0x66, 0x0f, 0x5b, 0xc1][..], mixed32), // cvtps2dq
        (&[0x66, 0x0f, 0xe6, 0xc1][..], mixed64), // cvttpd2dq
        (&[0xf2, 0x0f, 0xe6, 0xc1][..], mixed64), // cvtpd2dq
    ] {
        // MXCSR=0x0000 (IM=0, PM=0): #XM with only IE committed, dst intact.
        let (step, st) = run(bytes, 0x0000, sentinel, src);
        assert_eq!(
            step,
            Step::Trap {
                pc: 0x1000,
                vector: 19
            }
        );
        assert_eq!(st.mxcsr, 0x0001);
        assert_eq!(st.xmm[0], sentinel);
        // MXCSR=0x1000 (IM=0, PM=1): #XM with only IE committed, dst intact.
        let (step, st) = run(bytes, 0x1000, sentinel, src);
        assert_eq!(
            step,
            Step::Trap {
                pc: 0x1000,
                vector: 19
            }
        );
        assert_eq!(st.mxcsr, 0x1001);
        assert_eq!(st.xmm[0], sentinel);
        // MXCSR=0x0080 (IM=1, PM=0): IE masked, #XM on PE commits both IE|PE.
        let (step, st) = run(bytes, 0x0080, sentinel, src);
        assert_eq!(
            step,
            Step::Trap {
                pc: 0x1000,
                vector: 19
            }
        );
        assert_eq!(st.mxcsr, 0x00a1);
        assert_eq!(st.xmm[0], sentinel);
        // MXCSR=0x1080 (IM=1, PM=1): completes and commits both IE|PE.
        let (step, st) = run(bytes, 0x1080, sentinel, src);
        assert_eq!(step, Step::Executed);
        assert_eq!(st.mxcsr, 0x10a1);
        assert_eq!(
            i32::from_le_bytes(st.xmm[0][4..8].try_into().unwrap()),
            i32::MIN
        );
    }

    // On x86 (and ARM FPCR.AH=1), a NaN operand suppresses DE on a denormal peer
    // when DAZ=0, while DAZ=1 still flushes the denormal operand to signed zero.
    let mut sub_d = [0u8; 16];
    sub_d[..4].copy_from_slice(&0x0040_0000u32.to_le_bytes());
    let mut snan32 = [0u8; 16];
    snan32[..4].copy_from_slice(&0x7f80_0001u32.to_le_bytes());
    // addss(D, QNaN) and addss(QNaN, D): no DE, returns QNaN.
    let (step, st) = run(&[0xf3, 0x0f, 0x58, 0xc1], 0x1f80, sub_d, q32);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x00);
    let (step, st) = run(&[0xf3, 0x0f, 0x58, 0xc1], 0x1f80, q32, sub_d);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x00);
    // addss(D, SNaN): IE only (no DE).
    let (step, st) = run(&[0xf3, 0x0f, 0x58, 0xc1], 0x1f80, sub_d, snan32);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x01);
    // ucomiss(D, QNaN): no DE, no IE; comiss(D, QNaN): IE only (no DE).
    let (step, st) = run(&[0x0f, 0x2e, 0xc1], 0x1f80, sub_d, q32);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x00);
    let (step, st) = run(&[0x0f, 0x2f, 0xc1], 0x1f80, sub_d, q32);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x01);
    // minss(QNaN, D): DAZ=0 returns D with IE only (no DE); DAZ=1 returns +0 with IE.
    let (step, st) = run(&[0xf3, 0x0f, 0x5d, 0xc1], 0x1f80, q32, sub_d);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x01);
    assert_eq!(&st.xmm[0][..4], &0x0040_0000u32.to_le_bytes());
    let (step, st) = run(&[0xf3, 0x0f, 0x5d, 0xc1], 0x1fc0, q32, sub_d);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x01);
    assert_eq!(&st.xmm[0][..4], &0u32.to_le_bytes());
    // minss(D, 1.0) under FTZ=1, DAZ=0 preserves subnormal output with DE only (no UE|PE).
    let (step, st) = run(&[0xf3, 0x0f, 0x5d, 0xc1], 0x9f80, sub_d, one32);
    assert_eq!(step, Step::Executed);
    assert_eq!(st.mxcsr & 0x3f, 0x02);
    assert_eq!(&st.xmm[0][..4], &0x0040_0000u32.to_le_bytes());
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
fn input_checked(tx: &mut Transaction, mut a: u64, width: usize, report_de: bool) -> u64 {
    if denormal(a, width) {
        if *tx.state.mxcsr & 0x40 != 0 {
            a &= if width == 4 { 1 << 31 } else { 1 << 63 };
        } else if report_de {
            finish(tx, 2);
        }
    }
    a
}
/// [`input_checked`] for arithmetic that commits its flags itself: returns
/// the operand and the `DE` it raises.
fn input_flags(tx: &Transaction, mut a: u64, width: usize, report_de: bool) -> (u64, u16) {
    if denormal(a, width) {
        if *tx.state.mxcsr & 0x40 != 0 {
            a &= if width == 4 { 1 << 31 } else { 1 << 63 };
        } else if report_de {
            return (a, 2);
        }
    }
    (a, 0)
}
fn input(tx: &mut Transaction, a: u64, width: usize) -> u64 {
    input_checked(tx, a, width, true)
}
/// Denormal handling for SSE float-to-integer conversions: `DAZ = 1` flushes
/// subnormals to signed zero (`PE = 0`), while `DAZ = 0` converts subnormals
/// without raising `DE` (Intel SDM Vol. 2 `CVT*2SI`/`CVT*2DQ`/`CVT*2PI`).
fn input_to_int(tx: &Transaction, mut a: u64, width: usize) -> u64 {
    if denormal(a, width) && *tx.state.mxcsr & 0x40 != 0 {
        a &= if width == 4 { 1 << 31 } else { 1 << 63 };
    }
    a
}
fn finish(tx: &mut Transaction, flags: u16) {
    *tx.state.mxcsr |= flags as u32;
    let pending = (flags as u32 & 0x3f) & !(*tx.state.mxcsr >> 7);
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
fn floating(i: &Instruction, tx: &mut Transaction, memory: Operands<'_>) -> Result<(), Step> {
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
    sf::configure_sse(*tx.state.mxcsr);
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
            a = if to == 2 {
                input_to_int(tx, a, source_width)
            } else {
                input(tx, a, source_width)
            };
        }
        let rc = if matches!(m, M::Cvttss2si | M::Cvttsd2si) {
            1
        } else {
            sf::rounding((*tx.state.mxcsr >> 13) as u16)
        };
        let mut result = sf::sse_convert(a, from, to, rc);
        finish(tx, sf::flags());
        let target_width = if to == 1 { 8 } else { 4 };
        if to != 2 && *tx.state.mxcsr & 0x8000 != 0 && denormal(result, target_width) {
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
    let masks = (*tx.state.mxcsr >> 7) as u16 & 0x3f;
    // Pre-computation (IE/DE/ZE) and post-computation (OE/UE/PE) flags of
    // every lane, committed together after the last lane.
    let (mut pre, mut post) = (0u16, 0u16);
    for lane in 0..count {
        sf::configure_sse(*tx.state.mxcsr);
        let offset = lane * width;
        let unary_sqrt = matches!(m, M::Sqrtss | M::Sqrtsd | M::Sqrtps | M::Sqrtpd);
        let raw_a = if unary_sqrt {
            0
        } else {
            bits(&destination[offset..offset + width])
        };
        let raw_b = bits(&source[offset..offset + width]);
        // On x86, a NaN operand suppresses DE on a denormal peer when DAZ=0,
        // while DAZ=1 still flushes the denormal operand to signed zero.
        let report_de = !(nan(raw_a, width) || nan(raw_b, width));
        let (a, a_flags) = if unary_sqrt {
            (0, 0)
        } else {
            input_flags(tx, raw_a, width, report_de)
        };
        let (b, b_flags) = input_flags(tx, raw_b, width, report_de);
        pre |= a_flags | b_flags;
        if matches!(m, M::Comiss | M::Comisd | M::Ucomiss | M::Ucomisd) {
            if signaling(a, width)
                || signaling(b, width)
                || matches!(m, M::Comiss | M::Comisd) && (nan(a, width) || nan(b, width))
            {
                pre |= 1;
            }
            let c = comparison(a, b, width);
            *tx.state.eflags = (*tx.state.eflags & !0x8d5)
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
                pre |= 1;
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
            if nan(a, width) || nan(b, width) {
                pre |= 1;
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
        let mut flags = sf::flags();
        let mut result = result;
        // MIN*/MAX* select an operand directly and do not apply FTZ or underflow.
        if !matches!(
            m,
            M::Minss | M::Minsd | M::Minps | M::Minpd | M::Maxss | M::Maxsd | M::Maxps | M::Maxpd
        ) && *tx.state.mxcsr & 0x8000 != 0
            && denormal(result, width)
        {
            flags |= 0x30;
            result &= if width == 4 { 1 << 31 } else { 1 << 63 };
        }
        // An unmasked overflow does not also report PE (measured on Intel:
        // `mulps` overflow under MXCSR 0x1b80 commits OE alone).
        if flags & 0x08 != 0 && masks & 0x08 == 0 {
            flags &= !0x20;
        }
        pre |= flags & 0x07;
        post |= flags & 0x38;
        tx.state.xmm[slot][offset..offset + width].copy_from_slice(&result.to_le_bytes()[..width]);
    }
    // Intel SDM Vol. 1 11.5.3.2: an unmasked pre-computation exception in any
    // lane commits pre-computation flags alone before #XM.
    if pre & !masks != 0 {
        finish(tx, pre);
    } else {
        finish(tx, pre | post);
    }
    Ok(())
}
fn signed(value: u64, width: usize) -> i64 {
    ((value << (64 - width * 8)) as i64) >> (64 - width * 8)
}
fn packed_conversion(
    i: &Instruction,
    tx: &mut Transaction,
    memory: Operands<'_>,
) -> Result<(), Step> {
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
        sf::rounding((*tx.state.mxcsr >> 13) as u16)
    };
    let mut pre_flags = 0u16;
    let mut post_flags = 0u16;
    for lane in 0..count {
        sf::configure_sse(*tx.state.mxcsr);
        let offset = lane * source_width;
        let mut a = bits(&source[offset..offset + source_width]);
        if from != 2 {
            if to == 2 {
                a = input_to_int(tx, a, source_width);
            } else if denormal(a, source_width) {
                if *tx.state.mxcsr & 0x40 != 0 {
                    a &= if source_width == 4 { 1 << 31 } else { 1 << 63 };
                } else {
                    pre_flags |= 2;
                }
            }
        }
        let mut result = sf::sse_convert(a, from, to, rc);
        let flags = sf::flags();
        pre_flags |= flags & 0x07;
        post_flags |= flags & 0x38;
        if to != 2 && *tx.state.mxcsr & 0x8000 != 0 && denormal(result, target_width) {
            post_flags |= 0x30;
            result &= if target_width == 4 { 1 << 31 } else { 1 << 63 };
        }
        let offset = lane * target_width;
        out[offset..offset + target_width].copy_from_slice(&result.to_le_bytes()[..target_width]);
    }
    // Intel SDM Vol. 1 §11.5.3.2: unmasked pre-computation exceptions (IE/DE/ZE)
    // take priority over post-computation exceptions (OE/UE/PE) across packed
    // lanes and commit without post-computation flags before raising #XM.
    let masks = (*tx.state.mxcsr >> 7) as u16;
    if pre_flags & !masks != 0 {
        finish(tx, pre_flags);
    } else {
        finish(tx, pre_flags | post_flags);
    }
    // CVTPI2PS alone preserves the high 64 bits of the destination XMM register.
    let width = if m == M::Cvtpi2ps || mmx(i.op0_register()).is_some() {
        8
    } else {
        16
    };
    write(i, tx, memory, &out, width)
}
/// Exact 12-bit reciprocal estimate matching x86 `rcpss`/`rcpps` and ARMv8.7
/// `FEAT_RPRES` `frecpe` (under `FPCR.AH=1`) across all 2^32 binary32 inputs.
pub(crate) fn rcp_estimate(u: u32) -> u32 {
    let sign = u & 0x8000_0000;
    let exp = (u >> 23) & 0xff;
    let frac = u & 0x007f_ffff;
    if exp == 255 {
        return if frac != 0 { u | 0x0040_0000 } else { sign };
    }
    if exp == 0 {
        return sign | 0x7f80_0000;
    }
    if exp >= 253 {
        return sign;
    }
    let i = (u >> 12) & 0x7ff;
    let a = 4096 + 2 * i + 1;
    let sig = (((1u32 << 26) / a) + 1) >> 1;
    sign | ((253 - exp) << 23) | ((sig & 0xfff) << 11)
}

/// Exact 12-bit reciprocal square-root estimate matching x86 `rsqrtss`/`rsqrtps`
/// and ARMv8.7 `FEAT_RPRES` `frsqrte` (under `FPCR.AH=1`) across all 2^32 inputs.
pub(crate) fn rsqrt_estimate(u: u32) -> u32 {
    let sign = u & 0x8000_0000;
    let exp = (u >> 23) & 0xff;
    let frac = u & 0x007f_ffff;
    if exp == 255 && frac != 0 {
        return u | 0x0040_0000;
    }
    if exp == 0 {
        return sign | 0x7f80_0000;
    }
    if sign != 0 {
        return 0xffc0_0000;
    }
    if exp == 255 {
        return 0;
    }
    let i = u64::from((u >> 13) & 0x3ff);
    let b = (2048 + 2 * i + 1) << ((exp & 1) ^ 1);
    // Unique s in 4096..=8191 with b*(2s - 1)^2 <= 2^39 < b*(2s + 1)^2:
    // with r = isqrt(2^39 / b), 2s - 1 <= r <= 2s gives s = (r + 1) >> 1.
    let r = ((1u64 << 39) / b).isqrt();
    let sig = ((r + 1) >> 1) as u32;
    (((380 - exp) >> 1) << 23) | ((sig & 0xfff) << 11)
}

fn approximate(i: &Instruction, tx: &mut Transaction, memory: Operands<'_>) -> Result<(), Step> {
    let m = i.mnemonic();
    let count = if matches!(m, M::Rcpps | M::Rsqrtps) {
        4
    } else {
        1
    };
    let square_root = matches!(m, M::Rsqrtps | M::Rsqrtss);
    let source = read(i, 1, tx, memory, count * 4)?;
    let slot = xmm(i.op0_register()).ok_or(tx.unsupported(i))?;
    for (lane, bytes) in source[..count * 4].as_chunks::<4>().0.iter().enumerate() {
        let a = u32::from_le_bytes(*bytes);
        // These ISA approximations ignore MXCSR/RC and never raise exceptions.
        let result = if square_root {
            rsqrt_estimate(a)
        } else {
            rcp_estimate(a)
        };
        tx.state.xmm[slot][lane * 4..lane * 4 + 4].copy_from_slice(&result.to_le_bytes());
    }
    Ok(())
}
fn integer(
    i: &Instruction,
    tx: &mut Transaction,
    memory: Operands<'_>,
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
    if let Some(kernel) = packed::kernel(m) {
        let mut right = right;
        if i.op1_kind() == OpKind::Immediate8 {
            right[0] = i.immediate8();
        }
        return write(i, tx, memory, &kernel(left, right), width);
    }
    let mut out = [0; 16];
    if matches!(
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
    } else if m == M::Pmaddwd {
        for n in (0..width).step_by(4) {
            let a = signed(bits(&left[n..n + 2]), 2);
            let b = signed(bits(&right[n..n + 2]), 2);
            let c = signed(bits(&left[n + 2..n + 4]), 2);
            let d = signed(bits(&right[n + 2..n + 4]), 2);
            out[n..n + 4].copy_from_slice(&((a * b + c * d) as i32).to_le_bytes());
        }
    } else {
        return Err(tx.unsupported(i));
    }

    write(i, tx, memory, &out, width)
}

#[cfg(test)]
mod tests {
    use super::{rcp_estimate, rsqrt_estimate};

    #[test]
    fn rcp_and_rsqrt_edge_cases_match_x86_and_rpres() {
        // NaNs keep sign and payload with quiet bit set.
        for nan in [0x7fc0_0001u32, 0xffc0_0001, 0x7f80_0001, 0xff80_0001] {
            assert_eq!(rcp_estimate(nan), nan | 0x0040_0000);
            assert_eq!(rsqrt_estimate(nan), nan | 0x0040_0000);
        }

        // Zeros and subnormals (exp == 0) yield signed infinity for both,
        // including negative subnormals in rsqrt (checked before sign).
        for zero_or_sub in [
            0x0000_0000u32,
            0x8000_0000,
            0x0000_0001,
            0x8040_0000,
            0x007f_ffff,
            0x807f_ffff,
        ] {
            let expected = (zero_or_sub & 0x8000_0000) | 0x7f80_0000;
            assert_eq!(rcp_estimate(zero_or_sub), expected);
            assert_eq!(rsqrt_estimate(zero_or_sub), expected);
        }

        // Infinities and large exponents (exp >= 253) for rcp.
        assert_eq!(rcp_estimate(0x7f80_0000), 0x0000_0000);
        assert_eq!(rcp_estimate(0xff80_0000), 0x8000_0000);
        assert_eq!(rcp_estimate(0x7e00_0000), 0x00ff_f000);
        assert_eq!(rcp_estimate(0xfe00_0000), 0x80ff_f000);
        assert_eq!(rcp_estimate(0x7e80_0000), 0x0000_0000);
        assert_eq!(rcp_estimate(0xfe80_0000), 0x8000_0000);
        assert_eq!(rcp_estimate(0x7f7f_ffff), 0x0000_0000);
        assert_eq!(rcp_estimate(0xff7f_ffff), 0x8000_0000);

        // Negative normals and -Inf yield -QNaN indefinite in rsqrt; +Inf yields +0.
        for neg in [0x8080_0000u32, 0xbf80_0000, 0xff7f_ffff, 0xff80_0000] {
            assert_eq!(rsqrt_estimate(neg), 0xffc0_0000);
        }
        assert_eq!(rsqrt_estimate(0x7f80_0000), 0x0000_0000);

        // Hardware spot checks (Intel rcpps/rsqrtps and Apple/ARM RPRES AH=1).
        assert_eq!(rcp_estimate(1.0f32.to_bits()), 0x3f7f_f000);
        assert_eq!(rcp_estimate((-1.0f32).to_bits()), 0xbf7f_f000);
        assert_eq!(rcp_estimate(2.5f32.to_bits()), 0x3ecc_c000);
        assert_eq!(rcp_estimate((-3.5f32).to_bits()), 0xbe92_4000);
        assert_eq!(rsqrt_estimate(1.0f32.to_bits()), 0x3f7f_f000);
        assert_eq!(rsqrt_estimate(0x7f7f_ffff), 0x1f80_0800);
    }

    #[test]
    fn rcp_and_rsqrt_all_buckets_satisfy_closed_form_and_reference_hashes() {
        let mut h_rcp = 1469598103934665603u64;
        for i in 0..2048u32 {
            let u = (127u32 << 23) | (i << 12);
            let out = rcp_estimate(u);
            // Low 11 bits of the 12-bit estimate are always zero.
            assert_eq!(out & 0x7ff, 0);
            let x = f32::from_bits(u) as f64;
            let exact = 1.0 / x;
            let got = f32::from_bits(out) as f64;
            assert!((got - exact).abs() / exact <= 1.5 / 4096.0);
            // Result depends only on sign, exponent, and top 11 fraction bits.
            assert_eq!(rcp_estimate(u | 0xfff), out);
            h_rcp = (h_rcp ^ u64::from(out)).wrapping_mul(1099511628211);
        }
        assert_eq!(h_rcp, 0xc1d4_1477_5da7_e383);

        let mut h_rsq = 1469598103934665603u64;
        for parity in 0..2u32 {
            let exp = if parity != 0 { 127u32 } else { 128u32 };
            for i in 0..1024u32 {
                let u = (exp << 23) | (i << 13);
                let out = rsqrt_estimate(u);
                assert_eq!(out & 0x7ff, 0);
                // Verify the exact defining inequality:
                // b*(2s - 1)^2 <= 2^39 < b*(2s + 1)^2 (guards against off-by-1).
                let s = 4096u64 + u64::from((out >> 11) & 0xfff);
                let b = (2048u64 + 2 * u64::from(i) + 1) << ((exp & 1) ^ 1);
                assert!(b * (2 * s - 1) * (2 * s - 1) <= (1u64 << 39));
                assert!((1u64 << 39) < b * (2 * s + 1) * (2 * s + 1));
                let x = f32::from_bits(u) as f64;
                let exact = 1.0 / x.sqrt();
                let got = f32::from_bits(out) as f64;
                assert!((got - exact).abs() / exact <= 1.5 / 4096.0);
                // Result depends only on sign, exponent, and top 10 fraction bits.
                assert_eq!(rsqrt_estimate(u | 0x1fff), out);
                h_rsq = (h_rsq ^ u64::from(out)).wrapping_mul(1099511628211);
            }
        }
        assert_eq!(h_rsq, 0x9f44_85fd_687f_0b83);
    }
}
