//! Self-authored memory workloads; no application bytes or whole-game claims.
//! Usage: memory_pressure_bench [samples=5] [iterations=200000] [repeats=1]
//! Windows: ADV_BENCH_CPU selects a logical CPU (default 0). Thread cycles
//! are reported independently from wall time and are never converted to time.
//! Initializing memory, compiling, and checking complete outputs are untimed.
//! The three pressure loops deliberately count in memory, outside the current
//! register-counted loop recognizer. Pixels retain its normal counted form.
use adv_cpu::{CompileStrategy, CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::hint::black_box;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x60000;
const FRAME: u32 = STACK + 0x100;
const RETURN: u32 = STACK + 0xffc;
const PIXELS: u32 = 0x100000;
const WORDS: usize = 16384;
const NODES: usize = 4096;
const SLOTS: usize = 12;
const SEED: u32 = 0x13579bdf;

#[derive(Clone, Copy, Debug)]
enum Workload {
    Random,
    Pointer,
    Stack,
    Pixels,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Random => "random_rw",
            Self::Pointer => "pointer_chase",
            Self::Stack => "stack_pressure",
            Self::Pixels => "sequential_pixels",
        }
    }

    fn index(self) -> u32 {
        match self {
            Self::Random => 0,
            Self::Pointer => 1,
            Self::Stack => 2,
            Self::Pixels => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Output {
    data: Vec<u32>,
    slots: [u32; SLOTS],
    sum: u32,
    seed: u32,
    cursor: u32,
}

fn xorshift(mut value: u32) -> u32 {
    value ^= value << 13;
    value ^= value >> 17;
    value ^ (value << 5)
}

fn initial(workload: Workload, iterations: u32) -> Output {
    let len = match workload {
        Workload::Pointer => NODES * 2,
        Workload::Pixels => iterations as usize,
        _ => WORDS,
    };
    let mut seed = SEED;
    let mut data: Vec<u32> = (0..len)
        .map(|_| {
            seed = xorshift(seed);
            seed
        })
        .collect();
    if matches!(workload, Workload::Pointer) {
        // A deterministic shuffled single cycle. Each next field is a guest
        // address, so the host and guest chase exactly the same graph.
        let mut order: Vec<usize> = (0..NODES).collect();
        for index in (1..NODES).rev() {
            seed = xorshift(seed);
            order.swap(index, seed as usize % (index + 1));
        }
        for index in 0..NODES {
            data[order[index] * 2] = DATA + (order[(index + 1) % NODES] * 8) as u32;
        }
    }
    Output {
        data,
        slots: std::array::from_fn(|i| SEED.wrapping_add(i as u32 * 17)),
        sum: 0,
        seed: SEED,
        cursor: DATA,
    }
}

fn pixel_layout(iterations: u32) -> (u32, u32, usize) {
    let stride = (iterations as usize * 4).div_ceil(4096) * 4096;
    (PIXELS + stride as u32, PIXELS + 2 * stride as u32, stride)
}

fn code(workload: Workload) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    match workload {
        Workload::Random => {
            for (shift, left) in [(13, true), (17, false), (5, true)] {
                a.mov(edx, eax).unwrap();
                if left {
                    a.shl(edx, shift).unwrap();
                } else {
                    a.shr(edx, shift).unwrap();
                }
                a.xor(eax, edx).unwrap();
            }
            a.mov(esi, eax).unwrap();
            a.and(esi, (WORDS - 1) as u32).unwrap();
            a.mov(edx, dword_ptr(edi + esi * 4)).unwrap();
            a.xor(edx, eax).unwrap();
            a.rol(edx, 7).unwrap();
            a.add(edx, ebx).unwrap();
            a.mov(dword_ptr(edi + esi * 4), edx).unwrap();
            a.add(ebx, edx).unwrap();
        }
        Workload::Pointer => {
            a.mov(esi, dword_ptr(edi)).unwrap();
            a.mov(eax, dword_ptr(edi + 4)).unwrap();
            a.xor(eax, ebx).unwrap();
            a.rol(eax, 3).unwrap();
            a.add(eax, 17).unwrap();
            a.mov(dword_ptr(edi + 4), eax).unwrap();
            a.add(ebx, eax).unwrap();
            a.mov(edi, esi).unwrap();
        }
        Workload::Stack => {
            for slot in 0..SLOTS {
                let offset = -4 * (slot as i32 + 1);
                a.mov(eax, dword_ptr(ebp + offset)).unwrap();
                a.add(eax, ebx).unwrap();
                a.rol(eax, (slot % 7 + 1) as u32).unwrap();
                a.xor(ebx, eax).unwrap();
                a.mov(dword_ptr(ebp + offset), eax).unwrap();
            }
            a.mov(edx, ebx).unwrap();
            a.and(edx, (WORDS - 1) as u32).unwrap();
            a.mov(eax, dword_ptr(edi + edx * 4)).unwrap();
            a.add(ebx, eax).unwrap();
            a.xor(dword_ptr(edi + edx * 4), ebx).unwrap();
        }
        Workload::Pixels => {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.xor(eax, dword_ptr(edx)).unwrap();
            a.add(eax, 0x03030303u32).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(ebx, eax).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edx, 4).unwrap();
            a.add(edi, 4).unwrap();
        }
    }
    if matches!(workload, Workload::Pixels) {
        a.dec(ecx).unwrap();
    } else {
        a.dec(dword_ptr(ebp - 128)).unwrap();
    }
    a.jnz(top).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE + workload.index() * 4096))
        .unwrap()
}

