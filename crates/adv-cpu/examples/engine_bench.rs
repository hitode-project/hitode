//! Engine-neutral execution benchmark over the public `Backend` interface.
//!
//! Usage: `cargo run -p adv-cpu --release --example engine_bench [samples]`.
//! Pin the process externally (for example `taskset 80` on Android) so engines
//! are compared on the same core. Inputs: optional `ADV_ENGINE_BENCH_SCALE`
//! (percent of the default iteration counts, 1-1000),
//! `ADV_ENGINE_BENCH_ENGINES` (comma list of `interpreter` and the platform's
//! engine, `a64` on AArch64 hosts and `cranelift` elsewhere; default both)
//! and `ADV_ENGINE_BENCH_WORKLOADS` (comma list of workload names, default
//! all: alu, memory, call_shallow, call_deep, dispatch, x87_pc64, x87_pc53,
//! cold).
//! Output: CSV rows of first-run milliseconds on a fresh CPU and steady-state
//! nanoseconds per dynamic guest instruction. Workloads are self-authored; no
//! game code or addresses are involved. Every engine's first run must match
//! the interpreter's registers.
use adv_cpu::JitCpu;
use adv_cpu::backend::{Backend, Exit, Guest, Prot, Reg, TrapAction, TrapHandler};
use iced_x86::BlockEncoderOptions;
use iced_x86::code_asm::*;
use std::time::{Duration, Instant};

const CODE: u32 = 0x10000;
const CODE_SIZE: u32 = 0x80000;
const DATA: u32 = 0x100000;
const TABLE: u32 = DATA + 0x1000;
const BYTECODE: u32 = DATA + 0x2000;
const DATA_SIZE: u32 = 0x10000;
const STACK_BASE: u32 = 0x1f0000;
const STACK: u32 = 0x200000;
const PARK: u32 = 0x300000;
const VM_OPS: u32 = 255;
const COLD_BODY: u64 = 16;

struct Workload {
    name: &'static str,
    code: Vec<u8>,
    data: Vec<(u32, Vec<u8>)>,
    instructions: u64,
}

struct Park;
impl TrapHandler for Park {
    fn on_trap(&mut self, _: &mut dyn Guest, vector: u8) -> TrapAction {
        assert_eq!(vector, 0x2e);
        TrapAction::Park
    }
}

fn assemble(a: &mut CodeAssembler) -> (Vec<u8>, CodeAssemblerResult) {
    let result = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    (result.inner.code_buffer.clone(), result)
}

fn alu(n: u32) -> Workload {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.xor(eax, eax).unwrap();
    a.mov(ebx, 1).unwrap();
    a.xor(edx, edx).unwrap();
    a.xor(esi, esi).unwrap();
    a.xor(edi, edi).unwrap();
    a.mov(ecx, n).unwrap();
    a.set_label(&mut top).unwrap();
    a.add(eax, ebx).unwrap();
    a.xor(edx, eax).unwrap();
    a.shl(edx, 1).unwrap();
    a.adc(esi, edx).unwrap();
    a.sub(edi, esi).unwrap();
    a.ror(eax, 3).unwrap();
    a.inc(ebx).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.int(0x2e).unwrap();
    Workload {
        name: "alu",
        code: assemble(&mut a).0,
        data: Vec::new(),
        instructions: 7 + 9 * u64::from(n),
    }
}

