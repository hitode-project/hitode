//! Full-frame pixel workloads expressed as authored x86 SSE2 and MMX instructions.
use adv_jit::{CompileStrategy, CpuState, EXEC, Machine, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::hint::black_box;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x1f000;
const WEIGHTS: u32 = 0x20000;
const A: u32 = 0x100000;
const B: u32 = 0x1100000;
const OUT: u32 = 0x2100000;
const STACK: u32 = 0x2f00000;
const ALPHA: u16 = 93;

fn code(alpha: bool) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    if alpha {
        a.pxor(xmm7, xmm7).unwrap();
        a.movdqu(xmm5, xmmword_ptr(WEIGHTS as i32)).unwrap();
        a.movdqu(xmm6, xmmword_ptr(WEIGHTS as i32 + 16)).unwrap();
    }
    a.set_label(&mut top).unwrap();
    a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
    a.movdqu(xmm1, xmmword_ptr(edx)).unwrap();
    if alpha {
        a.movdqa(xmm2, xmm0).unwrap();
        a.movdqa(xmm3, xmm1).unwrap();
        a.punpcklbw(xmm0, xmm7).unwrap();
        a.punpckhbw(xmm2, xmm7).unwrap();
        a.punpcklbw(xmm1, xmm7).unwrap();
        a.punpckhbw(xmm3, xmm7).unwrap();
        a.pmullw(xmm0, xmm5).unwrap();
        a.pmullw(xmm2, xmm5).unwrap();
        a.pmullw(xmm1, xmm6).unwrap();
        a.pmullw(xmm3, xmm6).unwrap();
        a.paddw(xmm0, xmm1).unwrap();
        a.paddw(xmm2, xmm3).unwrap();
        a.psrlw(xmm0, 8).unwrap();
        a.psrlw(xmm2, 8).unwrap();
        a.packuswb(xmm0, xmm2).unwrap();
    } else {
        a.pavgb(xmm0, xmm1).unwrap();
    }
    a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
    a.add(esi, 16).unwrap();
    a.add(edx, 16).unwrap();
    a.add(edi, 16).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn mmx_code(alpha: bool) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    if alpha {
        a.pxor(mm7, mm7).unwrap();
        a.movq(mm5, qword_ptr(WEIGHTS as i32)).unwrap();
        a.movq(mm6, qword_ptr(WEIGHTS as i32 + 16)).unwrap();
    }
    a.set_label(&mut top).unwrap();
    a.movq(mm0, qword_ptr(esi)).unwrap();
    a.movq(mm1, qword_ptr(edx)).unwrap();
    if alpha {
        a.movq(mm2, mm0).unwrap();
        a.movq(mm3, mm1).unwrap();
        a.punpcklbw(mm0, mm7).unwrap();
        a.punpckhbw(mm2, mm7).unwrap();
        a.punpcklbw(mm1, mm7).unwrap();
        a.punpckhbw(mm3, mm7).unwrap();
        a.pmullw(mm0, mm5).unwrap();
        a.pmullw(mm2, mm5).unwrap();
        a.pmullw(mm1, mm6).unwrap();
        a.pmullw(mm3, mm6).unwrap();
        a.paddw(mm0, mm1).unwrap();
        a.paddw(mm2, mm3).unwrap();
        a.psrlw(mm0, 8).unwrap();
        a.psrlw(mm2, 8).unwrap();
        a.packuswb(mm0, mm2).unwrap();
    } else {
        a.pavgb(mm0, mm1).unwrap();
    }
    a.movq(qword_ptr(edi), mm0).unwrap();
    a.add(esi, 8).unwrap();
    a.add(edx, 8).unwrap();
    a.add(edi, 8).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.emms().unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn setup(code: &[u8], a: &[u8], b: &[u8], jit: bool, strategy: CompileStrategy) -> Machine {
    let mut m = Machine::new(CODE, 0x3000000).unwrap();
    m.protect(CODE, 0x3000000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.write(A, a).unwrap();
    m.write(B, b).unwrap();
    let weights: Vec<_> = [ALPHA, 256 - ALPHA]
        .into_iter()
        .flat_map(|v| [v; 8].into_iter().flat_map(u16::to_le_bytes))
        .collect();
    m.write(WEIGHTS, &weights).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        let mut region = Region::new(CODE, code.len());
        region.compile_strategy = strategy;
        m.set_region(region).unwrap();
    }
    m
}

fn call(m: &mut Machine, bytes: usize, jit: bool, mmx: bool) {
    m.state = CpuState {
        regs: [
            0,
            bytes as u32 / if mmx { 8 } else { 16 },
            B,
            0,
            STACK,
            0,
            A,
            OUT,
        ],
        eip: CODE,
        ..CpuState::default()
    };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    let result = if jit {
        m.run(STOP, bytes as u64)
    } else {
        m.run_unicorn_unbounded(STOP)
    }
    .unwrap();
    assert_eq!(result, RunOutcome::ReachedStop);
}

#[inline(never)]
fn host(a: &[u8], b: &[u8], out: &mut [u8], alpha: bool) {
    if alpha {
        for ((out, &a), &b) in out.iter_mut().zip(a).zip(b) {
            *out = ((u16::from(a) * ALPHA + u16::from(b) * (256 - ALPHA)) >> 8) as u8;
        }
    } else {
        for ((out, &a), &b) in out.iter_mut().zip(a).zip(b) {
            *out = ((u16::from(a) + u16::from(b) + 1) >> 1) as u8;
        }
    }
}

#[cfg(windows)]
fn pin() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(p: *mut c_void, mask: usize) -> i32;
    }
    assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
}
#[cfg(not(windows))]
fn pin() {}

