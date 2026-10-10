//! Authored directory contracts: one NT cursor/backing owner across all fronts.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TEXT: u32 = DATA + 0x1000;
static NEXT: AtomicU32 = AtomicU32::new(0);
fn fixture<B: Backend>(cpu: B) -> (Machine<B>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/nt-directory-tests/{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("game/nonempty")).unwrap();
    std::fs::write(root.join("game/nonempty/child.txt"), b"child").unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(root.join("game"), "authored.exe", root.join("data"));
    config.os.write_game = true;
    Win32::install(&mut m, config).unwrap();
    m.cpu.map(CODE, 0x2000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    (m, root)
}
fn text<B: Backend>(m: &mut Machine<B>, p: u32, s: &str, wide: bool) {
    let bytes = if wide {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        [s.as_bytes(), &[0]].concat()
    };
    m.cpu.write(p, &bytes).unwrap();
}
fn attrs<B: Backend>(m: &mut Machine<B>, p: u32, name: u32, s: &str) {
    let units: Vec<_> = s.encode_utf16().collect();
    m.cpu
        .write(
            name,
            &units
                .iter()
                .flat_map(|c| c.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write_u32(p, 24).unwrap();
    m.cpu.write_u32(p + 8, p + 24).unwrap();
    m.cpu.write_u32(p + 12, 0x40).unwrap();
    m.cpu
        .write(p + 24, &(units.len() as u16 * 2).to_le_bytes())
        .unwrap();
    m.cpu
        .write(p + 26, &(units.len() as u16 * 2).to_le_bytes())
        .unwrap();
    m.cpu.write_u32(p + 28, name).unwrap();
}
fn call<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    out: u32,
) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(m.resolve_import(module, name).unwrap());
    if module.starts_with("msvc") {
        code.add_esp((args.len() * 4) as u8);
    }
    code.mov_mem_eax(out);
}
fn handle_call<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    name: &str,
    slot: u32,
    tail: &[u32],
    out: u32,
) {
    for &arg in tail.iter().rev() {
        code.push_imm(arg);
    }
    code.mov_eax_mem(slot)
        .push_eax()
        .call(m.resolve_import("ntdll.dll", name).unwrap())
        .mov_mem_eax(out);
}
fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("authored directory contract", CODE, &[], 65536)
        .unwrap();
    let r = m.run_virtual(1_000_000);
    assert!(matches!(r, RunResult::Exited(0)), "{r:?}");
}
fn value<B: Backend>(m: &Machine<B>, p: u32) -> u32 {
    m.cpu.read_u32(p).unwrap()
}

