//! Authored Tier 0 workloads. No game bytes, native JIT, or downloaded programs.
//! Run: cargo run --release -p adv-cpu --example interpreter_bench -- [samples=7] [iterations=100000] [workload=all] [mode=warm|fresh]
//! Output: CSV medians/range, exact fuel units, retired instructions, and checksum.
//! Fresh samples construct a new executor outside the timer and include decoding.
//! REP elements are fuel units, not individually retired x86 instructions.
use adv_cpu::{CpuState, EXEC, ExecutionTrap, Executor, Memory, READ, RunOutcome, WRITE};
use iced_x86::{BlockEncoderOptions, code_asm::*};
use std::hint::black_box;
use std::time::Instant;

const CODE: u32 = 0x10000;
const TABLE: u32 = 0x18000;
const STOP: u32 = 0x1f000;
const SOURCE: u32 = 0x20000;
const OUTPUT: u32 = 0x24000;
const STACK: u32 = 0x3f000;
const NAMES: &[&str] = &[
    "bytecode_vm",
    "integer_pixels",
    "sse_pixels",
    "mmx_pixels",
    "rep_copy_fill",
    "small_calls",
    "flag_branches",
];

struct NoTraps;
impl ExecutionTrap for NoTraps {
    fn call(&mut self, _: &mut CpuState, _: &mut Memory, _: u32) -> Result<bool, String> {
        Err("unexpected trap in authored Tier 0 benchmark".into())
    }
}

struct Workload {
    machine: Executor,
    initial: CpuState,
    expected_eax: u32,
    output: Vec<u8>,
    units: u64,
    instructions: u64,
}

