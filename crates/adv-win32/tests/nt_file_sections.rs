//! Authored guest calls exercise NT adapters against real VFS file owners.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const FILE: u32 = DATA;
const SECTION: u32 = DATA + 4;
const VIEW: u32 = DATA + 8;
const VIEW_SIZE: u32 = DATA + 12;
const PROTECT_BASE: u32 = DATA + 16;
const PROTECT_SIZE: u32 = DATA + 20;
const OLD_PROTECT: u32 = DATA + 24;
const OWNER: u32 = DATA + 28;
const FAILED_FILE: u32 = DATA + 32;
const HEAD: u32 = DATA + 36;
const TAIL: u32 = DATA + 40;
const ALLOCATION: u32 = DATA + 44;
const NT_PATH: u32 = DATA + 0x100;
const WIN_PATH: u32 = DATA + 0x180;
const NAME: u32 = DATA + 0x200;
const ATTRS: u32 = DATA + 0x220;
const IOSB: u32 = DATA + 0x240;
const FAILED_IOSB: u32 = DATA + 0x248;
const RESULTS: u32 = DATA + 0x800;

enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as S, Value as V};

fn call<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    dll: &str,
    name: &str,
    args: &[Arg],
    result: u32,
) {
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
    code.call(machine.resolve_import(dll, name).unwrap())
        .mov_mem_eax(result);
}

fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[test]
fn nt_file_views_outlive_handles_obey_shared_open_rules_and_round_protection() {
    for jit in [false, true] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/nt-file-section-tests")
            .join(format!("{}-{jit}", std::process::id()));
        std::fs::create_dir_all(root.join("game")).unwrap();
        let mut bytes = [0x5a; 64];
        bytes[..4].copy_from_slice(&0x44332211u32.to_le_bytes());
        bytes[60..].copy_from_slice(&0xddccbbaau32.to_le_bytes());
        let source = root.join("game/映射.bin");
        std::fs::write(&source, bytes).unwrap();
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
            Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let path = wide(r"\??\C:\Game\映射.bin");
        machine.cpu.write(NT_PATH, &path).unwrap();
        // The DOS spelling normalizes to the same NT file-sharing key.
        machine
            .cpu
            .write(WIN_PATH, &wide("c:/game/./映射.bin"))
            .unwrap();
        machine
            .cpu
            .write(NAME, &((path.len() - 2) as u16).to_le_bytes())
            .unwrap();
        machine
            .cpu
            .write(NAME + 2, &(path.len() as u16).to_le_bytes())
            .unwrap();
        machine.cpu.write_u32(NAME + 4, NT_PATH).unwrap();
        for (offset, value) in [(0, 24), (4, 0), (8, NAME), (12, 0x40), (16, 0), (20, 0)] {
            machine.cpu.write_u32(ATTRS + offset, value).unwrap();
        }
        machine.cpu.write_u32(FAILED_FILE, 0x12345678).unwrap();
        machine.cpu.write_u32(FAILED_IOSB, 0x11111111).unwrap();
        machine.cpu.write_u32(FAILED_IOSB + 4, 0x22222222).unwrap();
        let mut code = Asm::new(CODE);
        // Allocate before mapping so a later allocation cannot reuse the
        // unmapped view address and invalidate the final unmap assertion.
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "VirtualAlloc",
            &[V(0), V(8192), V(0x3000), V(4)],
            ALLOCATION,
        );
        // Specific FILE_READ_DATA, READ_ATTRIBUTES and SYNCHRONIZE rights must
        // produce a readable section without relying on a GENERIC_READ request.
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtOpenFile",
            &[V(FILE), V(0x00100081), V(ATTRS), V(IOSB), V(0), V(0x60)],
            RESULTS,
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "CreateFileW",
            &[V(WIN_PATH), V(0x80000000), V(7), V(0), V(3), V(0), V(0)],
            RESULTS + 4,
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "GetLastError",
            &[],
            RESULTS + 8,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtCreateSection",
            &[V(SECTION), V(4), V(0), V(0), V(2), V(0x08000000), S(FILE)],
            RESULTS + 12,
        );
        code.mov_mem_imm(VIEW, 0).mov_mem_imm(VIEW_SIZE, 0);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtMapViewOfSection",
            &[
                S(SECTION),
                V(u32::MAX),
                V(VIEW),
                V(0),
                V(0),
                V(0),
                V(VIEW_SIZE),
                V(2),
                V(0),
                V(2),
            ],
            RESULTS + 16,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtClose",
            &[S(FILE)],
            RESULTS + 20,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtClose",
            &[S(SECTION)],
            RESULTS + 24,
        );
        code.mov_eax_mem(VIEW).mov_eax_ptr_eax().mov_mem_eax(HEAD);
        code.mov_eax_mem(VIEW)
            .add_eax_imm(60)
            .mov_eax_ptr_eax()
            .mov_mem_eax(TAIL);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtUnmapViewOfSection",
            &[V(u32::MAX), S(VIEW)],
            RESULTS + 28,
        );

        // The same shared registry must reject the reverse API combination.
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "CreateFileW",
            &[V(WIN_PATH), V(0x80000000), V(0), V(0), V(3), V(0), V(0)],
            OWNER,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtOpenFile",
            &[
                V(FAILED_FILE),
                V(0x00100081),
                V(ATTRS),
                V(FAILED_IOSB),
                V(7),
                V(0x60),
            ],
            RESULTS + 32,
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "CloseHandle",
            &[S(OWNER)],
            RESULTS + 36,
        );

        code.mov_eax_mem(ALLOCATION)
            .add_eax_imm(4095)
            .mov_mem_eax(PROTECT_BASE);
        code.mov_mem_imm(PROTECT_SIZE, 2);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll",
            "NtProtectVirtualMemory",
            &[
                V(u32::MAX),
                V(PROTECT_BASE),
                V(PROTECT_SIZE),
                V(2),
                V(OLD_PROTECT),
            ],
            RESULTS + 40,
        );
        code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine.spawn("NT file sections", CODE, &[], 65536).unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "jit={jit}: {result:?}"
        );

        for offset in [0, 12, 16, 20, 24, 28, 40] {
            assert_eq!(
                machine.cpu.read_u32(RESULTS + offset).unwrap(),
                0,
                "jit={jit}, result={offset}"
            );
        }
        assert_eq!(machine.cpu.read_u32(RESULTS + 4).unwrap(), u32::MAX);
        assert_eq!(machine.cpu.read_u32(RESULTS + 8).unwrap(), 32);
        assert_eq!(machine.cpu.read_u32(IOSB).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(IOSB + 4).unwrap(), 1); // FILE_OPENED
        assert_eq!(machine.cpu.read_u32(HEAD).unwrap(), 0x44332211);
        assert_eq!(machine.cpu.read_u32(TAIL).unwrap(), 0xddccbbaa);
        assert_eq!(machine.cpu.read_u32(VIEW_SIZE).unwrap(), 4096);
        let view = machine.cpu.read_u32(VIEW).unwrap();
        assert_ne!(view, 0);
        assert!(!machine.cpu.accessible(view, 1, Prot::READ));
        assert_ne!(machine.cpu.read_u32(OWNER).unwrap(), u32::MAX);
        assert_eq!(machine.cpu.read_u32(RESULTS + 32).unwrap(), 0xc0000043);
        // Native x86 clears FileHandle once OBJECT_ATTRIBUTES were accepted,
        // but a failed open never writes the IOSB.
        assert_eq!(machine.cpu.read_u32(FAILED_FILE).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(FAILED_IOSB).unwrap(), 0x11111111);
        assert_eq!(machine.cpu.read_u32(FAILED_IOSB + 4).unwrap(), 0x22222222);
        assert_eq!(machine.cpu.read_u32(RESULTS + 36).unwrap(), 1);
        let allocation = machine.cpu.read_u32(ALLOCATION).unwrap();
        assert_ne!(allocation, 0);
        assert_eq!(machine.cpu.read_u32(PROTECT_BASE).unwrap(), allocation);
        assert_eq!(machine.cpu.read_u32(PROTECT_SIZE).unwrap(), 8192);
        assert_eq!(machine.cpu.read_u32(OLD_PROTECT).unwrap(), 4);
        assert!(machine.cpu.accessible(allocation, 8192, Prot::READ));
        assert!(!machine.cpu.accessible(allocation, 8192, Prot::WRITE));
        assert_eq!(machine.sys.objects.hosts::<adv_win32::FileObj>().count(), 0);
        assert_eq!(std::fs::read(source).unwrap(), bytes);
    }
}