#[inline(never)]
fn host(workload: Workload, iterations: u32, input: &Output, pixel_b: &[u32], out: &mut Output) {
    let mut sum = 0u32;
    let mut seed = SEED;
    let mut cursor = DATA;
    match workload {
        Workload::Random => {
            for _ in 0..iterations {
                seed = xorshift(seed);
                let index = seed as usize & (WORDS - 1);
                let value = (out.data[index] ^ seed).rotate_left(7).wrapping_add(sum);
                out.data[index] = value;
                sum = sum.wrapping_add(value);
            }
        }
        Workload::Pointer => {
            for _ in 0..iterations {
                let index = ((cursor - DATA) / 4) as usize;
                // Every generated next field is an aligned node inside data;
                // this loop only writes its value field, preserving that fact.
                let (next, old) = unsafe {
                    (
                        *out.data.get_unchecked(index),
                        *out.data.get_unchecked(index + 1),
                    )
                };
                let value = (old ^ sum).rotate_left(3).wrapping_add(17);
                unsafe { *out.data.get_unchecked_mut(index + 1) = value };
                sum = sum.wrapping_add(value);
                cursor = next;
            }
        }
        Workload::Stack => {
            for _ in 0..iterations {
                for (slot, value) in out.slots.iter_mut().enumerate() {
                    *value = value.wrapping_add(sum).rotate_left((slot % 7 + 1) as u32);
                    sum ^= *value;
                }
                let index = sum as usize & (WORDS - 1);
                sum = sum.wrapping_add(out.data[index]);
                out.data[index] ^= sum;
            }
        }
        Workload::Pixels => {
            for ((&a, &b), value) in input.data.iter().zip(pixel_b).zip(&mut out.data) {
                *value = (a ^ b).wrapping_add(0x03030303);
                sum = sum.wrapping_add(*value);
            }
        }
    }
    out.sum = sum;
    out.seed = seed;
    out.cursor = cursor;
    black_box(&*out);
}

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn read_words(cpu: &Executor, address: u32, len: usize) -> Vec<u32> {
    cpu.memory()
        .read(address, len * 4)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect()
}

fn setup(workload: Workload, iterations: u32, full: bool, strategy: CompileStrategy) -> Executor {
    let (b, out, stride) = pixel_layout(iterations);
    let end = out as usize + stride;
    let mut cpu = if full {
        Executor::address_space().unwrap()
    } else {
        let mut cpu = Executor::new(CODE, end - CODE as usize).unwrap();
        cpu.protect(CODE, end - CODE as usize, READ | WRITE)
            .unwrap();
        cpu
    };
    let entry = CODE + workload.index() * 4096;
    if full {
        cpu.map(entry, 4096, READ | EXEC).unwrap();
        cpu.map(STOP, 4096, READ | EXEC).unwrap();
        cpu.map(DATA, WORDS * 4, READ | WRITE).unwrap();
        cpu.map(STACK, 4096, READ | WRITE).unwrap();
        if matches!(workload, Workload::Pixels) {
            for address in [PIXELS, b, out] {
                cpu.map(address, stride, READ | WRITE).unwrap();
            }
        }
    } else {
        cpu.protect(entry, 4096, READ | EXEC).unwrap();
        cpu.protect(STOP, 4096, READ | EXEC).unwrap();
    }
    let code = code(workload);
    cpu.write(entry, &code).unwrap();
    let mut region = Region::new(entry, code.len());
    region.compile_strategy = strategy;
    cpu.add_region(region).unwrap();
    cpu
}

