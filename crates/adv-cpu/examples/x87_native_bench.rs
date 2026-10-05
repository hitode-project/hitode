//! Diagnostic A/B benchmark; this is not a Majiro frame-time acceptance test.
//! Run the same binary twice, with ADV_JIT_X87_HELPER_ONLY=1 and with it unset.
//! Each workload is checked against the transactional interpreter before timing.
use adv_cpu::{CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::{hint::black_box, time::Instant};

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: i32 = 0x20000;
const STACK: u32 = 0x2f800;
const ITERATIONS: u32 = 20_000;

fn program(kind: usize) -> (Vec<u8>, u32) {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.mov(ecx, ITERATIONS).unwrap();
    a.set_label(&mut top).unwrap();
    let operations = match kind {
        0 => {
            a.fld(qword_ptr(DATA)).unwrap();
            a.fstp(qword_ptr(DATA + 24)).unwrap();
            2
        }
        1 => {
            a.fld(qword_ptr(DATA)).unwrap();
            a.fadd(qword_ptr(DATA + 8)).unwrap();
            a.fsub(qword_ptr(DATA + 8)).unwrap();
            a.fstp(qword_ptr(DATA + 24)).unwrap();
            4
        }
        2 => {
            a.fld(qword_ptr(DATA)).unwrap();
            a.fmul(qword_ptr(DATA + 8)).unwrap();
            a.fdiv(qword_ptr(DATA + 8)).unwrap();
            a.fstp(qword_ptr(DATA + 24)).unwrap();
            4
        }
        3 => {
            a.fld(qword_ptr(DATA)).unwrap();
            a.fld(qword_ptr(DATA + 8)).unwrap();
            a.fxch(st0, st1).unwrap();
            a.fucom(st0, st1).unwrap();
            a.fstp(st0).unwrap();
            a.fstp(qword_ptr(DATA + 24)).unwrap();
            6
        }
        4 => {
            a.fld(dword_ptr(DATA + 32)).unwrap();
            a.fmul(dword_ptr(DATA + 36)).unwrap();
            a.fistp(dword_ptr(DATA + 40)).unwrap();
            3
        }
        _ => {
            a.fld(qword_ptr(DATA)).unwrap();
            a.fsub(qword_ptr(DATA + 8)).unwrap();
            a.fmul(qword_ptr(DATA + 16)).unwrap();
            a.fadd(qword_ptr(DATA + 8)).unwrap();
            a.fstp(qword_ptr(DATA + 24)).unwrap();
            5
        }
    };
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    (a.assemble(u64::from(CODE)).unwrap(), operations)
}

fn machine(code: &[u8], jit: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    for (offset, value) in [(0, 1.3f64), (8, 0.7), (16, 0.125)] {
        m.write(DATA as u32 + offset, &value.to_le_bytes()).unwrap();
    }
    m.write(DATA as u32 + 32, &0.00314f32.to_le_bytes())
        .unwrap();
    m.write(DATA as u32 + 36, &32768.0f32.to_le_bytes())
        .unwrap();
    m.enable_auto_jit(false);
    if jit {
        m.add_region(Region::new(CODE, code.len())).unwrap();
    }
    m
}
fn run(m: &mut Executor) {
    m.state = CpuState {
        eip: CODE,
        ..CpuState::default()
    };
    m.state.regs[4] = STACK;
    assert_eq!(m.run(STOP, 10_000_000).unwrap(), RunOutcome::ReachedStop);
    black_box(&m.state);
}
fn check(m: &Executor, reference: &Executor) {
    assert_eq!(m.state.x87, reference.state.x87);
    assert_eq!(m.state.mmx, reference.state.mmx);
    assert_eq!(m.state.regs, reference.state.regs);
    assert_eq!(m.state.eflags, reference.state.eflags);
    assert_eq!(m.state.eip, reference.state.eip);
    assert_eq!(
        m.memory().read(DATA as u32, 48).unwrap(),
        reference.memory().read(DATA as u32, 48).unwrap()
    );
    assert_eq!(m.stats.interpreter_instructions, 0);
}
fn main() {
    let samples = std::env::args()
        .nth(1)
        .map_or(31, |s| s.parse::<usize>().expect("positive sample count"));
    assert!(samples > 0);
    let mode = if std::env::var_os("ADV_JIT_X87_HELPER_ONLY").is_some_and(|v| v == "1") {
        "helper"
    } else {
        "native"
    };
    println!(
        "mode,workload,samples,iterations,x87_ops,median_ns_per_op,p95_ns_per_op,max_ns_per_op,compile_ms,native_bytes,checksum"
    );
    for (kind, name) in [
        "load_store",
        "add_sub",
        "mul_div",
        "compare_exchange",
        "pcm",
        "interpolation",
    ]
    .iter()
    .enumerate()
    {
        let (code, operations) = program(kind);
        let mut reference = machine(&code, false);
        run(&mut reference);
        let mut m = machine(&code, true);
        for _ in 0..5 {
            run(&mut m);
            check(&m, &reference);
        }
        let mut timings = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            run(&mut m);
            timings.push(start.elapsed().as_secs_f64() * 1e9 / f64::from(ITERATIONS * operations));
            check(&m, &reference);
        }
        timings.sort_by(f64::total_cmp);
        let checksum = m
            .memory()
            .read(DATA as u32, 48)
            .unwrap()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
            });
        println!(
            "{mode},{name},{samples},{ITERATIONS},{operations},{:.3},{:.3},{:.3},{:.3},{},{checksum:016x}",
            timings[samples / 2],
            timings[(samples * 95).div_ceil(100) - 1],
            timings[samples - 1],
            m.stats.compile_time.as_secs_f64() * 1000.0,
            m.stats.native_code_bytes
        );
    }
}
