//! SEC_IMAGE file mappings lay out private, relocated image copies without
//! loading a module, and copies of system DLLs call the host implementations.
use std::path::PathBuf;

use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const PREFERRED: u32 = 0x1000_0000;
const SEC_IMAGE_READONLY: u32 = 0x0100_0002;
const MEM_IMAGE: u32 = 0x100_0000;

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

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A DLL with a writable, executable section holding one HIGHLOW fixup at
/// RVA 0x1000, and a read-only section at RVA 0x2000.
fn dll() -> Vec<u8> {
    let mut file = vec![0; 0x600];
    file[..2].copy_from_slice(b"MZ");
    put32(&mut file, 0x3c, 0x80);
    file[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut file, 0x84, 0x14c);
    put16(&mut file, 0x86, 2);
    put16(&mut file, 0x94, 224);
    put16(&mut file, 0x96, 0x2102);
    let optional = 0x98;
    put16(&mut file, optional, 0x10b);
    put32(&mut file, optional + 28, PREFERRED);
    put32(&mut file, optional + 32, 0x1000);
    put32(&mut file, optional + 36, 0x200);
    put16(&mut file, optional + 40, 5);
    put16(&mut file, optional + 48, 5);
    put32(&mut file, optional + 56, 0x3000);
    put32(&mut file, optional + 60, 0x200);
    put16(&mut file, optional + 68, 2);
    put32(&mut file, optional + 92, 16);
    put32(&mut file, optional + 96 + 5 * 8, 0x1100);
    put32(&mut file, optional + 100 + 5 * 8, 12);
    for (index, (name, va, raw, flags)) in [
        (b".text".as_slice(), 0x1000, 0x200, 0xe000_0020u32),
        (b".rdata".as_slice(), 0x2000, 0x400, 0x4000_0040),
    ]
    .into_iter()
    .enumerate()
    {
        let at = optional + 224 + index * 40;
        file[at..at + name.len()].copy_from_slice(name);
        put32(&mut file, at + 8, 0x1000);
        put32(&mut file, at + 12, va);
        put32(&mut file, at + 16, 0x200);
        put32(&mut file, at + 20, raw);
        put32(&mut file, at + 36, flags);
    }
    put32(&mut file, 0x200, PREFERRED + 0x1004);
    put32(&mut file, 0x300, 0x1000);
    put32(&mut file, 0x304, 12);
    put16(&mut file, 0x308, 0x3000);
    file
}

fn machine(jit: bool, name: &str) -> Machine<JitCpu> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/image-section-tests/{}-{name}-{jit}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/fixture.dll"), dll()).unwrap();
    std::fs::write(root.join("game/text.txt"), b"not an image").unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
    )
    .unwrap();
    for address in [CODE, DATA] {
        assert!(m.sys.vm.reserve_at(address, 0x1000));
    }
    m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
    m
}

