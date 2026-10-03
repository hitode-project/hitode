//! Guest-only private-registry tests. The authored image has no native registry
//! imports and is never executed as a host Windows program.
#![cfg(windows)]
use std::path::Path;
use std::time::Duration;

use adv_runtime::boot::Runtime;
use pal_cpu::asm::Asm;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::{Clock, RunResult, SliceResult};
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
