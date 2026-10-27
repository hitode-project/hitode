//! Pinned Wine zlib ABIs, shared by its embedded copies.
use adv_cpu::backend::Prot;
use adv_machine::{Conv, Env};

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
        body: adler32,
        handler: super::resume_original,
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
            body: inflate_fast,
            handler: super::resume_original,
        }),
    );
    bodies
}

// spec: NAT-BODY-10
fn adler32(env: &mut Env<'_>) -> Option<u64> {
    const BASE: u32 = 65521;
    let (seed, input, len) = (env.arg(0), env.arg(1), env.arg(2));
    // zlib checks the one-byte case before its null-buffer initializer.
    if input == 0 && len != 1 {
        return Some(1);
    }
    let bytes = if len == 0 {
        &[][..]
    } else {
        if !env.cpu.accessible(input, len, Prot::READ) {
            return None;
        }
        env.cpu.shared_bytes(input, len as usize)?
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
    Some(u64::from(a | (b << 16)))
}

/// Stateless fast-batch math, supplied by the device assembly. Histories are
/// chronological, tables pair code bytes with root widths, reservoir is (hold,
/// bits); success returns (input used, output used, end of block).
pub type InflateKernel = fn(
    &[u8],
    &mut [u8],
    [&[u8]; 3],
    [(&[u8], u32); 2],
    &mut (u32, u32),
) -> Option<(usize, usize, bool)>;

/// One reusable output buffer for all streams on this execution owner.
pub struct InflateFast {
    kernel: InflateKernel,
    output: Vec<u8>,
}
impl InflateFast {
    pub fn new(kernel: InflateKernel) -> Self {
        Self {
            kernel,
            output: Vec::new(),
        }
    }
}
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn overlap(a: (u32, u32), b: (u32, u32)) -> bool {
    a.1 != 0
        && b.1 != 0
        && u64::from(a.0) < u64::from(b.0) + u64::from(b.1)
        && u64::from(b.0) < u64::from(a.0) + u64::from(a.1)
}

// spec: NAT-BODY-11
fn inflate_fast(env: &mut Env<'_>) -> Option<u64> {
    let kernel = env.ext.try_get::<InflateFast>()?.kernel;
    let (stream, start) = (env.arg(0), env.arg(1));
    let mut header = [0; 32];
    env.cpu.read(stream, &mut header).ok()?;
    let (input, avail_in, output, avail_out, state) = (
        word(&header, 0),
        word(&header, 4),
        word(&header, 12),
        word(&header, 16),
        word(&header, 28),
    );
    // Wine i386 z_stream and inflate_state layouts are confirmed by DWARF.
    let mut fields = [0; 96];
    env.cpu.read(state, &mut fields).ok()?;
    let (mode, wsize, whave, wnext, window) = (
        word(&fields, 4),
        word(&fields, 44),
        word(&fields, 48),
        word(&fields, 52),
        word(&fields, 56),
    );
    let mut reservoir = (word(&fields, 60), word(&fields, 64));
    let (lcode, dcode, lbits, dbits) = (
        word(&fields, 80),
        word(&fields, 84),
        word(&fields, 88),
        word(&fields, 92),
    );
    if mode != 16200
        || avail_in < 6
        || avail_out < 258
        || reservoir.1 >= 8
        || start < avail_out
        || whave > wsize
        || (whave != 0 && wnext >= wsize)
        || !env.cpu.accessible(stream, 20, Prot::RW)
        || !env.cpu.accessible(state + 4, 4, Prot::WRITE)
        || !env.cpu.accessible(state + 60, 8, Prot::WRITE)
        || !env.cpu.accessible(output, avail_out, Prot::WRITE)
    {
        return None;
    }
    let prefix_len = (start - avail_out).min(32768);
    let prefix = output.checked_sub(prefix_len)?;
    let reads = [
        (input, avail_in),
        (state, 7120),
        (stream, 32),
        (lcode, 852 * 4),
        (dcode, 592 * 4),
        (window, if whave == 0 { 0 } else { wsize }),
        (prefix, prefix_len),
    ];
    for span in reads {
        if overlap((output, avail_out), span) {
            return None;
        }
    }
    // Borrow input, code tables and ring segments directly; only output needs
    // scratch so a rejected batch cannot expose partial guest writes.
    let bytes = |at, len| {
        if len == 0 {
            return Some(&[][..]);
        }
        if !env.cpu.accessible(at, len, Prot::READ) {
            return None;
        }
        env.cpu.shared_bytes(at, len as usize)
    };
    let input_bytes = bytes(input, avail_in)?;
    let ltable = bytes(lcode, 852 * 4)?;
    let dtable = bytes(dcode, 592 * 4)?;
    let window_bytes = bytes(window, if whave == 0 { 0 } else { wsize })?;
    let (first, second) = if whave == 0 {
        (&[][..], &[][..])
    } else {
        let at = if wnext >= whave {
            wnext - whave
        } else {
            wsize - (whave - wnext)
        } as usize;
        let count = (whave as usize).min(window_bytes.len() - at);
        (
            &window_bytes[at..at + count],
            &window_bytes[..whave as usize - count],
        )
    };
    let history = [first, second, bytes(prefix, prefix_len)?];
    let service = env.ext.get_mut::<InflateFast>();
    if service.output.len() < avail_out as usize {
        service.output.resize(avail_out as usize, 0);
    }
    let scratch = &mut service.output[..avail_out as usize];
    let (read, written, ended) = kernel(
        input_bytes,
        scratch,
        history,
        [(ltable, lbits), (dtable, dbits)],
        &mut reservoir,
    )?;
    env.cpu.write(output, &scratch[..written]).ok()?;
    header[..4].copy_from_slice(&input.wrapping_add(read as u32).to_le_bytes());
    header[4..8].copy_from_slice(&(avail_in - read as u32).to_le_bytes());
    header[12..16].copy_from_slice(&output.wrapping_add(written as u32).to_le_bytes());
    header[16..20].copy_from_slice(&(avail_out - written as u32).to_le_bytes());
    env.cpu.write(stream, &header[..8]).ok()?;
    env.cpu.write(stream + 12, &header[12..20]).ok()?;
    if ended {
        env.cpu.write(state + 4, &16191u32.to_le_bytes()).ok()?;
    }
    fields[60..64].copy_from_slice(&reservoir.0.to_le_bytes());
    fields[64..68].copy_from_slice(&reservoir.1.to_le_bytes());
    env.cpu.write(state + 60, &fields[60..68]).ok()?;
    Some(0)
}
