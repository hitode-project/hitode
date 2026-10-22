use super::*;
use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = DATA + 0x800;
const DISP: u32 = OUT + 4;
const MARKER: u32 = 0x55555555;
fn guarded<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/registry-transactions-tests"),
    )
    .unwrap();
    m.ext.insert(adv_os::registry::VirtualRegistry::default());
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    const NAME: u32 = DATA + 0x900;
    m.cpu
        .write(
            NAME,
            &"AuthoredTransactionGuard\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let create = m.resolve_import("advapi32.dll", "RegCreateKeyExW").unwrap();
    let query = m.resolve_import("advapi32.dll", "RegOpenKeyExW").unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    for arg in [0, DATA + 0x880, 0, 0xf003f, 0, 0, 0, NAME, 0x80000001] {
        code.push_imm(arg);
    }
    code.call(create).mov_mem_eax(DATA + 0x300);
    let mut expected = Vec::new();
    for transaction in [0u32, u32::MAX, 0x55555555] {
        for (api, args, result, key) in [
            (
                "RegOpenKeyTransactedW",
                vec![0x80000001, 0, 0, 0x20019, OUT, transaction, 0],
                if transaction == 0 || transaction == u32::MAX {
                    6700
                } else {
                    0
                },
                if transaction == 0 || transaction == u32::MAX {
                    MARKER
                } else {
                    0x80000001
                },
            ),
            (
                "RegOpenKeyTransactedW",
                vec![0x80000001, NAME, 0, 0x20019, OUT, transaction, 0],
                if transaction == 0 || transaction == u32::MAX {
                    6700
                } else {
                    6
                },
                if transaction == 0 || transaction == u32::MAX {
                    MARKER
                } else {
                    0
                },
            ),
            (
                "RegCreateKeyTransactedW",
                vec![
                    0x80000001,
                    NAME,
                    0,
                    0,
                    0,
                    0x20019,
                    0,
                    OUT,
                    DISP,
                    transaction,
                    0,
                ],
                if transaction == 0 || transaction == u32::MAX {
                    6700
                } else {
                    6
                },
                if transaction == 0 || transaction == u32::MAX {
                    MARKER
                } else {
                    0
                },
            ),
            (
                "RegDeleteKeyTransactedW",
                vec![0x80000001, NAME, 0, 0, transaction, 0],
                if transaction == 0 || transaction == u32::MAX {
                    6700
                } else {
                    6
                },
                MARKER,
            ),
        ] {
            code.mov_mem_imm(OUT, MARKER)
                .mov_mem_imm(DISP, MARKER)
                .push_imm(MARKER)
                .call(set_error);
            for arg in args.iter().rev() {
                code.push_imm(*arg);
            }
            let row = DATA + expected.len() as u32 * 16;
            code.call(m.resolve_import("advapi32.dll", api).unwrap())
                .mov_mem_eax(row)
                .mov_eax_mem(OUT)
                .mov_mem_eax(row + 4)
                .mov_eax_mem(DISP)
                .mov_mem_eax(row + 8)
                .call(get_error)
                .mov_mem_eax(row + 12);
            expected.push((result, key));
        }
    }
    for arg in [DATA + 0x884, 1, 0, NAME, 0x80000001] {
        code.push_imm(arg);
    }
    code.call(query).mov_mem_eax(DATA + 0x304);
    code.mov_mem_imm(DATA + 0x308, 0)
        .mov_eax_mem(DATA + 0x308)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("transaction guards", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, (result, key)) in expected.into_iter().enumerate() {
        let row = DATA + index as u32 * 16;
        assert_eq!(m.cpu.read_u32(row).unwrap(), result, "row {index}");
        assert_eq!(m.cpu.read_u32(row + 4).unwrap(), key, "row {index}");
        assert_eq!(m.cpu.read_u32(row + 8).unwrap(), MARKER);
        assert_eq!(m.cpu.read_u32(row + 12).unwrap(), MARKER);
    }
    assert_eq!(m.cpu.read_u32(DATA + 0x300).unwrap(), 0);
    assert_eq!(
        m.cpu.read_u32(DATA + 0x304).unwrap(),
        0,
        "failed transactions retain the key"
    );
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: SYS-REG-1
fn invalid_transactions_preserve_keys_outputs_and_cached_root_behavior_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        guarded(cpu);
    }
    guarded(UnicornCpu::new(PARK_ADDR).unwrap());
}
