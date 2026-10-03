//! Execute the exact x86 functions from tools/perf/workload-native.c.
//! Input is a mapped PE image and a text manifest produced by pack-workload.py.
//! This measures synthetic workloads, not whole-game performance or ARM efficiency.
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Backend, Exit, Guest, Prot, Reg, SegmentSetup, TrapAction, TrapHandler};
use std::path::Path;
use std::time::Instant;

const STACK: u32 = 0x0200_0000;
const GDT: u32 = 0x0300_0000;
const STOP: u32 = 0x0310_0000;
const INPUT: u32 = 0x0400_0000;
const OUTPUT: u32 = 0x0500_0000;

struct Done;
impl TrapHandler for Done {
    fn on_trap(&mut self, _: &mut dyn Guest, _: u8) -> TrapAction {
        TrapAction::Park
    }
}

#[cfg(windows)]
mod host {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(process: *mut c_void, mask: usize) -> i32;
        fn GetProcessTimes(process: *mut c_void, c: *mut u64, e: *mut u64, k: *mut u64, u: *mut u64) -> i32;
    }
    pub fn pin() {
        assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
    }
    pub fn cpu_seconds() -> f64 {
        let (mut c, mut e, mut k, mut u) = (0, 0, 0, 0);
        assert_ne!(unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }, 0);
        (k + u) as f64 * 1e-7
    }
}
#[cfg(not(windows))]
mod host {
    pub fn pin() {}
    pub fn cpu_seconds() -> f64 {
        f64::NAN
    }
}

fn next(s: &mut u32) -> u32 {
    *s ^= *s << 13;
    *s ^= *s >> 17;
    *s ^= *s << 5;
    *s
}
fn hash(cpu: &UnicornCpu, n: u32) -> u32 {
    let mut data = vec![0; n as usize * 4];
    cpu.read(OUTPUT, &mut data).unwrap();
    data.chunks_exact(4)
        .fold(2166136261u32, |h, v| (h ^ u32::from_le_bytes(v.try_into().unwrap())).wrapping_mul(16777619))
}
fn call(cpu: &mut UnicornCpu, entry: u32, n: u32, rounds: u32) {
    let args = [STOP, OUTPUT, INPUT, n, rounds];
    let data: Vec<u8> = args.into_iter().flat_map(u32::to_le_bytes).collect();
    cpu.write(STACK - 0x100, &data).unwrap();
    cpu.set_reg(Reg::Esp, STACK - 0x100);
    cpu.set_reg(Reg::Eip, entry);
    assert_eq!(cpu.run(&mut Done), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Eip), STOP + 0x100);
}

struct CopyBlend {
    input: Vec<u8>,
    output: Vec<u8>,
}
impl CopyBlend {
    fn run(&mut self, cpu: &mut UnicornCpu, rounds: u32) {
        // Intentionally include full guest reads and writes once per image, without
        // zero-copy mappings or unchanged-input caching. Allocation is outside timing.
        for _ in 0..rounds {
            cpu.read(INPUT, &mut self.input).unwrap();
            let (a, b) = self.input.split_at(self.output.len());
            for ((out, a), b) in self.output.iter_mut().zip(a).zip(b) {
                *out = ((u16::from(*a) + u16::from(*b) + 1) >> 1) as u8;
            }
            cpu.write(OUTPUT, &self.output).unwrap();
        }
    }
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(!args.is_empty(), "workload_bench <packed-image-directory> [samples]");
    let root = Path::new(&args[0]);
    let samples: usize = args.get(1).map_or(3, |s| s.parse().unwrap());
    let metadata = std::fs::read_to_string(root.join("workload.txt")).unwrap();
    let mut lines = metadata.lines();
    let base = u32::from_str_radix(lines.next().unwrap(), 16).unwrap();
    let image = std::fs::read(root.join("workload.bin")).unwrap();
    host::pin();
    println!("backend,kernel,sample,calls,units_per_call,wall_ns_per_call,cpu_ns_per_call,hash");
    for line in lines {
        let (name, rva) = line.split_once(' ').unwrap();
        let entry = base + u32::from_str_radix(rva, 16).unwrap();
        let (n, rounds) = if name.starts_with("blend") { (1280 * 720u32, 4u32) } else { (65536, 16) };
        let mut cpu = UnicornCpu::new(STOP + 0x100).unwrap();
        cpu.map(base, (image.len() as u32).next_multiple_of(4096), Prot::RWX).unwrap();
        cpu.write(base, &image).unwrap();
        cpu.map(STACK - 0x10000, 0x10000, Prot::RW).unwrap();
        cpu.map(GDT, 0x1000, Prot::RW).unwrap();
        cpu.map(STOP, 0x1000, Prot::RX).unwrap();
        cpu.write(STOP, &[0xcd, 0x2e]).unwrap();
        cpu.setup_segments(SegmentSetup { gdt_addr: GDT }).unwrap();
        cpu.map(INPUT, (n * 8).next_multiple_of(4096), Prot::RW).unwrap();
        cpu.map(OUTPUT, (n * 4).next_multiple_of(4096), Prot::RW).unwrap();
        let mut seed = 0x13579bdf;
        let mut input: Vec<u32> = (0..n * 2).map(|_| next(&mut seed)).collect();
        if name == "dispatch_predictable" {
            for word in &mut input[..n as usize] {
                *word &= !7;
            }
        }
        if name == "chase" {
            for i in 0..n {
                input[i as usize] = i;
            }
            for i in (1..n).rev() {
                let j = next(&mut seed) % (i + 1);
                input.swap(i as usize, j as usize);
            }
        }
        let data: Vec<u8> = input.into_iter().flat_map(u32::to_le_bytes).collect();
        cpu.write(INPUT, &data).unwrap();
        call(&mut cpu, entry, n, rounds);
        let expected = hash(&cpu, n);
        for accelerated in [false, true] {
            if accelerated && name != "blend_sse2" {
                continue;
            }
            let backend = if accelerated { "host_blend_copy" } else { "unicorn_x86" };
            let mut blend = CopyBlend { input: vec![0; n as usize * 8], output: vec![0; n as usize * 4] };
            let mut operation = |cpu: &mut UnicornCpu| {
                if accelerated {
                    blend.run(cpu, rounds);
                } else {
                    call(cpu, entry, n, rounds);
                }
            };
            operation(&mut cpu);
            let mut calls = 1;
            loop {
                let start = Instant::now();
                for _ in 0..calls {
                    operation(&mut cpu);
                }
                if start.elapsed().as_secs_f64() >= 0.12 {
                    break;
                }
                calls *= 2;
            }
            for sample in 0..samples {
                let cpu_start = host::cpu_seconds();
                let start = Instant::now();
                for _ in 0..calls {
                    operation(&mut cpu);
                }
                let wall = start.elapsed().as_secs_f64();
                let cpu_time = host::cpu_seconds() - cpu_start;
                assert_eq!(hash(&cpu, n), expected);
                println!(
                    "{backend},{name},{sample},{calls},{},{:.3},{:.3},{expected:08x}",
                    n * rounds,
                    wall * 1e9 / f64::from(calls),
                    cpu_time * 1e9 / f64::from(calls)
                );
            }
        }
    }
}
