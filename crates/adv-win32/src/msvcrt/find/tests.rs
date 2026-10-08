//! VC90 native layout/errno evidence: target/engine-loop/20261007-resume/crt-find-native.
//! Authored oracle source SHA256 AAFB9EFD45E94ED15FC67804EE8DF2582681ABE19DE32452DB8B4F652F179EF0.
use super::*;
use crate::Win32;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x430000;
const WRITE_ONLY: u32 = 0x440000;
const CALLBACK: u32 = CODE + 0x3000;
const CALLS: u32 = DATA + 0x3f00;
const SEEN: u32 = CALLS + 4;
static NEXT: AtomicUsize = AtomicUsize::new(0);

enum Arg {
    Value(u32),
    Memory(u32),
}
use Arg::{Memory as M, Value as I};

struct Fixture {
    machine: Machine<JitCpu>,
    root: PathBuf,
    code: Asm,
    code_address: u32,
    data: u32,
}
impl Fixture {
    fn new(jit: bool) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/crt-find-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(root.join("game")).unwrap();
        std::fs::create_dir_all(root.join("data/overlay")).unwrap();
        std::fs::write(root.join("game/alpha.dat"), b"old").unwrap();
        std::fs::write(root.join("game/beta.dat"), b"bb").unwrap();
        let file = root.join("data/overlay/ALPHA.dat");
        std::fs::write(&file, b"overlay").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(file)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(UNIX_EPOCH + Duration::from_secs(946684900))
                    .set_modified(UNIX_EPOCH + Duration::from_secs(946685000)),
            )
            .unwrap();
        let mut registry = Registry::default();
        crate::register(&mut registry);
        super::super::register(&mut registry);
        for module in MODULES {
            for (name, argc) in [
                ("_wfindfirst64i32", 2),
                ("_wfindnext64i32", 2),
                ("_findclose", 1),
            ] {
                assert_eq!(
                    registry.get(module, name).unwrap().conv,
                    adv_abi::Conv::Cdecl { argc }
                );
                assert!(registry.dynamic_available(module, name));
            }
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut config =
            crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
        config.os.codepage = 936;
        Win32::install(&mut machine, config).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        machine.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
        machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
        machine.cpu.map(WRITE_ONLY, 4096, Prot::WRITE).unwrap();
        machine.cpu.write(READ_ONLY, &[0xcc; 560]).unwrap();
        machine.cpu.write(WRITE_ONLY, &[0xcc; 560]).unwrap();
        Self {
            machine,
            root,
            code: Asm::new(CODE),
            code_address: CODE,
            data: DATA,
        }
    }
    fn block(&mut self, bytes: &[u8]) -> u32 {
        let pointer = self.data;
        self.data += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.data < CALLS);
        self.machine.cpu.write(pointer, bytes).unwrap();
        pointer
    }
    fn wide(&mut self, text: &str) -> u32 {
        self.block(
            &text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
    }
    fn output(&mut self) -> u32 {
        self.block(&[0xcc; 560])
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("msvcr90.dll", name));
        let conv = adv_abi::signature::lookup(module, name)
            .unwrap()
            .conv
            .unwrap();
        let entry = self.machine.resolve_import(module, name).unwrap();
        for arg in args.iter().rev() {
            match arg {
                I(value) => {
                    self.code.push_imm(*value);
                }
                M(pointer) => {
                    self.code.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
        let output = self.block(&[0; 4]);
        self.code.call(entry);
        if matches!(conv, adv_abi::Conv::Cdecl { .. }) {
            self.code.add_esp((args.len() * 4) as u8);
        }
        self.code.mov_mem_eax(output);
        output
    }
    fn seed(&mut self) -> u32 {
        let address = self.call("_errno", &[]);
        self.code
            .mov_eax_mem(address)
            .bytes
            .extend_from_slice(&[0xc7, 0x00]);
        self.code.bytes.extend_from_slice(&79u32.to_le_bytes());
        self.code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
        self.code.bytes.extend_from_slice(&91u32.to_le_bytes());
        address
    }
    fn errno(&mut self) -> u32 {
        let pointer = self.call("_errno", &[]);
        let value = self.block(&[0; 4]);
        self.code
            .mov_eax_mem(pointer)
            .mov_eax_ptr_eax()
            .mov_mem_eax(value);
        value
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine
            .cpu
            .write(self.code_address, &self.code.bytes)
            .unwrap();
        self.machine
            .cpu
            .executor_mut()
            .add_region(Region::new(self.code_address, self.code.bytes.len()))
            .unwrap();
        self.machine
            .spawn("CRT find guest", self.code_address, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        self.code_address += 4096;
        self.code = Asm::new(self.code_address);
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn value64(&self, pointer: u32) -> u64 {
        u64::from(self.value(pointer)) | (u64::from(self.value(pointer + 4)) << 32)
    }
    fn name(&self, pointer: u32) -> String {
        let mut units = Vec::new();
        for i in 0..260 {
            let value = self.machine.cpu.read_u16(pointer + 36 + i * 2).unwrap();
            if value == 0 {
                break;
            }
            units.push(value);
        }
        String::from_utf16(&units).unwrap()
    }
    fn env(&mut self, name: &str) -> Env<'_> {
        let entry = self.machine.resolve_import("msvcr90.dll", name).unwrap();
        Env {
            context: self.machine.sys.current,
            cpu: &mut self.machine.cpu,
            sys: &mut self.machine.sys,
            ext: &mut self.machine.ext,
            thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
        }
    }
}

#[test]
fn wide_layout_attributes_overlay_metadata_and_independent_frozen_cursors_match_native_contract() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let pattern = test.wide("*.dat");
        let alpha = test.wide("alpha.dat");
        let metadata = std::fs::metadata(test.root.join("data/overlay/ALPHA.dat")).unwrap();
        let created = unix_seconds(metadata.created().ok()).unwrap();
        test.call("kernel32.dll!SetFileAttributesW", &[I(alpha), I(0x80)]);
        let cell = test.seed();
        let first_out = test.output();
        let independent_out = test.output();
        let handle = test.call("_wfindfirst64i32", &[I(pattern), I(first_out)]);
        let independent = test.call(
            "msvcr80.dll!_wfindfirst64i32",
            &[I(pattern), I(independent_out)],
        );
        let wrong_close = test.call("kernel32.dll!FindClose", &[M(handle)]);
        let wrong_kernel_close = test.call("kernel32.dll!CloseHandle", &[M(handle)]);
        test.run();
        assert_ne!(test.value(handle), u32::MAX);
        assert_ne!(test.value(handle), test.value(independent));
        assert_eq!(
            (test.value(wrong_close), test.value(wrong_kernel_close)),
            (0, 0)
        );
        assert_eq!(test.name(first_out), "ALPHA.dat");
        assert_eq!((test.value(first_out), test.value(first_out + 32)), (0, 7));
        for (offset, value) in [(8, created), (16, 946684900), (24, 946685000)] {
            assert_eq!(test.value64(first_out + offset), value as u64);
        }
        assert_eq!(test.value(first_out + 4), 0xcccccccc);
        assert_eq!(test.value(first_out + 556), 0xcccccccc);
        assert_eq!(
            test.machine.cpu.read_u16(first_out + 36 + 20).unwrap(),
            0xcccc
        );
        assert_eq!(
            (
                test.value(test.value(cell)),
                test.value(test.value(cell) + 4)
            ),
            (79, 91)
        );
        std::fs::write(test.root.join("data/overlay/beta.dat"), b"new-betas").unwrap();
        std::fs::write(test.root.join("data/overlay/gamma.dat"), b"gggg").unwrap();
        let cell = test.seed();
        let next_out = test.output();
        let other_next_out = test.output();
        let next_result = test.call("_wfindnext64i32", &[M(handle), I(next_out)]);
        let other_next = test.call(
            "msvcrt.dll!_wfindnext64i32",
            &[M(independent), I(other_next_out)],
        );
        let eof = test.call("_wfindnext64i32", &[M(handle), I(next_out)]);
        let eof_errno = test.errno();
        let new_out = test.output();
        let new_next_out = test.output();
        let third_out = test.output();
        let fresh = test.call("_wfindfirst64i32", &[I(pattern), I(new_out)]);
        test.call("_wfindnext64i32", &[M(fresh), I(new_next_out)]);
        test.call("_wfindnext64i32", &[M(fresh), I(third_out)]);
        let close1 = test.call("_findclose", &[M(handle)]);
        let close2 = test.call("msvcr90.dll!_findclose", &[M(independent)]);
        let close3 = test.call("_findclose", &[M(fresh)]);
        let stale = test.call("_findclose", &[M(handle)]);
        let stale_errno = test.errno();
        test.run();
        assert_eq!((test.value(next_result), test.value(other_next)), (0, 0));
        assert_eq!(test.name(next_out), "beta.dat");
        assert_eq!(test.name(other_next_out), "beta.dat");
        assert_eq!(
            (test.value(next_out + 32), test.value(other_next_out + 32)),
            (2, 2)
        );
        assert_eq!((test.value(eof), test.value(eof_errno)), (u32::MAX, 2));
        assert_eq!(
            (test.name(new_next_out), test.value(new_next_out + 32)),
            ("beta.dat".into(), 9)
        );
        assert_eq!(
            (test.name(third_out), test.value(third_out + 32)),
            ("gamma.dat".into(), 4)
        );
        assert_eq!(
            (test.value(close1), test.value(close2), test.value(close3)),
            (0, 0, 0)
        );
        assert_eq!((test.value(stale), test.value(stale_errno)), (u32::MAX, 22));
        assert_eq!(test.value(test.value(cell) + 4), 91);
        let searches = test.machine.ext.get::<Searches>();
        assert_eq!(
            test.machine
                .sys
                .objects
                .alias_count(searches.namespace.unwrap()),
            0
        );
        assert_eq!(
            std::fs::read(test.root.join("game/alpha.dat")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(test.root.join("game/beta.dat")).unwrap(),
            b"bb"
        );
    }
}

#[test]
fn invalid_handler_errno_eof_and_output_guards_never_publish_or_consume_failed_rows() {
    let mut test = Fixture::new(true);
    let pattern = test.wide("*.dat");
    let missing = test.wide("absent*.dat");
    let errno_api = test
        .machine
        .resolve_import("msvcr90.dll", "_errno")
        .unwrap();
    let mut callback = Asm::new(CALLBACK);
    callback.bytes.extend_from_slice(&[0xff, 0x05]);
    callback.bytes.extend_from_slice(&CALLS.to_le_bytes());
    callback
        .call(errno_api)
        .mov_eax_ptr_eax()
        .mov_mem_eax(SEEN)
        .ret();
    test.machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    test.machine
        .cpu
        .executor_mut()
        .add_region(Region::new(CALLBACK, callback.bytes.len()))
        .unwrap();
    test.call("_set_invalid_parameter_handler", &[I(CALLBACK)]);
    let cell = test.seed();
    let out = test.output();
    let null_pattern = test.call("_wfindfirst64i32", &[I(0), I(out)]);
    let null_output = test.call("_wfindfirst64i32", &[I(pattern), I(0)]);
    let minus_one = test.call("_wfindnext64i32", &[I(u32::MAX), I(out)]);
    let absent = test.call("_wfindfirst64i32", &[I(missing), I(out)]);
    let absent_errno = test.errno();
    let real_out = test.output();
    let handle = test.call("_wfindfirst64i32", &[I(pattern), I(real_out)]);
    let kernel_out = test.block(&[0xcc; 592]);
    let kernel_handle = test.call("kernel32.dll!FindFirstFileW", &[I(pattern), I(kernel_out)]);
    let wrong_family = test.call("_findclose", &[M(kernel_handle)]);
    test.run();
    for result in [null_pattern, null_output, minus_one, absent, wrong_family] {
        assert_eq!(test.value(result), u32::MAX);
    }
    assert_eq!((test.value(CALLS), test.value(SEEN)), (3, 22));
    assert_eq!(test.value(absent_errno), 2);
    assert_eq!(test.value(test.value(cell) + 4), 91);
    assert_eq!(test.value64(out), 0xcccccccccccccccc);
    let h = test.value(handle);
    let before = test.machine.sys.objects.object_count();
    let outcome = next(
        &mut test.env("_wfindnext64i32"),
        h as i32,
        GuestPtr::new(READ_ONLY),
    );
    assert!(matches!(outcome, Outcome::Fail(_)));
    let outcome = next(
        &mut test.env("_wfindnext64i32"),
        h as i32,
        GuestPtr::new(u32::MAX - 3),
    );
    assert!(matches!(outcome, Outcome::Fail(_)));
    assert_eq!(test.value(READ_ONLY), 0xcccccccc);
    let outcome = first(
        &mut test.env("_wfindfirst64i32"),
        WideStr(GuestPtr::new(pattern)),
        GuestPtr::new(READ_ONLY),
    );
    assert!(matches!(outcome, Outcome::Fail(_)));
    assert_eq!(test.machine.sys.objects.object_count(), before);
    assert!(
        test.machine
            .sys
            .objects
            .get(test.value(kernel_handle))
            .is_some()
    );
    let out = test.output();
    test.call("_wfindnext64i32", &[M(handle), I(out)]);
    test.call("_findclose", &[M(handle)]);
    test.call("kernel32.dll!FindClose", &[M(kernel_handle)]);
    test.run();
    assert_eq!(
        (test.name(out), test.value(out + 32)),
        ("beta.dat".into(), 2)
    );
}

#[test]
fn write_only_outputs_preserve_padding_and_unsupported_patterns_have_explicit_failures() {
    let mut test = Fixture::new(false);
    let unicode = "銀河.dat";
    std::fs::write(test.root.join("game").join(unicode), b"unicode").unwrap();
    let pattern = test.wide(unicode);
    let bad = test.block(&[0x00, 0xd8, 0, 0]);
    test.seed();
    test.run();
    assert!(!test.machine.cpu.accessible(WRITE_ONLY, 560, Prot::READ));
    let result = first(
        &mut test.env("_wfindfirst64i32"),
        WideStr(GuestPtr::new(pattern)),
        GuestPtr::new(WRITE_ONLY),
    );
    let Outcome::Return(handle) = result else {
        panic!("valid write-only output must succeed")
    };
    test.machine
        .cpu
        .protect(WRITE_ONLY, 4096, Prot::RW)
        .unwrap();
    assert_eq!(test.name(WRITE_ONLY), unicode);
    assert_eq!(test.value(WRITE_ONLY + 32), 7);
    assert_eq!(test.value(WRITE_ONLY + 4), 0xcccccccc);
    assert_eq!(test.value(WRITE_ONLY + 556), 0xcccccccc);
    let before = test.machine.sys.objects.object_count();
    let out = test.output();
    let result = first(
        &mut test.env("_wfindfirst64i32"),
        WideStr(GuestPtr::new(bad)),
        GuestPtr::new(out),
    );
    assert!(matches!(result, Outcome::Return(u32::MAX)));
    assert_eq!(test.value(out), 0xcccccccc);
    assert_eq!(test.machine.sys.objects.object_count(), before);
    assert_eq!(test.value(out), 0xcccccccc);
    assert!(test.machine.ext.get::<adv_abi::ApiDiagnostics>().warnings.keys().any(|key|key.0=="msvcr90.dll!_wfindfirst64i32"&&key.1.contains("unpaired UTF-16")));
    assert!(matches!(
        close(&mut test.env("_findclose"), handle as i32),
        Outcome::Return(0)
    ));
}
