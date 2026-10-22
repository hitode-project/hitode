//! Legacy enumeration and extended deletion operate on the existing A/W/NT tree.
use super::*;
use adv_abi::{ApiDiagnostics, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    L(u32),
}
use Arg::{I, L};
fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[Arg], row: u32) {
    let module = if name.starts_with("Nt") {
        "ntdll.dll"
    } else {
        "advapi32.dll"
    };
    let api = m.resolve_import(module, name).unwrap();
    for arg in args.iter().rev() {
        match arg {
            I(v) => {
                code.push_imm(*v);
            }
            L(p) => {
                code.mov_eax_mem(*p).push_eax();
            }
        }
    }
    code.call(api).mov_mem_eax(row);
}
fn shared<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        crate::Win32Config::new(".", "fixture.exe", "target/registry-closure-tests"),
    )
    .unwrap();
    m.ext.insert(VirtualRegistry::default());
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    const PARENT: u32 = DATA + 0x800;
    const ALPHA: u32 = PARENT + 4;
    const BETA: u32 = PARENT + 8;
    const JAPANESE: u32 = PARENT + 12;
    const NATIVE: u32 = PARENT + 16;
    const ROOT_NAME: u32 = DATA + 0x1000;
    const ALPHA_NAME: u32 = DATA + 0x1100;
    const BETA_NAME: u32 = DATA + 0x1140;
    const JP_NAME: u32 = DATA + 0x1200;
    const A_U16: u32 = DATA + 0x1240;
    const DESCRIPTOR: u32 = DATA + 0x1280;
    const ATTRIBUTES: u32 = DATA + 0x1290;
    const OUT_A: u32 = DATA + 0x1400;
    const OUT_B: u32 = DATA + 0x1500;
    const OUT_JP: u32 = DATA + 0x1600;
    const OUT_EMPTY: u32 = DATA + 0x1700;
    m.cpu
        .write(ROOT_NAME, b"Software\\Authored\\ClosureRegistry\0")
        .unwrap();
    m.cpu.write(ALPHA_NAME, b"Alpha\0").unwrap();
    for (at, text) in [(BETA_NAME, "Beta"), (JP_NAME, "あ"), (A_U16, "Alpha")] {
        m.cpu
            .write(
                at,
                &text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    m.cpu.write(DESCRIPTOR, &[10, 0, 12, 0]).unwrap();
    m.cpu.write_u32(DESCRIPTOR + 4, A_U16).unwrap();
    m.cpu.write_u32(ATTRIBUTES, 24).unwrap();
    m.cpu.write_u32(ATTRIBUTES + 8, DESCRIPTOR).unwrap();
    m.cpu.write_u32(ATTRIBUTES + 12, 0x40).unwrap();
    for at in [OUT_A, OUT_B, OUT_JP, OUT_EMPTY] {
        m.cpu.write(at, &[0xcd; 64]).unwrap();
    }
    let mut code = Asm::new(CODE);
    code.push_imm(0x12345678)
        .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
    call(
        &mut m,
        &mut code,
        "RegCreateKeyExA",
        &[
            I(0x80000001),
            I(ROOT_NAME),
            I(0),
            I(0),
            I(0),
            I(0xf003f),
            I(0),
            I(PARENT),
            I(0),
        ],
        DATA,
    );
    call(
        &mut m,
        &mut code,
        "RegCreateKeyExA",
        &[
            L(PARENT),
            I(ALPHA_NAME),
            I(0),
            I(0),
            I(0),
            I(0xf003f),
            I(0),
            I(ALPHA),
            I(0),
        ],
        DATA + 4,
    );
    for (name, out, result) in [(BETA_NAME, BETA, 8), (JP_NAME, JAPANESE, 12)] {
        call(
            &mut m,
            &mut code,
            "RegCreateKeyExW",
            &[
                L(PARENT),
                I(name),
                I(0),
                I(0),
                I(0),
                I(0xf003f),
                I(0),
                I(out),
                I(0),
            ],
            DATA + result,
        );
    }
    code.mov_eax_mem(PARENT).mov_mem_eax(ATTRIBUTES + 4);
    call(
        &mut m,
        &mut code,
        "NtOpenKey",
        &[I(NATIVE), I(1), I(ATTRIBUTES)],
        DATA + 16,
    );
    call(
        &mut m,
        &mut code,
        "RegEnumKeyA",
        &[L(PARENT), I(0), I(OUT_EMPTY), I(1)],
        DATA + 20,
    );
    call(
        &mut m,
        &mut code,
        "RegEnumKeyA",
        &[L(PARENT), I(0), I(OUT_A), I(64)],
        DATA + 24,
    );
    call(
        &mut m,
        &mut code,
        "RegDeleteKeyExA",
        &[L(PARENT), I(ALPHA_NAME), I(0x200), I(1)],
        DATA + 28,
    );
    call(
        &mut m,
        &mut code,
        "RegDeleteKeyExA",
        &[L(PARENT), I(ALPHA_NAME), I(0x300), I(0)],
        DATA + 32,
    );
    call(
        &mut m,
        &mut code,
        "RegDeleteKeyExA",
        &[L(PARENT), I(ALPHA_NAME), I(0x200), I(0)],
        DATA + 36,
    );
    call(&mut m, &mut code, "RegFlushKey", &[L(ALPHA)], DATA + 40);
    call(&mut m, &mut code, "RegFlushKey", &[L(NATIVE)], DATA + 44);
    call(&mut m, &mut code, "RegCloseKey", &[L(ALPHA)], DATA + 48);
    call(&mut m, &mut code, "RegCloseKey", &[L(NATIVE)], DATA + 52);
    call(
        &mut m,
        &mut code,
        "RegEnumKeyA",
        &[L(PARENT), I(0), I(OUT_B), I(64)],
        DATA + 56,
    );
    call(
        &mut m,
        &mut code,
        "RegDeleteKeyExW",
        &[L(PARENT), I(BETA_NAME), I(0), I(0)],
        DATA + 60,
    );
    call(
        &mut m,
        &mut code,
        "RegEnumKeyA",
        &[L(PARENT), I(0), I(OUT_JP), I(64)],
        DATA + 64,
    );
    call(
        &mut m,
        &mut code,
        "RegDeleteKeyExW",
        &[L(PARENT), I(JP_NAME), I(0x200), I(0)],
        DATA + 68,
    );
    call(
        &mut m,
        &mut code,
        "RegEnumKeyA",
        &[L(PARENT), I(0), I(OUT_EMPTY), I(64)],
        DATA + 72,
    );
    code.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(DATA + 76);
    code.mov_mem_imm(DATA + 80, 0).mov_eax_mem(DATA + 80).ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("registry closure", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (offset, value) in [
        (0, 0),
        (4, 0),
        (8, 0),
        (12, 0),
        (16, 0),
        (20, 234),
        (24, 0),
        (28, 87),
        (32, 87),
        (36, 0),
        (40, 1018),
        (44, 1018),
        (48, 0),
        (52, 0),
        (56, 0),
        (60, 0),
        (64, 0),
        (68, 0),
        (72, 259),
        (76, 0x12345678),
    ] {
        assert_eq!(
            m.cpu.read_u32(DATA + offset).unwrap(),
            value,
            "offset {offset}"
        );
    }
    for (at, expected) in [
        (OUT_A, &b"Alpha\0"[..]),
        (OUT_B, &b"Beta\0"[..]),
        (OUT_JP, &[0x82, 0xa0, 0][..]),
    ] {
        let mut bytes = vec![0; expected.len()];
        m.cpu.read(at, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    let mut bytes = [0; 64];
    m.cpu.read(OUT_EMPTY, &mut bytes).unwrap();
    assert_eq!(bytes, [0xcd; 64]);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: SYS-REG-1
fn legacy_names_and_extended_deletion_share_a_w_nt_owners_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        shared(cpu);
    }
    shared(UnicornCpu::new(PARK_ADDR).unwrap());
}