fn data_view_extent<B: Backend>(cpu: B, fixed: bool) {
    const REQUESTED: u32 = 0x3000_0000;
    const BLOCKED: u32 = 0x3100_0000;
    const NEXT_VIEW: u32 = DATA + 0x40;
    const NEXT_ALLOCATION: u32 = DATA + 0x44;
    const REUSED: u32 = DATA + 0x48;
    const QUERY: u32 = DATA + 0x100;
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/data-view",
            "fixture.exe",
            "/nonexistent/data-view-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    // Even an independently tracked owner inside the unused last granule must
    // retain the fixed-view conflict rule; it is not exposed as view memory.
    assert!(machine.sys.vm.reserve_at(BLOCKED + 0x3000, 4096));
    let mut code = Asm::new(CODE);
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "CreateFileMappingW",
        &[V(u32::MAX), V(0), V(4), V(0), V(0x3000), V(0)],
        SECTION,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "MapViewOfFileEx",
        &[
            S(SECTION),
            V(2),
            V(0),
            V(0),
            V(0x1001),
            V(if fixed { REQUESTED } else { 0 }),
        ],
        VIEW,
    );
    for (delta, output) in [(0, QUERY), (0x2000, QUERY + 0x40)] {
        code.mov_eax_mem(VIEW).add_eax_imm(delta).mov_mem_eax(HEAD);
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "VirtualQuery",
            &[S(HEAD), V(output), V(28)],
            output + 28,
        );
    }
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "VirtualAlloc",
        &[S(HEAD), V(4096), V(0x2000), V(1)],
        RESULTS,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        RESULTS + 4,
    );
    code.mov_eax_mem(VIEW)
        .add_eax_imm(0x10000)
        .mov_mem_eax(HEAD);
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "VirtualAlloc",
        &[if fixed { S(HEAD) } else { V(0) }, V(4096), V(0x2000), V(1)],
        NEXT_ALLOCATION,
    );
    // A mapped view remains unavailable, and a non-granule-aligned request is
    // rejected independently of the new page-rounded owner extent.
    code.mov_eax_mem(VIEW).add_eax_imm(1).mov_mem_eax(HEAD);
    for (index, base) in [S(VIEW), S(HEAD), V(BLOCKED)].into_iter().enumerate() {
        let output = RESULTS + 8 + index as u32 * 8;
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "MapViewOfFileEx",
            &[S(SECTION), V(2), V(0), V(0), V(0x1001), base],
            output,
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "GetLastError",
            &[],
            output + 4,
        );
    }
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "UnmapViewOfFile",
        &[S(VIEW)],
        RESULTS + 32,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "VirtualQuery",
        &[S(VIEW), V(QUERY + 0x80), V(28)],
        QUERY + 0x80 + 28,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "MapViewOfFileEx",
        &[S(SECTION), V(2), V(0), V(0), V(0x1001), S(VIEW)],
        NEXT_VIEW,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "UnmapViewOfFile",
        &[S(NEXT_VIEW)],
        RESULTS + 36,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "VirtualAlloc",
        &[S(VIEW), V(4096), V(0x2000), V(1)],
        REUSED,
    );
    for (index, address) in [REUSED, NEXT_ALLOCATION].into_iter().enumerate() {
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "VirtualFree",
            &[S(address), V(0), V(0x8000)],
            RESULTS + 40 + index as u32 * 4,
        );
    }
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "CloseHandle",
        &[S(SECTION)],
        RESULTS + 48,
    );
    code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("data view extent", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(0)),
        "fixed={fixed}: {result:?}"
    );
    let base = machine.cpu.read_u32(VIEW).unwrap();
    assert_ne!(base, 0);
    assert_eq!(base & 0xffff, 0);
    if fixed {
        assert_eq!(base, REQUESTED);
    }
    let words = |address| -> [u32; 7] {
        std::array::from_fn(|index| machine.cpu.read_u32(address + index as u32 * 4).unwrap())
    };
    assert_eq!(words(QUERY), [base, base, 4, 0x2000, 0x1000, 4, 0x40000]);
    let tail = words(QUERY + 0x40);
    assert_eq!(tail[0], base + 0x2000);
    assert_eq!(tail[4], 0x10000);
    assert!(tail[3] >= 0xe000);
    assert_eq!(machine.cpu.read_u32(RESULTS).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(RESULTS + 4).unwrap(), 487);
    let next = machine.cpu.read_u32(NEXT_ALLOCATION).unwrap();
    assert_ne!(next, 0);
    assert_eq!(next & 0xffff, 0);
    assert!(next >= base + 0x10000 || next + 0x1000 <= base);
    if fixed {
        assert_eq!(next, base + 0x10000);
    }
    for (offset, error) in [(8, 487), (16, 1132), (24, 487)] {
        assert_eq!(machine.cpu.read_u32(RESULTS + offset).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(RESULTS + offset + 4).unwrap(), error);
    }
    assert_eq!(words(QUERY + 0x80)[4], 0x10000);
    assert_eq!(machine.cpu.read_u32(NEXT_VIEW).unwrap(), base);
    assert_eq!(machine.cpu.read_u32(REUSED).unwrap(), base);
    for offset in [32, 36, 40, 44, 48] {
        assert_eq!(machine.cpu.read_u32(RESULTS + offset).unwrap(), 1);
    }
    for offset in [0, 0x40, 0x80] {
        assert_eq!(machine.cpu.read_u32(QUERY + offset + 28).unwrap(), 28);
    }
    assert!(machine.sys.vm.containing(base).is_none());
    assert!(machine.sys.vm.containing(base + 0x2000).is_none());
    assert!(!machine.cpu.accessible(base, 0x2000, Prot::default()));
}

#[test]
fn auto_and_fixed_data_views_publish_the_same_extent_and_preserve_granule_conflicts() {
    for fixed in [false, true] {
        for jit in [false, true] {
            data_view_extent(JitCpu::new(PARK_ADDR, jit).unwrap(), fixed);
        }
        data_view_extent(UnicornCpu::new(PARK_ADDR).unwrap(), fixed);
    }
}
