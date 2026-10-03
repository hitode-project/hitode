//! Per-call cost of the three ways guest code reaches the host.
//!
//!   cargo run --release -p pal-cpu --example trap_bench
//!
//! * baseline: `call` to a guest `ret` (no host involvement)
//! * fast:     `call` to an `int 0x2e; ret` thunk; the handler returns Continue in-hook
//! * park:     the handler parks; the host completes the call and re-enters emu_start
//!
//! Optional arguments: `all|watchdog|native [samples]` (defaults: all, 5).
//! The backend pair measures production overhead; the native trio isolates Unicorn's
//! timeout/count costs with identical raw-engine hooks. These groups have different
//! hook dispatch, so compare modes within each group. Thread startup and warm-up are
//! outside the timed samples. Native timeout uses fewer parks because it creates and
//! joins a timer thread on every emu_start. No game files are used.

use std::time::{Duration, Instant};

use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Backend, Exit, Guest, Prot, Reg, SegmentSetup, TrapAction, TrapHandler};
use unicorn_engine::unicorn_const::{Arch, Mode, Prot as UcProt};
use unicorn_engine::{RegisterX86, Unicorn};

const CODE: u32 = 0x0040_0000;
const THUNKS: u32 = 0x0050_0000;
const PARK: u32 = THUNKS + 0xff0;
const STACK_TOP: u32 = 0x0020_0000;
const GDT: u32 = 0x0060_0000;

const RET: u32 = THUNKS; // plain ret
const FAST: u32 = THUNKS + 8; // trap, Continue
const BLOCK: u32 = THUNKS + 16; // trap, Park
const DONE: u32 = THUNKS + 24; // trap, Park (end of loop)

struct H;
impl TrapHandler for H {
    fn on_trap(&mut self, cpu: &mut dyn Guest, _v: u8) -> TrapAction {
        match cpu.reg(Reg::Eip) - 2 {
            FAST => {
                cpu.set_reg(Reg::Eax, 1);
                TrapAction::Continue
            }
            _ => TrapAction::Park,
        }
    }
}

fn program(target: u32, n: u32) -> (Vec<u8>, Vec<u8>) {
    let mut t = Asm::new(THUNKS);
    t.ret().pad_to(8, 0xcc);
    for _ in 0..3 {
        t.trap().ret().pad_to(8, 0xcc);
    }
    // loop: push ecx; push 1; call target; add esp,4; pop ecx; dec ecx; jnz loop; call DONE
    let mut a = Asm::new(CODE);
    a.mov_ecx_imm(n);
    let top = a.here();
    a.push_ecx().push_imm(1).call(target).add_esp(4).pop_ecx().dec_ecx().jnz(top).call(DONE);
    (a.bytes, t.bytes)
}

fn setup(target: u32, n: u32) -> UnicornCpu {
    let mut cpu = UnicornCpu::new(PARK).unwrap();
    cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    cpu.map(THUNKS, 0x1000, Prot::RX).unwrap();
    cpu.map(STACK_TOP - 0x10000, 0x10000, Prot::RW).unwrap();
    cpu.map(GDT, 0x1000, Prot::RW).unwrap();
    let (code, thunks) = program(target, n);
    cpu.write(THUNKS, &thunks).unwrap();
    cpu.write(CODE, &code).unwrap();
    cpu.setup_segments(SegmentSetup { gdt_addr: GDT }).unwrap();
    cpu.set_reg(Reg::Esp, STACK_TOP - 0x100);
    cpu.set_reg(Reg::Eip, CODE);
    cpu
}

fn run_backend(cpu: &mut UnicornCpu, target: u32, n: u32) {
    // Non-BLOCK loops park once, at DONE. BLOCK parks n times inside the thunk, then at DONE.
    let parks = if target == BLOCK { n } else { 0 };
    for _ in 0..parks {
        assert_eq!(cpu.run(&mut H), Exit::Parked);
        cpu.set_reg(Reg::Eax, 1);
        cpu.set_reg(Reg::Eip, BLOCK + 2);
    }
    assert_eq!(cpu.run(&mut H), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Ecx), 0, "loop did not finish");
}

fn bench_backend(name: &str, target: u32, n: u32, enabled: bool) -> f64 {
    let mut cpu = setup(target, n);
    cpu.set_execution_budget(enabled.then_some(Duration::from_secs(5)));
    run_backend(&mut cpu, target, n);
    cpu.set_reg(Reg::Esp, STACK_TOP - 0x100);
    cpu.set_reg(Reg::Eip, CODE);
    let t = Instant::now();
    run_backend(&mut cpu, target, n);
    report(if enabled { "watchdog" } else { "off" }, name, n, t.elapsed())
}

