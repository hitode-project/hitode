//! Authored MCIWnd callers exercise real x86 ABI and the shared USER/MCI owners.
use adv_abi::{ApiDiagnostics, Conv, Registry};
use adv_cpu::backend::{Guest, GuestExt, Prot, asm::Asm};
use adv_machine::{Clock, Machine, Outcome, RunResult};
use adv_win32::{Win32, Win32Config};

#[path = "support/cpu.rs"]
mod cpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HWND: u32 = DATA + 0x800;
const RESULTS: u32 = DATA + 0x900;
const STYLE: u32 = 0x4002; // MCIWNDF_NOPLAYBAR | MCIWNDF_NOERRORDLG

fn machine(backend: &str, name: &str) -> Machine<cpu::Cpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    adv_dx::dshow::register(&mut registry);
    adv_dx::dsound::register(&mut registry);
    registry.add("fixture.dll", "done", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut m = Machine::new(
        cpu::Cpu::new(backend).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/mciwnd-tests")
        .join(name)
        .join(backend);
    std::fs::create_dir_all(&root).unwrap();
    let mut config = Win32Config::new(&root, "fixture.exe", root.join("data"));
    config.os.codepage = 932;
    Win32::install(&mut m, config).unwrap();
    adv_dx::dshow::install(&mut m);
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m
}
fn call(m: &mut Machine<cpu::Cpu>, a: &mut Asm, dll: &str, api: &str, args: &[u32], out: u32) {
    for &arg in args.iter().rev() {
        a.push_imm(arg);
    }
    a.call(m.resolve_import(dll, api).unwrap()).mov_mem_eax(out);
    if matches!(api, "MCIWndCreateA" | "MCIWndCreateW") {
        a.bytes.extend_from_slice(&[0x83, 0xc4, 16]); // cdecl caller cleanup
    }
}
fn push_saved(a: &mut Asm, pointer: u32) {
    a.bytes.extend_from_slice(&[0xff, 0x35]);
    a.bytes.extend_from_slice(&pointer.to_le_bytes());
}
fn send(m: &mut Machine<cpu::Cpu>, a: &mut Asm, hwnd: u32, msg: u32, wp: u32, lp: u32, slot: u32) {
    a.push_imm(lp).push_imm(wp).push_imm(msg);
    push_saved(a, hwnd);
    a.call(m.resolve_import("user32.dll", "SendMessageA").unwrap())
        .mov_mem_eax(RESULTS + slot * 4);
}
fn destroy(m: &mut Machine<cpu::Cpu>, a: &mut Asm, hwnd: u32) {
    push_saved(a, hwnd);
    a.call(m.resolve_import("user32.dll", "DestroyWindow").unwrap());
}
fn run(m: &mut Machine<cpu::Cpu>, mut a: Asm) {
    a.call(m.resolve_import("fixture.dll", "done").unwrap());
    assert!(a.bytes.len() <= 4096);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("MCIWnd fixture", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(5_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .map_or(0, |d| d.stub_calls),
        0
    );
}
fn value(m: &Machine<cpu::Cpu>, slot: u32) -> u32 {
    m.cpu.read_u32(RESULTS + slot * 4).unwrap()
}

#[test]
// spec: MEDIA-MCI-4, PERS-ABI-1
fn class_creation_encoding_failure_and_destroy() {
    for backend in ["interpreter", "jit", "unicorn"] {
        let mut m = machine(backend, "windows");
        let mut a = Asm::new(CODE);
        a.bytes.extend_from_slice(&[0x89, 0xe0]);
        a.mov_mem_eax(DATA + 0x700);
        call(
            &mut m,
            &mut a,
            "msvfw32.dll",
            "MCIWndRegisterClass",
            &[],
            RESULTS,
        );
        call(
            &mut m,
            &mut a,
            "msvfw32.dll",
            "MCIWndRegisterClass",
            &[],
            RESULTS + 4,
        );
        call(
            &mut m,
            &mut a,
            "msvfw32.dll",
            "MCIWndCreateA",
            &[0, 0, STYLE, 0],
            HWND,
        );
        // A W child shares the class and follows the ordinary parent lifecycle.
        a.push_imm(0).push_imm(STYLE).push_imm(0);
        push_saved(&mut a, HWND);
        a.call(m.resolve_import("msvfw32.dll", "MCIWndCreateW").unwrap())
            .mov_mem_eax(HWND + 4);
        a.bytes.extend_from_slice(&[0x83, 0xc4, 16]);
        for (saved, slot) in [(HWND, 2), (HWND + 4, 3)] {
            push_saved(&mut a, saved);
            a.call(m.resolve_import("user32.dll", "IsWindowUnicode").unwrap())
                .mov_mem_eax(RESULTS + slot * 4);
        }
        send(&mut m, &mut a, HWND, 0x464, 0, 0, 4);
        send(&mut m, &mut a, HWND, 0x806, 0, 0, 5);
        send(&mut m, &mut a, HWND, 0x480, 0, 0, 6);
        send(&mut m, &mut a, HWND, 0x804, 0, 0, 7);
        send(&mut m, &mut a, HWND, 0x488, 0, 0, 8);
        // An unsupported toolbar cannot produce a successful placeholder.
        call(
            &mut m,
            &mut a,
            "msvfw32.dll",
            "MCIWndCreateA",
            &[0, 0, 0x4000, 0],
            RESULTS + 36,
        );
        destroy(&mut m, &mut a, HWND);
        for (saved, slot) in [(HWND, 10), (HWND + 4, 11)] {
            push_saved(&mut a, saved);
            a.call(m.resolve_import("user32.dll", "IsWindow").unwrap())
                .mov_mem_eax(RESULTS + slot * 4);
        }
        a.bytes.extend_from_slice(&[0x89, 0xe0]);
        a.mov_mem_eax(DATA + 0x704);
        run(&mut m, a);
        for (slot, expected) in [
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 0),
            (5, 257),
            (6, 257),
            (7, 0),
            (8, STYLE),
            (9, 0),
            (10, 0),
            (11, 0),
        ] {
            assert_eq!(value(&m, slot), expected, "{backend} slot {slot}");
        }
        assert_ne!(m.cpu.read_u32(HWND).unwrap(), 0);
        assert_ne!(m.cpu.read_u32(HWND + 4).unwrap(), 0);
        assert_eq!(
            m.cpu.read_u32(DATA + 0x700).unwrap(),
            m.cpu.read_u32(DATA + 0x704).unwrap()
        );
        assert_eq!(
            m.ext
                .get::<Win32>()
                .user
                .classes
                .iter()
                .filter(|c| c.name == "MCIWndClass")
                .count(),
            1
        );
    }
}

#[test]
// spec: MEDIA-MCI-4, MEDIA-MCI-3
fn failed_open_keeps_a_real_empty_window_and_records_error() {
    for backend in ["interpreter", "jit", "unicorn"] {
        for wide in [false, true] {
            let mut m = machine(backend, if wide { "missing-w" } else { "missing-a" });
            let name = "missing.avi\0";
            let bytes = if wide {
                name.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else {
                name.as_bytes().to_vec()
            };
            m.cpu.write(DATA, &bytes).unwrap();
            let mut a = Asm::new(CODE);
            call(
                &mut m,
                &mut a,
                "msvfw32.dll",
                if wide {
                    "MCIWndCreateW"
                } else {
                    "MCIWndCreateA"
                },
                &[0, 0, STYLE, DATA],
                HWND,
            );
            send(&mut m, &mut a, HWND, 0x464, 0, 0, 0);
            send(&mut m, &mut a, HWND, 0x480, 0, 0, 1);
            destroy(&mut m, &mut a, HWND);
            run(&mut m, a);
            assert_ne!(m.cpu.read_u32(HWND).unwrap(), 0);
            assert_eq!(value(&m, 0), 0);
            assert_ne!(value(&m, 1), 0);
        }
    }
}

#[cfg(feature = "media")]
#[test]
// spec: MEDIA-MCI-4, MEDIA-MCI-1
fn video_open_controls_reopen_and_destroy_share_mci_devices() {
    for backend in ["interpreter", "jit", "unicorn"] {
        for wide in [false, true] {
            let name = if wide { "media-w" } else { "media-a" };
            let mut m = machine(backend, name);
            adv_runtime::install_graphics(&mut m, 320, 240).unwrap();
            adv_dx::dsound::install(&mut m).unwrap();
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/mciwnd-tests")
                .join(name)
                .join(backend);
            std::fs::write(
                root.join("動画.wmv"),
                include_bytes!("../../adv-device/src/media/packets/ramp.wmv"),
            )
            .unwrap();
            let bytes = if wide {
                "動画.wmv\0"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect()
            } else {
                encoding_rs::SHIFT_JIS.encode("動画.wmv\0").0.into_owned()
            };
            m.cpu.write(DATA, &bytes).unwrap();
            let mut a = Asm::new(CODE);
            call(
                &mut m,
                &mut a,
                "msvfw32.dll",
                if wide {
                    "MCIWndCreateW"
                } else {
                    "MCIWndCreateA"
                },
                &[0, 0, STYLE, DATA],
                HWND,
            );
            a.push_imm(DATA + 0x300);
            push_saved(&mut a, HWND);
            a.call(m.resolve_import("user32.dll", "GetClientRect").unwrap());
            a.push_imm(0x16)
                .push_imm(150)
                .push_imm(200)
                .push_imm(0)
                .push_imm(0)
                .push_imm(0);
            push_saved(&mut a, HWND);
            a.call(m.resolve_import("user32.dll", "SetWindowPos").unwrap());
            a.push_imm(DATA + 0x310);
            push_saved(&mut a, HWND);
            a.call(m.resolve_import("user32.dll", "GetClientRect").unwrap());
            a.push_imm(5);
            push_saved(&mut a, HWND);
            a.call(m.resolve_import("user32.dll", "GetWindow").unwrap())
                .mov_mem_eax(HWND + 8);
            a.push_imm(DATA + 0x320);
            push_saved(&mut a, HWND + 8);
            a.call(m.resolve_import("user32.dll", "GetClientRect").unwrap());
            send(&mut m, &mut a, HWND, 0x464, 0, 0, 0);
            for (i, msg) in [
                0x480, 0x468, 0x806, 0x46a, 0x809, 0x46a, 0x855, 0x46a, 0x46e, 0x46f, 0x808, 0x46a,
                0x804, 0x464,
            ]
            .into_iter()
            .enumerate()
            {
                send(
                    &mut m,
                    &mut a,
                    HWND,
                    msg,
                    0,
                    if msg == 0x46e { 500 } else { 0 },
                    i as u32 + 1,
                );
                if msg == 0x46f {
                    for (index, word) in [0, 0x4002, 250, 0, 0, 0].into_iter().enumerate() {
                        m.cpu
                            .write_u32(DATA + 0x400 + index as u32 * 4, word)
                            .unwrap();
                    }
                    a.push_imm(DATA + 0x400).push_imm(0x1800002).push_imm(0x873);
                    push_saved(&mut a, RESULTS);
                    a.call(m.resolve_import("winmm.dll", "mciSendCommandA").unwrap())
                        .mov_mem_eax(RESULTS + 20 * 4);
                    send(&mut m, &mut a, HWND, 0x46f, 0, 0, 21);
                }
            }
            send(
                &mut m,
                &mut a,
                HWND,
                if wide { 0x4fc } else { 0x499 },
                0,
                DATA,
                15,
            );
            send(&mut m, &mut a, HWND, 0x464, 0, 0, 16);
            destroy(&mut m, &mut a, HWND);
            // Retired device IDs must fail through the public MCI API as well.
            m.cpu.write_u32(DATA + 0x108, 4).unwrap();
            a.push_imm(DATA + 0x100).push_imm(0x100).push_imm(0x814);
            push_saved(&mut a, RESULTS + 16 * 4);
            a.call(m.resolve_import("winmm.dll", "mciSendCommandA").unwrap())
                .mov_mem_eax(RESULTS + 17 * 4);
            let class = if wide {
                "MCIWndClass\0"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect()
            } else {
                b"MCIWndClass\0".to_vec()
            };
            m.cpu.write(DATA + 0x200, &class).unwrap();
            call(
                &mut m,
                &mut a,
                "user32.dll",
                if wide {
                    "CreateWindowExW"
                } else {
                    "CreateWindowExA"
                },
                &[
                    0,
                    DATA + 0x200,
                    0,
                    0x80000000 | STYLE,
                    0,
                    0,
                    64,
                    48,
                    0,
                    0,
                    0,
                    DATA,
                ],
                HWND + 4,
            );
            send(&mut m, &mut a, HWND + 4, 0x464, 0, 0, 18);
            send(&mut m, &mut a, HWND + 4, 0x480, 0, 0, 19);
            destroy(&mut m, &mut a, HWND + 4);
            run(&mut m, a);
            assert_eq!(m.cpu.read_u32(DATA + 0x308).unwrap(), 64);
            assert_eq!(m.cpu.read_u32(DATA + 0x30c).unwrap(), 48);
            for offset in [8, 12] {
                assert_eq!(
                    m.cpu.read_u32(DATA + 0x310 + offset).unwrap(),
                    m.cpu.read_u32(DATA + 0x320 + offset).unwrap()
                );
            }
            assert_ne!(value(&m, 18), 0);
            assert_eq!(value(&m, 19), 0);
            assert_eq!(value(&m, 20), 0);
            assert_eq!(value(&m, 21), 250);
            assert_ne!(value(&m, 0), 0, "{backend} open error {}", value(&m, 1));
            assert_eq!(value(&m, 1), 0);
            assert!(value(&m, 2) > 0);
            for slot in [3, 5, 7, 9, 11, 13, 14, 15] {
                assert_eq!(value(&m, slot), 0, "{backend} slot {slot}");
            }
            for (slot, expected) in [(4, 526), (6, 529), (8, 526), (10, 500), (12, 525)] {
                assert_eq!(value(&m, slot), expected);
            }
            assert_ne!(value(&m, 16), 0);
            assert_eq!(value(&m, 17), 257);
        }
    }
}
