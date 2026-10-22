//! Legacy and Point32 extents share selected fonts, encoding and output checks.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
#[path = "support/test_cpu.rs"]
mod test_cpu;
use test_cpu::TestCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn extents<B: TestCpu>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gdi-extent-tests");
    Win32::install(&mut m, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.write(DATA + 512, b"A\0V").unwrap();
    m.cpu.write(DATA + 544, &[65, 0, 0, 0, 86, 0]).unwrap();
    m.cpu.write(DATA + 4096, &[0xcc; 8]).unwrap();
    m.cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0)
        .call(m.resolve_import("gdi32.dll", "CreateCompatibleDC").unwrap())
        .mov_mem_eax(DATA);
    let mut outputs = Vec::new();
    let mut statuses = Vec::new();
    let mut next = DATA + 32;
    for (name, pointer) in [
        ("GetTextExtentPointA", DATA + 512),
        ("GetTextExtentPoint32A", DATA + 512),
        ("GetTextExtentPointW", DATA + 544),
        ("GetTextExtentPoint32W", DATA + 544),
    ] {
        let output = next + 8;
        code.push_imm(output)
            .push_imm(3)
            .push_imm(pointer)
            .mov_eax_mem(DATA)
            .push_eax()
            .call(m.resolve_import("gdi32.dll", name).unwrap())
            .mov_mem_eax(next);
        outputs.push(output);
        statuses.push((next, 1));
        next += 24;
        code.push_imm(DATA + 4096)
            .push_imm(3)
            .push_imm(pointer)
            .mov_eax_mem(DATA)
            .push_eax()
            .call(m.resolve_import("gdi32.dll", name).unwrap())
            .mov_mem_eax(next);
        statuses.push((next, 0));
        next += 4;
    }
    code.push_imm(0x4567)
        .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
    code.push_imm(DATA + 4096)
        .push_imm(u32::MAX)
        .push_imm(u32::MAX)
        .mov_eax_mem(DATA)
        .push_eax()
        .call(
            m.resolve_import("gdi32.dll", "GetSystemPaletteEntries")
                .unwrap(),
        )
        .mov_mem_eax(next)
        .call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(next + 4);
    code.push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .mov_eax_mem(DATA)
        .push_eax()
        .call(
            m.resolve_import("gdi32.dll", "GetSystemPaletteEntries")
                .unwrap(),
        )
        .mov_mem_eax(next + 8)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.cpu.code_region(CODE, code.bytes.len());
    m.spawn("font extent aliases", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(_)), "{result:?}");
    let size = [
        m.cpu.read_u32(outputs[0]).unwrap(),
        m.cpu.read_u32(outputs[0] + 4).unwrap(),
    ];
    assert!(size[0] > 0 && size[1] > 0);
    for output in outputs {
        assert_eq!(
            [
                m.cpu.read_u32(output).unwrap(),
                m.cpu.read_u32(output + 4).unwrap()
            ],
            size
        );
    }
    for (status, expected) in statuses {
        assert_eq!(m.cpu.read_u32(status).unwrap(), expected);
    }
    assert_eq!(m.cpu.read_u32(next).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(next + 4).unwrap(), 0x4567);
    assert_eq!(m.cpu.read_u32(next + 8).unwrap(), 0);
    let mut unchanged = [0; 8];
    m.cpu.read(DATA + 4096, &mut unchanged).unwrap();
    assert_eq!(unchanged, [0xcc; 8]);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}
#[test]
// spec: WIN32-GDI-1 WIN32-GDI-10
fn counted_legacy_and_point32_extents_match_and_true_color_has_no_system_palette() {
    extents(JitCpu::new(PARK_ADDR, false).unwrap());
    extents(JitCpu::new(PARK_ADDR, true).unwrap());
    extents(UnicornCpu::new(PARK_ADDR).unwrap());
}
