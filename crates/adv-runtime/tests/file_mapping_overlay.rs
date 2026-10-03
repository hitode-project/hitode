//! Writable views must preserve source assets even in portable-save mode.
use iced_x86::code_asm::*;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::{Clock, Machine, Registry, RunResult};
use pal_win32::{Win32, Win32Config};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "support/cpu.rs"]
mod cpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + 2048;

enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as S, Value as V};

struct Program {
    machine: Machine<cpu::Cpu>,
    code: CodeAssembler,
    next: u32,
}
impl Program {
    fn call(&mut self, name: &str, arguments: &[Arg]) -> u32 {
        for argument in arguments.iter().rev() {
            match argument {
                V(value) => self.code.push(*value).unwrap(),
                S(address) => self.code.push(dword_ptr(*address)).unwrap(),
            }
        }
        let address = self.machine.resolve_import("kernel32.dll", name).unwrap();
        self.code.call(u64::from(address)).unwrap();
        let output = self.next;
        self.next += 4;
        self.code.mov(dword_ptr(output), eax).unwrap();
        output
    }
    fn view_word(&mut self, view: u32) -> u32 {
        self.code.mov(eax, dword_ptr(view)).unwrap();
        self.code.mov(eax, dword_ptr(eax)).unwrap();
        let output = self.next;
        self.next += 4;
        self.code.mov(dword_ptr(output), eax).unwrap();
        output
    }
    fn value(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
}

#[test]
fn writable_mapping_copies_up_and_retargets_existing_handles_without_touching_assets() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/file-mapping-overlay")
        .join(format!("{}-{stamp}", std::process::id()));
    let original: Vec<_> = (0..8192).map(|index| (index % 251) as u8).collect();
    for backend in ["interpreter", "jit", "unicorn"] {
        for portable in [false, true] {
            let root = directory.join(format!("{backend}-{portable}"));
            let game = root.join("game");
            let profile = root.join("profile");
            std::fs::create_dir_all(&game).unwrap();
            std::fs::write(game.join("pack.bin"), &original).unwrap();
            let mut registry = Registry::default();
            pal_win32::register(&mut registry);
            let mut machine = Machine::new(
                cpu::Cpu::new(backend).unwrap(),
                registry,
                Clock::virtual_time(),
            )
            .unwrap();
            let mut config = Win32Config::new(&game, "authored.exe", &profile);
            config.write_game = portable;
            Win32::install(&mut machine, config).unwrap();
            machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
            machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
            machine.cpu.write(DATA, b"pack.bin\0").unwrap();
            machine.set_execution_budget(Some(Duration::from_secs(5)));
            let mut p = Program {
                machine,
                code: CodeAssembler::new(32).unwrap(),
                next: OUTPUT,
            };
            // Opening the writable FD first also tests the ordinary isolated
            // VFS copy-up. In portable mode the mapping itself must copy up.
            let writable = p.call(
                "CreateFileA",
                &[V(DATA), V(0xc0000000), V(3), V(0), V(3), V(0), V(0)],
            );
            let readonly = p.call(
                "CreateFileA",
                &[V(DATA), V(0x80000000), V(3), V(0), V(3), V(0), V(0)],
            );
            p.call("SetFilePointer", &[S(writable), V(31), V(0), V(0)]);
            p.call("SetFilePointer", &[S(readonly), V(17), V(0), V(0)]);
            let ro_section = p.call(
                "CreateFileMappingA",
                &[S(readonly), V(0), V(2), V(0), V(0), V(0)],
            );
            let ro_view = p.call("MapViewOfFile", &[S(ro_section), V(4), V(0), V(0), V(8192)]);
            let before = p.view_word(ro_view); // Materialize before copy-up.
            let rw_section = p.call(
                "CreateFileMappingA",
                &[S(writable), V(0), V(4), V(0), V(0), V(0)],
            );
            let rw_view = p.call("MapViewOfFile", &[S(rw_section), V(2), V(0), V(0), V(8192)]);
            p.code.mov(eax, dword_ptr(rw_view)).unwrap();
            p.code.mov(dword_ptr(eax), 0x13579bdfu32).unwrap();
            let shared = p.view_word(ro_view);
            let flushed = p.call("FlushViewOfFile", &[S(rw_view), V(0)]);
            let ro_cursor = p.call("SetFilePointer", &[S(readonly), V(0), V(0), V(1)]);
            let rw_cursor = p.call("SetFilePointer", &[S(writable), V(0), V(0), V(1)]);
            p.call("SetFilePointer", &[S(readonly), V(0), V(0), V(0)]);
            let read = p.call(
                "ReadFile",
                &[S(readonly), V(DATA + 128), V(4), V(DATA + 132), V(0)],
            );
            let close = [writable, readonly, ro_section, rw_section]
                .map(|handle| p.call("CloseHandle", &[S(handle)]));
            let retained = p.view_word(ro_view);
            let unmap = [ro_view, rw_view].map(|view| p.call("UnmapViewOfFile", &[S(view)]));
            p.call("ExitProcess", &[V(0)]);
            let code = p.code.assemble(u64::from(CODE)).unwrap();
            p.machine.cpu.write(CODE, &code).unwrap();
            p.machine
                .spawn("mapping copy-up", CODE, &[], 0x10000)
                .unwrap();
            assert!(
                matches!(p.machine.run_virtual(1_000_000), RunResult::Exited(0)),
                "{backend}/{portable}"
            );
            assert_eq!(
                p.value(before),
                u32::from_le_bytes(original[..4].try_into().unwrap())
            );
            assert_eq!(
                (p.value(shared), p.value(retained)),
                (0x13579bdf, 0x13579bdf)
            );
            assert_eq!((p.value(ro_cursor), p.value(rw_cursor)), (17, 31));
            assert_eq!(
                (p.value(read), p.value(DATA + 128), p.value(DATA + 132)),
                (1, 0x13579bdf, 4)
            );
            for output in close.into_iter().chain(unmap).chain([flushed]) {
                assert_eq!(p.value(output), 1, "{backend}/{portable}: cleanup/flush");
            }
            assert_eq!(std::fs::read(game.join("pack.bin")).unwrap(), original);
            let mut expected = original.clone();
            expected[..4].copy_from_slice(&0x13579bdfu32.to_le_bytes());
            assert_eq!(
                std::fs::read(profile.join("overlay/pack.bin")).unwrap(),
                expected
            );
        }
    }
}