fn reset(cpu: &mut Executor, workload: Workload, iterations: u32, input: &Output) {
    cpu.state = CpuState::default();
    cpu.state.eip = CODE + workload.index() * 4096;
    cpu.state.regs = [SEED, iterations, 0, 0, RETURN, FRAME, 0, DATA];
    cpu.write(RETURN, &STOP.to_le_bytes()).unwrap();
    cpu.write(FRAME - 128, &iterations.to_le_bytes()).unwrap();
    for (slot, value) in input.slots.iter().enumerate() {
        cpu.write(FRAME - (slot as u32 + 1) * 4, &value.to_le_bytes())
            .unwrap();
    }
    if matches!(workload, Workload::Pixels) {
        let (b, out, _) = pixel_layout(iterations);
        cpu.write(PIXELS, &bytes(&input.data)).unwrap();
        cpu.write(
            b,
            &bytes(
                &input
                    .data
                    .iter()
                    .map(|v| v.rotate_left(11))
                    .collect::<Vec<_>>(),
            ),
        )
        .unwrap();
        cpu.write(out, &vec![0; input.data.len() * 4]).unwrap();
        cpu.state.regs[2] = b;
        cpu.state.regs[6] = PIXELS;
        cpu.state.regs[7] = out;
    } else {
        cpu.write(DATA, &bytes(&input.data)).unwrap();
    }
}

fn verify(cpu: &Executor, workload: Workload, iterations: u32, expected: &Output) {
    assert_eq!(cpu.state.regs[3], expected.sum, "{} sum", workload.name());
    assert_eq!(cpu.state.regs[4], RETURN + 4);
    assert_eq!(cpu.state.eip, STOP);
    let address = if matches!(workload, Workload::Pixels) {
        let (b, out, _) = pixel_layout(iterations);
        assert_eq!(cpu.state.regs[1], 0);
        assert_eq!(cpu.state.regs[2], b + iterations * 4);
        assert_eq!(cpu.state.regs[6], PIXELS + iterations * 4);
        assert_eq!(cpu.state.regs[7], out + iterations * 4);
        out
    } else {
        assert_eq!(read_words(cpu, FRAME - 128, 1), [0]);
        let slots: Vec<u32> = (0..SLOTS)
            .map(|slot| read_words(cpu, FRAME - (slot as u32 + 1) * 4, 1)[0])
            .collect();
        assert_eq!(slots, expected.slots);
        if matches!(workload, Workload::Random) {
            assert_eq!(cpu.state.regs[0], expected.seed);
        }
        if matches!(workload, Workload::Pointer) {
            assert_eq!(cpu.state.regs[7], expected.cursor);
        }
        DATA
    };
    assert_eq!(read_words(cpu, address, expected.data.len()), expected.data);
}

fn checksum(output: &Output) -> u64 {
    output
        .data
        .iter()
        .chain(&output.slots)
        .copied()
        .chain([output.sum, output.seed, output.cursor])
        .fold(0xcbf29ce484222325u64, |hash, value| {
            (hash ^ u64::from(value)).wrapping_mul(0x100000001b3)
        })
}

fn median(times: &mut [f64]) -> f64 {
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

#[cfg(windows)]
fn pin() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(process: *mut c_void, mask: usize) -> i32;
        fn SetPriorityClass(process: *mut c_void, priority_class: u32) -> i32;
    }
    let core: usize =
        std::env::var("ADV_BENCH_CPU").map_or(0, |v| v.parse().expect("ADV_BENCH_CPU"));
    assert!(
        core < usize::BITS as usize,
        "CPU must be in the current processor group"
    );
    let process = unsafe { GetCurrentProcess() };
    assert_ne!(
        unsafe { SetProcessAffinityMask(process, 1usize << core) },
        0
    );
    const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x8000;
    assert_ne!(
        unsafe { SetPriorityClass(process, ABOVE_NORMAL_PRIORITY_CLASS) },
        0
    );
    eprintln!("bench_cpu={core},process_priority=AboveNormal");
}
#[cfg(not(windows))]
fn pin() {}

#[cfg(windows)]
fn thread_cycles() -> Option<u64> {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn QueryThreadCycleTime(thread: *mut c_void, cycles: *mut u64) -> i32;
    }
    let mut cycles = 0;
    assert_ne!(
        unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) },
        0
    );
    Some(cycles)
}

#[cfg(not(windows))]
fn thread_cycles() -> Option<u64> {
    None
}

fn cycles_median(times: &mut [f64]) -> String {
    if times.is_empty() {
        String::new()
    } else {
        format!("{:.3}", median(times))
    }
}