/// An x87 loop in the style of compiled per-pixel colour math: loads from
/// tables, add/sub/mul/div, a compare through `fnstsw`/`test ah`, `fxch`,
/// stack copies and an integer store, under control word `cw` (0x133f is
/// precision 64, 0x027f precision 53, both round to nearest, all masked).
fn x87(name: &'static str, cw: u16, n: u32) -> Workload {
    const A: u32 = DATA;
    const B: u32 = DATA + 0x800;
    const HALF: u32 = DATA + 0x1000;
    const K: u32 = DATA + 0x1008;
    const SCALE: u32 = DATA + 0x1010;
    const CW: u32 = DATA + 0x1018;
    const OUT: u32 = DATA + 0x1020;
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut ordered = a.create_label();
    a.fldcw(word_ptr(CW)).unwrap();
    a.mov(ecx, n).unwrap();
    a.xor(edx, edx).unwrap();
    a.set_label(&mut top).unwrap();
    a.mov(esi, ecx).unwrap();
    a.and(esi, 255).unwrap();
    a.fld(qword_ptr(esi * 8 + A)).unwrap();
    a.fld(qword_ptr(esi * 8 + B)).unwrap();
    a.fcom_2(st0, st1).unwrap();
    a.fnstsw(ax).unwrap();
    a.test(ah, 0x41).unwrap();
    a.jne(ordered).unwrap();
    a.fxch(st0, st1).unwrap();
    a.set_label(&mut ordered).unwrap();
    a.fld(st1).unwrap();
    a.fadd_2(st0, st1).unwrap();
    a.fmul(qword_ptr(HALF)).unwrap();
    a.fld(st1).unwrap();
    a.fsub_2(st0, st3).unwrap();
    a.fdiv(qword_ptr(K)).unwrap();
    a.faddp(st1, st0).unwrap();
    a.fmul(qword_ptr(SCALE)).unwrap();
    a.fistp(dword_ptr(OUT)).unwrap();
    a.fstp(st0).unwrap();
    a.fstp(st0).unwrap();
    a.add(edx, dword_ptr(OUT)).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.int(0x2e).unwrap();
    // B > A everywhere, so the compare never branches and fxch always runs.
    let table = |f: &dyn Fn(f64) -> f64| -> Vec<u8> {
        (0..256)
            .flat_map(|i| f(f64::from(i)).to_le_bytes())
            .collect()
    };
    let mut constants = Vec::new();
    for value in [0.5f64, 3.0, 255.0] {
        constants.extend_from_slice(&value.to_le_bytes());
    }
    constants.extend_from_slice(&cw.to_le_bytes());
    Workload {
        name,
        code: assemble(&mut a).0,
        data: vec![
            (A, table(&|i| 0.37 * i + 1.0)),
            (B, table(&|i| 0.37 * i + 1.7 + i / 300.0)),
            (HALF, constants),
        ],
        instructions: 4 + 23 * u64::from(n),
    }
}

fn memory(n: u32) -> Workload {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.mov(ecx, n).unwrap();
    a.xor(edx, edx).unwrap();
    a.set_label(&mut top).unwrap();
    a.mov(esi, ecx).unwrap();
    a.and(esi, 1022).unwrap();
    a.mov(eax, dword_ptr(esi * 4 + DATA)).unwrap();
    a.add(eax, ecx).unwrap();
    a.mov(dword_ptr(esi * 4 + DATA), eax).unwrap();
    a.add(edx, dword_ptr(esi * 4 + DATA + 4)).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.int(0x2e).unwrap();
    Workload {
        name: "memory",
        code: assemble(&mut a).0,
        data: vec![(DATA, (0..4096u32).map(|i| (i * 37) as u8).collect())],
        instructions: 3 + 8 * u64::from(n),
    }
}

fn call_shallow(n: u32) -> Workload {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut leaf = a.create_label();
    a.mov(ecx, n).unwrap();
    a.xor(eax, eax).unwrap();
    a.set_label(&mut top).unwrap();
    a.call(leaf).unwrap();
    a.call(leaf).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.int(0x2e).unwrap();
    a.set_label(&mut leaf).unwrap();
    a.add(eax, ecx).unwrap();
    a.ret().unwrap();
    Workload {
        name: "call_shallow",
        code: assemble(&mut a).0,
        data: Vec::new(),
        instructions: 3 + 8 * u64::from(n),
    }
}