#[cfg(windows)]
fn lifecycle<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let mut code = Asm::new(CODE);
    text(&mut m, TEXT, r"C:\Game\fresh", false);
    text(&mut m, TEXT + 128, r"C:\Game\fresh", true);
    attrs(&mut m, DATA + 0x200, TEXT + 256, r"\??\C:\Game\fresh");
    attrs(&mut m, DATA + 0x240, TEXT + 384, r"\??\C:\Game\nonempty");
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "SetLastError",
        &[0x12345678],
        DATA + 0xf0,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "CreateDirectoryA",
        &[TEXT, 0],
        DATA,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        DATA + 4,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtCreateFile",
        &[
            DATA + 0x100,
            0x110081,
            DATA + 0x200,
            DATA + 0x180,
            0,
            0x80,
            7,
            1,
            0x21,
            0,
            0,
        ],
        DATA + 8,
    );
    // DuplicateHandle reaches the same cursor and physical handle payload.
    code.push_imm(2)
        .push_imm(0)
        .push_imm(0)
        .push_imm(DATA + 0x104)
        .push_imm(u32::MAX)
        .mov_eax_mem(DATA + 0x100)
        .push_eax()
        .push_imm(u32::MAX)
        .call(m.resolve_import("kernel32.dll", "DuplicateHandle").unwrap())
        .mov_mem_eax(DATA + 12);
    m.cpu.write(DATA + 0x340, &[1]).unwrap();
    handle_call(
        &mut m,
        &mut code,
        "NtSetInformationFile",
        DATA + 0x104,
        &[DATA + 0x180, DATA + 0x340, 1, 13],
        DATA + 16,
    );
    handle_call(
        &mut m,
        &mut code,
        "NtQueryInformationFile",
        DATA + 0x104,
        &[DATA + 0x180, DATA + 0x300, 24, 5],
        DATA + 20,
    );
    handle_call(&mut m, &mut code, "NtClose", DATA + 0x100, &[], DATA + 24);
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetFileAttributesA",
        &[TEXT],
        DATA + 28,
    );
    // The new open observes the host's pending state and cannot create another owner.
    m.cpu.write(DATA + 0x188, &[0xcd; 8]).unwrap();
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtCreateFile",
        &[
            DATA + 0x108,
            0x100081,
            DATA + 0x200,
            DATA + 0x188,
            0,
            0x80,
            7,
            1,
            0x21,
            0,
            0,
        ],
        DATA + 32,
    );
    handle_call(&mut m, &mut code, "NtClose", DATA + 0x104, &[], DATA + 36);
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetFileAttributesA",
        &[TEXT],
        DATA + 40,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtCreateFile",
        &[
            DATA + 0x10c,
            0x110080,
            DATA + 0x240,
            DATA + 0x190,
            0,
            0x80,
            7,
            1,
            0x21,
            0,
            0,
        ],
        DATA + 44,
    );
    m.cpu.write(DATA + 0x340, &[1]).unwrap();
    handle_call(
        &mut m,
        &mut code,
        "NtSetInformationFile",
        DATA + 0x10c,
        &[DATA + 0x198, DATA + 0x340, 1, 13],
        DATA + 48,
    );
    handle_call(&mut m, &mut code, "NtClose", DATA + 0x10c, &[], DATA + 52);
    run(&mut m, code);
    for (offset, expected) in [
        (0, 1),
        (4, 0x12345678),
        (8, 0),
        (12, 1),
        (16, 0),
        (20, 0),
        (24, 0),
        (28, u32::MAX), // Native GetFileAttributes reports the pending instance.
        (32, 0xc0000056),
        (36, 0),
        (40, u32::MAX),
        (44, 0),
        (48, 0xc0000101),
        (52, 0),
    ] {
        assert_eq!(value(&m, DATA + offset), expected, "offset {offset}");
    }
    assert_eq!(value(&m, DATA + 0x108), 0);
    assert_eq!(value(&m, DATA + 0x188), 0xcdcdcdcd);
    let mut flags = [0; 2];
    m.cpu.read(DATA + 0x314, &mut flags).unwrap();
    assert_eq!(flags, [1, 1]);
    assert_eq!(value(&m, DATA + 0x198), 0xc0000101);
    assert_eq!(value(&m, DATA + 0x19c), 0);
    assert!(!root.join("game/fresh").exists());
    assert_eq!(
        std::fs::read(root.join("game/nonempty/child.txt")).unwrap(),
        b"child"
    );
}