fn workload(name: &str, n: u32) -> Workload {
    assert!(n > 0 && n <= 10_000_000);
    let source: Vec<_> = (0..1024).map(|i| (i * 17 + 3) as u8).collect();
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut regs = [0; 8];
    regs[1] = n;
    regs[4] = STACK;
    let mut labels = Vec::new();
    let mut output = Vec::new();
    let units;
    let instructions;
    let expected_eax;
    a.set_label(&mut top).unwrap();
    match name {
        "bytecode_vm" => {
            let mut join = a.create_label();
            labels = (0..4).map(|_| a.create_label()).collect();
            a.movzx(ebx, byte_ptr(edx + SOURCE as i32)).unwrap();
            a.and(ebx, 3).unwrap();
            a.jmp(dword_ptr(ebx * 4 + TABLE as i32)).unwrap();
            for (i, label) in labels.iter_mut().enumerate() {
                a.set_label(label).unwrap();
                a.add(eax, i as i32 * 2 + 1).unwrap();
                a.jmp(join).unwrap();
            }
            a.set_label(&mut join).unwrap();
            a.inc(edx).unwrap();
            a.and(edx, 255).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
            expected_eax = (0..n)
                .map(|i| u32::from(source[(i & 255) as usize] & 3) * 2 + 1)
                .sum();
            units = u64::from(n) * 9 + 1;
            instructions = units;
        }
        "integer_pixels" => {
            a.and(edx, 1023).unwrap();
            a.movzx(eax, byte_ptr(edx + SOURCE as i32)).unwrap();
            a.imul_3(eax, eax, 257).unwrap();
            a.shr(eax, 3).unwrap();
            a.xor(eax, 0x12345678).unwrap();
            a.mov(dword_ptr(edx * 4 + OUTPUT as i32), eax).unwrap();
            a.inc(edx).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
            let pixel = |i: u32| (u32::from(source[(i & 1023) as usize]) * 257 >> 3) ^ 0x12345678;
            output.resize(4096, 0);
            for i in 0..n.min(1024) {
                output[i as usize * 4..i as usize * 4 + 4].copy_from_slice(&pixel(i).to_le_bytes());
            }
            expected_eax = pixel(n - 1);
            units = u64::from(n) * 9 + 1;
            instructions = units;
        }
        "sse_pixels" | "mmx_pixels" => {
            let sse = name == "sse_pixels";
            let width = if sse { 16 } else { 8 };
            if sse {
                a.movdqu(xmm0, ptr(SOURCE)).unwrap();
                a.paddb(xmm0, ptr(SOURCE + 16)).unwrap();
                a.pxor(xmm0, ptr(SOURCE + 32)).unwrap();
                a.psrlw(xmm0, 1).unwrap();
                a.movdqu(ptr(OUTPUT), xmm0).unwrap();
            } else {
                a.movq(mm0, ptr(SOURCE)).unwrap();
                a.paddb(mm0, ptr(SOURCE + 8)).unwrap();
                a.pxor(mm0, ptr(SOURCE + 16)).unwrap();
                a.psrlw(mm0, 1).unwrap();
                a.movq(ptr(OUTPUT), mm0).unwrap();
            }
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            if !sse {
                a.emms().unwrap();
            }
            a.ret().unwrap();
            output = (0..width)
                .map(|i| source[i].wrapping_add(source[i + width]) ^ source[i + 2 * width])
                .collect();
            for pair in output.chunks_exact_mut(2) {
                let value = u16::from_le_bytes([pair[0], pair[1]]) >> 1;
                pair.copy_from_slice(&value.to_le_bytes());
            }
            expected_eax = 0;
            units = u64::from(n) * 7 + if sse { 1 } else { 2 };
            instructions = units;
        }
        "rep_copy_fill" => {
            let outer = n.div_ceil(128);
            regs[5] = outer;
            a.mov(esi, SOURCE).unwrap();
            a.mov(edi, OUTPUT).unwrap();
            a.mov(ecx, 128).unwrap();
            a.rep().movsd().unwrap();
            a.mov(edi, OUTPUT + 512).unwrap();
            a.mov(ecx, 128).unwrap();
            a.mov(eax, 0x5a5a5a5a).unwrap();
            a.rep().stosd().unwrap();
            a.mov(esi, SOURCE).unwrap();
            a.mov(edi, OUTPUT + 1024).unwrap();
            a.mov(ebx, 8).unwrap();
            let mut copy = a.create_label();
            a.set_label(&mut copy).unwrap();
            a.mov(edx, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), edx).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ebx).unwrap();
            a.jnz(copy).unwrap();
            a.dec(ebp).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
            output.extend_from_slice(&source[..512]);
            output.extend_from_slice(&[0x5a; 512]);
            output.extend_from_slice(&source[..32]);
            expected_eax = 0x5a5a5a5a;
            units = u64::from(outer) * 315 + 1;
            instructions = u64::from(outer) * 61 + 1;
        }
        "small_calls" => {
            let mut leaf = a.create_label();
            a.call(leaf).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
            a.set_label(&mut leaf).unwrap();
            a.push(ebx).unwrap();
            a.lea(ebx, ptr(eax + 3)).unwrap();
            a.mov(eax, ebx).unwrap();
            a.pop(ebx).unwrap();
            a.ret().unwrap();
            expected_eax = n * 3;
            units = u64::from(n) * 8 + 1;
            instructions = units;
        }
        "flag_branches" => {
            let mut small = a.create_label();
            let mut join = a.create_label();
            a.cmp(ecx, n / 2).unwrap();
            a.jbe(small).unwrap();
            a.add(eax, 3).unwrap();
            a.jmp(join).unwrap();
            a.set_label(&mut small).unwrap();
            a.sub(eax, 1).unwrap();
            a.set_label(&mut join).unwrap();
            a.test(ecx, 1).unwrap();
            a.setz(dl).unwrap();
            a.adc(eax, edx).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
            expected_eax = (0..n).rev().fold(0u32, |acc, i| {
                let value = if i + 1 > n / 2 {
                    acc.wrapping_add(3)
                } else {
                    acc.wrapping_sub(1)
                };
                value.wrapping_add(u32::from((i + 1) % 2 == 0))
            });
            units = u64::from(n - n / 2) * 9 + u64::from(n / 2) * 8 + 1;
            instructions = units;
        }
        _ => panic!("unknown workload"),
    }
    let assembled = a
        .assemble_options(
            u64::from(CODE),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let table: Vec<_> = labels
        .iter()
        .flat_map(|label| (assembled.label_ip(label).unwrap() as u32).to_le_bytes())
        .collect();
    let mut machine = Executor::new(CODE, 0x30000).unwrap();
    machine.protect(CODE, 0x30000, READ | WRITE).unwrap();
    machine.write(CODE, &assembled.inner.code_buffer).unwrap();
    machine.write(TABLE, &table).unwrap();
    machine.write(SOURCE, &source).unwrap();
    machine.write(STACK, &STOP.to_le_bytes()).unwrap();
    machine.protect(CODE, 4096, READ | EXEC).unwrap();
    machine.enable_auto_jit(false);
    let initial = CpuState {
        regs,
        eip: CODE,
        eflags: 2,
        ..Default::default()
    };
    Workload {
        machine,
        initial,
        expected_eax,
        output,
        units,
        instructions,
    }
}

