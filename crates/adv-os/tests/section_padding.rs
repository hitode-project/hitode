//! Execute authored padding entrypoints through the real PE loader, including
//! the section protection boundary adjacent to oversized raw data.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Process};
use adv_unicorn::UnicornCpu;
#[path = "../../adv-pe/tests/support/section_fixture.rs"]
mod fixture;

struct File(Vec<u8>);
impl FileSource for File {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        path.eq_ignore_ascii_case("fixture.exe")
            .then(|| self.0.clone())
    }
}
fn padding<B: Backend>(cpu: B, virtual_size: u32, raw_size: u32, next: u32, entry: u32) {
    let mut machine = Machine::new(cpu, Registry::default(), Clock::virtual_time()).unwrap();
    let files = File(fixture::image(
        virtual_size,
        raw_size,
        Some(next),
        next + 0x1000,
    ));
    let id = machine.load(&files, "fixture.exe").unwrap();
    let base = adv_os::modules(&machine.ext).get(id).base;
    assert_eq!(base, fixture::BASE);
    assert!(machine.cpu.accessible(base + entry, 6, Prot::EXEC));
    assert!(!machine.cpu.accessible(base + entry, 6, Prot::WRITE));
    assert!(machine.cpu.accessible(base + next + 0x180, 4, Prot::READ));
    assert!(!machine.cpu.accessible(base + next + 0x180, 4, Prot::EXEC));
    assert_eq!(
        machine.cpu.read_u32(base + next + 0x180).unwrap(),
        0x5a5a5a5a
    );
    assert_eq!(machine.cpu.read_u32(base + next + 0x220).unwrap(), 0);
    machine
        .start_main(base, base + entry, 65536, "fixture.exe")
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(0x11223344)),
        "{result:?}"
    );
}

#[test]
fn same_page_padding_executes_without_enlarging_raw_overlap_protection() {
    for (virtual_size, raw_size, next, entry) in [
        (0x101, 0x200, 0x2000, 0x1180),
        (0x101, 0x2000, 0x2000, 0x1180),
        (0, 0x2000, 0x3000, 0x2180),
    ] {
        for jit in [false, true] {
            let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
            cpu.set_async_compilation(false);
            padding(cpu, virtual_size, raw_size, next, entry);
        }
        padding(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            virtual_size,
            raw_size,
            next,
            entry,
        );
    }
}
