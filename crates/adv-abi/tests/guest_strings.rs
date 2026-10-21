//! Authored guest memory only; no game files.
use adv_abi::{AnsiStr, GuestPtr, WideStr};
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot},
};
use adv_machine::layout::PARK_ADDR;

const PAGE: u32 = 0x1000;
const BASE: u32 = 0x40_0000;

/// One readable page followed by an unmapped page.
fn cpu() -> JitCpu {
    let mut cpu = JitCpu::new(PARK_ADDR, false).unwrap();
    cpu.map(BASE, PAGE, Prot::RW).unwrap();
    cpu
}

/// The exact unit-at-a-time scan the chunked readers must match.
fn units<T: bytemuck::Pod + PartialEq + Default>(
    cpu: &dyn Guest,
    start: u32,
    limit: usize,
) -> Result<Vec<T>, (u32, u32)> {
    let width = size_of::<T>() as u32;
    let mut units = Vec::new();
    for i in 0..limit as u32 {
        let unit = GuestPtr::<T>::new(start + i * width)
            .read(cpu)
            .map_err(|f| (f.addr, f.len))?;
        if unit == T::default() {
            return Ok(units);
        }
        units.push(unit);
    }
    Err((start, limit as u32 * width))
}

#[test]
fn chunked_strings_match_unit_scans_across_page_ends_limits_and_faults() {
    let mut cpu = cpu();
    let end = BASE + PAGE;
    let text: Vec<u8> = (0..PAGE).map(|i| (i % 251 + 1) as u8).collect();
    cpu.write(BASE, &text).unwrap();
    for start in [BASE, BASE + 1, end - 200, end - 3, end - 2, end - 1] {
        for terminator in [None, Some(end - 8), Some(end - 2)] {
            cpu.write(BASE, &text).unwrap();
            if let Some(at) = terminator {
                cpu.write(at, &[0, 0]).unwrap();
            }
            for limit in [0, 1, 5, 64, 65, 3000] {
                let wide = WideStr(GuestPtr::new(start)).read(&cpu, limit);
                assert_eq!(
                    wide.map_err(|f| (f.addr, f.len)),
                    units::<u16>(&cpu, start, limit),
                    "{start:#x} {terminator:?} {limit}"
                );
                let ansi = AnsiStr(GuestPtr::new(start)).read(&cpu, limit);
                assert_eq!(
                    ansi.map_err(|f| (f.addr, f.len)),
                    units::<u8>(&cpu, start, limit)
                );
            }
        }
    }
    assert!(WideStr(GuestPtr::new(0)).read(&cpu, 4).is_err());
}
