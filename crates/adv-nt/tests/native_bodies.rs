//! Native bodies against Wine's own functions (NAT-BODY-1).
//!
//! Two machines map the distribution's ntdll.dll and bind its bodies; the
//! reference then drops its redirects, so it runs Wine's code. The same
//! random calls must leave the same memory, return registers and stack in
//! both, and a call that faults must fault at the same place after copying
//! the same bytes. `ADV_WINE_DIST` names the distribution; without it the
//! test is skipped.

#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use std::path::{Path, PathBuf};

use adv_cpu::Cpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
use adv_pe::Pe;

const CODE: u32 = 0x0040_0000;
/// The function's address, then its arguments, then what the call left in
/// EAX, EDX and ESP.
const SLOT: u32 = 0x0041_0000;
const ARGS: u32 = SLOT + 4;
const RESULT: u32 = SLOT + 16;
const DATA: u32 = 0x0050_0000;
const DATA_LEN: u32 = 0x1_0000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

fn machine(dll: &Path, image: &[u8], function: &str) -> Machine<Cpu> {
    let mut machine = Machine::new(
        Cpu::new(PARK_ADDR, true).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    let pe = Pe::parse(image).unwrap();
    let mut memory = adv_nt::memory::Memory::default();
    adv_nt::image::map(
        &mut machine.cpu,
        &mut machine.sys.vm,
        &mut memory,
        &pe,
        pe.image_base,
    )
    .unwrap();
    adv_nt::identity::note_ntdll(&mut machine, dll, image);
    let (_, entry) = pe
        .exports()
        .unwrap()
        .into_iter()
        .find_map(|export| match (export.name, export.target) {
            (Some(name), adv_pe::ExportTarget::Rva(rva)) if name == function => {
                Some((name, pe.image_base + rva))
            }
            _ => None,
        })
        .unwrap();
    machine.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    machine.cpu.map(SLOT, 0x1000, Prot::RW).unwrap();
    machine.cpu.map(DATA, DATA_LEN, Prot::RW).unwrap();
    machine.cpu.write_u32(SLOT, entry).unwrap();
    let mut code = Vec::new();
    for offset in [8, 4, 0] {
        code.extend_from_slice(&[0xff, 0x35]);
        code.extend_from_slice(&(ARGS + offset).to_le_bytes());
    }
    code.extend_from_slice(&[0xff, 0x15]);
    code.extend_from_slice(&SLOT.to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, 0x0c, 0xa3]);
    code.extend_from_slice(&RESULT.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x15]);
    code.extend_from_slice(&(RESULT + 4).to_le_bytes());
    code.extend_from_slice(&[0x89, 0x25]);
    code.extend_from_slice(&(RESULT + 8).to_le_bytes());
    code.push(0xc3);
    machine.cpu.write(CODE, &code).unwrap();
    machine
}

/// Runs `function(args)` on `data`; returns the data, EAX, EDX and ESP,
/// or the failure.
fn call(machine: &mut Machine<Cpu>, data: &[u8], args: [u32; 3]) -> Result<Vec<u32>, String> {
    machine.cpu.write(DATA, data).unwrap();
    for (index, arg) in args.into_iter().enumerate() {
        machine.cpu.write_u32(ARGS + 4 * index as u32, arg).unwrap();
    }
    machine.spawn("call", CODE, &[], 0x10000).unwrap();
    match machine.run() {
        RunResult::Exited(_) => {}
        RunResult::Failed(error) => return Err(format!("{:#x} {}", error.pc, error.message)),
        other => return Err(format!("{other:?}")),
    }
    let mut words = vec![0; DATA_LEN as usize / 4];
    for (index, word) in words.iter_mut().enumerate() {
        *word = machine.cpu.read_u32(DATA + 4 * index as u32).unwrap();
    }
    for offset in [0, 4, 8] {
        words.push(machine.cpu.read_u32(RESULT + offset).unwrap());
    }
    Ok(words)
}

// spec: NAT-BODY-1
#[test]
fn ntdll_memcpy_matches_wine() {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return;
    };
    let dll = dist.join("lib/wine/i386-windows/ntdll.dll");
    let image = std::fs::read(&dll).unwrap();
    let mut native = machine(&dll, &image, "memcpy");
    let mut reference = machine(&dll, &image, "memcpy");
    assert!(
        !native.sys.redirects.is_empty(),
        "memcpy is not bound: is the identity table installed?"
    );
    reference.sys.redirects.clear();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut data = vec![0; DATA_LEN as usize];
    const CASES: u32 = 300;
    for case in 0..CASES {
        data.iter_mut().for_each(|byte| *byte = rng.next() as u8);
        let n = match case % 4 {
            0 => rng.below(16),
            1 => rng.below(300),
            _ => rng.below(DATA_LEN / 2),
        };
        let src = DATA + rng.below(DATA_LEN - n + 1);
        // Overlapping both ways, and apart.
        let dst = match case % 3 {
            0 => src.saturating_sub(rng.below(64)).max(DATA),
            1 => (src + rng.below(64)).min(DATA + DATA_LEN - n),
            _ => DATA + rng.below(DATA_LEN - n + 1),
        };
        let args = [dst, src, n];
        assert_eq!(
            call(&mut native, &data, args),
            call(&mut reference, &data, args),
            "memcpy({dst:#x}, {src:#x}, {n:#x})"
        );
    }
    assert!(
        native.sys.host_calls > u64::from(CASES / 2),
        "the native body ran {} times",
        native.sys.host_calls
    );
    assert_eq!(reference.sys.host_calls, 0);
    // A source that runs into an unmapped page: the body declines, and
    // Wine's code copies up to the fault.
    let args = [DATA, DATA + DATA_LEN - 100, 500];
    let (faulted, expected) = (
        call(&mut native, &data, args),
        call(&mut reference, &data, args),
    );
    assert!(faulted.is_err(), "{faulted:?}");
    assert_eq!(
        adv_nt::native::declines(&native.ext),
        1,
        "the body declined"
    );
    assert_eq!(faulted, expected);
    let mut copied = [0; 100];
    native.cpu.read(DATA, &mut copied).unwrap();
    assert_eq!(copied[..], data[DATA_LEN as usize - 100..]);
}
