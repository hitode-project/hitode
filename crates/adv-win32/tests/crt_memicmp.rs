//! Authored x86 MSVCRT oracle binds locale byte folding and legacy error state.
//! Source SHA256 db71986db47eac0c9fa03cc6719f7bf9ff3084387d4b27614dc24ad691225171.
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

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LEFT: u32 = 0x430000;
const RIGHT: u32 = 0x440000;
const ERRNO: u32 = DATA + 0xf00;
const LAST_ERROR: u32 = 0x12345678;

fn fixture<B: Backend>(cpu: B, name: &str) -> Machine<B> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/crt-memicmp-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let signature = adv_abi::signature::lookup("msvcrt.dll", "_memicmp").unwrap();
    assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 3 }));
    assert_eq!(signature.stack_words(), 3);
    assert_eq!(
        registry.level("msvcrt.dll", "_memicmp"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("msvcrt.dll", "_memicmp"));
    assert!(!registry.dynamic_available("msvcrt.dll", "_memicmp_l"));
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.codepage = 936;
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    machine.cpu.map(LEFT, 0x2000, Prot::RW).unwrap();
    machine.cpu.map(RIGHT, 0x2000, Prot::RW).unwrap();
    machine
}

fn save_esp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}

fn actual<B: Backend>(cpu: B, name: &str) {
    let mut machine = fixture(cpu, name);
    let left = [
        b"A".as_slice(),
        b"a",
        b"Z",
        &[0x80],
        b"A\0Z",
        &[0xc4],
        &[0x8a],
        &[0x9f],
        &[0],
        b"@",
        b"A",
        b"A",
    ];
    let right = [
        b"a".as_slice(),
        b"d",
        b"a",
        &[0xff],
        b"a\0b",
        &[0xe4],
        &[0x9a],
        &[0xff],
        b"A",
        b"A",
        b"B",
        b"B",
    ];
    for (index, bytes) in left.into_iter().enumerate() {
        machine.cpu.write(LEFT + index as u32 * 16, bytes).unwrap();
        machine
            .cpu
            .write(RIGHT + index as u32 * 16, right[index])
            .unwrap();
    }
    machine.cpu.protect(LEFT, 0x2000, Prot::READ).unwrap();
    machine.cpu.protect(RIGHT, 0x2000, Prot::READ).unwrap();
    let errno = machine.resolve_import("msvcrt.dll", "_errno").unwrap();
    let compare = machine.resolve_import("msvcrt.dll", "_memicmp").unwrap();
    let locale = machine.resolve_import("msvcrt.dll", "setlocale").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.call(errno).mov_mem_eax(ERRNO);
    let locales = [
        "C",
        "Japanese_Japan.932",
        "Chinese (Simplified)_China.936",
        "English_United States.1252",
        "C",
    ];
    let golden: [[i32; 12]; 5] = [
        [0, -1, 1, -1, 1, -1, -1, -1, -1, -1, 0, i32::MAX],
        [0, -3, 25, -127, 24, -32, -16, -96, -97, -33, 0, i32::MAX],
        [0, -3, 25, -127, 24, -32, -16, -96, -97, -33, 0, i32::MAX],
        [0, -3, 25, -127, 24, 0, 0, 0, -97, -33, 0, i32::MAX],
        [0, -1, 1, -1, 1, -1, -1, -1, -1, -1, 0, i32::MAX],
    ];
    for (profile, locale_name) in locales.into_iter().enumerate() {
        let pointer = DATA + 0x2000 + profile as u32 * 0x100;
        machine
            .cpu
            .write(pointer, &[locale_name.as_bytes(), &[0]].concat())
            .unwrap();
        code.push_imm(pointer)
            .push_imm(2)
            .call(locale)
            .add_esp(8)
            .mov_mem_eax(DATA + 0x1000 + profile as u32 * 4);
        for case in 0..12u32 {
            code.mov_eax_mem(ERRNO);
            code.bytes.extend_from_slice(&[0xc7, 0x00]);
            code.bytes.extend_from_slice(&79u32.to_le_bytes());
            code.bytes.extend_from_slice(&[0xc7, 0x40, 0x04]);
            code.bytes.extend_from_slice(&91u32.to_le_bytes());
            code.push_imm(LAST_ERROR).call(set_error);
            let out = DATA + (profile as u32 * 12 + case) * 28;
            save_esp(&mut code, out + 16);
            let a = if case >= 10 { 0 } else { LEFT + case * 16 };
            let b = if case == 10 { 0 } else { RIGHT + case * 16 };
            let count = if case == 4 { 3 } else { u32::from(case != 10) };
            code.push_imm(count)
                .push_imm(b)
                .push_imm(a)
                .call(compare)
                .mov_mem_eax(out);
            save_esp(&mut code, out + 20);
            code.add_esp(12);
            save_esp(&mut code, out + 24);
            code.mov_eax_mem(ERRNO)
                .mov_eax_ptr_eax()
                .mov_mem_eax(out + 4);
            code.mov_eax_mem(ERRNO);
            code.bytes.extend_from_slice(&[0x8b, 0x40, 0x04]);
            code.mov_mem_eax(out + 8)
                .call(get_error)
                .mov_mem_eax(out + 12);
        }
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("legacy memory comparisons", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (profile, cases) in golden.into_iter().enumerate() {
        assert_ne!(
            machine
                .cpu
                .read_u32(DATA + 0x1000 + profile as u32 * 4)
                .unwrap(),
            0
        );
        for (case, expected) in cases.into_iter().enumerate() {
            let out = DATA + (profile as u32 * 12 + case as u32) * 28;
            assert_eq!(
                machine.cpu.read_u32(out).unwrap() as i32,
                expected,
                "profile {profile}, case {case}"
            );
            assert_eq!(
                machine.cpu.read_u32(out + 4).unwrap(),
                if case == 11 { 22 } else { 79 }
            );
            assert_eq!(machine.cpu.read_u32(out + 8).unwrap(), 91);
            assert_eq!(machine.cpu.read_u32(out + 12).unwrap(), LAST_ERROR);
            let before = machine.cpu.read_u32(out + 16).unwrap();
            assert_eq!(machine.cpu.read_u32(out + 20).unwrap(), before - 12);
            assert_eq!(machine.cpu.read_u32(out + 24).unwrap(), before);
        }
    }
}

#[test]
fn memicmp_real_cdecl_calls_match_native_locale_and_error_state() {
    actual(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    actual(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
}

fn invoke(machine: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> Outcome {
    let entry = machine.resolve_import("msvcrt.dll", name).unwrap();
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
        .get("msvcrt.dll", name)
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

fn returned(outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Return(value) => value as i32,
        _ => panic!("unexpected outcome"),
    }
}

#[test]
fn compare_chunks_and_fault_spans_keep_memcmp_case_sensitive() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "guards");
    machine
        .spawn("comparison guards", PARK_ADDR, &[], 65536)
        .unwrap();
    let cell = returned(invoke(&mut machine, "_errno", &[])) as u32;
    machine.cpu.write_u32(cell, 79).unwrap();
    machine.cpu.write_u32(cell + 4, 91).unwrap();
    let mut left = vec![b'A'; 5000];
    let mut right = vec![b'a'; 5000];
    left[4999] = b'B';
    right[4999] = b'z';
    machine.cpu.write(LEFT, &left).unwrap();
    machine.cpu.write(RIGHT, &right).unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "_memicmp", &[LEFT, RIGHT, 5000])),
        -1
    );
    assert_eq!(
        returned(invoke(&mut machine, "memcmp", &[LEFT, RIGHT, 5000])),
        -32
    );
    machine.cpu.write(RIGHT, &left).unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "memcmp", &[LEFT, RIGHT, 5000])),
        0
    );
    let mut changed = left.clone();
    changed[4999] = b'z';
    machine.cpu.write(RIGHT, &changed).unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "memcmp", &[LEFT, RIGHT, 5000])),
        -56
    );
    machine.cpu.write(LEFT + 0x1fff, b"A").unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "memcmp", &[LEFT + 0x1fff, RIGHT, 1])),
        0
    );
    for name in ["memcmp", "_memicmp"] {
        assert!(matches!(
            invoke(&mut machine, name, &[LEFT + 0x1fff, RIGHT, 2]),
            Outcome::Fail(_)
        ));
    }
    assert!(matches!(
        invoke(&mut machine, "_memicmp", &[0xfffffffe, RIGHT, 4]),
        Outcome::Fail(_)
    ));
    assert_eq!(machine.cpu.read_u32(cell).unwrap(), 79);
    assert_eq!(machine.cpu.read_u32(cell + 4).unwrap(), 91);
}