fn call_deep(n: u32) -> Workload {
    const DEPTH: u64 = 32;
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut rec = a.create_label();
    let mut done = a.create_label();
    a.mov(ecx, n).unwrap();
    a.xor(eax, eax).unwrap();
    a.set_label(&mut top).unwrap();
    a.mov(edx, DEPTH as u32).unwrap();
    a.call(rec).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.int(0x2e).unwrap();
    a.set_label(&mut rec).unwrap();
    a.test(edx, edx).unwrap();
    a.jz(done).unwrap();
    a.dec(edx).unwrap();
    a.call(rec).unwrap();
    a.inc(edx).unwrap();
    a.set_label(&mut done).unwrap();
    a.inc(eax).unwrap();
    a.ret().unwrap();
    Workload {
        name: "call_deep",
        code: assemble(&mut a).0,
        data: Vec::new(),
        // Each non-base level: test, jz, dec, call, inc, inc, ret; base: 4.
        instructions: 3 + (4 + DEPTH * 7 + 4) * u64::from(n),
    }
}

/// A byte-code dispatch loop shaped like a guest script interpreter.
fn dispatch(n: u32) -> Workload {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut next = a.create_label();
    let mut ops = [(); 5].map(|_| a.create_label());
    a.xor(ebx, ebx).unwrap();
    a.mov(edx, 0x12345).unwrap();
    a.mov(ecx, n).unwrap();
    a.set_label(&mut top).unwrap();
    a.mov(esi, BYTECODE).unwrap();
    a.set_label(&mut next).unwrap();
    a.movzx(eax, byte_ptr(esi)).unwrap();
    a.inc(esi).unwrap();
    a.jmp(dword_ptr(eax * 4 + TABLE)).unwrap();
    a.set_label(&mut ops[0]).unwrap();
    a.add(ebx, 1).unwrap();
    a.jmp(next).unwrap();
    a.set_label(&mut ops[1]).unwrap();
    a.xor(ebx, edx).unwrap();
    a.jmp(next).unwrap();
    a.set_label(&mut ops[2]).unwrap();
    a.lea(ebx, ebx + ebx * 2).unwrap();
    a.jmp(next).unwrap();
    a.set_label(&mut ops[3]).unwrap();
    a.sub(edx, ebx).unwrap();
    a.jmp(next).unwrap();
    a.set_label(&mut ops[4]).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.int(0x2e).unwrap();
    let (code, result) = assemble(&mut a);
    let table: Vec<u8> = ops
        .iter()
        .flat_map(|label| (result.label_ip(label).unwrap() as u32).to_le_bytes())
        .collect();
    let mut seed = 0x2468_ace1u32;
    let mut bytecode: Vec<u8> = (0..VM_OPS)
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((seed >> 16) & 3) as u8
        })
        .collect();
    bytecode.push(4);
    Workload {
        name: "dispatch",
        code,
        data: vec![(TABLE, table), (BYTECODE, bytecode)],
        instructions: 4 + (1 + u64::from(VM_OPS) * 5 + 3 + 2) * u64::from(n),
    }
}

