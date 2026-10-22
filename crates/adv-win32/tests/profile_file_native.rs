#![cfg(windows)]
//! Native control for the authored INI structure encoding and section buffers.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
use std::os::windows::ffi::OsStrExt;
#[link(name = "kernel32")]
unsafe extern "system" {
    fn WritePrivateProfileStructW(
        section: *const u16,
        key: *const u16,
        bytes: *const u8,
        length: u32,
        file: *const u16,
    ) -> i32;
    fn GetPrivateProfileStructW(
        section: *const u16,
        key: *const u16,
        bytes: *mut u8,
        length: u32,
        file: *const u16,
    ) -> i32;
    fn GetPrivateProfileSectionW(
        section: *const u16,
        output: *mut u16,
        capacity: u32,
        file: *const u16,
    ) -> u32;
    fn WritePrivateProfileStringW(
        section: *const u16,
        key: *const u16,
        value: *const u16,
        file: *const u16,
    ) -> i32;
}
fn units(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
#[test]
// spec: PERS-PROFILEFILE-1, PERS-PROFILEFILE-2
fn ini_structure_bytes_and_section_buffers_match_native_windows() {
    for jit in [false, true] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/native-profile-structure")
            .join(format!("{}-{jit}", std::process::id()));
        std::fs::create_dir_all(root.join("game")).unwrap();
        let initial = [0xff, 0xfe]
            .into_iter()
            .chain("[S]\r\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect::<Vec<_>>();
        let native_file = root.join("native.ini");
        std::fs::write(&native_file, &initial).unwrap();
        std::fs::write(root.join("game/profile.ini"), &initial).unwrap();
        let native_path = native_file
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>();
        let section = units("S");
        let key = units("blob");
        let payload = [0, 128, 255, 127];
        assert_ne!(
            unsafe {
                WritePrivateProfileStructW(
                    section.as_ptr(),
                    key.as_ptr(),
                    payload.as_ptr(),
                    payload.len() as u32,
                    native_path.as_ptr(),
                )
            },
            0
        );
        let mut native_bytes = [0xcd; 8];
        assert_ne!(
            unsafe {
                GetPrivateProfileStructW(
                    section.as_ptr(),
                    key.as_ptr(),
                    native_bytes.as_mut_ptr(),
                    payload.len() as u32,
                    native_path.as_ptr(),
                )
            },
            0
        );
        let mut native_section = [0xcccc; 128];
        let native_count = unsafe {
            GetPrivateProfileSectionW(
                section.as_ptr(),
                native_section.as_mut_ptr(),
                native_section.len() as u32,
                native_path.as_ptr(),
            )
        };
        unsafe {
            WritePrivateProfileStringW(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                native_path.as_ptr(),
            );
        }
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
        )
        .unwrap();
        const CODE: u32 = 0x400000;
        const DATA: u32 = 0x410000;
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        for (offset, value) in [(0x100, "S"), (0x200, "blob"), (0x300, ".\\profile.ini")] {
            machine
                .cpu
                .write(
                    DATA + offset,
                    &units(value)
                        .into_iter()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
        }
        machine.cpu.write(DATA + 0x400, &payload).unwrap();
        machine.cpu.write(DATA + 0x500, &[0xcd; 8]).unwrap();
        machine.cpu.write(DATA + 0x600, &[0xcc; 256]).unwrap();
        let write = machine
            .resolve_import("kernel32.dll", "WritePrivateProfileStructW")
            .unwrap();
        let read = machine
            .resolve_import("kernel32.dll", "GetPrivateProfileStructW")
            .unwrap();
        let section = machine
            .resolve_import("kernel32.dll", "GetPrivateProfileSectionW")
            .unwrap();
        let mut code = Asm::new(CODE);
        code.push_imm(DATA + 0x300)
            .push_imm(4)
            .push_imm(DATA + 0x400)
            .push_imm(DATA + 0x200)
            .push_imm(DATA + 0x100)
            .call(write)
            .mov_mem_eax(DATA)
            .push_imm(DATA + 0x300)
            .push_imm(4)
            .push_imm(DATA + 0x500)
            .push_imm(DATA + 0x200)
            .push_imm(DATA + 0x100)
            .call(read)
            .mov_mem_eax(DATA + 4)
            .push_imm(DATA + 0x300)
            .push_imm(128)
            .push_imm(DATA + 0x600)
            .push_imm(DATA + 0x100)
            .call(section)
            .mov_mem_eax(DATA + 8)
            .mov_mem_imm(DATA + 12, 0)
            .mov_eax_mem(DATA + 12)
            .ret();
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine
            .spawn("INI structure control", CODE, &[], 65536)
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), native_count);
        let mut guest_bytes = [0; 8];
        machine.cpu.read(DATA + 0x500, &mut guest_bytes).unwrap();
        assert_eq!(guest_bytes, native_bytes);
        let mut guest_section = [0; 256];
        machine.cpu.read(DATA + 0x600, &mut guest_section).unwrap();
        assert_eq!(
            guest_section.to_vec(),
            native_section
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let guest_path = machine
            .ext
            .get::<Win32>()
            .vfs
            .lookup(".\\profile.ini")
            .unwrap();
        assert_eq!(
            std::fs::read(guest_path).unwrap(),
            std::fs::read(&native_file).unwrap()
        );
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
