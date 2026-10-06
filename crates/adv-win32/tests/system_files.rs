//! Virtual System32 files use the ordinary handle/mapping lifecycle.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as S, Value as V};
fn call(m: &mut Machine<JitCpu>, code: &mut Asm, name: &str, args: &[Arg], out: u32) {
    for arg in args.iter().rev() {
        match arg {
            V(value) => {
                code.push_imm(*value);
            }
            S(address) => {
                code.mov_eax_mem(*address).push_eax();
            }
        }
    }
    let thunk = m.resolve_import("kernel32.dll", name).unwrap();
    code.call(thunk).mov_mem_eax(out);
}

#[test]
fn system_pe_files_read_map_copy_and_close_without_loading_modules_or_writing_disk() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/system-file-tests/{jit}"));
        std::fs::create_dir_all(root.join("game")).unwrap();
        Win32::install(
            &mut m,
            Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let path = DATA + 0x100;
        let wide: Vec<_> = "C:\\Windows\\system32\\version.dll\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        m.cpu.write(path, &wide).unwrap();
        let (file, attrs, section, view, writable, denied, copy_section, copy_view) = (
            DATA,
            DATA + 4,
            DATA + 8,
            DATA + 12,
            DATA + 16,
            DATA + 20,
            DATA + 24,
            DATA + 28,
        );
        let mut code = Asm::new(CODE);
        call(
            &mut m,
            &mut code,
            "GetSystemDirectoryW",
            &[V(DATA + 0x300), V(128)],
            DATA + 32,
        );
        call(&mut m, &mut code, "GetFileAttributesW", &[V(path)], attrs);
        call(
            &mut m,
            &mut code,
            "CreateFileW",
            &[V(path), V(0x80000000), V(1), V(0), V(3), V(0), V(0)],
            file,
        );
        call(
            &mut m,
            &mut code,
            "GetFileSizeEx",
            &[S(file), V(DATA + 0x280)],
            DATA + 36,
        );
        call(
            &mut m,
            &mut code,
            "ReadFile",
            &[S(file), V(DATA + 0x250), V(2), V(DATA + 0x258), V(0)],
            DATA + 40,
        );
        call(
            &mut m,
            &mut code,
            "CreateFileMappingW",
            &[S(file), V(0), V(2), V(0), V(0), V(0)],
            section,
        );
        call(
            &mut m,
            &mut code,
            "MapViewOfFile",
            &[S(section), V(4), V(0), V(0), V(0)],
            view,
        );
        call(
            &mut m,
            &mut code,
            "CreateFileMappingW",
            &[S(file), V(0), V(4), V(0), V(0), V(0)],
            writable,
        );
        call(
            &mut m,
            &mut code,
            "CreateFileMappingW",
            &[S(file), V(0), V(8), V(0), V(0), V(0)],
            copy_section,
        );
        call(
            &mut m,
            &mut code,
            "MapViewOfFile",
            &[S(copy_section), V(1), V(0), V(0), V(0)],
            copy_view,
        );
        code.mov_eax_mem(copy_view)
            .bytes
            .extend_from_slice(&[0xc6, 0x00, b'Z']);
        call(
            &mut m,
            &mut code,
            "CreateFileW",
            &[V(path), V(0x40000000), V(1), V(0), V(3), V(0), V(0)],
            denied,
        );
        call(
            &mut m,
            &mut code,
            "CloseHandle",
            &[S(copy_section)],
            DATA + 44,
        );
        call(&mut m, &mut code, "CloseHandle", &[S(section)], DATA + 48);
        call(&mut m, &mut code, "CloseHandle", &[S(file)], DATA + 52);
        code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        m.cpu.write(CODE, &code.bytes).unwrap();
        m.start_main(CODE, CODE, 65536, "fixture.exe").unwrap();
        assert!(matches!(m.run(), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(attrs).unwrap(), 0x21);
        assert_ne!(m.cpu.read_u32(file).unwrap(), u32::MAX);
        assert!(m.cpu.read_u32(DATA + 0x280).unwrap() > 4096);
        assert_eq!(m.cpu.read_u16(DATA + 0x250).unwrap(), 0x5a4d);
        assert_eq!(m.cpu.read_u32(DATA + 0x258).unwrap(), 2);
        assert_eq!(m.cpu.read_u32(writable).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(denied).unwrap(), u32::MAX);
        let mapped = m.cpu.read_u32(view).unwrap();
        let copied = m.cpu.read_u32(copy_view).unwrap();
        assert_ne!(mapped, 0);
        assert_ne!(copied, 0);
        assert_eq!(m.cpu.read_u16(mapped).unwrap(), 0x5a4d);
        assert_eq!(m.cpu.read_u8(copied).unwrap(), b'Z');
        // The startup gate keeps NTDLL and kernel32 resident; reading the file
        // must not load it as a module.
        assert!(
            adv_os::modules(&m.ext)
                .iter()
                .all(|(_, module)| !module.name.eq_ignore_ascii_case("version.dll"))
        );
        assert_eq!(m.sys.objects.hosts::<adv_win32::FileObj>().count(), 0);
        assert!(!root.join("data/c/Windows/system32/version.dll").exists());
    }
}