fn main() {
    let samples: usize = std::env::args().nth(1).map_or(7, |v| v.parse().unwrap());
    assert!(samples > 0);
    let strategy = match std::env::args().nth(2).as_deref() {
        None | Some("speed") => CompileStrategy::Speed,
        Some("fast") => CompileStrategy::Fast,
        _ => panic!("pixel_bench [samples] [speed|fast]"),
    };
    pin();
    println!(
        "kernel,resolution,jit_ms,unicorn_ms,host_rust_ms,speedup,jit_over_host,compile_ms,fallbacks"
    );
    for (width, height) in [(1280, 720), (1920, 1080)] {
        let bytes = width * height * 4;
        let mut seed = 0x13579bdfu32;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        };
        let a: Vec<_> = (0..bytes).map(|_| random()).collect();
        let b: Vec<_> = (0..bytes).map(|_| random()).collect();
        for (mmx, alpha) in [(false, false), (false, true), (true, false), (true, true)] {
            let code = if mmx { mmx_code(alpha) } else { code(alpha) };
            let mut jit = setup(&code, &a, &b, true, strategy);
            let mut uc = setup(&code, &a, &b, false, strategy);
            let mut expected = vec![0; bytes];
            host(&a, &b, &mut expected, alpha);
            call(&mut uc, bytes, false, mmx);
            call(&mut jit, bytes, true, mmx);
            assert_eq!(jit.memory().read(OUT, bytes).unwrap(), expected);
            assert_eq!(uc.memory().read(OUT, bytes).unwrap(), expected);
            let mut host_out = vec![0; bytes];
            let mut timings = [Vec::new(), Vec::new(), Vec::new()];
            for sample in 0..samples {
                for index in if sample % 2 == 0 {
                    [0, 1, 2]
                } else {
                    [2, 1, 0]
                } {
                    let start = Instant::now();
                    match index {
                        0 => call(&mut jit, bytes, true, mmx),
                        1 => call(&mut uc, bytes, false, mmx),
                        _ => host(
                            black_box(&a),
                            black_box(&b),
                            black_box(&mut host_out),
                            alpha,
                        ),
                    }
                    timings[index].push(start.elapsed().as_secs_f64() * 1000.0);
                    let output = match index {
                        0 => jit.memory().read(OUT, bytes).unwrap(),
                        1 => uc.memory().read(OUT, bytes).unwrap(),
                        _ => &host_out,
                    };
                    assert_eq!(output, expected, "full-frame output mismatch");
                }
            }
            for t in &mut timings {
                t.sort_by(f64::total_cmp);
            }
            let (j, u, h) = (
                timings[0][samples / 2],
                timings[1][samples / 2],
                timings[2][samples / 2],
            );
            println!(
                "{}_{},{width}x{height},{j:.4},{u:.4},{h:.4},{:.2},{:.2},{:.3},{}",
                if mmx { "mmx" } else { "sse2" },
                if alpha {
                    "alpha_93_over_256"
                } else {
                    "average_50_percent"
                },
                u / j,
                j / h,
                jit.stats.compile_time.as_secs_f64() * 1000.0,
                jit.stats.fallback_entries
            );
        }
    }
}
