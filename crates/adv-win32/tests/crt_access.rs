//! Authored x86 MSVCRT oracle: access modes, shared error state and cdecl cleanup.
//! Source SHA256 026416664f01764d555668a55ab60d86df912e657080b81b8c8882ee4b7ea207.
use adv_abi::{Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{
    Clock, Env, Machine, Outcome, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
use std::path::PathBuf;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const PATH: u32 = DATA + 0x1000;
const ERRNO_POINTER: u32 = DATA + 0xf00;
const SAVED_ESP: u32 = DATA + 0xf04;
const LAST_ERROR: u32 = 0x12345678;

fn fixture<B: Backend>(cpu: B, name: &str) -> (Machine<B>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/crt-access-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(root.join("game/existing")).unwrap();
    std::fs::create_dir_all(root.join("data/overlay")).unwrap();
    std::fs::write(root.join("game/base.dat"), b"base").unwrap();
    std::fs::write(root.join("data/overlay/overlay.dat"), b"overlay").unwrap();
    std::fs::write(root.join("data/overlay/日本.dat"), b"unicode").unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["_access", "_waccess"] {
        let signature = adv_abi::signature::lookup("msvcrt.dll", name).unwrap();
        assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 2 }));
        assert_eq!(signature.parameters.len(), 2);
        assert_eq!(signature.stack_words(), 2);
        assert_eq!(
            registry.level("msvcrt.dll", name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available("msvcrt.dll", name));
    }
    // Modern profiles have different validation contracts and remain unadvertised.
    assert!(!registry.dynamic_available("msvcr90.dll", "_access"));
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.codepage = 936;
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    (machine, root)
}

fn save_esp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}

fn seed(code: &mut Asm, errno: u32, set_error: u32) {
    code.call(errno).mov_mem_eax(ERRNO_POINTER);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.bytes.extend_from_slice(&[0xc7, 0x40, 0x04]);
    code.bytes.extend_from_slice(&91u32.to_le_bytes());
    code.push_imm(LAST_ERROR).call(set_error);
}

fn query<B: Backend>(cpu: B, name: &str) {
    let (mut machine, root) = fixture(cpu, name);
    let original = std::fs::metadata(root.join("game/base.dat"))
        .unwrap()
        .modified()
        .unwrap();
    let errno = machine.resolve_import("msvcrt.dll", "_errno").unwrap();
    let a = machine.resolve_import("msvcrt.dll", "_access").unwrap();
    let w = machine.resolve_import("msvcrt.dll", "_waccess").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    let mut golden = Vec::new();
    let cases = [
        (false, "base.dat", 0, [0, 79, 91, LAST_ERROR]),
        (false, "BASE.DAT", 6, [0, 79, 91, LAST_ERROR]),
        (false, "overlay.dat", 4, [0, 79, 91, LAST_ERROR]),
        (true, "日本.dat", 0, [0, 79, 91, LAST_ERROR]),
        (false, "日本.dat", 0, [0, 79, 91, LAST_ERROR]),
        (true, "existing", 2, [0, 79, 91, LAST_ERROR]),
        (false, "absent.dat", 0, [u32::MAX, 2, 2, 2]),
        (true, "absent/leaf.dat", 4, [u32::MAX, 2, 3, 3]),
        (false, "", 0, [u32::MAX, 2, 3, 3]),
        (false, "base.dat", 15, [0, 79, 91, LAST_ERROR]),
        (true, "base.dat", 16, [u32::MAX, 22, 0, LAST_ERROR]),
        (false, "base.dat", u32::MAX, [u32::MAX, 22, 0, LAST_ERROR]),
        (
            true,
            "C:\\Windows\\System32\\kernel32.dll",
            4,
            [0, 79, 91, LAST_ERROR],
        ),
        (
            true,
            "C:\\Windows\\System32\\kernel32.dll",
            2,
            [u32::MAX, 13, 5, LAST_ERROR],
        ),
    ];
    for (index, (wide, path, mode, expected)) in cases.into_iter().enumerate() {
        let pointer = PATH + index as u32 * 0x100;
        let mut bytes = if wide {
            path.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        } else {
            machine.ext.get::<Win32>().codec.encode(path)
        };
        bytes.extend(if wide { vec![0, 0] } else { vec![0] });
        machine.cpu.write(pointer, &bytes).unwrap();
        seed(&mut code, errno, set_error);
        save_esp(&mut code, SAVED_ESP);
        let record = DATA + index as u32 * 24;
        code.push_imm(mode)
            .push_imm(pointer)
            .call(if wide { w } else { a });
        code.mov_mem_eax(record);
        save_esp(&mut code, record + 16);
        code.add_esp(8);
        save_esp(&mut code, record + 20);
        code.mov_eax_mem(ERRNO_POINTER)
            .mov_eax_ptr_eax()
            .mov_mem_eax(record + 4);
        code.mov_eax_mem(ERRNO_POINTER);
        code.bytes.extend_from_slice(&[0x8b, 0x40, 0x04]);
        code.mov_mem_eax(record + 8);
        code.call(get_error).mov_mem_eax(record + 12);
        golden.push(expected);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("legacy access queries", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, expected) in golden.into_iter().enumerate() {
        let record = DATA + index as u32 * 24;
        for (offset, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                machine.cpu.read_u32(record + offset as u32 * 4).unwrap(),
                expected,
                "case {index}, field {offset}"
            );
        }
        let esp = machine.cpu.read_u32(record + 20).unwrap();
        assert_eq!(esp, machine.cpu.read_u32(SAVED_ESP).unwrap());
        assert_eq!(machine.cpu.read_u32(record + 16).unwrap(), esp - 8);
    }
    assert_eq!(std::fs::read(root.join("game/base.dat")).unwrap(), b"base");
    assert_eq!(
        std::fs::metadata(root.join("game/base.dat"))
            .unwrap()
            .modified()
            .unwrap(),
        original
    );
    assert!(!root.join("data/overlay/base.dat").exists());
    assert!(!root.join("data/overlay/absent").exists());
    assert!(!root.join("game/absent").exists());
}

