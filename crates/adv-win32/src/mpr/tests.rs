use super::*;
use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const WORKER: u32 = CODE + 0x3000;
const DATA: u32 = 0x410000;
const ENUM: u32 = DATA + 0x800;
const COUNT: u32 = DATA + 0x804;
const LENGTH: u32 = DATA + 0x808;
const USER_LENGTH: u32 = DATA + 0x810;
const PROVIDER_LENGTH: u32 = DATA + 0x814;
const SHORT: u32 = DATA + 0x818;
const ERROR: u32 = DATA + 0x830;
const WORKER_ERROR: u32 = DATA + 0x840;
const THREAD: u32 = DATA + 0x854;
const CANARY: u32 = DATA + 0x3c00;
const MARKER: u32 = 0x12345678;
#[derive(Clone, Copy)]
enum Arg {
    V(u32),
    At(u32),
}
fn profile<B: Backend>(cpu: B, capabilities: bool) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let registered = registry
        .registered()
        .filter(|(module, _, _)| *module == MPR)
        .map(|(_, name, _)| name.to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    for declaration in signature::signatures()
        .iter()
        .filter(|declaration| declaration.module == MPR)
    {
        assert!(
            registered.contains(declaration.name),
            "{}",
            declaration.name
        );
    }
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/mpr-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    m.cpu.write(DATA + 0x3000, &[0xcd; 4096]).unwrap();
    for (offset, value) in [
        (COUNT, u32::MAX),
        (LENGTH, 256),
        (USER_LENGTH, 64),
        (PROVIDER_LENGTH, 64),
        (SHORT, 1),
    ] {
        m.cpu.write_u32(offset, value).unwrap();
    }
    for (index, wide) in [false, true].into_iter().enumerate() {
        for (offset, text) in [
            (0x1000, r"\\offline.invalid\share"),
            (0x1200, "Microsoft Windows Network"),
            (0x1400, "Z:"),
            (0x1600, r"C:\Game\file.bin"),
            (0x1800, "説明 text"),
            (0x1a00, "Authored provider"),
            (0x1c00, "unknown provider"),
        ] {
            let bytes = if wide {
                text.encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            } else {
                let mut bytes = encoding_rs::SHIFT_JIS.encode(text).0.into_owned();
                bytes.push(0);
                bytes
            };
            m.cpu
                .write(DATA + offset + index as u32 * 0x100, &bytes)
                .unwrap();
        }
        let delta = index as u32 * 0x80;
        let fields = [
            2u32,
            1,
            3,
            3,
            0,
            DATA + 0x1000 + index as u32 * 0x100,
            0,
            DATA + 0x1200 + index as u32 * 0x100,
        ];
        m.cpu
            .write(
                DATA + 0x2000 + delta,
                &fields
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        m.cpu
            .write(
                DATA + 0x2100 + delta,
                &[20u32, 0, DATA + 0x2000 + delta, 0, 0]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        m.cpu
            .write(
                DATA + 0x2200 + delta,
                &[20u32, 0, DATA + 0x1400 + index as u32 * 0x100, 0, 0]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    m.cpu.write_u32(DATA + 0x2400, 32).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let set_provider_error = m.resolve_import(MPR, "WNetSetLastErrorW").unwrap();
    let get_provider_error = m.resolve_import(MPR, "WNetGetLastErrorA").unwrap();
    let mut worker = Asm::new(WORKER);
    worker
        .push_imm(MARKER)
        .call(set_error)
        .push_imm(DATA + 0x1b00)
        .push_imm(DATA + 0x1900)
        .push_imm(0x202)
        .call(set_provider_error);
    for arg in [128u32, DATA + 0x3b00, 128, DATA + 0x3a00, WORKER_ERROR] {
        worker.push_imm(arg);
    }
    worker
        .call(get_provider_error)
        .mov_mem_eax(DATA + 0x844)
        .call(get_error)
        .mov_mem_eax(DATA + 0x848)
        .mov_mem_imm(DATA + 0x84c, 0)
        .mov_eax_mem(DATA + 0x84c)
        .ret_n(4);
    m.cpu.write(WORKER, &worker.bytes).unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    use Arg::{At, V};
    let mut call = |name: &str, args: &[Arg], value: u32| {
        let module = if name.starts_with("WNet") || name.starts_with("Multinet") {
            MPR
        } else {
            "kernel32.dll"
        };
        let declaration = signature::lookup(module, name).unwrap();
        code.push_imm(MARKER).call(set_error);
        for arg in args.iter().rev() {
            match arg {
                V(value) => {
                    code.push_imm(*value);
                }
                At(pointer) => {
                    code.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
        let function = m.resolve_import(module, name).unwrap();
        let row = DATA + expected.len() as u32 * 8;
        code.call(function).mov_mem_eax(row);
        if name == "CreateThread" {
            code.mov_mem_eax(THREAD);
        }
        code.call(get_error).mov_mem_eax(row + 4);
        expected.push((
            name.to_owned(),
            value,
            declaration.return_kind != signature::AbiKind::Void && name != "CreateThread",
        ));
    };
    for (index, suffix) in ["A", "W"].into_iter().enumerate() {
        let delta = index as u32 * 0x100;
        let remote = DATA + 0x1000 + delta;
        let provider = DATA + 0x1200 + delta;
        let drive = DATA + 0x1400 + delta;
        let local = DATA + 0x1600 + delta;
        let res = DATA + 0x2000 + index as u32 * 0x80;
        let name = |base: &str| format!("{base}{suffix}");
        call(
            &name("WNetAddConnection"),
            &[V(remote), V(0), V(drive)],
            1222,
        );
        call(
            &name("WNetAddConnection2"),
            &[V(res), V(0), V(0), V(0)],
            1222,
        );
        call(
            &name("WNetAddConnection3"),
            &[V(0), V(res), V(0), V(0), V(0)],
            1222,
        );
        call(
            &name("WNetAddConnection4"),
            &[V(0), V(res), V(0), V(0), V(0), V(0), V(0)],
            1222,
        );
        call(
            &name("WNetUseConnection"),
            &[
                V(0),
                V(res),
                V(0),
                V(0),
                V(0),
                V(CANARY),
                V(LENGTH),
                V(CANARY + 128),
            ],
            1222,
        );
        call(
            &name("WNetUseConnection4"),
            &[
                V(0),
                V(res),
                V(0),
                V(0),
                V(0),
                V(0),
                V(0),
                V(CANARY),
                V(LENGTH),
                V(CANARY + 128),
            ],
            1222,
        );
        call(&name("WNetCancelConnection"), &[V(drive), V(0)], 2250);
        call(
            &name("WNetCancelConnection2"),
            &[V(drive), V(0), V(0)],
            2250,
        );
        call(
            &name("WNetGetConnection"),
            &[V(drive), V(CANARY), V(LENGTH)],
            2250,
        );
        call(
            &name("WNetGetUniversalName"),
            &[V(local), V(1), V(CANARY), V(LENGTH)],
            2250,
        );
        call(
            &name("WNetGetUniversalName"),
            &[V(local), V(7), V(CANARY), V(LENGTH)],
            124,
        );
        call(
            &name("WNetOpenEnum"),
            &[V(2), V(1), V(0), V(0), V(CANARY)],
            1222,
        );
        call(
            &name("WNetOpenEnum"),
            &[V(if index == 0 { 1 } else { 3 }), V(1), V(0), V(0), V(ENUM)],
            0,
        );
        call(
            &name("WNetEnumResource"),
            &[At(ENUM), V(COUNT), V(CANARY), V(LENGTH)],
            259,
        );
        call("WNetCloseEnum", &[At(ENUM)], 0);
        call("WNetCloseEnum", &[At(ENUM)], 6);
        call(
            &name("WNetEnumResource"),
            &[At(ENUM), V(COUNT), V(CANARY), V(LENGTH)],
            6,
        );
        call(
            &name("WNetGetResourceInformation"),
            &[V(res), V(CANARY), V(LENGTH), V(CANARY + 128)],
            1222,
        );
        call(
            &name("WNetGetResourceParent"),
            &[V(res), V(CANARY), V(LENGTH)],
            1222,
        );
        call(
            &name("MultinetGetConnectionPerformance"),
            &[V(res), V(CANARY)],
            1222,
        );
        call(
            &name("WNetConnectionDialog1"),
            &[V(DATA + 0x2100 + index as u32 * 0x80)],
            1222,
        );
        call(
            &name("WNetDisconnectDialog1"),
            &[V(DATA + 0x2200 + index as u32 * 0x80)],
            1222,
        );
        call(
            &name("WNetGetUser"),
            &[V(0), V(DATA + 0x3800 + index as u32 * 0x80), V(USER_LENGTH)],
            0,
        );
        call(
            &name("WNetGetUser"),
            &[V(drive), V(CANARY), V(LENGTH)],
            2250,
        );
        call(
            &name("WNetGetProviderName"),
            &[
                V(0x0002ffff),
                V(DATA + 0x3000 + index as u32 * 0x200),
                V(PROVIDER_LENGTH),
            ],
            0,
        );
        call(
            &name("WNetGetNetworkInformation"),
            &[V(DATA + 0x1c00 + delta), V(DATA + 0x2400)],
            1204,
        );
        if capabilities {
            call(
                &name("WNetGetNetworkInformation"),
                &[V(provider), V(DATA + 0x2400)],
                50,
            );
        }
    }
    call("WNetConnectionDialog", &[V(0), V(1)], 1222);
    call("WNetDisconnectDialog", &[V(0), V(1)], 1222);
    call("WNetGetUserA", &[V(0), V(CANARY), V(SHORT)], 234);
    call(
        "WNetSetLastErrorA",
        &[V(0x101), V(DATA + 0x1800), V(DATA + 0x1a00)],
        0,
    );
    call(
        "CreateThread",
        &[V(0), V(0), V(WORKER), V(0), V(0), V(0)],
        0,
    );
    call("WaitForSingleObject", &[At(THREAD), V(u32::MAX)], 0);
    call("CloseHandle", &[At(THREAD)], 1);
    call(
        "WNetGetLastErrorA",
        &[V(ERROR), V(DATA + 0x3400), V(128), V(DATA + 0x3600), V(128)],
        0,
    );
    call(
        "WNetGetLastErrorW",
        &[V(ERROR), V(DATA + 0x3500), V(128), V(DATA + 0x3700), V(128)],
        0,
    );
    code.mov_mem_imm(DATA + 0xf00, 0)
        .mov_eax_mem(DATA + 0xf00)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("offline router", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, (name, value, check)) in expected.into_iter().enumerate() {
        if check {
            assert_eq!(
                m.cpu.read_u32(DATA + index as u32 * 8).unwrap(),
                value,
                "{name}"
            );
        }
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(),
            MARKER,
            "{name} LastError"
        );
    }
    assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(LENGTH).unwrap(), 256);
    assert_eq!(m.cpu.read_u32(SHORT).unwrap(), 7);
    let mut canary = [0; 512];
    m.cpu.read(CANARY, &mut canary).unwrap();
    assert_eq!(canary, [0xcd; 512]);
    let read = |at, length| {
        let mut bytes = vec![0; length];
        m.cpu.read(at, &mut bytes).unwrap();
        bytes
    };
    let wide = |text: &str| {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    assert_eq!(read(DATA + 0x3000, 26), b"Microsoft Windows Network\0");
    assert_eq!(read(DATA + 0x3200, 52), wide("Microsoft Windows Network"));
    assert_eq!(read(DATA + 0x3800, 7), b"Player\0");
    assert_eq!(read(DATA + 0x3880, 14), wide("Player"));
    assert_eq!(m.cpu.read_u32(ERROR).unwrap(), 0x101);
    assert_eq!(m.cpu.read_u32(WORKER_ERROR).unwrap(), 0x202);
    assert_eq!(m.cpu.read_u32(DATA + 0x844).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 0x848).unwrap(), MARKER);
    assert_eq!(
        read(DATA + 0x3500, wide("説明 text").len()),
        wide("説明 text")
    );
    assert_eq!(
        read(DATA + 0x3700, wide("Authored provider").len()),
        wide("Authored provider")
    );
    let diagnostics = m.ext.get_or_default::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.acceptance_eligible(RunMode::Strict));
    assert_eq!(
        diagnostics.warnings.values().sum::<u64>(),
        if capabilities { 2 } else { 0 }
    );
}
#[test]
// spec: PERS-NET-3, PERS-NET-4
fn offline_router_owns_local_configuration_and_thread_errors_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        profile(cpu, false);
    }
    profile(UnicornCpu::new(PARK_ADDR).unwrap(), false);
}
#[test]
// spec: PERS-NET-5
fn provider_capability_queries_report_the_missing_provider_bridge() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        profile(cpu, true);
    }
}