#[cfg(windows)]
#[test]
fn directory_delete_pending_aliases_and_nonempty_iosb_match_native() {
    lifecycle(UnicornCpu::new(PARK_ADDR).unwrap());
    lifecycle(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[cfg(windows)]
fn rename<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let mut code = Asm::new(CODE);
    text(&mut m, TEXT, r"C:\Game\nonempty", true);
    text(&mut m, TEXT + 128, r"C:\Game\renamed", false);
    attrs(&mut m, DATA + 0x200, TEXT + 256, r"\??\C:\Game\nonempty");
    text(&mut m, TEXT + 384, r"C:\Game\nonempty\child.txt", false);
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtCreateFile",
        &[
            DATA + 0x100,
            0x100081,
            DATA + 0x200,
            DATA + 0x180,
            0,
            0x80,
            7,
            1,
            0x21,
            0,
            0,
        ],
        DATA,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "CreateFileA",
        &[TEXT + 384, 0x80000000, 7, 0, 3, 0, 0],
        DATA + 0x104,
    );
    // Seed the shared cursor before changing the directory's physical name.
    handle_call(
        &mut m,
        &mut code,
        "NtQueryDirectoryFile",
        DATA + 0x100,
        &[0, 0, 0, DATA + 0x180, DATA + 0x400, 512, 12, 1, 0, 1],
        DATA + 4,
    );
    text(&mut m, TEXT + 768, r"C:\Game\renamed", true);
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "MoveFileW",
        &[TEXT, TEXT + 768],
        DATA + 8,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        DATA + 20,
    );
    code.mov_eax_mem(DATA + 0x104)
        .push_eax()
        .call(m.resolve_import("kernel32.dll", "CloseHandle").unwrap())
        .mov_mem_eax(DATA + 24);
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "MoveFileW",
        &[TEXT, TEXT + 768],
        DATA + 28,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        DATA + 32,
    );
    handle_call(
        &mut m,
        &mut code,
        "NtQueryDirectoryFile",
        DATA + 0x100,
        &[0, 0, 0, DATA + 0x180, DATA + 0x400, 512, 12, 1, 0, 0],
        DATA + 12,
    );
    run(&mut m, code);
    assert_eq!(value(&m, DATA), 0);
    assert_eq!(value(&m, DATA + 4), 0);
    assert_eq!(
        value(&m, DATA + 8),
        0,
        "last error {}",
        value(&m, DATA + 20)
    );
    assert_eq!(value(&m, DATA + 12), 0x80000006);
    assert_eq!(value(&m, DATA + 20), 5);
    assert_eq!(value(&m, DATA + 24), 1);
    assert_eq!(
        value(&m, DATA + 28),
        1,
        "rename after child close error {}",
        value(&m, DATA + 32)
    );
    assert!(root.join("game/renamed/child.txt").exists());
    assert!(!root.join("game/nonempty").exists());
    m.sys.objects.close(value(&m, DATA + 0x100));
}
#[cfg(windows)]
#[test]
fn move_file_directory_respects_open_children_and_keeps_the_enumeration_cursor() {
    rename(UnicornCpu::new(PARK_ADDR).unwrap());
    rename(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[cfg(windows)]
fn boolean_slots<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    std::fs::write(root.join("game/nonempty/a.int"), b"a").unwrap();
    std::fs::write(root.join("game/nonempty/b.int"), b"b").unwrap();
    let mut code = Asm::new(CODE);
    attrs(&mut m, DATA + 0x200, TEXT, r"\??\C:\Game\nonempty");
    let mask: Vec<u8> = "*.int".encode_utf16().flat_map(u16::to_le_bytes).collect();
    m.cpu.write(TEXT + 256, &mask).unwrap();
    m.cpu.write(DATA + 0x300, &10u16.to_le_bytes()).unwrap();
    m.cpu.write(DATA + 0x302, &10u16.to_le_bytes()).unwrap();
    m.cpu.write_u32(DATA + 0x304, TEXT + 256).unwrap();
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtCreateFile",
        &[
            DATA + 0x100,
            0x100081,
            DATA + 0x200,
            DATA + 0x180,
            0,
            0x80,
            7,
            1,
            0x21,
            0,
            0,
        ],
        DATA,
    );
    // Only the low byte of a BOOLEAN stack slot is significant: these single
    // entries continue the cursor although the slots carry nonzero garbage.
    for (index, buffer) in [0x400, 0x600, 0x800].into_iter().enumerate() {
        handle_call(
            &mut m,
            &mut code,
            "NtQueryDirectoryFile",
            DATA + 0x100,
            &[
                0,
                0,
                0,
                DATA + 0x180,
                DATA + buffer,
                512,
                12,
                0x145e_0b01,
                DATA + 0x300,
                0x1462_2700,
            ],
            DATA + 4 + index as u32 * 4,
        );
    }
    // A restart whose low byte is set rescans; a cleared low byte returns
    // every remaining entry in one buffer.
    handle_call(
        &mut m,
        &mut code,
        "NtQueryDirectoryFile",
        DATA + 0x100,
        &[
            0,
            0,
            0,
            DATA + 0x180,
            DATA + 0xa00,
            512,
            12,
            0x145e_0b00,
            0,
            0xffff_ff01,
        ],
        DATA + 16,
    );
    run(&mut m, code);
    let name = |m: &Machine<B>, p: u32| {
        let length = value(m, p + 8);
        let mut bytes = vec![0; length as usize];
        m.cpu.read(p + 12, &mut bytes).unwrap();
        String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    };
    assert_eq!(value(&m, DATA), 0);
    assert_eq!(value(&m, DATA + 4), 0);
    assert_eq!(value(&m, DATA + 8), 0);
    assert_eq!(value(&m, DATA + 12), 0x80000006);
    let mut found = [name(&m, DATA + 0x400), name(&m, DATA + 0x600)];
    found.sort();
    assert_eq!(found, ["a.int", "b.int"]);
    assert_eq!(value(&m, DATA + 16), 0);
    let next = value(&m, DATA + 0xa00);
    assert_ne!(next, 0, "a non-single query returns every entry");
    let mut all = [name(&m, DATA + 0xa00), name(&m, DATA + 0xa00 + next)];
    all.sort();
    assert_eq!(all, ["a.int", "b.int"]);
    m.sys.objects.close(value(&m, DATA + 0x100));
}
#[cfg(windows)]
#[test]
fn directory_query_reads_only_the_low_byte_of_boolean_arguments() {
    boolean_slots(UnicornCpu::new(PARK_ADDR).unwrap());
    boolean_slots(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn hook<B: Backend>(m: &mut Machine<B>, name: &str, slot: u32, argc: u8, deny_close: bool) {
    let binding = m.resolve_import("ntdll.dll", name).unwrap();
    let original = m
        .sys
        .thunks
        .pristine_entry((binding - THUNK_BASE) / THUNK_STRIDE);
    let at = CODE + 0x1000 + slot * 0x200;
    m.cpu.write(at + 0x100, &original).unwrap();
    let record = DATA + 0x800 + slot * 0x40;
    let mut code = Asm::new(at);
    code.mov_eax_mem(record).add_eax_imm(1).mov_mem_eax(record);
    for index in 0..argc {
        code.mov_eax_esp(4 + index * 4)
            .mov_mem_eax(record + 4 + u32::from(index) * 4);
    }
    if deny_close {
        code.bytes.push(0xb8);
        code.bytes.extend_from_slice(&0xc0000008u32.to_le_bytes());
        code.ret_n(4);
    } else if name == "NtSetInformationFile" {
        // A provider's negative Ex-disposition result triggers the native fallback.
        code.mov_eax_esp(20);
        code.bytes.extend_from_slice(&[0x83, 0xf8, 0x40, 0x75, 8]);
        code.bytes.push(0xb8);
        code.bytes.extend_from_slice(&0xc0000001u32.to_le_bytes());
        code.ret_n(20);
        code.jmp(at + 0x100);
    } else {
        code.jmp(at + 0x100);
    }
    m.cpu.write(at, &code.bytes).unwrap();
    let modules = adv_os::modules(&m.ext);
    let image = modules.get(modules.by_name("ntdll.dll").unwrap());
    let entry = match image.exports_by_name.get(name).unwrap() {
        adv_pe::ExportTarget::Rva(rva) => image.base + rva,
        _ => panic!("native code entry"),
    };
    let mut patch = Asm::new(entry);
    patch.jmp(at);
    m.cpu.write(entry, &patch.bytes).unwrap();
}
fn live<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    Win32::prepare_process_infrastructure(&mut m).unwrap();
    for (slot, name, argc) in [
        (0, "NtCreateFile", 11),
        (1, "NtOpenFile", 6),
        (2, "NtSetInformationFile", 5),
        (3, "NtClose", 1),
    ] {
        hook(&mut m, name, slot, argc, false);
    }
    text(&mut m, TEXT, r"C:\Game\front", true);
    text(&mut m, TEXT + 128, r"C:\Game\front", false);
    let mut code = Asm::new(CODE);
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "CreateDirectoryW",
        &[TEXT, 0],
        DATA,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "RemoveDirectoryA",
        &[TEXT + 128],
        DATA + 4,
    );
    call(
        &mut m,
        &mut code,
        "msvcr90.dll",
        "_mkdir",
        &[TEXT + 128],
        DATA + 8,
    );
    call(
        &mut m,
        &mut code,
        "msvcr90.dll",
        "_wrmdir",
        &[TEXT],
        DATA + 12,
    );
    run(&mut m, code);
    assert_eq!(
        (0..4).map(|i| value(&m, DATA + i * 4)).collect::<Vec<_>>(),
        [1, 1, 0, 0]
    );
    assert_eq!(value(&m, DATA + 0x800), 2);
    assert_eq!(value(&m, DATA + 0x840), 2);
    assert_eq!(value(&m, DATA + 0x880), 4);
    assert_eq!(value(&m, DATA + 0x8c0), 4);
    for (offset, expected) in [
        (0x808, 0x100001),
        (0x81c, 3),
        (0x820, 2),
        (0x824, 0x204021),
        (0x848, 0x110080),
        (0x854, 7),
        (0x858, 0x204021),
        (0x894, 13),
    ] {
        assert_eq!(
            value(&m, DATA + offset),
            expected,
            "native argument {offset:#x}"
        );
    }
    assert!(!root.join("game/front").exists());
}
#[test]
fn directory_win32_and_crt_fronts_follow_live_native_entries_and_ex_fallback() {
    live(UnicornCpu::new(PARK_ADDR).unwrap());
    live(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn cleanup<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    Win32::prepare_process_infrastructure(&mut m).unwrap();
    hook(&mut m, "NtClose", 3, 1, true);
    text(&mut m, TEXT, r"C:\Game\cleanup", false);
    let mut code = Asm::new(CODE);
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "CreateDirectoryA",
        &[TEXT, 0],
        DATA,
    );
    run(&mut m, code);
    assert_eq!(value(&m, DATA), 1);
    assert!(root.join("game/cleanup").is_dir());
    assert_eq!(value(&m, DATA + 0x8c0), 1);
    m.sys.objects.close(value(&m, DATA + 0x8c4));
}
#[test]
fn create_directory_preserves_its_success_when_native_cleanup_reports_failure() {
    cleanup(UnicornCpu::new(PARK_ADDR).unwrap());
    cleanup(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn crt_remove<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let mut code = Asm::new(CODE);
    for (index, module) in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"]
        .into_iter()
        .enumerate()
    {
        let index = index as u32;
        let p = TEXT + index * 512;
        let record = DATA + index * 128;
        text(&mut m, p, r"C:\Game\nonempty", false);
        text(&mut m, p + 128, r"C:\Game\nonempty\child.txt", true);
        std::fs::create_dir(root.join("game").join(format!("empty{index}"))).unwrap();
        text(&mut m, p + 256, &format!(r"C:\Game\empty{index}"), true);
        let errno = m.resolve_import(module, "_errno").unwrap();

        for (case, name, path) in [
            (0, "_rmdir", p),
            (1, "_wrmdir", p + 128),
            (2, "_wrmdir", p + 256),
        ] {
            for (offset, seed) in [(0, 79u32), (4, 0xabcdef)] {
                code.call(errno).add_eax_imm(offset);
                code.bytes.extend_from_slice(&[0xc7, 0x00]);
                code.bytes.extend_from_slice(&seed.to_le_bytes());
            }
            call(
                &mut m,
                &mut code,
                "kernel32.dll",
                "SetLastError",
                &[0x12345678],
                record + 0x60,
            );
            call(&mut m, &mut code, module, name, &[path], record + case * 16);
            code.call(errno)
                .mov_eax_ptr_eax()
                .mov_mem_eax(record + case * 16 + 4);
            code.call(errno)
                .add_eax_imm(4)
                .mov_eax_ptr_eax()
                .mov_mem_eax(record + case * 16 + 8);
            call(
                &mut m,
                &mut code,
                "kernel32.dll",
                "GetLastError",
                &[],
                record + case * 16 + 12,
            );
        }
    }
    run(&mut m, code);
    for index in 0..3 {
        let record = DATA + index * 128;
        for (case, expected) in [
            (0, [u32::MAX, 41, 145, 145]),
            (1, [u32::MAX, 22, 267, 267]),
            (2, [0, 79, 0xabcdef, 0x12345678]),
        ] {
            let actual: Vec<_> = (0..4)
                .map(|i| value(&m, record + case * 16 + i * 4))
                .collect();
            assert_eq!(actual, expected, "CRT module {index} case {case}");
        }
    }
    assert!(root.join("game/nonempty/child.txt").is_file());
}
#[test]
fn three_crt_directory_fronts_match_native_errno_dos_and_success_preservation() {
    crt_remove(UnicornCpu::new(PARK_ADDR).unwrap());
    crt_remove(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[cfg(not(windows))]
#[test]
fn portable_directory_rename_keeps_the_existing_child_instance() {
    for jit in [false, true] {
        let (mut m, root) = fixture(JitCpu::new(PARK_ADDR, jit).unwrap());
        let mut code = Asm::new(CODE);
        text(&mut m, TEXT, r"C:\Game\nonempty", false);
        text(&mut m, TEXT + 128, r"C:\Game\renamed", false);
        text(&mut m, TEXT + 256, r"C:\Game\nonempty\child.txt", false);
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "CreateFileA",
            &[TEXT + 256, 0x80000000, 7, 0, 3, 0, 0],
            DATA + 4,
        );
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "MoveFileA",
            &[TEXT, TEXT + 128],
            DATA,
        );
        run(&mut m, code);
        assert_eq!(value(&m, DATA), 1);
        assert_eq!(
            m.sys
                .objects
                .host::<adv_win32::FileObj>(value(&m, DATA + 4))
                .unwrap()
                .guest_path,
            r"C:\Game\renamed\child.txt"
        );
        assert!(root.join("game/renamed/child.txt").is_file());
        m.sys.objects.close(value(&m, DATA + 4));
    }
}
