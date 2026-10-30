//! Pinned Wine zlib ABIs, shared by its embedded copies.
use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};
use adv_machine::Conv;

pub(super) fn bodies() -> Vec<super::Body> {
    let mut bodies: Vec<_> = [
        "cabinet.dll",
        "dbghelp.dll",
        "opcservices.dll",
        "user32.dll",
        "wininet.dll",
        "windowscodecs.dll",
    ]
    .into_iter()
    .map(|module| super::Body {
        name: format!("{module}!adler32_z").into(),
        conv: Conv::Cdecl { argc: 3 },
        body: None,
        handler: super::resume_original,
        direct: Some(ADLER32),
    })
    .collect();
    bodies.extend(
        [
            "dbghelp.dll",
            "user32.dll",
            "wininet.dll",
            "windowscodecs.dll",
        ]
        .map(|module| super::Body {
            name: format!("{module}!inflate_fast").into(),
            conv: Conv::Cdecl { argc: 2 },
            body: None,
            handler: super::resume_original,
            direct: Some(adv_cpu::body::Body {
                bind: Some(inputs),
                ..INFLATE_FAST
            }),
        }),
    );
    bodies
}

// spec: NAT-BODY-10
pub(super) fn adler32<M: Mem, const SIMD: bool>(
    mem: &mut M,
    [seed, input, len, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    const BASE: u32 = 65521;
    // zlib checks the one-byte case before its null-buffer initializer.
    if input == 0 && len != 1 {
        return Ok(1u32.into());
    }
    let bytes = if len == 0 {
        &[][..]
    } else {
        let pointer = mem.input(input, len)?;
        // SAFETY: the complete input was checked.
        unsafe { std::slice::from_raw_parts(pointer, len as usize) }
    };
    let (mut a, mut b) = (seed & 0xffff, seed >> 16);
    if len == 1 {
        a += u32::from(bytes[0]);
        if a >= BASE {
            a -= BASE;
        }
        b += a;
        if b >= BASE {
            b -= BASE;
        }
    } else if len < 16 {
        for &byte in bytes {
            a += u32::from(byte);
            b += a;
        }
        if a >= BASE {
            a -= BASE;
        }
        b %= BASE;
    } else {
        // 5552 is zlib's bound before the 32-bit accumulators can overflow.
        // Independent sums within each group expose byte-lane parallelism.
        for block in bytes.chunks(5552) {
            let (groups, tail) = block.as_chunks::<16>();
            for group in groups {
                let (mut sum, mut weighted) = (0, 0);
                #[cfg(target_arch = "aarch64")]
                if SIMD {
                    use std::arch::aarch64::*;
                    // SAFETY: every group contains sixteen checked bytes;
                    // NEON is mandatory on the AArch64 product targets.
                    unsafe {
                        let bytes = vld1q_u8(group.as_ptr());
                        let weights = vld1q_u8(
                            [16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1].as_ptr(),
                        );
                        sum = u32::from(vaddlvq_u8(bytes));
                        weighted = vaddlvq_u16(vmull_u8(vget_low_u8(bytes), vget_low_u8(weights)))
                            + vaddlvq_u16(vmull_high_u8(bytes, weights));
                    }
                } else {
                    for (i, &byte) in group.iter().enumerate() {
                        sum += u32::from(byte);
                        weighted += u32::from(byte) * (16 - i as u32);
                    }
                }
                #[cfg(not(target_arch = "aarch64"))]
                for (i, &byte) in group.iter().enumerate() {
                    sum += u32::from(byte);
                    weighted += u32::from(byte) * (16 - i as u32);
                }
                b += 16 * a + weighted;
                a += sum;
            }
            for &byte in tail {
                a += u32::from(byte);
                b += a;
            }
            a %= BASE;
            b %= BASE;
        }
    }
    Ok((a | (b << 16)).into())
}

const ARGS: &[Input] = &[Input::Stack(0), Input::Stack(1), Input::Stack(2)];
adv_cpu::body!(ADLER32 = adler32::<_, true>, inputs: ARGS, outputs: EAX, pop: 0);
adv_cpu::body!(INFLATE_FAST = inflate_fast, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 0);

// Error pointers belong to this module, including after relocation. Resolve
// them once when binding, then pass them as fixed direct-body inputs.
fn inputs(
    cpu: &dyn adv_cpu::backend::Guest,
    entry: u32,
    code: &[u8],
) -> Result<[Option<Input>; 7], String> {
    let mut messages = [0; 3];
    let texts = [
        b"invalid literal/length code\0".as_slice(),
        b"invalid distance code\0",
        b"invalid distance too far back\0",
    ];
    for instruction in
        iced_x86::Decoder::with_ip(32, code, u64::from(entry), iced_x86::DecoderOptions::NONE)
    {
        for operand in 0..instruction.op_count() {
            if instruction.op_kind(operand) == iced_x86::OpKind::Immediate32 {
                let pointer = instruction.immediate32();
                for (i, text) in texts.iter().enumerate() {
                    let mut bytes = [0; 32];
                    if cpu.read(pointer, &mut bytes[..text.len()]).is_ok()
                        && &bytes[..text.len()] == *text
                    {
                        messages[i] = pointer;
                    }
                }
            }
        }
    }
    if messages.contains(&0) {
        return Err("missing zlib error message operand".into());
    }
    Ok([
        Some(Input::Stack(0)),
        Some(Input::Stack(1)),
        Some(Input::Const(messages[0])),
        Some(Input::Const(messages[1])),
        Some(Input::Const(messages[2])),
        None,
        None,
    ])
}

// SAFETY: every helper accesses a byte or dword in a checked guest span.
unsafe fn word(bytes: *const u8, offset: usize) -> u32 {
    u32::from_le(unsafe { bytes.add(offset).cast::<u32>().read_unaligned() })
}
unsafe fn set(bytes: *mut u8, offset: usize, value: u32) {
    unsafe {
        bytes
            .add(offset)
            .cast::<u32>()
            .write_unaligned(value.to_le())
    };
}

// spec: NAT-BODY-11
fn inflate_fast<M: Mem>(
    mem: &mut M,
    [stream, start, bad_length, bad_distance, too_far, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let header = mem.input(stream, 32)?;
    // SAFETY: the header contains each field.
    let (input, avail_in, output, avail_out, state) = unsafe {
        (
            word(header, 0),
            word(header, 4),
            word(header, 12),
            word(header, 16),
            word(header, 28),
        )
    };
    let fields = mem.input(state, 96)?;
    // Wine i386 z_stream and inflate_state layouts match the pinned DWARF.
    // SAFETY: all these fields belong to the checked state prefix.
    let (wsize, whave, wnext, window, mut hold, mut bits, lcode, dcode, lbits, dbits) = unsafe {
        (
            word(fields, 44),
            word(fields, 48),
            word(fields, 52),
            word(fields, 56),
            word(fields, 60),
            word(fields, 64),
            word(fields, 80),
            word(fields, 84),
            word(fields, 88),
            word(fields, 92),
        )
    };
    let prefix_len = start.wrapping_sub(avail_out);
    let [
        source,
        literals,
        distances,
        history,
        prefix,
        dest,
        header,
        fields,
        sane,
    ] = mem.spans([
        (input, avail_in, false),
        (lcode, 852 * 4, false),
        (dcode, 592 * 4, false),
        (window, wsize, false),
        (output.wrapping_sub(prefix_len), prefix_len, false),
        (output, avail_out, true),
        (stream, 32, true),
        (state, 68, true),
        (state.wrapping_add(7108), 4, false),
    ])?;
    let (mut read, mut written) = (0u32, 0u32);
    let (mut mode, mut message) = (None, 0);
    let lmask = (1u32 << lbits) - 1;
    let dmask = (1u32 << dbits) - 1;
    // Raw pointers deliberately retain live aliasing: output may overwrite
    // later input, table entries or sliding-window bytes, exactly as zlib does.
    // SAFETY: each accepted stream supplies the complete spans checked above;
    // at least six input and 258 output bytes remain at every iteration.
    unsafe {
        'decode: loop {
            if bits < 15 {
                hold = hold.wrapping_add(u32::from(source.add(read as usize).read()) << bits);
                read += 1;
                bits += 8;
                hold = hold.wrapping_add(u32::from(source.add(read as usize).read()) << bits);
                read += 1;
                bits += 8;
            }
            let mut here = literals.add((hold & lmask) as usize * 4);
            let mut op;
            let mut len;
            loop {
                let used = u32::from(here.add(1).read());
                hold >>= used;
                bits -= used;
                op = u32::from(here.read());
                len = u32::from(u16::from_le(here.add(2).cast::<u16>().read_unaligned()));
                if op == 0 {
                    dest.add(written as usize).write(len as u8);
                    written += 1;
                    break;
                } else if op & 16 != 0 {
                    op &= 15;
                    if bits < op {
                        hold =
                            hold.wrapping_add(u32::from(source.add(read as usize).read()) << bits);
                        read += 1;
                        bits += 8;
                    }
                    len += hold & ((1 << op) - 1);
                    hold >>= op;
                    bits -= op;
                    if bits < 15 {
                        hold =
                            hold.wrapping_add(u32::from(source.add(read as usize).read()) << bits);
                        read += 1;
                        bits += 8;
                        hold =
                            hold.wrapping_add(u32::from(source.add(read as usize).read()) << bits);
                        read += 1;
                        bits += 8;
                    }
                    here = distances.add((hold & dmask) as usize * 4);
                    let mut dist;
                    loop {
                        let used = u32::from(here.add(1).read());
                        hold >>= used;
                        bits -= used;
                        op = u32::from(here.read());
                        dist = u32::from(u16::from_le(here.add(2).cast::<u16>().read_unaligned()));
                        if op & 16 != 0 {
                            op &= 15;
                            while bits < op {
                                hold = hold.wrapping_add(
                                    u32::from(source.add(read as usize).read()) << bits,
                                );
                                read += 1;
                                bits += 8;
                            }
                            dist += hold & ((1 << op) - 1);
                            hold >>= op;
                            bits -= op;
                            break;
                        } else if op & 64 == 0 {
                            here = distances.add((dist + (hold & ((1 << op) - 1))) as usize * 4);
                        } else {
                            mode = Some(16209);
                            message = bad_distance;
                            break 'decode;
                        }
                    }
                    let available = prefix_len + written;
                    let mut back = dist.saturating_sub(available);
                    if back > whave && word(sane, 0) != 0 {
                        mode = Some(16209);
                        message = too_far;
                        break 'decode;
                    }
                    while len != 0 {
                        let byte = if back != 0 {
                            let at = if wnext >= back {
                                wnext - back
                            } else {
                                wsize.wrapping_add(wnext).wrapping_sub(back)
                            };
                            back -= 1;
                            history.add(at as usize).read()
                        } else if dist > written {
                            prefix.add((prefix_len + written - dist) as usize).read()
                        } else {
                            dest.add((written - dist) as usize).read()
                        };
                        dest.add(written as usize).write(byte);
                        written += 1;
                        len -= 1;
                    }
                    break;
                } else if op & 64 == 0 {
                    here = literals.add((len + (hold & ((1 << op) - 1))) as usize * 4);
                } else {
                    if op & 32 != 0 {
                        mode = Some(16191);
                    } else {
                        mode = Some(16209);
                        message = bad_length;
                    }
                    break 'decode;
                }
            }
            if read >= avail_in.wrapping_sub(5) || written >= avail_out.wrapping_sub(257) {
                break;
            }
        }
        read -= bits >> 3;
        bits &= 7;
        hold &= (1 << bits) - 1;
        set(header, 0, input.wrapping_add(read));
        set(header, 4, avail_in.wrapping_sub(read));
        set(header, 12, output.wrapping_add(written));
        set(header, 16, avail_out.wrapping_sub(written));
        if let Some(mode) = mode {
            set(fields, 4, mode);
        }
        if message != 0 {
            set(header, 24, message);
        }
        set(fields, 60, hold);
        set(fields, 64, bits);
    }
    mem.commit()?;
    Ok(0u32.into())
}