fn run(m: &mut Machine<JitCpu>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("image sections", CODE, &[], 0x10000).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

fn words(m: &Machine<JitCpu>, address: u32, count: u32) -> Vec<u32> {
    (0..count)
        .map(|i| m.cpu.read_u32(address + i * 4).unwrap())
        .collect()
}

#[test]
// spec: WIN32-IMAGE-8
fn image_views_relocate_private_copies_with_section_protections() {
    for jit in [false, true] {
        let mut m = machine(jit, "fixture");
        // Occupy the preferred base so the view must relocate.
        let _ = m.sys.vm.reserve_at(PREFERRED, 0x1000);
        m.cpu
            .write(DATA + 0x400, b"fixture.dll\0text.txt\0")
            .unwrap();
        let (file, section, view, text, bad, error) =
            (DATA, DATA + 4, DATA + 8, DATA + 12, DATA + 16, DATA + 20);
        let mut code = Asm::new(CODE);
        let open = |m: &mut Machine<JitCpu>, code: &mut Asm, path, out| {
            let args = [V(path), V(0x8000_0000), V(1), V(0), V(3), V(0), V(0)];
            call(m, code, "CreateFileA", &args, out);
        };
        open(&mut m, &mut code, DATA + 0x400, file);
        let create = [S(file), V(0), V(SEC_IMAGE_READONLY), V(0), V(0), V(0)];
        call(&mut m, &mut code, "CreateFileMappingA", &create, section);
        let write = [S(section), V(2), V(0), V(0), V(0)];
        call(&mut m, &mut code, "MapViewOfFile", &write, DATA + 24);
        call(&mut m, &mut code, "GetLastError", &[], DATA + 28);
        let read = [S(section), V(4), V(0), V(0), V(0)];
        call(&mut m, &mut code, "MapViewOfFile", &read, view);
        let query = m.resolve_import("kernel32.dll", "VirtualQuery").unwrap();
        for (offset, out) in [(0, 0x100), (0x1000, 0x120), (0x2000, 0x140)] {
            code.push_imm(28).push_imm(DATA + out);
            code.mov_eax_mem(view)
                .add_eax_imm(offset)
                .push_eax()
                .call(query);
        }
        open(&mut m, &mut code, DATA + 0x40c, text);
        let create = [S(text), V(0), V(SEC_IMAGE_READONLY), V(0), V(0), V(0)];
        call(&mut m, &mut code, "CreateFileMappingA", &create, bad);
        call(&mut m, &mut code, "GetLastError", &[], error);
        run(&mut m, code);

        let base = m.cpu.read_u32(view).unwrap();
        assert_ne!(m.cpu.read_u32(section).unwrap(), 0);
        assert_eq!(words(&m, DATA + 24, 2), [0, 5]);
        assert_ne!(base, 0);
        assert_ne!(base, PREFERRED);
        assert_eq!(m.cpu.read_u32(base + 0x1000).unwrap(), base + 0x1004);
        let header = words(&m, DATA + 0x100, 7);
        assert_eq!(
            [header[0], header[1], header[2], header[5], header[6]],
            [base, base, 0x80, 2, MEM_IMAGE]
        );
        assert_eq!(words(&m, DATA + 0x120, 7)[5], 0x80);
        assert_eq!(words(&m, DATA + 0x140, 7)[5], 2);
        assert_eq!(words(&m, bad, 2), [0, 193]);
    }
}

#[test]
// spec: WIN32-IMAGE-8
fn system_image_copies_call_the_host_exports() {
    let mut m = machine(false, "system");
    let path = "C:\\Windows\\system32\\kernel32.dll";
    let bytes = adv_os::system_files::SystemFiles::default()
        .open(&mut m.cpu, &mut m.sys, path)
        .unwrap()
        .unwrap();
    let rva = adv_pe::Pe::parse(&bytes)
        .unwrap()
        .exports()
        .unwrap()
        .into_iter()
        .find(|export| export.name.as_deref() == Some("GetCurrentProcessId"))
        .map(|export| match export.target {
            adv_pe::ExportTarget::Rva(rva) => rva,
            other => panic!("{other:?}"),
        })
        .unwrap();
    m.cpu.write(DATA + 0x400, path.as_bytes()).unwrap();
    let (file, section, view) = (DATA, DATA + 4, DATA + 8);
    let mut code = Asm::new(CODE);
    let args = [
        V(DATA + 0x400),
        V(0x8000_0000),
        V(1),
        V(0),
        V(3),
        V(0),
        V(0),
    ];
    call(&mut m, &mut code, "CreateFileA", &args, file);
    let create = [S(file), V(0), V(SEC_IMAGE_READONLY), V(0), V(0), V(0)];
    call(&mut m, &mut code, "CreateFileMappingA", &create, section);
    let read = [S(section), V(4), V(0), V(0), V(0)];
    call(&mut m, &mut code, "MapViewOfFile", &read, view);
    // call [view] + rva
    code.mov_eax_mem(view).add_eax_imm(rva);
    code.bytes.extend_from_slice(&[0xff, 0xd0]);
    code.mov_mem_eax(DATA + 12);
    call(&mut m, &mut code, "GetCurrentProcessId", &[], DATA + 16);
    let unmap = [S(view)];
    call(&mut m, &mut code, "UnmapViewOfFile", &unmap, DATA + 20);
    run(&mut m, code);

    let values = words(&m, DATA + 8, 4);
    assert_ne!(values[0], 0);
    assert_ne!(values[2], 0);
    assert_eq!(values[1], values[2]);
    assert_eq!(values[3], 1);
}
