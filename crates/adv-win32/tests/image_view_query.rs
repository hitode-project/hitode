//! A relocated SEC_IMAGE view as native Windows describes it: the mapped header
//! carries the load base, the view is PAGE_EXECUTE_WRITECOPY image memory, and
//! the rest of its last allocation granule is free address space.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::Process;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const PREFERRED: u32 = 0x1000_0000;
const IMAGE_SIZE: u32 = 0x3000;
const GRANULE: u32 = 0x1_0000;
const MEM_COMMIT: u32 = 0x1000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_IMAGE: u32 = 0x100_0000;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A PE32 DLL with one section and one HIGHLOW fixup at RVA 0x1000.
fn dll() -> Vec<u8> {
    let mut file = vec![0; 0x400];
    file[..2].copy_from_slice(b"MZ");
    put32(&mut file, 0x3c, 0x80);
    file[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut file, 0x84, 0x14c);
    put16(&mut file, 0x86, 1);
    put16(&mut file, 0x94, 224);
    put16(&mut file, 0x96, 0x2102);
    let optional = 0x98;
    put16(&mut file, optional, 0x10b);
    put32(&mut file, optional + 28, PREFERRED);
    put32(&mut file, optional + 32, 0x1000);
    put32(&mut file, optional + 36, 0x200);
    put16(&mut file, optional + 40, 5);
    put16(&mut file, optional + 48, 5);
    put32(&mut file, optional + 56, IMAGE_SIZE);
    put32(&mut file, optional + 60, 0x200);
    put16(&mut file, optional + 68, 2);
    put16(&mut file, optional + 70, 0x40);
    put32(&mut file, optional + 92, 16);
    put32(&mut file, optional + 96 + 5 * 8, 0x1100);
    put32(&mut file, optional + 100 + 5 * 8, 12);
    let section = optional + 224;
    file[section..section + 5].copy_from_slice(b".text");
    put32(&mut file, section + 8, IMAGE_SIZE - 0x1000);
    put32(&mut file, section + 12, 0x1000);
    put32(&mut file, section + 16, 0x200);
    put32(&mut file, section + 20, 0x200);
    put32(&mut file, section + 36, 0xe000_0020);
    put32(&mut file, 0x200, PREFERRED + 0x1004);
    put32(&mut file, 0x300, 0x1000);
    put32(&mut file, 0x304, 12);
    put16(&mut file, 0x308, 0x3000);
    file
}

struct Files;
impl adv_os::FileSource for Files {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        path.eq_ignore_ascii_case("fixture.dll").then(dll)
    }
}

fn words(machine: &Machine<JitCpu>, address: u32) -> [u32; 7] {
    std::array::from_fn(|i| machine.cpu.read_u32(address + i as u32 * 4).unwrap())
}

#[test]
fn relocated_image_publishes_its_base_and_frees_the_granule_tail() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                "/nonexistent/image-view",
                "fixture.exe",
                "/nonexistent/image-view-data",
            ),
        )
        .unwrap();
        // Occupy the preferred base, unless the runtime already does, so the
        // loader must relocate the view.
        let _ = machine.sys.vm.reserve_at(PREFERRED, 0x1000);
        assert!(machine.sys.vm.containing(PREFERRED).is_some());
        let id = machine.load(&Files, "fixture.dll").unwrap();
        let base = adv_os::modules(&machine.ext).get(id).base;
        assert_ne!(base, PREFERRED);
        assert_eq!(base % GRANULE, 0);
        assert_eq!(machine.cpu.read_u32(base + 0x80 + 24 + 28).unwrap(), base);
        assert_eq!(machine.cpu.read_u32(base + 0x1000).unwrap(), base + 0x1004);

        for address in [CODE, DATA] {
            assert!(machine.sys.vm.reserve_at(address, 0x1000));
        }
        machine.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
        let query = machine
            .resolve_import("kernel32.dll", "VirtualQuery")
            .unwrap();
        let mut code = Asm::new(CODE);
        for (address, output) in [(base, DATA), (base + IMAGE_SIZE, DATA + 0x40)] {
            code.push_imm(28)
                .push_imm(output)
                .push_imm(address)
                .call(query);
        }
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine.spawn("image query", CODE, &[], 0x10000).unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");

        let image = words(&machine, DATA);
        assert_eq!([image[0], image[1], image[2]], [base, base, 0x80]);
        assert_eq!([image[4], image[6]], [MEM_COMMIT, MEM_IMAGE]);
        let tail = words(&machine, DATA + 0x40);
        assert_eq!(
            [tail[0], tail[1], tail[2], tail[4], tail[5], tail[6]],
            [base + IMAGE_SIZE, 0, 0, MEM_FREE, 1, 0]
        );
        assert!(tail[3] >= GRANULE - IMAGE_SIZE);
        // The unused part of the granule still cannot host a new reservation.
        let next = machine.sys.vm.reserve(0x1000).unwrap();
        assert!(next >= base + GRANULE || next + 0x1000 <= base);
    }
}

