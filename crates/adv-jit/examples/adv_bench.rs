//! Authored synthetic x86 workloads, without game code or resources.
//! Run: cargo run --release -p adv-jit --example adv_bench -- [samples]
#[path = "../tests/support/mod.rs"]
mod oracle_support;
use adv_jit::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::{BlockEncoderOptions, code_asm::*};
use oracle_support::Machine;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x1f000;
const DATA: u32 = 0x20000;
const TABLE: u32 = 0x18000;
const STACK: u32 = 0x300000;
const N: u32 = 1 << 18;

struct Workload {
    code: Vec<u8>,
    entries: Vec<u32>,
    data: Vec<u8>,
    table: Vec<u8>,
    regs: [u32; 8],
}

fn workload(name: &str) -> Workload {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut labels = Vec::new();
    let mut regs = [0; 8];
    regs[1] = N;
    regs[6] = DATA;
    let mut data = Vec::new();
    if name.starts_with("dispatch") {
        let mut done = a.create_label();
        labels = (0..4).map(|_| a.create_label()).collect();
        a.set_label(&mut top).unwrap();
        let packed = name == "dispatch_bytes";
        if packed {
            a.movzx(edx, byte_ptr(esi)).unwrap();
        } else {
            a.mov(edx, dword_ptr(esi)).unwrap();
        }
        a.and(edx, 3).unwrap();
        a.mov(ebx, dword_ptr(esi + if packed { 1 } else { 4 }))
            .unwrap();
        a.jmp(dword_ptr(edx * 4 + TABLE as i32)).unwrap();
        for (i, label) in labels.iter_mut().enumerate() {
            a.set_label(label).unwrap();
            match i {
                0 => a.add(eax, ebx),
                1 => a.xor(eax, ebx),
                2 => a.sub(eax, ebx),
                _ => a.or(eax, ebx),
            }
            .unwrap();
            a.jmp(done).unwrap();
        }
        a.set_label(&mut done).unwrap();
        a.add(esi, if packed { 5 } else { 8 }).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        let mut seed = 0x12345678u32;
        for _ in 0..N {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let op = if name == "dispatch_predictable" {
                0
            } else {
                seed & 3
            };
            if packed {
                data.push(op as u8);
            } else {
                data.extend_from_slice(&op.to_le_bytes());
            }
            data.extend_from_slice(&seed.to_le_bytes());
        }
    } else if name == "call_return" {
        let mut sub = a.create_label();
        a.set_label(&mut top).unwrap();
        a.call(sub).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        a.set_label(&mut sub).unwrap();
        a.add(eax, 3).unwrap();
        a.xor(eax, 0x12345678).unwrap();
        a.ret().unwrap();
    } else if name == "pointer_chase" {
        a.set_label(&mut top).unwrap();
        a.mov(edx, dword_ptr(esi + edx * 4)).unwrap();
        a.add(eax, edx).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        for i in 0..65536u32 {
            data.extend_from_slice(
                &(i.wrapping_mul(1664525).wrapping_add(1013904223) & 65535).to_le_bytes(),
            );
        }
    } else {
        if name == "cold_then_hot" {
            a.bswap(eax).unwrap();
        }
        a.set_label(&mut top).unwrap();
        a.add(eax, ecx).unwrap();
        a.xor(eax, 0x12345678).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    }
    let result = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let entries: Vec<_> = labels
        .iter()
        .map(|label| result.label_ip(label).unwrap() as u32)
        .collect();
    let table = entries.iter().flat_map(|pc| pc.to_le_bytes()).collect();
    Workload {
        code: result.inner.code_buffer,
        entries,
        data,
        table,
        regs,
    }
}

fn setup(w: &Workload, jit: bool) -> Machine {
    let mut m = Machine::new(CODE, 0x300000).unwrap();
    m.protect(CODE, 0x300000, READ | WRITE).unwrap();
    m.write(CODE, &w.code).unwrap();
    m.write(DATA, &w.data).unwrap();
    m.write(TABLE, &w.table).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        let mut r = Region::new(CODE, w.code.len());
        r.entries.clone_from(&w.entries);
        m.set_region(r).unwrap();
    }
    m
}

fn call(m: &mut Machine, w: &Workload, jit: bool) -> (u32, u32) {
    m.state = CpuState {
        regs: w.regs,
        eip: CODE,
        ..CpuState::default()
    };
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    let outcome = if jit {
        m.run(STOP, 20_000_000)
    } else {
        m.run_unicorn_unbounded(STOP)
    }
    .unwrap();
    assert_eq!(outcome, RunOutcome::ReachedStop);
    (m.state.regs[0], m.state.eflags & 0x8d5)
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
    let samples: usize = std::env::args().nth(1).map_or(5, |v| v.parse().unwrap());
    assert!(samples > 0);
    pin();
    println!(
        "workload,jit_ms,unicorn_ms,speedup,compile_ms,native_bytes,native_entries_per_call,interpreter_entries_per_call,checksum"
    );
    for name in [
        "integer_loop",
        "dispatch_predictable",
        "dispatch_random",
        "dispatch_bytes",
        "pointer_chase",
        "call_return",
        "cold_then_hot",
    ] {
        let w = workload(name);
        let mut jit = setup(&w, true);
        let mut uc = setup(&w, false);
        let expected = call(&mut uc, &w, false);
        let warmup = Instant::now();
        assert_eq!(call(&mut jit, &w, true), expected);
        let calls = (0.01 / warmup.elapsed().as_secs_f64())
            .ceil()
            .clamp(1.0, 256.0) as u64;
        let mut times = [Vec::new(), Vec::new()];
        for sample in 0..samples {
            for index in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                let start = Instant::now();
                for _ in 0..calls {
                    let result = if index == 0 {
                        call(&mut jit, &w, true)
                    } else {
                        call(&mut uc, &w, false)
                    };
                    assert_eq!(result, expected);
                }
                times[index].push(start.elapsed().as_secs_f64() * 1000.0 / calls as f64);
            }
        }
        for t in &mut times {
            t.sort_by(f64::total_cmp);
        }
        let j = times[0][samples / 2];
        let u = times[1][samples / 2];
        println!(
            "{name},{j:.4},{u:.4},{:.2},{:.3},{},{},{},{:08x}",
            u / j,
            jit.stats.compile_time.as_secs_f64() * 1000.0,
            jit.stats.native_code_bytes,
            jit.stats.native_entries / (samples as u64 * calls + 1),
            jit.stats.interpreter_entries / (samples as u64 * calls + 1),
            expected.0
        );
    }
}
