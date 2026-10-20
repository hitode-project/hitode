//! Authored file sequences compare read-ahead with an interposed unbuffered provider.
use super::*;
use adv_abi::Handle;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg, asm::Asm},
};
use adv_machine::{
    Clock, Machine, Resume,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_os::{Loader, Modules};
use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

const DATA: u32 = 0x410000;
const OUT: u32 = DATA + 4096;
const HOOK: u32 = 0x400000;
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    machine: Machine<JitCpu>,
    entry: u32,
    original: Vec<u8>,
    reads: Vec<u32>,
    leaves: usize,
}
impl Fixture {
    fn new(root: &Path) -> Self {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        super::super::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut config = crate::Win32Config::new(root, "fixture.exe", root.join("data"));
        config.os.write_game = true;
        crate::Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(DATA, 24576, Prot::RW).unwrap();
        machine.cpu.map(HOOK, 4096, Prot::RX).unwrap();
        machine.cpu.write(HOOK, &[0xc3]).unwrap();
        machine.cpu.protect(DATA + 20480, 4096, Prot::READ).unwrap();
        machine.cpu.write(DATA, b"stream.bin\0").unwrap();
        machine.cpu.write(DATA + 32, b"rb\0").unwrap();
        machine.cpu.write(DATA + 48, b"r+b\0").unwrap();
        machine.sys.current = machine
            .spawn("stdio buffering", PARK_ADDR, &[], 65536)
            .unwrap();
        machine.cpu.set_reg(Reg::Esp, DATA + 256);
        let (win32, modules) = machine.ext.get2_mut::<crate::Win32, Modules>();
        let mut loader = Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs);
        let module = loader.load("ntdll.dll").unwrap();
        let entry = loader
            .resolve_dynamic(
                module,
                &adv_pe::ImportName::Name {
                    hint: 0,
                    name: "NtReadFile".into(),
                },
            )
            .unwrap()
            .0;
        let mut original = vec![0; 16];
        machine.cpu.read(entry, &mut original).unwrap();
        Self {
            machine,
            entry,
            original,
            reads: vec![],
            leaves: 0,
        }
    }
    fn hook(&mut self, enabled: bool) {
        let mut jump = Asm::new(self.entry);
        jump.jmp(HOOK);
        self.machine
            .cpu
            .write(
                self.entry,
                if enabled { &jump.bytes } else { &self.original },
            )
            .unwrap();
    }
    fn env(&mut self, thunk: u32) -> Env<'_> {
        Env {
            context: self.machine.sys.current,
            cpu: &mut self.machine.cpu,
            sys: &mut self.machine.sys,
            ext: &mut self.machine.ext,
            thunk,
        }
    }
    fn binding(&mut self, module: &str, name: &str, args: &[u32]) -> (u32, adv_machine::HostFn) {
        let entry = self.machine.resolve_import(module, name).unwrap();
        let index = (entry - THUNK_BASE) / THUNK_STRIDE;
        for (i, &arg) in args.iter().enumerate() {
            self.machine
                .cpu
                .write_u32(DATA + 260 + i as u32 * 4, arg)
                .unwrap();
        }
        (index, self.machine.sys.thunks.get(index).f)
    }
    fn leaf(&mut self, module: &str, name: &str, args: &[u32]) -> Option<u64> {
        let (index, binding) = self.binding(module, name, args);
        binding.env_leaf.unwrap()(&mut self.env(index))
    }
    fn call(&mut self, module: &str, name: &str, args: &[u32], leaf: bool) -> u32 {
        let (index, binding) = self.binding(module, name, args);
        if leaf
            && let Some(handler) = binding.env_leaf
            && let Some(value) = handler(&mut self.env(index))
        {
            self.leaves += 1;
            return value as u32;
        }
        let mut outcome = (binding.handler)(&mut self.env(index));
        loop {
            match outcome {
                Outcome::Return(value) => return value,
                Outcome::Call {
                    target, args, then, ..
                } => {
                    assert_eq!(target, self.entry);
                    self.reads.push(args[6]);
                    let mut env = self.env(index);
                    let status = crate::ntdll::file::read(
                        &mut env,
                        Handle(args[0]),
                        Handle(args[1]),
                        GuestPtr::new(args[2]),
                        GuestPtr::new(args[3]),
                        GuestPtr::new(args[4]),
                        GuestPtr::new(args[5]),
                        args[6],
                        GuestPtr::new(args[7]),
                        GuestPtr::new(args[8]),
                    );
                    outcome = then(&mut env, Resume::Returned(status as u32));
                }
                Outcome::Fail(message) => panic!("{module}!{name}: {message}"),
                _ => panic!("unexpected stdio outcome"),
            }
        }
    }
    fn bytes(&self, address: u32, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        self.machine.cpu.read(address, &mut bytes).unwrap();
        bytes
    }
}

