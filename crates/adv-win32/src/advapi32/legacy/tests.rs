//! Authored native oracle: target/investigations/katahane-legacy-registry/native.
//! Probe source SHA256: DF6C2EF1C1EB2DC2B71571454908EF8256609E779353719186DFE45EB306C914.
use super::super::{INVALID_HANDLE, INVALID_PARAMETER, SET, VirtualRegistry};
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = 0x420000;
const READ_ONLY: u32 = 0x430000;
const HKCU: u32 = 0x80000001;
const SENTINEL: u32 = 0x12345678;

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["RegQueryValueA", "RegQueryValueW"] {
        assert_eq!(
            registry.get("advapi32.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 4 }
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(
            ".",
            "legacy-query-test.exe",
            "target/scratch/katahane_legacy_registry/tests",
        ),
    )
    .unwrap();
    machine.ext.insert(VirtualRegistry::default());
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(OUT, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine
        .spawn("legacy registry query", CODE, &[], 65536)
        .unwrap();
    machine
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn bytes<B: Backend>(machine: &Machine<B>, pointer: u32, length: usize) -> Vec<u8> {
    let mut output = vec![0; length];
    machine.cpu.read(pointer, &mut output).unwrap();
    output
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn text<B: Backend>(machine: &mut Machine<B>, pointer: u32, value: &str, wide: bool) {
    let bytes = if wide {
        utf16(&format!("{value}\0"))
    } else {
        [machine.ext.get::<Win32>().codec.encode(value), vec![0]].concat()
    };
    machine.cpu.write(pointer, &bytes).unwrap();
}
fn key<B: Backend>(
    machine: &mut Machine<B>,
    path: &str,
    value: Option<(u32, &[u8])>,
    access: u32,
) -> u32 {
    let registry = machine.ext.get_mut::<VirtualRegistry>();
    let root = registry.key(&machine.sys.objects, HKCU).unwrap();
    let (key, parents) = child_key(&root.key, &path.encode_utf16().collect::<Vec<_>>()).unwrap();
    for parent in parents {
        registry.keys.entry(parent).or_default();
    }
    if let Some((kind, bytes)) = value {
        registry.keys.get_mut(&key).unwrap().insert(
            Vec::new(),
            Value {
                kind,
                display_name: Vec::new(),
                bytes: bytes.to_vec(),
            },
        );
    }
    registry
        .allocate(&mut machine.sys.objects, key, access)
        .unwrap()
}
fn invoke<B: Backend>(machine: &mut Machine<B>, values: [u32; 4], wide: bool) -> u32 {
    machine.cpu.set_reg(Reg::Esp, DATA);
    for (index, value) in values.into_iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, value)
            .unwrap();
    }
    let before = machine
        .ext
        .get::<VirtualRegistry>()
        .handle_count(&machine.sys.objects);
    let handler = if wide {
        query_w_ABI.handler
    } else {
        query_a_ABI.handler
    };
    let result = match handler(&mut env(machine)) {
        Outcome::Return(value) => value,
        _ => panic!("legacy queries are synchronous"),
    };
    assert_eq!(
        machine
            .ext
            .get::<VirtualRegistry>()
            .handle_count(&machine.sys.objects),
        before
    );
    assert_eq!(crate::util::last_error(&env(machine)), SENTINEL);
    result
}
fn reset<B: Backend>(machine: &mut Machine<B>, capacity: u32) {
    machine.cpu.write_u32(DATA + 0x300, capacity).unwrap();
    machine.cpu.write(OUT, &[0xcc; 64]).unwrap();
    crate::util::set_last_error(&mut env(machine), SENTINEL);
}

fn actual_guest<B: Backend>(cpu: B) {
    let mut machine = fixture(cpu);
    let handle = key(
        &mut machine,
        "Software\\Legacy\\Child",
        Some((1, &utf16("東京\0"))),
        QUERY,
    );
    text(&mut machine, DATA + 0x100, "sOfTwArE\\lEgAcY\\cHiLd", true);
    let query_a = machine
        .resolve_import("advapi32.dll", "RegQueryValueA")
        .unwrap();
    let query_w = machine
        .resolve_import("advapi32.dll", "RegQueryValueW")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x380);
    code.push_imm(SENTINEL).call(set_error);
    for (api, values, record) in [
        (query_a, [handle, 0, 0, DATA + 0x300], DATA + 0x340),
        (query_w, [HKCU, DATA + 0x100, 0, DATA + 0x304], DATA + 0x348),
        (
            query_w,
            [HKCU, DATA + 0x100, OUT, DATA + 0x304],
            DATA + 0x350,
        ),
    ] {
        for value in values.into_iter().rev() {
            code.push_imm(value);
        }
        code.call(api).mov_mem_eax(record);
        code.call(get_error).mov_mem_eax(record + 4);
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x384);
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    let handles = machine
        .ext
        .get::<VirtualRegistry>()
        .handle_count(&machine.sys.objects);
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        bytes(&machine, DATA + 0x340, 24),
        [0, SENTINEL, 0, SENTINEL, 0, SENTINEL]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x300).unwrap(), 5);
    assert_eq!(machine.cpu.read_u32(DATA + 0x304).unwrap(), 6);
    assert_eq!(bytes(&machine, OUT, 6), utf16("東京\0"));
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x380).unwrap(),
        machine.cpu.read_u32(DATA + 0x384).unwrap()
    );
    assert_eq!(
        machine
            .ext
            .get::<VirtualRegistry>()
            .handle_count(&machine.sys.objects),
        handles
    );
}
#[test]
fn jit_legacy_stdcall() {
    actual_guest(JitCpu::new(PARK_ADDR, false).unwrap());
}
#[test]
fn unicorn_legacy_stdcall() {
    actual_guest(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn byte_capacities_and_guest_codepages() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    for (cp, value) in [(932, "東京"), (936, "中文"), (1252, "café"), (65001, "😀")] {
        machine.ext.get_mut::<Win32>().codec.acp = cp;
        let handle = key(
            &mut machine,
            value,
            Some((1, &utf16(&format!("{value}\0")))),
            QUERY,
        );
        for wide in [false, true] {
            let expected = if wide {
                utf16(&format!("{value}\0"))
            } else {
                [machine.ext.get::<Win32>().codec.encode(value), vec![0]].concat()
            };
            text(&mut machine, DATA + 0x100, value, wide);
            for capacity in [
                0,
                expected.len() as u32 - 1,
                expected.len() as u32,
                u32::MAX,
            ] {
                reset(&mut machine, capacity);
                let status = invoke(&mut machine, [HKCU, DATA + 0x100, OUT, DATA + 0x300], wide);
                assert_eq!(
                    machine.cpu.read_u32(DATA + 0x300).unwrap() as usize,
                    expected.len()
                );
                if capacity < expected.len() as u32 {
                    assert_eq!(status, MORE_DATA);
                    assert_eq!(bytes(&machine, OUT, 64), vec![0xcc; 64]);
                } else {
                    assert_eq!(status, 0);
                    assert_eq!(bytes(&machine, OUT, expected.len()), expected);
                }
            }
            reset(&mut machine, 0);
            assert_eq!(invoke(&mut machine, [handle, 0, 0, DATA + 0x300], wide), 0);
            assert_eq!(
                machine.cpu.read_u32(DATA + 0x300).unwrap() as usize,
                expected.len()
            );
            assert_eq!(invoke(&mut machine, [handle, 0, 0, 0], wide), 0);
        }
    }
}

#[test]
fn missing_default_and_preserved_string_terminators() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    let absent = key(&mut machine, "Absent", None, QUERY);
    for wide in [false, true] {
        text(&mut machine, DATA + 0x100, "", wide);
        for subkey in [0, DATA + 0x100] {
            reset(&mut machine, 64);
            assert_eq!(
                invoke(&mut machine, [absent, subkey, OUT, DATA + 0x300], wide),
                0
            );
            let count = if wide { 2 } else { 1 };
            assert_eq!(machine.cpu.read_u32(DATA + 0x300).unwrap(), count);
            assert_eq!(
                bytes(&machine, OUT, count as usize),
                vec![0; count as usize]
            );
        }
        reset(&mut machine, 0);
        assert_eq!(
            invoke(&mut machine, [absent, 0, OUT, DATA + 0x300], wide),
            0
        );
        assert_eq!(bytes(&machine, OUT, 64), vec![0xcc; 64]);
        for (stored, expected) in [
            (utf16("ab"), "ab\0"),
            (utf16("a\0\0"), "a\0\0"),
            (utf16(""), ""),
            (utf16("\0"), "\0"),
        ] {
            let handle = key(&mut machine, "Raw", Some((1, &stored)), QUERY);
            reset(&mut machine, 64);
            assert_eq!(
                invoke(&mut machine, [handle, 0, OUT, DATA + 0x300], wide),
                0
            );
            let expected = if wide {
                utf16(expected)
            } else {
                expected.as_bytes().to_vec()
            };
            assert_eq!(
                machine.cpu.read_u32(DATA + 0x300).unwrap() as usize,
                expected.len()
            );
            assert_eq!(bytes(&machine, OUT, expected.len()), expected);
            assert_eq!(bytes(&machine, OUT + expected.len() as u32, 1), [0xcc]);
        }
    }
}