/// Many distinct straight-line functions, each called once per run: the
/// shape of first execution during startup rather than a hot loop.
fn cold(functions: u32) -> Workload {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut bodies: Vec<CodeLabel> = (0..functions).map(|_| a.create_label()).collect();
    a.xor(ebx, ebx).unwrap();
    a.mov(ecx, 7).unwrap();
    for body in &bodies {
        a.call(*body).unwrap();
    }
    a.int(0x2e).unwrap();
    for (i, body) in bodies.iter_mut().enumerate() {
        let i = i as u32;
        let slot = DATA + (i % 256) * 4;
        a.set_label(body).unwrap();
        a.mov(eax, dword_ptr(slot)).unwrap();
        a.add(eax, i.wrapping_mul(2_654_435_761)).unwrap();
        a.xor(eax, ebx).unwrap();
        a.rol(eax, (i % 31 + 1) as u32).unwrap();
        a.mov(dword_ptr(slot), eax).unwrap();
        a.lea(edx, eax + ecx * 4 + (i as i32 & 0x7ff)).unwrap();
        a.sub(edx, ebx).unwrap();
        a.imul_3(edx, edx, (i | 1) as i32 & 0xffff).unwrap();
        a.add(ebx, edx).unwrap();
        a.shr(edx, 3).unwrap();
        a.or(ecx, edx).unwrap();
        a.and(ecx, 0xffff).unwrap();
        a.mov(esi, dword_ptr(DATA + ((i + 1) % 256) * 4)).unwrap();
        a.add(esi, ebx).unwrap();
        a.xor(ebx, esi).unwrap();
        a.inc(ecx).unwrap();
        a.ret().unwrap();
    }
    Workload {
        name: "cold",
        code: assemble(&mut a).0,
        data: vec![(DATA, (0..1024u32).map(|i| (i * 13) as u8).collect())],
        // driver: 2 + call per function + int; body: COLD_BODY + ret.
        instructions: 3 + u64::from(functions) * (1 + COLD_BODY + 1),
    }
}

/// What `measure` drives: the facade's `Backend`.
trait Engine {
    fn load(&mut self, w: &Workload);
    fn run(&mut self) -> [u32; 8];
}

struct Facade<B>(B);

impl<B: Backend> Engine for Facade<B> {
    fn load(&mut self, w: &Workload) {
        load(&mut self.0, w);
    }

    fn run(&mut self) -> [u32; 8] {
        run(&mut self.0)
    }
}

fn load<B: Backend>(cpu: &mut B, w: &Workload) {
    assert!(
        w.code.len() <= CODE_SIZE as usize,
        "{} code too large",
        w.name
    );
    cpu.map(CODE, CODE_SIZE, Prot::RW).unwrap();
    cpu.write(CODE, &w.code).unwrap();
    cpu.protect(CODE, CODE_SIZE, Prot::RX).unwrap();
    cpu.map(DATA, DATA_SIZE, Prot::RW).unwrap();
    for (addr, bytes) in &w.data {
        cpu.write(*addr, bytes).unwrap();
    }
    cpu.map(STACK_BASE, STACK - STACK_BASE, Prot::RW).unwrap();
    cpu.map(PARK, 0x1000, Prot::RX).unwrap();
    cpu.set_execution_budget(None);
}

fn run<B: Backend>(cpu: &mut B) -> [u32; 8] {
    for reg in [Reg::Eax, Reg::Ecx, Reg::Edx, Reg::Ebx, Reg::Esi, Reg::Edi] {
        cpu.set_reg(reg, 0);
    }
    cpu.set_reg(Reg::Esp, STACK);
    cpu.set_reg(Reg::Eip, CODE);
    let exit = cpu.run(&mut Park);
    assert_eq!(exit, Exit::Parked);
    [
        Reg::Eax,
        Reg::Ecx,
        Reg::Edx,
        Reg::Ebx,
        Reg::Esp,
        Reg::Ebp,
        Reg::Esi,
        Reg::Edi,
    ]
    .map(|reg| cpu.reg(reg))
}

fn measure<E: Engine>(
    engine: &str,
    make: &dyn Fn() -> E,
    w: &Workload,
    samples: usize,
    expected: Option<[u32; 8]>,
) -> [u32; 8] {
    let mut cpu = make();
    cpu.load(w);
    let start = Instant::now();
    let first = cpu.run();
    let first_ms = start.elapsed().as_secs_f64() * 1e3;
    if let Some(expected) = expected {
        assert_eq!(first, expected, "{engine} {} diverged", w.name);
    }
    // Settle any background translation: at least ten runs and one second.
    let warm = Instant::now();
    let mut runs = 0;
    while runs < 10 || warm.elapsed() < Duration::from_secs(1) {
        cpu.run();
        runs += 1;
        if runs >= 10_000 {
            break;
        }
    }
    let per_sample = (runs / 10).max(1);
    let mut timings: Vec<f64> = (0..samples)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..per_sample {
                cpu.run();
            }
            start.elapsed().as_secs_f64() * 1e9 / (per_sample as f64 * w.instructions as f64)
        })
        .collect();
    timings.sort_by(f64::total_cmp);
    println!(
        "{engine},{},{},{:.3},{:.4},{:.4},{:.4},{runs}",
        w.name,
        w.instructions,
        first_ms,
        timings[samples / 2],
        timings[0],
        timings[samples - 1],
    );
    first
}

