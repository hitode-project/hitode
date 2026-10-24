//! Native performance counter against Wine's real system call stub.

#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use std::path::{Path, PathBuf};

use adv_cpu::Cpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::{self, Platform};
use adv_pe::Pe;

const CODE: u32 = 0x0040_0000;
const SLOT: u32 = 0x0041_0000;
const ARGS: u32 = SLOT + 4;
const RESULT: u32 = SLOT + 16;
const DATA: u32 = 0x0050_0000;

fn machine(dll: &Path, image: &[u8]) -> Machine<Cpu> {
    let mut machine = Machine::new(
        Cpu::new(PARK_ADDR, true).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    let pe = Pe::parse(image).unwrap();
    adv_nt::image::map(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut adv_nt::memory::Memory::default(),
        &pe,
        pe.image_base,
    )
    .unwrap();
    adv_nt::identity::note_ntdll(&mut machine, dll, image);
    let export = |name| adv_nt::process::module_export(&machine.cpu, pe.image_base, name).unwrap();
    let entry = export("NtQueryPerformanceCounter");
    let dispatcher = export(dispatch::SYSCALL);
    let mut bytes = [0; 16];
    machine.cpu.read(entry, &mut bytes).unwrap();
    assert!(
        machine.sys.redirects.contains_key(&entry),
        "NtQueryPerformanceCounter is not bound; stub bytes: {bytes:02x?}"
    );
    let syscall = machine
        .resolve_import(dispatch::MODULE, dispatch::SYSCALL)
        .unwrap();
    machine.cpu.write_u32(dispatcher, syscall).unwrap();
    for (address, prot) in [(CODE, Prot::RX), (SLOT, Prot::RW), (DATA, Prot::RW)] {
        machine.cpu.map(address, 0x1000, prot).unwrap();
    }
    machine.cpu.write_u32(SLOT, entry).unwrap();
    let mut code = vec![0x89, 0x25]; // mov [result + 4], esp
    code.extend_from_slice(&(RESULT + 4).to_le_bytes());
    for offset in [4, 0] {
        code.extend_from_slice(&[0xff, 0x35]);
        code.extend_from_slice(&(ARGS + offset).to_le_bytes());
    }
    code.extend_from_slice(&[0xff, 0x15]); // call [slot]
    code.extend_from_slice(&SLOT.to_le_bytes());
    code.push(0xa3); // mov [result], eax
    code.extend_from_slice(&RESULT.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x25]); // mov [result + 8], esp
    code.extend_from_slice(&(RESULT + 8).to_le_bytes());
    code.push(0xc3);
    machine.cpu.write(CODE, &code).unwrap();
    machine
}

fn call(machine: &mut Machine<Cpu>, counter: u32, frequency: u32) -> [u32; 7] {
    machine.cpu.write(DATA, &[0xcc; 16]).unwrap();
    machine.cpu.write_u32(ARGS, counter).unwrap();
    machine.cpu.write_u32(ARGS + 4, frequency).unwrap();
    machine.spawn("call", CODE, &[], 0x10000).unwrap();
    let result = machine.run();
    assert!(matches!(result, RunResult::Exited(_)), "{result:?}");
    [
        RESULT,
        RESULT + 4,
        RESULT + 8,
        DATA,
        DATA + 4,
        DATA + 8,
        DATA + 12,
    ]
    .map(|address| machine.cpu.read_u32(address).unwrap())
}

// spec: NAT-BODY-2
#[test]
fn performance_counter_matches_wine() {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return;
    };
    let dll = dist.join("lib/wine/i386-windows/ntdll.dll");
    let image = std::fs::read(&dll).unwrap();
    let mut native = machine(&dll, &image);
    let mut reference = machine(&dll, &image);
    reference.sys.redirects.clear();
    const CASES: u32 = 300;
    for case in 0..CASES {
        let now = 1_000_000 + u64::from(case) * 12_345_678;
        native.sys.clock.advance_to(now);
        reference.sys.clock.advance_to(now);
        let frequency = if case % 2 == 0 { DATA + 8 } else { 0 };
        let got = call(&mut native, DATA, frequency);
        assert_eq!(got, call(&mut reference, DATA, frequency));
        assert_eq!(got[0], 0);
        assert_eq!(got[1], got[2], "stdcall restores ESP");
        assert_eq!(
            u64::from(got[3]) | u64::from(got[4]) << 32,
            native.sys.clock.now_us() * 10
        );
        assert_eq!(
            got[5..],
            if frequency == 0 {
                [0xcccc_cccc; 2]
            } else {
                [10_000_000, 0]
            }
        );
    }
    let syscalls = |machine: &Machine<Cpu>| {
        adv_nt::counters::Boundaries::of(&machine.ext)
            .syscalls
            .values()
            .map(|tally| tally.calls)
            .sum::<u64>()
    };
    assert!(native.sys.host_calls > u64::from(CASES / 2));
    assert!(
        syscalls(&native) < u64::from(CASES / 2),
        "the body bypasses system calls"
    );
    assert_eq!(syscalls(&reference), u64::from(CASES));
    for (counter, frequency) in [
        (0, DATA + 8),
        (DATA + 0x1000, DATA + 8),
        (DATA, DATA + 0x1000),
    ] {
        let abandons = native.sys.stats.leaf_abandons;
        let got = call(&mut native, counter, frequency);
        assert_eq!(got, call(&mut reference, counter, frequency));
        assert_eq!(got[0], adv_nt::status::ACCESS_VIOLATION);
        assert_eq!(got[1], got[2]);
        assert_eq!(native.sys.stats.leaf_abandons, abandons + 1);
    }
}