#[test]
fn missing_keys_access_and_checked_pointers_return_status() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    let handle = key(&mut machine, "Default", Some((1, &utf16("abc\0"))), QUERY);
    let denied = key(&mut machine, "Denied", None, SET);
    text(&mut machine, DATA + 0x100, "Missing", false);
    for (values, expected) in [
        ([HKCU, DATA + 0x100, OUT, DATA + 0x300], NOT_FOUND),
        ([0xdeadbeef, 0, OUT, DATA + 0x300], INVALID_HANDLE),
        ([denied, 0, OUT, DATA + 0x300], ACCESS_DENIED),
        ([handle, 0, OUT, 0], INVALID_PARAMETER),
        ([handle, 0, OUT, READ_ONLY], NOACCESS),
        ([handle, CODE + 0x1000, OUT, DATA + 0x300], NOACCESS),
    ] {
        reset(&mut machine, 64);
        assert_eq!(invoke(&mut machine, values, false), expected);
        assert_eq!(machine.cpu.read_u32(DATA + 0x300).unwrap(), 64);
        assert_eq!(bytes(&machine, OUT, 64), vec![0xcc; 64]);
    }
    reset(&mut machine, 64);
    assert_eq!(
        invoke(&mut machine, [handle, 0, READ_ONLY, DATA + 0x300], false),
        NOACCESS
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x300).unwrap(), 4);
    let registry = machine.ext.get_mut::<VirtualRegistry>();
    let opened = registry.key(&machine.sys.objects, handle).unwrap();
    registry.mark_deleted(&mut machine.sys.objects, &opened.key);
    reset(&mut machine, 64);
    assert_eq!(
        invoke(&mut machine, [handle, 0, OUT, DATA + 0x300], false),
        1018
    );
}