fn root() -> std::path::PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/crt-buffer-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).unwrap();
    root
}
fn contents() -> Vec<u8> {
    (0..12317).map(|i| (i * 37) as u8).collect()
}

// spec: WIN32-API-19, A64-CTL-12
#[test]
fn buffered_and_live_unbuffered_sequences_match() {
    let root = root();
    std::fs::write(root.join("stream.bin"), contents()).unwrap();
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        let mut observations = vec![];
        for hooked in [false, true] {
            let mut f = Fixture::new(&root);
            if hooked {
                f.hook(true);
            }
            let stream = f.call(module, "fopen", &[DATA, DATA + 32], false);
            assert_ne!(stream, 0);
            let errno = f.call(module, "_errno", &[], false);
            f.machine
                .cpu
                .write(errno, &[79i32.to_le_bytes(), 91u32.to_le_bytes()].concat())
                .unwrap();
            crate::util::set_last_error(&mut f.env(0), 77);
            let mut rows = vec![];
            for (name, args) in [
                ("fgetc", vec![stream]),
                ("fread", vec![OUT, 3, 1364, stream]),
                ("getc", vec![stream]),
                ("fread", vec![OUT, 1, 19, stream]),
                ("fseek", vec![stream, 0, 1]),
                ("fread", vec![OUT, 2, 3000, stream]),
                ("fseek", vec![stream, 7, 0]),
                ("fread", vec![OUT, 1, 9, stream]),
                ("fseek", vec![stream, (-3i32) as u32, 1]),
                ("fgetc", vec![stream]),
                ("fseek", vec![stream, 9000, 0]),
                ("fseek", vec![stream, (-20i32) as u32, 2]),
                ("fgetc", vec![stream]),
                ("fseek", vec![stream, 2, 1]),
            ] {
                let value = f.call(module, name, &args, true);
                let tell = f.call(module, "ftell", &[stream], true);
                assert_eq!(f.call(module, "fgetpos", &[stream, DATA + 80], true), 0);
                assert_eq!(
                    u64::from_le_bytes(f.bytes(DATA + 80, 8).try_into().unwrap()),
                    u64::from(tell)
                );
                let eof = f.call(module, "feof", &[stream], true);
                rows.push((
                    value,
                    tell,
                    eof,
                    f.bytes(OUT, 6000),
                    f.bytes(errno, 8),
                    crate::util::last_error(&f.env(0)),
                ));
            }
            assert_eq!(f.call(module, "fseek", &[stream, 0, 0], false), 0);
            let mut chars = vec![];
            loop {
                let ch = f.call(module, "fgetc", &[stream], true);
                chars.push(ch);
                if ch == u32::MAX {
                    break;
                }
            }
            assert_eq!(
                &chars[..chars.len() - 1],
                contents().iter().map(|&b| u32::from(b)).collect::<Vec<_>>()
            );
            assert_eq!(f.call(module, "feof", &[stream], true), 1);
            rows.push((
                0,
                f.call(module, "ftell", &[stream], true),
                1,
                vec![],
                f.bytes(errno, 8),
                crate::util::last_error(&f.env(0)),
            ));
            assert_eq!(f.call(module, "fclose", &[stream], false), 0);
            assert!(f.machine.ext.get::<CrtState>().files.is_empty());
            if hooked {
                assert!(f.reads.contains(&6000));
                assert!(f.reads.iter().filter(|&&n| n == 1).count() >= chars.len());
            } else {
                assert!(f.leaves > 15);
            }
            observations.push((rows, chars));
        }
        assert_eq!(observations[0], observations[1], "{module}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WIN32-API-19
#[test]
fn update_writes_and_flush_synchronize_the_original_handle() {
    let root = root();
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        let mut observations = vec![];
        for hooked in [false, true] {
            std::fs::write(root.join("stream.bin"), contents()).unwrap();
            let mut f = Fixture::new(&root);
            if hooked {
                f.hook(true);
            }
            let stream = f.call(module, "fopen", &[DATA, DATA + 48], false);
            let mut rows = vec![];
            rows.push(f.call(module, "fgetc", &[stream], true));
            let handle = f.machine.ext.get::<CrtState>().files[&stream].handle;
            let os = f
                .machine
                .sys
                .objects
                .host_mut::<crate::FileObj>(handle)
                .unwrap()
                .file
                .stream_position()
                .unwrap();
            assert_eq!(os, if hooked { 1 } else { 4096 });
            rows.push(f.call(module, "fflush", &[stream], false));
            let tell = f.call(module, "ftell", &[stream], true);
            assert_eq!(
                f.call("kernel32.dll", "SetFilePointer", &[handle, 0, 0, 1], false),
                tell
            );
            rows.push(f.call(module, "fgetc", &[stream], true));
            rows.push(f.call(module, "fseek", &[stream, 4, 0], false));
            f.machine.cpu.write(OUT, b"XY").unwrap();
            rows.push(f.call(module, "fwrite", &[OUT, 1, 2, stream], false));
            rows.push(f.call(module, "fseek", &[stream, 0, 0], false));
            rows.push(f.call(module, "fread", &[OUT, 1, 8, stream], true));
            rows.push(f.call(module, "ftell", &[stream], true));
            // A write directly after read-ahead also resumes at the logical cursor.
            f.machine.cpu.write(OUT + 16, b"Z").unwrap();
            rows.push(f.call(module, "fwrite", &[OUT + 16, 1, 1, stream], false));
            rows.push(f.call(module, "fclose", &[stream], false));
            observations.push((
                rows,
                f.bytes(OUT, 8),
                std::fs::read(root.join("stream.bin")).unwrap(),
            ));
        }
        assert_eq!(observations[0], observations[1]);
        assert_eq!(&observations[0].1[4..6], b"XY");
        assert_eq!(observations[0].2[8], b'Z');
    }
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WIN32-API-19, A64-CTL-12
#[test]
fn leaves_decline_before_output_or_cursor_changes_and_notice_late_hooks() {
    let root = root();
    std::fs::write(root.join("stream.bin"), contents()).unwrap();
    for module in [
        "msvcrt.dll",
        "msvcr80.dll",
        "msvcr90.dll",
        "ucrtbase.dll",
        "api-ms-win-crt-stdio-l1-1-0.dll",
    ] {
        let mut f = Fixture::new(&root);
        let modern = module.contains("ucrt") || module.starts_with("api-");
        let stream = if modern {
            f.machine
                .cpu
                .write(
                    DATA + 96,
                    &"stream.bin\0"
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            f.machine
                .cpu
                .write(
                    DATA + 128,
                    &"rb\0"
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            assert_eq!(
                f.call(
                    module,
                    "_wfopen_s",
                    &[DATA + 64, DATA + 96, DATA + 128],
                    false
                ),
                0
            );
            f.machine.cpu.read_u32(DATA + 64).unwrap()
        } else {
            f.call(module, "fopen", &[DATA, DATA + 32], false)
        };
        assert_eq!(f.leaf(module, "fread", &[OUT, 1, 1, stream]), None);
        assert_eq!(f.call(module, "fread", &[OUT, 1, 1, stream], false), 1);
        let before = f.bytes(OUT, 16384);
        for (output, length) in [(OUT, 4096), (DATA + 20476, 8), (u32::MAX - 2, 8)] {
            assert_eq!(f.leaf(module, "fread", &[output, 1, length, stream]), None);
            assert_eq!(f.machine.ext.get::<CrtState>().files[&stream].position, 1);
            assert_eq!(f.bytes(OUT, 16384), before);
        }
        assert_eq!(f.leaf(module, "ftell", &[stream]), Some(1));
        if !modern {
            assert_eq!(f.leaf(module, "fgetpos", &[stream, DATA + 20476]), None);
            assert_eq!(
                f.leaf(module, "getc", &[stream]),
                Some(u64::from(contents()[1]))
            );
        }
        let position = f.call(module, "ftell", &[stream], true);
        f.hook(true);
        assert_eq!(f.leaf(module, "fread", &[OUT, 1, 1, stream]), None);
        assert_eq!(f.leaf(module, "ftell", &[stream]), None);
        assert_eq!(
            f.machine.ext.get::<CrtState>().files[&stream].position,
            u64::from(position)
        );
        assert_eq!(f.call(module, "fread", &[OUT, 1, 1, stream], false), 1);
        assert_eq!(
            f.bytes(OUT, 1),
            contents()[position as usize..position as usize + 1]
        );
        assert_eq!(f.reads, [1]);
        assert_eq!(f.call(module, "fclose", &[stream], false), 0);
    }
    std::fs::remove_dir_all(root).unwrap();
}