fn run_native(cpu: &mut Unicorn<'_, ()>, target: u32, n: u32, timeout: u64, count: usize) {
    let parks = if target == BLOCK { n } else { 0 };
    let mut pc = u64::from(CODE);
    for _ in 0..parks {
        cpu.emu_start(pc, PARK.into(), timeout, count).unwrap();
        assert_eq!(cpu.reg_read(RegisterX86::EIP).unwrap(), u64::from(PARK));
        cpu.reg_write(RegisterX86::EAX, 1).unwrap();
        pc = u64::from(BLOCK + 2);
    }
    cpu.emu_start(pc, PARK.into(), timeout, count).unwrap();
    assert_eq!(cpu.reg_read(RegisterX86::EIP).unwrap(), u64::from(PARK));
    assert_eq!(cpu.reg_read(RegisterX86::ECX).unwrap(), 0);
}

fn bench_native(mode: &str, name: &str, target: u32, n: u32) -> f64 {
    let mut cpu = Unicorn::new(Arch::X86, Mode::MODE_32).unwrap();
    cpu.mem_map(CODE.into(), 0x1000, UcProt::READ | UcProt::EXEC).unwrap();
    cpu.mem_map(THUNKS.into(), 0x1000, UcProt::READ | UcProt::EXEC).unwrap();
    cpu.mem_map((STACK_TOP - 0x10000).into(), 0x10000, UcProt::READ | UcProt::WRITE).unwrap();
    // Match the production backend's flat segment descriptors (including SS.D/B).
    // Unicorn's initial segments otherwise produce different call/ret timings.
    let reference = setup(target, n);
    let mut gdt = [0; 128];
    reference.read(GDT, &mut gdt).unwrap();
    cpu.mem_map(GDT.into(), 0x1000, UcProt::READ | UcProt::WRITE).unwrap();
    cpu.mem_write(GDT.into(), &gdt).unwrap();
    let mut mmr = [0; 24];
    mmr[8..16].copy_from_slice(&u64::from(GDT).to_le_bytes());
    mmr[16..20].copy_from_slice(&127u32.to_le_bytes());
    cpu.reg_write_long(RegisterX86::GDTR, &mmr).unwrap();
    for (reg, selector) in [
        (RegisterX86::SS, 0x30),
        (RegisterX86::CS, 0x23),
        (RegisterX86::DS, 0x2b),
        (RegisterX86::ES, 0x2b),
        (RegisterX86::FS, 0x53),
    ] {
        cpu.reg_write(reg, selector).unwrap();
    }
    let (code, thunks) = program(target, n);
    cpu.mem_write(CODE.into(), &code).unwrap();
    cpu.mem_write(THUNKS.into(), &thunks).unwrap();
    cpu.add_intr_hook(|cpu, _| {
        if cpu.reg_read(RegisterX86::EIP).unwrap() - 2 == u64::from(FAST) {
            cpu.reg_write(RegisterX86::EAX, 1).unwrap();
        } else {
            cpu.set_pc(PARK.into()).unwrap();
        }
    })
    .unwrap();
    let timeout = if mode == "timeout" { 5_000_000 } else { 0 };
    let count = if mode == "count" { 1_000_000_000 } else { 0 };
    cpu.reg_write(RegisterX86::ESP, (STACK_TOP - 0x100).into()).unwrap();
    run_native(&mut cpu, target, n, timeout, count);
    cpu.reg_write(RegisterX86::ESP, (STACK_TOP - 0x100).into()).unwrap();
    let t = Instant::now();
    run_native(&mut cpu, target, n, timeout, count);
    report(mode, name, n, t.elapsed())
}

fn report(mode: &str, name: &str, n: u32, elapsed: Duration) -> f64 {
    let ns = elapsed.as_nanos() as f64 / f64::from(n);
    println!("{mode:12} {name:9} {n:>9} calls  {ns:10.1} ns/call");
    ns
}

fn main() {
    let group = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    assert!(["all", "watchdog", "native"].contains(&group.as_str()), "all|watchdog|native [samples]");
    let samples: usize = std::env::args().nth(2).map(|s| s.parse().unwrap()).unwrap_or(5);
    assert!(samples > 0);
    let mut results = std::collections::BTreeMap::<(String, &str), Vec<f64>>::new();
    for sample in 0..samples {
        println!("sample {}/{samples}", sample + 1);
        for (name, target, n) in [("baseline", RET, 10_000_000), ("fast", FAST, 1_000_000), ("park", BLOCK, 200_000)] {
            if group != "native" {
                // Alternate pair order to reduce systematic thermal/frequency bias.
                for enabled in if sample % 2 == 0 { [false, true] } else { [true, false] } {
                    let ns = bench_backend(name, target, n, enabled);
                    results.entry((if enabled { "watchdog" } else { "off" }.into(), name)).or_default().push(ns);
                }
            }
            if group != "watchdog" {
                for mode in ["native-off", "timeout", "count"] {
                    let n = if target == BLOCK && mode == "timeout" { 100 } else { n };
                    let ns = bench_native(mode, name, target, n);
                    results.entry((mode.into(), name)).or_default().push(ns);
                }
            }
        }
    }
    println!("\nmedian [min, max], ns/call; {samples} samples (warm-up excluded)");
    for ((mode, name), mut values) in results {
        values.sort_by(f64::total_cmp);
        println!("{mode:12} {name:9} {:10.1} [{:.1}, {:.1}]", values[samples / 2], values[0], values[samples - 1]);
    }
}