#[test]
fn non_strings_and_unavailable_expansion_never_report_success() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    for (kind, data) in [
        (4, 42u32.to_le_bytes().to_vec()),
        (3, b"abc\0".to_vec()),
        (7, utf16("a\0b\0\0")),
    ] {
        let handle = key(&mut machine, "Type", Some((kind, &data)), QUERY);
        for wide in [false, true] {
            reset(&mut machine, 64);
            assert_eq!(
                invoke(&mut machine, [handle, 0, 0, DATA + 0x300], wide),
                INVALID_DATA
            );
            reset(&mut machine, 0);
            assert_eq!(
                invoke(&mut machine, [handle, 0, OUT, DATA + 0x300], wide),
                MORE_DATA
            );
            reset(&mut machine, 64);
            assert_eq!(
                invoke(&mut machine, [handle, 0, OUT, DATA + 0x300], wide),
                INVALID_DATA
            );
        }
    }
    let handle = key(
        &mut machine,
        "Expand",
        Some((2, &utf16("%AUTHORED_VAR%\0"))),
        QUERY,
    );
    reset(&mut machine, 64);
    assert_eq!(
        invoke(&mut machine, [handle, 0, OUT, DATA + 0x300], false),
        NOT_SUPPORTED
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x300).unwrap(), 64);
    assert_eq!(bytes(&machine, OUT, 64), vec![0xcc; 64]);
    let handle = key(&mut machine, "Odd", Some((1, &[0x61, 0, 0x62])), QUERY);
    for wide in [false, true] {
        reset(&mut machine, 64);
        assert_eq!(
            invoke(&mut machine, [handle, 0, OUT, DATA + 0x300], wide),
            INVALID_DATA
        );
        assert_eq!(bytes(&machine, OUT, 64), vec![0xcc; 64]);
    }
    machine.ext.get_mut::<Win32>().codec.acp = 437;
    reset(&mut machine, 64);
    assert_eq!(
        invoke(&mut machine, [HKCU, 0, OUT, DATA + 0x300], false),
        NOT_SUPPORTED
    );
}