/// The authored native-cow-1 fixture: three RW and two RWX pages, including
/// zero-filled tails. Relocation occurs in the RWX section before publication.
fn copy_dll() -> Vec<u8> {
    let mut file = dll();
    file.resize(0x2400, 0);
    file[0x200..].fill(0);
    put16(&mut file, 0x86, 2);
    put32(&mut file, 0x98 + 56, 0x6000);
    put32(&mut file, 0x98 + 96 + 5 * 8, 0x4100);
    for (index, (name, va, size, raw, flags)) in [
        (b".rw".as_slice(), 0x1000, 0x3000, 0x200, 0xc0000040),
        (b".rwx".as_slice(), 0x4000, 0x2000, 0x1200, 0xe0000020),
    ]
    .into_iter()
    .enumerate()
    {
        let at = 0x98 + 224 + index * 40;
        file[at..at + 40].fill(0);
        file[at..at + name.len()].copy_from_slice(name);
        put32(&mut file, at + 8, size);
        put32(&mut file, at + 12, va);
        put32(&mut file, at + 16, 0x1000);
        put32(&mut file, at + 20, raw);
        put32(&mut file, at + 36, flags);
    }
    put32(&mut file, 0x1200, PREFERRED + 0x1004);
    put32(&mut file, 0x1300, 0x4000);
    put32(&mut file, 0x1304, 12);
    put16(&mut file, 0x1308, 0x3000);
    file[0x1210..0x1216].copy_from_slice(&[0xb8, 37, 0, 0, 0, 0xc3]);
    file
}
struct CopyFiles;
impl adv_os::FileSource for CopyFiles {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        path.eq_ignore_ascii_case("copy.dll").then(copy_dll)
    }
}
fn copy_query(code: &mut Asm, api: u32, base: u32, rva: u32, slot: u32) {
    code.push_imm(28)
        .push_imm(DATA + slot)
        .push_imm(base + rva)
        .call(api);
}
fn copy_protect(code: &mut Asm, api: u32, address: u32, protection: u32) {
    code.push_imm(DATA + 0xf00)
        .push_imm(protection)
        .push_imm(4096)
        .push_imm(address)
        .call(api);
}
fn image_copy_contract<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/image-copy",
            "fixture.exe",
            "/nonexistent/image-copy-data",
        ),
    )
    .unwrap();
    let id = machine.load(&CopyFiles, "copy.dll").unwrap();
    let base = adv_os::modules(&machine.ext).get(id).base;
    for address in [CODE, DATA] {
        assert!(machine.sys.vm.reserve_at(address, 4096));
    }
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let query = machine
        .resolve_import("kernel32.dll", "VirtualQuery")
        .unwrap();
    let protect = machine
        .resolve_import("kernel32.dll", "VirtualProtect")
        .unwrap();
    let mut code = Asm::new(CODE);
    for page in 0..6 {
        copy_query(&mut code, query, base, page * 4096, page * 0x40);
    }
    code.call(base + 0x4010).mov_mem_eax(DATA + 0x800);
    code.call(base + 0x4010).mov_mem_eax(DATA + 0x804);
    copy_protect(&mut code, protect, base + 0x4000, 0x20);
    copy_query(&mut code, query, base, 0x4000, 0x180);
    code.call(base + 0x4010).mov_mem_eax(DATA + 0x80c);
    copy_protect(&mut code, protect, base + 0x4000, 0x10);
    copy_query(&mut code, query, base, 0x4000, 0x400);
    code.call(base + 0x4010).mov_mem_eax(DATA + 0x810);
    copy_protect(&mut code, protect, base + 0x4000, 0x40);
    copy_query(&mut code, query, base, 0x4000, 0x1c0);
    // An actual guest store patches a previously executed instruction.
    code.bytes.extend([0xc7, 0x05]);
    code.bytes.extend((base + 0x4011).to_le_bytes());
    code.bytes.extend(21u32.to_le_bytes());
    copy_query(&mut code, query, base, 0x4000, 0x200);
    copy_query(&mut code, query, base, 0x5000, 0x240);
    code.call(base + 0x4010).mov_mem_eax(DATA + 0x808);
    // LOCK XADD follows the same first-store barrier as an ordinary write.
    code.bytes
        .extend([0xb8, 1, 0, 0, 0, 0xf0, 0x0f, 0xc1, 0x05]);
    code.bytes.extend((base + 0x1000).to_le_bytes());
    copy_query(&mut code, query, base, 0x1000, 0x280);
    copy_query(&mut code, query, base, 0x2000, 0x2c0);
    // REP STOSB must also retire only the page it actually writes.
    code.bytes.push(0xbf);
    code.bytes.extend((base + 0x3000).to_le_bytes());
    code.bytes
        .extend([0xb9, 0, 0x10, 0, 0, 0xb8, 0x63, 0, 0, 0, 0xf3, 0xaa]);
    copy_query(&mut code, query, base, 0x3000, 0x300);
    copy_query(&mut code, query, base, 0x2000, 0x340);
    copy_protect(&mut code, protect, base + 0x1000, 8);
    copy_query(&mut code, query, base, 0x1000, 0x380);
    copy_protect(&mut code, protect, base + 0x2000, 4);
    copy_query(&mut code, query, base, 0x2000, 0x3c0);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored image COPY", CODE, &[], GRANULE)
        .unwrap();
    let result = machine.run_virtual(5_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (slot, protect, len) in [
        (0, 2, 4096),
        (0x40, 8, 12288),
        (0x80, 8, 8192),
        (0xc0, 8, 4096),
        (0x100, 0x80, 8192),
        (0x140, 0x80, 4096),
        (0x180, 0x20, 4096),
        (0x1c0, 0x80, 8192),
        (0x200, 0x40, 4096),
        (0x240, 0x80, 4096),
        (0x280, 4, 4096),
        (0x2c0, 8, 8192),
        (0x300, 4, 4096),
        (0x340, 8, 4096),
        (0x380, 4, 4096),
        (0x3c0, 8, 4096),
        (0x400, 0x10, 4096),
    ] {
        let row: [u32; 7] =
            std::array::from_fn(|i| machine.cpu.read_u32(DATA + slot + i as u32 * 4).unwrap());
        assert_eq!(
            [row[1], row[2], row[3], row[4], row[5], row[6]],
            [base, 0x80, len, MEM_COMMIT, protect, MEM_IMAGE],
            "slot {slot:#x}"
        );
    }
    for (slot, expected) in [
        (0x800, 37),
        (0x804, 37),
        (0x808, 21),
        (0x80c, 37),
        (0x810, 37),
    ] {
        assert_eq!(machine.cpu.read_u32(DATA + slot).unwrap(), expected);
    }
    assert_eq!(
        machine.cpu.file_page_copy_on_write(base + 0x1000),
        Some(false)
    );
    assert_eq!(
        machine.cpu.file_page_copy_on_write(base + 0x2000),
        Some(true)
    );
    assert_eq!(
        machine.cpu.file_page_copy_on_write(base + 0x3000),
        Some(false)
    );
    assert_eq!(
        machine.cpu.file_page_copy_on_write(base + 0x4000),
        Some(false)
    );
    assert_eq!(
        machine.cpu.file_page_copy_on_write(base + 0x5000),
        Some(true)
    );
    {
        let (win32, modules) = machine.ext.get2_mut::<Win32, adv_os::Modules>();
        adv_os::Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
            .retire(id)
            .unwrap();
    }
    assert_eq!(machine.cpu.file_page_copy_on_write(base + 0x1000), None);
    let replacement = machine.load(&CopyFiles, "copy.dll").unwrap();
    assert_ne!(replacement, id);
    let fresh = adv_os::modules(&machine.ext).get(replacement).base;
    assert_eq!(fresh, base);
    assert_eq!(
        adv_os::modules(&machine.ext).image_protection(&machine.cpu, fresh + 0x1000),
        Some(8)
    );
    assert_eq!(
        machine.cpu.file_page_copy_on_write(fresh + 0x1000),
        Some(true)
    );
}
#[test]
fn image_copy_protection_tracks_first_stores_atomic_rep_and_code_reexecution() {
    for jit in [false, true] {
        image_copy_contract(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    image_copy_contract(adv_unicorn::UnicornCpu::new(PARK_ADDR).unwrap());
}

fn import_copy_dll() -> Vec<u8> {
    let mut file = copy_dll();
    put32(&mut file, 0x98 + 96 + 8, 0x1100);
    put32(&mut file, 0x98 + 100 + 8, 40);
    put32(&mut file, 0x300, 0x1180);
    put32(&mut file, 0x30c, 0x11c0);
    put32(&mut file, 0x310, 0x11a0);
    put32(&mut file, 0x380, 0x11d0);
    put32(&mut file, 0x3a0, 0x11d0);
    file[0x3c0..0x3cd].copy_from_slice(b"kernel32.dll\0");
    file[0x3d2..0x3df].copy_from_slice(b"GetTickCount\0");
    file
}
struct ImportCopyFiles;
impl adv_os::FileSource for ImportCopyFiles {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        path.eq_ignore_ascii_case("import-copy.dll")
            .then(import_copy_dll)
    }
}
/// Native Windows normal loading privatizes the whole IAT-containing section;
/// mapping-time HIGHLOW relocation retains COPY on its separate code section.
fn import_initial_copy<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/import-copy",
            "fixture.exe",
            "/nonexistent/import-copy-data",
        ),
    )
    .unwrap();
    let id = machine.load(&ImportCopyFiles, "import-copy.dll").unwrap();
    let module = adv_os::modules(&machine.ext).get(id);
    let base = module.base;
    assert_ne!(machine.cpu.read_u32(base + 0x11a0).unwrap(), 0x11d0);
    assert_eq!(machine.cpu.read_u32(base + 0x4000).unwrap(), base + 0x1004);
    for rva in [0x1000, 0x2000, 0x3000] {
        assert_eq!(machine.cpu.file_page_copy_on_write(base + rva), Some(false));
        assert_eq!(
            adv_os::modules(&machine.ext).image_protection(&machine.cpu, base + rva),
            Some(4)
        );
    }
    assert_eq!(
        machine.cpu.file_page_copy_on_write(base + 0x4000),
        Some(true)
    );
    assert_eq!(
        adv_os::modules(&machine.ext).image_protection(&machine.cpu, base + 0x4000),
        Some(0x80)
    );
}
#[test]
fn bound_imports_initialize_their_section_private_without_dirtying_relocation_pages() {
    for jit in [false, true] {
        import_initial_copy(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    import_initial_copy(adv_unicorn::UnicornCpu::new(PARK_ADDR).unwrap());
}
