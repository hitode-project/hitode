//! Authored integer pixel loops, checked against scalar arithmetic.
//! Run on ARM64 with `cargo run -r -p adv-cpu-a64 --example pixel_bench`.
//! Inputs are deterministic 1280x720 images; output is median warm frame time.
use adv_cpu_a64::{CpuState, Executor, RunOutcome};
use iced_x86::code_asm::*;
use std::time::Instant;

const CODE: u32 = 0x10000;
const SRC: u32 = 0x100000;
const DST: u32 = 0x500000;
const PIXELS: usize = 1280 * 720;

fn code(kind: usize) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    if kind < 2 {
        a.pxor(mm7, mm7).unwrap();
        a.mov(eax, 93).unwrap();
        a.movd(mm6, eax).unwrap();
        a.pshufw(mm6, mm6, 0).unwrap();
    } else if kind == 2 {
        a.pxor(xmm7, xmm7).unwrap();
        a.mov(eax, 93 * 0x10001).unwrap();
        a.movd(xmm6, eax).unwrap();
        a.pshufd(xmm6, xmm6, 0).unwrap();
    }
    a.set_label(&mut top).unwrap();
    match kind {
        0 | 1 => {
            a.movd(mm0, dword_ptr(esi)).unwrap();
            a.movd(mm1, dword_ptr(edi)).unwrap();
            if kind == 1 {
                a.movd(eax, mm0).unwrap();
                a.shr(eax, 24).unwrap();
                a.movd(mm6, eax).unwrap();
                a.pshufw(mm6, mm6, 0).unwrap();
            }
            a.punpcklbw(mm0, mm7).unwrap();
            a.punpcklbw(mm1, mm7).unwrap();
            a.psubw(mm0, mm1).unwrap();
            a.pmullw(mm0, mm6).unwrap();
            a.psllw(mm1, 8).unwrap();
            a.paddw(mm0, mm1).unwrap();
            a.psrlw(mm0, 8).unwrap();
            a.packuswb(mm0, mm7).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
        }
        2 => {
            a.movq(xmm0, qword_ptr(esi)).unwrap();
            a.movq(xmm1, qword_ptr(edi)).unwrap();
            a.punpcklbw(xmm0, xmm7).unwrap();
            a.punpcklbw(xmm1, xmm7).unwrap();
            a.psubw(xmm0, xmm1).unwrap();
            a.pmullw(xmm0, xmm6).unwrap();
            a.psllw(xmm1, 8).unwrap();
            a.paddw(xmm0, xmm1).unwrap();
            a.psrlw(xmm0, 8).unwrap();
            a.packuswb(xmm0, xmm7).unwrap();
            a.movq(qword_ptr(edi), xmm0).unwrap();
            a.add(esi, 8).unwrap();
            a.add(edi, 8).unwrap();
        }
        3 => {
            a.mov(eax, ebx).unwrap();
            a.shr(eax, 16).unwrap();
            a.movd(mm0, dword_ptr(esi + eax * 4)).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            a.add(ebx, edx).unwrap();
            a.add(edi, 4).unwrap();
        }
        _ => {
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.pavgb(mm0, qword_ptr(esi + 4)).unwrap();
            a.movq(qword_ptr(edi), mm0).unwrap();
            a.add(esi, 8).unwrap();
            a.add(edi, 8).unwrap();
        }
    }
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    if kind != 2 {
        a.emms().unwrap();
    }
    a.assemble(u64::from(CODE)).unwrap()
}

fn main() {
    let src: Vec<u8> = (0..PIXELS * 4 + 4)
        .map(|n| (n * 73 + n / 11) as u8)
        .collect();
    let dst: Vec<u8> = (0..PIXELS * 4).map(|n| (n * 19 + n / 7) as u8).collect();
    for (kind, name) in [
        "mmx-constant",
        "mmx-alpha",
        "sse2-constant",
        "nearest",
        "linear-half",
    ]
    .into_iter()
    .enumerate()
    {
        let code = code(kind);
        let stop = CODE + code.len() as u32;
        let mut cpu = Executor::new(CODE, 0x900000).unwrap();
        cpu.protect(CODE, 0x900000, 3).unwrap();
        cpu.write(CODE, &code).unwrap();
        cpu.write(SRC, &src).unwrap();
        cpu.protect(CODE, 0x4000, 5).unwrap();
        let mut times = Vec::new();
        for frame in 0..22 {
            cpu.write(DST, &dst).unwrap();
            cpu.state = CpuState {
                eip: CODE,
                regs: [
                    0,
                    (PIXELS / if matches!(kind, 2 | 4) { 2 } else { 1 }) as u32,
                    0x8000,
                    0,
                    0,
                    0,
                    SRC,
                    DST,
                ],
                ..Default::default()
            };
            let start = Instant::now();
            assert_eq!(cpu.run(stop, 40_000_000).unwrap(), RunOutcome::ReachedStop);
            if frame > 0 {
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        let actual = cpu.memory().read(DST, dst.len()).unwrap();
        for (n, &value) in actual.iter().enumerate() {
            let expected = match kind {
                0..=2 => {
                    let alpha = if kind == 1 {
                        u32::from(src[n / 4 * 4 + 3])
                    } else {
                        93
                    };
                    ((u32::from(src[n]) * alpha + u32::from(dst[n]) * (256 - alpha)) >> 8) as u8
                }
                3 => src[(n / 8 % 65536) * 4 + n % 4],
                _ => (u16::from(src[n]) + u16::from(src[n + 4])).div_ceil(2) as u8,
            };
            assert_eq!(value, expected, "{name}: byte {n}");
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{name}: {:.3} ms/frame, {} code bytes, {} compilations",
            times[times.len() / 2],
            cpu.stats.native_code_bytes,
            cpu.stats.compilations
        );
    }
}