/// Cold work only exists once per fresh CPU, so sample whole first runs.
fn measure_cold<E: Engine>(
    engine: &str,
    make: &dyn Fn() -> E,
    w: &Workload,
    samples: usize,
    expected: Option<[u32; 8]>,
) -> [u32; 8] {
    let mut timings = Vec::new();
    let mut first = None;
    for _ in 0..samples {
        let mut cpu = make();
        cpu.load(w);
        let start = Instant::now();
        let regs = cpu.run();
        timings.push(start.elapsed().as_secs_f64() * 1e3);
        if let Some(expected) = expected {
            assert_eq!(regs, expected, "{engine} {} diverged", w.name);
        }
        first.get_or_insert(regs);
    }
    timings.sort_by(f64::total_cmp);
    let median = timings[samples / 2];
    println!(
        "{engine},{},{},{:.3},{:.4},{:.4},{:.4},1",
        w.name,
        w.instructions,
        median,
        median * 1e6 / w.instructions as f64,
        timings[0] * 1e6 / w.instructions as f64,
        timings[samples - 1] * 1e6 / w.instructions as f64,
    );
    first.unwrap()
}

fn main() {
    let samples: usize = std::env::args()
        .nth(1)
        .map_or(7, |s| s.parse().expect("sample count"));
    assert!(samples > 0);
    let scale: u64 =
        std::env::var("ADV_ENGINE_BENCH_SCALE").map_or(100, |s| s.parse().expect("scale percent"));
    assert!((1..=1000).contains(&scale));
    let list = |name| std::env::var(name).ok();
    let engines = list("ADV_ENGINE_BENCH_ENGINES");
    let selected = list("ADV_ENGINE_BENCH_WORKLOADS");
    let engine = |name: &str| {
        engines
            .as_ref()
            .is_none_or(|v| v.split(',').any(|e| e == name))
    };
    let wanted = |name: &str| {
        selected
            .as_ref()
            .is_none_or(|v| v.split(',').any(|w| w == name))
    };
    let interpreter = engine("interpreter");
    let n = |base: u64| (base * scale / 100).max(1) as u32;
    let workloads = [
        alu(n(1_000_000)),
        memory(n(1_000_000)),
        call_shallow(n(1_000_000)),
        call_deep(n(40_000)),
        dispatch(n(8_000)),
        x87("x87_pc64", 0x133f, n(200_000)),
        x87("x87_pc53", 0x027f, n(200_000)),
    ];
    let cold = cold(n(4_000));
    let name = if cfg!(target_arch = "aarch64") {
        "a64"
    } else {
        "cranelift"
    };
    let native = engine(name);
    let reference = || Facade(JitCpu::new(PARK, false).unwrap());
    let jit = || Facade(JitCpu::new(PARK, true).unwrap());
    println!(
        "engine,workload,guest_instructions,first_run_ms,median_ns_per_instruction,min_ns,max_ns,warm_runs"
    );
    for w in workloads.iter().filter(|w| wanted(w.name)) {
        let expected = interpreter.then(|| measure("interpreter", &reference, w, 3, None));
        if native {
            measure(name, &jit, w, samples, expected);
        }
    }
    if wanted(cold.name) {
        let expected = interpreter.then(|| measure_cold("interpreter", &reference, &cold, 3, None));
        if native {
            measure_cold(name, &jit, &cold, samples, expected);
        }
    }
}