impl Workload {
    fn run(&mut self) -> f64 {
        self.machine.state = self.initial.clone();
        let before = self.machine.stats.interpreter_instructions;
        let start = Instant::now();
        let outcome = self
            .machine
            .run_with_traps(STOP, self.units, &mut NoTraps, true)
            .unwrap();
        let ns = start.elapsed().as_secs_f64() * 1e9;
        assert_eq!(outcome, RunOutcome::ReachedStop);
        assert_eq!(self.machine.state.fuel, 0);
        assert_eq!(
            self.machine.stats.interpreter_instructions - before,
            self.units
        );
        assert_eq!(self.machine.stats.native_entries, 0);
        assert_eq!(self.machine.stats.compilations, 0);
        assert_eq!(self.machine.state.regs[0], self.expected_eax);
        assert_eq!(self.machine.state.regs[4], STACK + 4);
        assert_eq!(
            &*self
                .machine
                .memory()
                .read_data(OUTPUT, self.output.len())
                .unwrap(),
            self.output
        );
        black_box(&self.machine.state);
        ns
    }
}

pub fn main() {
    let args: Vec<_> = std::env::args().collect();
    let samples: usize = args.get(1).map_or(7, |x| x.parse().expect("samples"));
    let n = args
        .get(2)
        .map_or(100_000, |x| x.parse().expect("iterations"));
    let selection = args.get(3).map_or("all", String::as_str);
    let mode = args.get(4).map_or("warm", String::as_str);
    assert!(matches!(mode, "warm" | "fresh"));
    assert!(samples > 0 && (selection == "all" || NAMES.contains(&selection)));
    println!(
        "workload,mode,samples,iterations,units,instructions,cold_ns_per_unit,median_ns_per_unit,min_ns_per_unit,max_ns_per_unit,median_ns_per_instruction,checksum"
    );
    for &name in NAMES
        .iter()
        .filter(|&&name| selection == "all" || name == selection)
    {
        let mut w = workload(name, n);
        let cold = w.run() / w.units as f64;
        let mut times: Vec<_> = (0..samples)
            .map(|_| {
                // Construction and assembly remain outside the timer. A fresh
                // executor has no decoded instructions from earlier samples.
                if mode == "fresh" {
                    w = workload(name, n);
                }
                w.run()
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!(
            "{name},{mode},{samples},{n},{},{},{cold:.3},{:.3},{:.3},{:.3},{:.3},{}",
            w.units,
            w.instructions,
            times[samples / 2] / w.units as f64,
            times[0] / w.units as f64,
            times[samples - 1] / w.units as f64,
            times[samples / 2] / w.instructions as f64,
            w.expected_eax
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_workloads_validate_fuel_and_outputs_at_small_boundaries() {
        for &name in NAMES {
            for n in [1, 3, 128, 1025] {
                workload(name, n).run();
            }
        }
    }
}