pub fn main() {
    let args: Vec<_> = std::env::args().collect();
    let samples: usize = args.get(1).map_or(5, |v| v.parse().expect("samples"));
    let iterations: u32 = args
        .get(2)
        .map_or(200_000, |v| v.parse().expect("iterations"));
    let repeats: usize = args.get(3).map_or(1, |v| v.parse().expect("repeats"));
    assert!(samples > 0 && repeats > 0 && iterations > 0 && iterations <= 10_000_000);
    pin();
    println!(
        "workload,backend,window,samples,iterations,repeats,median_ns_per_iter,median_thread_cycles_per_iter,native_bytes,interpreted_units,interpreter_entries,native_entries,compile_ms,checksum"
    );
    for workload in [
        Workload::Random,
        Workload::Pointer,
        Workload::Stack,
        Workload::Pixels,
    ] {
        let input = initial(workload, iterations);
        let pixel_b: Vec<u32> = input.data.iter().map(|v| v.rotate_left(11)).collect();
        let mut expected = input.clone();
        host(workload, iterations, &input, &pixel_b, &mut expected);
        let expected_checksum = checksum(&expected);
        let mut host_times = Vec::with_capacity(samples);
        let mut host_cycles = Vec::with_capacity(samples);
        for sample in 0..samples + 2 {
            let mut ns = 0;
            let mut cycles = 0;
            for _ in 0..repeats {
                let mut out = input.clone();
                let cycles_before = thread_cycles();
                let start = Instant::now();
                host(
                    black_box(workload),
                    black_box(iterations),
                    black_box(&input),
                    black_box(&pixel_b),
                    black_box(&mut out),
                );
                ns += start.elapsed().as_nanos();
                if let Some((before, after)) = cycles_before.zip(thread_cycles()) {
                    cycles += after - before;
                }
                assert_eq!(out, expected);
            }
            if sample >= 2 {
                host_times.push(ns as f64 / iterations as f64 / repeats as f64);
                if cfg!(windows) {
                    host_cycles.push(cycles as f64 / iterations as f64 / repeats as f64);
                }
            }
        }
        println!(
            "{},host_rust,host,{samples},{iterations},{repeats},{:.3},{},0,0,0,0,0,{expected_checksum:016x}",
            workload.name(),
            median(&mut host_times),
            cycles_median(&mut host_cycles)
        );
        for full in [false, true] {
            for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
                let mut cpu = setup(workload, iterations, full, strategy);
                let mut times = Vec::with_capacity(samples);
                let mut cycle_times = Vec::with_capacity(samples);
                let mut counts = [0u64; 3];
                for sample in 0..samples + 2 {
                    let mut ns = 0;
                    let mut cycles = 0;
                    for _ in 0..repeats {
                        reset(&mut cpu, workload, iterations, &input);
                        let before = [
                            cpu.stats.interpreter_instructions,
                            cpu.stats.interpreter_entries,
                            cpu.stats.native_entries,
                        ];
                        let cycles_before = thread_cycles();
                        let start = Instant::now();
                        let result = cpu.run(STOP, u64::from(iterations) * 256 + 1024).unwrap();
                        ns += start.elapsed().as_nanos();
                        if let Some((before, after)) = cycles_before.zip(thread_cycles()) {
                            cycles += after - before;
                        }
                        assert_eq!(result, RunOutcome::ReachedStop);
                        verify(&cpu, workload, iterations, &expected);
                        if sample >= 2 {
                            let after = [
                                cpu.stats.interpreter_instructions,
                                cpu.stats.interpreter_entries,
                                cpu.stats.native_entries,
                            ];
                            for i in 0..3 {
                                counts[i] += after[i] - before[i];
                            }
                        }
                    }
                    if sample >= 2 {
                        times.push(ns as f64 / iterations as f64 / repeats as f64);
                        if cfg!(windows) {
                            cycle_times.push(cycles as f64 / iterations as f64 / repeats as f64);
                        }
                    }
                }
                println!(
                    "{},{},{},{samples},{iterations},{repeats},{:.3},{},{},{},{},{},{:.3},{expected_checksum:016x}",
                    workload.name(),
                    if strategy == CompileStrategy::Fast {
                        "jit_fast"
                    } else {
                        "jit_speed"
                    },
                    if full { "full4g" } else { "short" },
                    median(&mut times),
                    cycles_median(&mut cycle_times),
                    cpu.stats.native_code_bytes,
                    counts[0],
                    counts[1],
                    counts[2],
                    cpu.stats.compile_time.as_secs_f64() * 1000.0
                );
            }
        }
    }
    if let Some(report) = adv_cpu::loop_coverage() {
        eprintln!("{report}");
    }
}