#[test]
fn access_cdecl_paths_overlay_and_error_state() {
    query(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    query(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
}

fn invoke(machine: &mut Machine<JitCpu>, module: &str, name: &str, args: &[u32]) -> Outcome {
    let entry = machine.resolve_import(module, name).unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    for (index, &arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, arg)
            .unwrap();
    }
    let handler = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .unwrap()
        .get(module, name)
        .unwrap()
        .handler;
    handler(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
    })
}

fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("unexpected non-returning outcome"),
    }
}

fn state(machine: &mut Machine<JitCpu>, cell: u32) -> [u32; 3] {
    let errno = machine.cpu.read_u32(cell).unwrap();
    let dos = machine.cpu.read_u32(cell + 4).unwrap();
    let last_error = returned(invoke(machine, "kernel32.dll", "GetLastError", &[]));
    [errno, dos, last_error]
}

#[test]
fn access_guards_preserve_other_crt_state_and_read_only_guest_inputs() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "guards");
    machine
        .spawn("access guards", PARK_ADDR, &[], 65536)
        .unwrap();
    let cell = returned(invoke(&mut machine, "msvcrt.dll", "_errno", &[]));
    let other = returned(invoke(&mut machine, "msvcr80.dll", "_errno", &[]));
    machine.cpu.write_u32(cell, 79).unwrap();
    machine.cpu.write_u32(cell + 4, 91).unwrap();
    machine.cpu.write_u32(other, 71).unwrap();
    invoke(&mut machine, "kernel32.dll", "SetLastError", &[LAST_ERROR]);
    assert!(matches!(
        invoke(&mut machine, "msvcrt.dll", "_access", &[0xdead0000, 0]),
        Outcome::Fail(_)
    ));
    assert_eq!(state(&mut machine, cell), [79, 91, LAST_ERROR]);
    machine.cpu.write(DATA + 0x3ffe, &[b'a', 0]).unwrap();
    assert!(matches!(
        invoke(&mut machine, "msvcrt.dll", "_waccess", &[DATA + 0x3ffe, 0]),
        Outcome::Fail(_)
    ));
    assert_eq!(state(&mut machine, cell), [79, 91, LAST_ERROR]);
    assert_eq!(
        returned(invoke(&mut machine, "msvcrt.dll", "_access", &[0, 0])),
        u32::MAX
    );
    assert_eq!(state(&mut machine, cell), [22, 0, LAST_ERROR]);
    let input = 0x500000;
    machine.cpu.map(input, 4096, Prot::RW).unwrap();
    machine.cpu.write(input, b"base.dat\0").unwrap();
    let bytes: Vec<_> = "base.dat"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(input + 0x100, &bytes).unwrap();
    machine.cpu.protect(input, 4096, Prot::READ).unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "msvcrt.dll", "_access", &[input, 4])),
        0
    );
    assert_eq!(state(&mut machine, cell), [22, 0, LAST_ERROR]);
    machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(437);
    assert_eq!(
        returned(invoke(&mut machine, "msvcrt.dll", "_access", &[input, 0])),
        u32::MAX
    );
    assert_eq!(state(&mut machine, cell), [40, 50, 50]);
    assert_eq!(
        returned(invoke(
            &mut machine,
            "msvcrt.dll",
            "_waccess",
            &[input + 0x100, 0]
        )),
        0
    );
    assert_eq!(state(&mut machine, cell), [40, 50, 50]);
    assert_eq!(machine.cpu.read_u32(other).unwrap(), 71);
    assert!(
        machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, reason)| api == "msvcrt.dll!_access" && reason.contains("ACP"))
    );
    assert!(!root.join("data/overlay/base.dat").exists());
    assert_eq!(std::fs::read(root.join("game/base.dat")).unwrap(), b"base");
}
