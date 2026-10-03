//! Self-authored full-frame clear -> MMX blend -> REP copy -> checksum benchmark.
#[path = "../tests/support/mod.rs"]
mod oracle_support;
use oracle_support::Executor;
use std::time::Instant;

use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const SRC: u32 = 0x100000;

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, &value| {
        (hash ^ u64::from(value)).wrapping_mul(0x100000001b3)
    })
}

fn bench(width: u32, height: u32, frames: usize, jit: bool) {
    let size = width * height * 4;
    let aligned = (size + 4095) & !4095;
    let work = SRC + aligned;
    let output = work + aligned;
    let stack_page = output + aligned;
    let stack = stack_page + 0x800;
    let end = stack_page + 4096;
    let source: Vec<u8> = (0..size)
        .map(|i| (i.wrapping_mul(37) + i / 13) as u8)
        .collect();
    let expected: Vec<u8> = source
        .iter()
        .map(|&value| ((u16::from(value) + 0x20 + 1) >> 1) as u8)
        .collect();
    let checksum = expected
        .as_chunks::<4>()
        .0
        .iter()
        .fold(0u32, |sum, bytes| sum ^ u32::from_le_bytes(*bytes));
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, 0x2020_2020).unwrap();
    a.mov(edi, work).unwrap();
    a.mov(ecx, size / 4).unwrap();
    a.rep().stosd().unwrap();
    let mut blend = a.create_label();
    a.mov(esi, SRC).unwrap();
    a.mov(edi, work).unwrap();
    a.mov(ecx, size / 8).unwrap();
    a.set_label(&mut blend).unwrap();
    a.movq(mm0, qword_ptr(esi)).unwrap();
    a.pavgb(mm0, qword_ptr(edi)).unwrap();
    a.movq(qword_ptr(edi), mm0).unwrap();
    a.add(esi, 8).unwrap();
    a.add(edi, 8).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(blend).unwrap();
    a.emms().unwrap();
    a.mov(esi, work).unwrap();
    a.mov(edi, output).unwrap();
    a.mov(ecx, size / 4).unwrap();
    a.rep().movsd().unwrap();
    let mut check = a.create_label();
    a.xor(eax, eax).unwrap();
    a.mov(esi, output).unwrap();
    a.mov(ecx, size / 4).unwrap();
    a.set_label(&mut check).unwrap();
    a.xor(eax, dword_ptr(esi)).unwrap();
    a.add(esi, 4).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(check).unwrap();
    a.ret().unwrap();
    let bytes = a.assemble(u64::from(CODE)).unwrap();
    let mut m = Executor::new(CODE, (end - CODE) as usize).unwrap();
    m.protect(CODE, (end - CODE) as usize, READ | WRITE)
        .unwrap();
    m.write(CODE, &bytes).unwrap();
    m.write(SRC, &source).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    let run = |m: &mut Executor| {
        m.state.eip = CODE;
        m.state.regs[4] = stack;
        m.write(stack, &STOP.to_le_bytes()).unwrap();
        let outcome = if jit {
            m.run(STOP, 10_000_000)
        } else {
            m.run_unicorn_unbounded(STOP)
        };
        assert_eq!(outcome.unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], checksum);
    };
    run(&mut m);
    let actual = m.memory().read(output, size as usize).unwrap();
    assert!(
        actual == expected,
        "full frame mismatch at {:?}",
        actual.iter().zip(&expected).position(|(a, b)| a != b)
    );
    let expected_hash = fnv(&expected);
    let mut timings = Vec::with_capacity(frames);
    for _ in 0..frames {
        let start = Instant::now();
        run(&mut m);
        timings.push(start.elapsed().as_secs_f64());
    }
    assert_eq!(
        fnv(m.memory().read(output, size as usize).unwrap()),
        expected_hash
    );
    if jit {
        assert_eq!(m.stats.interpreter_entries, 0);
    }
    let mean = timings.iter().sum::<f64>() / frames as f64;
    timings.sort_by(f64::total_cmp);
    let p95 = timings[(frames * 95).div_ceil(100).saturating_sub(1)];
    println!(
        "{width}x{height} {} frames={frames} mean_ms={:.3} p95_ms={:.3} fps={:.1} checksum={checksum:08x} fnv={expected_hash:016x} bulk_bytes={} native_entries={} interpreter_entries={} compile_ms={:.3}",
        if jit { "jit" } else { "uc" },
        mean * 1000.0,
        p95 * 1000.0,
        1.0 / mean,
        m.stats.bulk_bytes,
        m.stats.native_entries,
        m.stats.interpreter_entries,
        m.stats.compile_time.as_secs_f64() * 1000.0
    );
}

fn main() {
    let frames = std::env::args()
        .nth(1)
        .map(|s| s.parse::<usize>().expect("frame count"))
        .unwrap_or(30);
    assert!(frames > 0);
    for (width, height) in [(800, 600), (1280, 720), (1920, 1080)] {
        for jit in [false, true] {
            bench(width, height, frames, jit);
        }
    }
}
