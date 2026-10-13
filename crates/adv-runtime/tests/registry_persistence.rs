//! Guest-only private-registry tests. The authored image has no native registry
//! imports and is never executed as a host Windows program.
#![cfg(windows)]
use std::path::Path;
use std::time::Duration;

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod cpu;

const HKCU: u32 = 0x80000001;
#[derive(Clone, Copy)]
enum Arg {
    Imm(u32),
    Load(u32),
}
use Arg::{Imm as I, Load as L};

fn executable() -> Vec<u8> {
    let mut bytes = vec![0; 1024];
    let w =
        |bytes: &mut [u8], at, value: u32| bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    bytes[..2].copy_from_slice(b"MZ");
    w(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
    bytes[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
    bytes[0x94..0x96].copy_from_slice(&224u16.to_le_bytes());
    bytes[0x96..0x98].copy_from_slice(&0x103u16.to_le_bytes());
    bytes[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
    for (at, value) in [
        (16, 4096),
        (28, 0x400000),
        (32, 4096),
        (36, 512),
        (56, 8192),
        (60, 512),
        (72, 0x100000),
        (76, 0x1000),
        (92, 16),
    ] {
        w(&mut bytes, 0x98 + at, value);
    }
    bytes[0xdc..0xde].copy_from_slice(&3u16.to_le_bytes());
    let section = 0x98 + 224;
    bytes[section..section + 8].copy_from_slice(b".text\0\0\0");
    for (at, value) in [
        (8, 4096),
        (12, 4096),
        (16, 512),
        (20, 512),
        (36, 0x60000020),
    ] {
        w(&mut bytes, section + at, value);
    }
    bytes[512] = 0xc3;
    bytes
}
struct Program {
    runtime: Runtime<cpu::Cpu>,
    asm: Asm,
    next: u32,
    checks: Vec<(String, u32, u32)>,
}
impl Program {
    fn new(exe: &Path, data: &Path, backend: &str) -> Self {
        let mut runtime =
            Runtime::with_cpu(exe, data, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(10)));
        let next = runtime
            .machine
            .sys
            .vm
            .alloc(&mut runtime.machine.cpu, 0x10000, Prot::RW)
            .unwrap();
        Self {
            runtime,
            asm: Asm::new(0x401000),
            next,
            checks: Vec::new(),
        }
    }
    fn blob(&mut self, bytes: &[u8]) -> u32 {
        let at = self.next;
        self.next += (bytes.len().max(4) as u32 + 3) & !3;
        self.runtime.machine.cpu.write(at, bytes).unwrap();
        at
    }
    fn word(&mut self, value: u32) -> u32 {
        self.blob(&value.to_le_bytes())
    }
    fn name(&mut self, value: &str) -> u32 {
        self.blob(
            &value
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
    }
    fn call(&mut self, name: &str, args: &[Arg], expected: u32) {
        let target = self
            .runtime
            .machine
            .resolve_import("advapi32.dll", name)
            .unwrap();
        for &arg in args.iter().rev() {
            match arg {
                I(v) => {
                    self.asm.push_imm(v);
                }
                L(p) => {
                    self.asm.mov_eax_mem(p).push_eax();
                }
            }
        }
        let result = self.word(0xffffffff);
        self.asm.call(target).mov_mem_eax(result);
        self.checks.push((name.into(), result, expected));
    }
    fn create(&mut self, path: &str, options: u32) -> u32 {
        let path = self.name(path);
        let key = self.word(0);
        self.call(
            "RegCreateKeyExW",
            &[
                I(HKCU),
                I(path),
                I(0),
                I(0),
                I(options),
                I(3),
                I(0),
                I(key),
                I(0),
            ],
            0,
        );
        key
    }
    fn write(&mut self, value: u32, flush: bool) {
        let data = self.word(value);
        let key = self.create("Software\\Persistence Fixture\\Permanent", 0);
        self.call(
            "RegSetValueExW",
            &[L(key), I(0), I(0), I(4), I(data), I(4)],
            0,
        );
        let transient = self.create("Software\\Persistence Fixture\\Transient", 1);
        self.call(
            "RegSetValueExW",
            &[L(transient), I(0), I(0), I(4), I(data), I(4)],
            0,
        );
        if flush {
            self.call("RegFlushKey", &[L(key)], 0);
        }
    }
    fn read(&mut self, value: u32) {
        let path = self.name("software\\persistence fixture\\permanent");
        let key = self.word(0);
        self.call("RegOpenKeyExW", &[I(HKCU), I(path), I(0), I(1), I(key)], 0);
        let kind = self.word(0);
        let len = self.word(4);
        let output = self.word(0);
        self.call(
            "RegQueryValueExW",
            &[L(key), I(0), I(0), I(kind), I(output), I(len)],
            0,
        );
        self.checks.extend([
            ("type".into(), kind, 4),
            ("length".into(), len, 4),
            ("value".into(), output, value),
        ]);
        let path = self.name("Software\\Persistence Fixture\\Transient");
        let absent = self.word(0);
        self.call(
            "RegOpenKeyExW",
            &[I(HKCU), I(path), I(0), I(1), I(absent)],
            2,
        );
        self.call("RegFlushKey", &[L(key)], 0);
    }
    fn corrupt(&mut self) {
        let path = self.name("Software\\Persistence Fixture\\Permanent");
        let key = self.word(0);
        self.call("RegOpenKeyExW", &[I(HKCU), I(path), I(0), I(1), I(key)], 13);
        let data = self.word(10);
        self.call(
            "RegSetValueExW",
            &[I(HKCU), I(0), I(0), I(4), I(data), I(4)],
            13,
        );
        self.call("RegFlushKey", &[I(HKCU)], 13);
    }
    fn busy(&mut self) {
        let path = self.name("Software\\Persistence Fixture\\Permanent");
        let key = self.word(0);
        self.call("RegOpenKeyExW", &[I(HKCU), I(path), I(0), I(1), I(key)], 32);
        self.call("RegFlushKey", &[I(HKCU)], 32);
    }
    fn metadata(&mut self, write: bool) -> u32 {
        let path = self.name("Metadata Fixture");
        let class = self.name("TypeX");
        let key = self.word(0);
        if write {
            self.call(
                "RegCreateKeyExW",
                &[
                    I(HKCU),
                    I(path),
                    I(0),
                    I(class),
                    I(0),
                    I(0xf003f),
                    I(0),
                    I(key),
                    I(0),
                ],
                0,
            );
            let name = self.name("mixedCase");
            let data = self.word(0x87654321);
            self.call(
                "RegSetValueExW",
                &[L(key), I(name), I(0), I(4), I(data), I(4)],
                0,
            );
            let transient = self.name("Temporary");
            let out = self.word(0);
            self.call(
                "RegCreateKeyExW",
                &[
                    L(key),
                    I(transient),
                    I(0),
                    I(0),
                    I(1),
                    I(1),
                    I(0),
                    I(out),
                    I(0),
                ],
                0,
            );
        } else {
            self.call(
                "RegOpenKeyExW",
                &[I(HKCU), I(path), I(0), I(0xf003f), I(key)],
                0,
            );
            let class_out = self.blob(&[0xcc; 32]);
            let class_size = self.word(16);
            let child_count = self.word(0xffffffff);
            let value_count = self.word(0xffffffff);
            self.call(
                "RegQueryInfoKeyW",
                &[
                    L(key),
                    I(class_out),
                    I(class_size),
                    I(0),
                    I(child_count),
                    I(0),
                    I(0),
                    I(value_count),
                    I(0),
                    I(0),
                    I(0),
                    I(0),
                ],
                0,
            );
            self.checks.extend([
                ("persistent class length".into(), class_size, 5),
                ("persistent class spelling".into(), class_out, 0x00790054),
                ("volatile child excluded".into(), child_count, 0),
                ("persistent value count".into(), value_count, 1),
            ]);
            let name_out = self.blob(&[0xcc; 32]);
            let name_size = self.word(16);
            let data_out = self.word(0);
            let data_size = self.word(4);
            self.call(
                "RegEnumValueW",
                &[
                    L(key),
                    I(0),
                    I(name_out),
                    I(name_size),
                    I(0),
                    I(0),
                    I(data_out),
                    I(data_size),
                ],
                0,
            );
            self.checks.extend([
                ("persistent value name length".into(), name_size, 9),
                ("persistent first spelling".into(), name_out, 0x0069006d),
                ("persistent value data".into(), data_out, 0x87654321),
            ]);
        }
        key
    }
    fn delete_metadata(&mut self, key: u32) {
        let value = self.name("MIXEDcase");
        self.call("RegDeleteValueW", &[L(key), I(value)], 0);
        let path = self.name("metadata fixture");
        self.call("RegDeleteKeyW", &[I(HKCU), I(path)], 0);
        self.call("RegFlushKey", &[I(HKCU)], 0);
    }
    fn run(&mut self) {
        let exit = self
            .runtime
            .machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        self.asm.push_imm(0).call(exit);
        self.runtime
            .machine
            .cpu
            .write(0x401000, &self.asm.bytes)
            .unwrap();
        loop {
            match self.runtime.step_for(Duration::from_millis(8)) {
                SliceResult::Yielded => {}
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        for (name, at, expected) in &self.checks {
            assert_eq!(
                self.runtime.machine.cpu.read_u32(*at).unwrap(),
                *expected,
                "{name}"
            );
        }
    }
}

#[test]
fn metadata_spelling_classes_and_deletion_survive_runtime_reopen() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/registry-metadata-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("registry-test.exe");
    std::fs::write(&exe, executable()).unwrap();
    for backend in ["unicorn", "interpreter", "jit"] {
        let data = root.join(backend);
        let mut writer = Program::new(&exe, &data, backend);
        writer.metadata(true);
        writer.run();
        drop(writer);
        let mut reader = Program::new(&exe, &data, backend);
        let key = reader.metadata(false);
        reader.delete_metadata(key);
        reader.run();
        drop(reader);
        let mut verify = Program::new(&exe, &data, backend);
        let path = verify.name("Metadata Fixture");
        let out = verify.word(0);
        verify.call("RegOpenKeyExW", &[I(HKCU), I(path), I(0), I(1), I(out)], 2);
        verify.run();
    }
}

// spec: HOST-SAVE-1
#[test]
// spec: WIN32-OBJECT-5
fn nonvolatile_flush_and_drop_survive_runtime_reopen_volatile_and_corruption_do_not() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/registry-runtime-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("registry-test.exe");
    std::fs::write(&exe, executable()).unwrap();
    for backend in ["unicorn", "interpreter", "jit"] {
        let data = root.join(backend);
        let store = data.join("registry/registry-v1.bin");
        let mut writer = Program::new(&exe, &data, backend);
        writer.write(0x12345678, true);
        writer.run();
        assert!(
            store.exists(),
            "explicit Flush writes before Runtime destruction"
        );
        let mut busy = Program::new(&exe, &data, backend);
        busy.busy();
        busy.run();
        drop(busy);
        drop(writer);
        let mut reader = Program::new(&exe, &data, backend);
        reader.read(0x12345678);
        reader.run();
        drop(reader);
        let mut writer = Program::new(&exe, &data, backend);
        writer.write(0xabcdef01, false);
        writer.run();
        drop(writer);
        let mut reader = Program::new(&exe, &data, backend);
        reader.read(0xabcdef01);
        reader.run();
        drop(reader);
        let mut damaged = std::fs::read(&store).unwrap();
        damaged[20] ^= 1;
        std::fs::write(&store, &damaged).unwrap();
        let mut reader = Program::new(&exe, &data, backend);
        reader.corrupt();
        reader.run();
        drop(reader);
        assert_eq!(
            std::fs::read(&store).unwrap(),
            damaged,
            "failed loads and writes preserve the damaged original"
        );
        assert!(
            !std::fs::read_dir(store.parent().unwrap())
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-")),
            "no unfinished temporary files"
        );
    }
}
