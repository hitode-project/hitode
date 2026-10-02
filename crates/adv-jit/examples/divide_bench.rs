//! Authored full-frame normalization stress: seven integer divisions per pixel.
#[path = "../tests/support/mod.rs"]
mod oracle_support;
use oracle_support::Machine;
use std::time::Instant;

use adv_jit::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const PIXELS: u32 = 0x100000;
const COUNT: usize = 800 * 600;

fn main() {
    let frames = std::env::args()
        .nth(1)
        .map_or(5, |s| s.parse::<usize>().expect("frames"));
    assert!(frames > 0);
    let mut a = CodeAssembler::new(32).unwrap();
    let mut pass = a.create_label();
    let mut pixel = a.create_label();
    a.mov(ebx, 7).unwrap();
    a.set_label(&mut pass).unwrap();
    a.mov(esi, PIXELS).unwrap();
    a.mov(ebp, COUNT as u32).unwrap();
    a.set_label(&mut pixel).unwrap();
    a.mov(eax, dword_ptr(esi)).unwrap();
    a.imul_3(eax, eax, 251).unwrap();
    a.add(eax, 12345).unwrap();
    a.mov(ecx, eax).unwrap();
    a.shr(ecx, 24).unwrap();
    a.inc(ecx).unwrap();
    a.xor(edx, edx).unwrap();
    a.div(ecx).unwrap();
    a.xor(eax, edx).unwrap();
    a.mov(dword_ptr(esi), eax).unwrap();
    a.add(esi, 4).unwrap();
    a.dec(ebp).unwrap();
    a.jnz(pixel).unwrap();
    a.dec(ebx).unwrap();
    a.jnz(pass).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let source: Vec<_> = (0..COUNT)
        .map(|i| (i as u32).wrapping_mul(0x1234_5679))
        .collect();
    let expected: Vec<_> = source
        .iter()
        .map(|&value| {
            (0..7).fold(value, |value, _| {
                let dividend = value.wrapping_mul(251).wrapping_add(12345);
                let divisor = (dividend >> 24) + 1;
                (dividend / divisor) ^ (dividend % divisor)
            })
        })
        .flat_map(u32::to_le_bytes)
        .collect();
    let source: Vec<_> = source.into_iter().flat_map(u32::to_le_bytes).collect();
    let stack_page = PIXELS + (COUNT as u32 * 4).next_multiple_of(4096);
    let stack = stack_page + 0x800;
    let end = stack_page + 4096;
    for jit in [false, true] {
        let mut m = Machine::new(CODE, (end - CODE) as usize).unwrap();
        m.protect(CODE, (end - CODE) as usize, READ | WRITE)
            .unwrap();
        m.write(CODE, &code).unwrap();
        m.protect(CODE, 4096, READ | EXEC).unwrap();
        m.protect(STOP, 4096, READ | EXEC).unwrap();
        if jit {
            m.add_region(Region::new(CODE, code.len())).unwrap();
        }
        let mut times = Vec::new();
        for frame in 0..=frames {
            m.write(PIXELS, &source).unwrap();
            m.write(stack, &STOP.to_le_bytes()).unwrap();
            m.state.eip = CODE;
            m.state.regs[4] = stack;
            let start = Instant::now();
            let result = if jit {
                m.run(STOP, 20_000_000)
            } else {
                m.run_unicorn_unbounded(STOP)
            };
            let elapsed = start.elapsed().as_secs_f64();
            assert_eq!(result.unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.memory().read(PIXELS, COUNT * 4).unwrap(), expected);
            if frame != 0 {
                times.push(elapsed);
            }
        }
        let mean = times.iter().sum::<f64>() / frames as f64;
        times.sort_by(f64::total_cmp);
        let p95 = times[(frames * 95).div_ceil(100) - 1];
        let hash = expected
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
            });
        if jit {
            assert_eq!(m.stats.interpreter_entries, 0);
        }
        println!(
            "{} 800x600 divisions/frame={} mean_ms={:.3} p95_ms={:.3} fps={:.2} fnv={hash:016x} interpreter_entries={}",
            if jit { "jit" } else { "unicorn" },
            COUNT * 7,
            mean * 1000.0,
            p95 * 1000.0,
            1.0 / mean,
            m.stats.interpreter_entries
        );
    }
}
