//! The SDK no-configured-controller personality across the complete query
//! family. These calls never sample a host device or acquire input capture.
use adv_abi::{ApiDiagnostics, Conv, ImplementationLevel, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x430000;
const UNMAPPED: u32 = 0x600000;
const MARKER: u32 = 0x12345678;
const WINDOW: u32 = DATA + 0x800;
const STACK_BEFORE: u32 = WINDOW + 4;
const STACK_AFTER: u32 = WINDOW + 8;

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Window,
}
impl From<u32> for Arg {
    fn from(value: u32) -> Self {
        Self::Value(value)
    }
}
struct Case {
    api: &'static str,
    args: Vec<Arg>,
    expected: u32,
}

fn profile<B: Backend>(cpu: B) {
    let mut m = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/joystick",
            "fixture.exe",
            "/nonexistent/joystick-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 65536, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 800]).unwrap();
    m.cpu.write(DATA + 0x900, b"Static\0").unwrap();
    let mut cases = vec![Case {
        api: "joyGetNumDevs",
        args: vec![],
        expected: 16,
    }];
    let mut buffers: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut buffer_index = 0;
    for (api, wide, sizes, key_offset) in [
        ("joyGetDevCapsA", false, [404, 452], 112),
        ("joyGetDevCapsW", true, [728, 776], 144),
    ] {
        for size in sizes {
            for id in [0, 15, 16, u32::MAX] {
                let base = DATA + 0x1000 + buffer_index * 1024;
                buffer_index += 1;
                m.cpu.write(base, &[0xcd; 800]).unwrap();
                let mut expected = vec![0xcd; 800];
                if wide || id < 16 {
                    expected[4..4 + size as usize].fill(0);
                }
                if id == u32::MAX {
                    let key: Vec<_> = if wide {
                        "DINPUT.DLL\0"
                            .encode_utf16()
                            .flat_map(u16::to_le_bytes)
                            .collect()
                    } else {
                        b"DINPUT.DLL\0".to_vec()
                    };
                    expected[4 + key_offset..4 + key_offset + key.len()].copy_from_slice(&key);
                }
                cases.push(Case {
                    api,
                    args: vec![id.into(), (base + 4).into(), size.into()],
                    expected: if id == u32::MAX {
                        0
                    } else if id < 16 {
                        165
                    } else {
                        6
                    },
                });
                buffers.push((base, expected));
            }
        }
        for size in [0, sizes[0] - 1, sizes[0] + 1] {
            let base = DATA + 0x1000 + buffer_index * 1024;
            buffer_index += 1;
            m.cpu.write(base, &[0xcd; 800]).unwrap();
            cases.push(Case {
                api,
                args: vec![u32::MAX.into(), (base + 4).into(), size.into()],
                expected: 165,
            });
            buffers.push((base, vec![0xcd; 800]));
        }
        for (id, pointer, size, expected) in [
            (0, 0, sizes[0], 11),
            (16, 0, 0, 11),
            (u32::MAX, 0, sizes[1], 11),
            (0, READ_ONLY, sizes[0], 11),
            (u32::MAX, READ_ONLY, sizes[1], 11),
            (0, UNMAPPED, sizes[0], 11),
            (u32::MAX, UNMAPPED, sizes[1], 11),
            (16, UNMAPPED, sizes[0], if wide { 11 } else { 6 }),
        ] {
            cases.push(Case {
                api,
                args: vec![id.into(), pointer.into(), size.into()],
                expected,
            });
        }
    }
    for id in [0, 15, 16, u32::MAX] {
        let base = DATA + 0x1000 + buffer_index * 1024;
        buffer_index += 1;
        m.cpu.write(base, &[0xcd; 64]).unwrap();
        for pointer in [base + 4, READ_ONLY, UNMAPPED, 0] {
            cases.push(Case {
                api: "joyGetPosEx",
                args: vec![id.into(), pointer.into()],
                expected: 165,
            });
        }
        buffers.push((base, vec![0xcd; 64]));
        for window in [Arg::Value(0), Arg::Value(0x1234), Arg::Window] {
            cases.push(Case {
                api: "joySetCapture",
                args: vec![window, id.into(), 10.into(), 1.into()],
                expected: if matches!(window, Arg::Window) && id < 16 {
                    6
                } else {
                    165
                },
            });
        }
        cases.push(Case {
            api: "joyReleaseCapture",
            args: vec![id.into()],
            expected: if id < 16 { 11 } else { 165 },
        });
    }
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    // Create one owned hidden guest window; no host window is created by tests.
    for arg in [0, DATA + 0x900, 0, 0, 0, 0, 32, 32, 0, 0, 0, 0]
        .into_iter()
        .rev()
    {
        code.push_imm(arg);
    }
    code.call(m.resolve_import("user32.dll", "CreateWindowExA").unwrap())
        .mov_mem_eax(WINDOW);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(STACK_BEFORE);
    for (index, case) in cases.iter().enumerate() {
        code.push_imm(MARKER).call(set_error);
        for &arg in case.args.iter().rev() {
            match arg {
                Arg::Value(value) => {
                    code.push_imm(value);
                }
                Arg::Window => {
                    code.mov_eax_mem(WINDOW).push_eax();
                }
            }
        }
        code.call(m.resolve_import("winmm.dll", case.api).unwrap())
            .mov_mem_eax(DATA + index as u32 * 8);
        code.call(get_error)
            .mov_mem_eax(DATA + index as u32 * 8 + 4);
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(STACK_AFTER);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("no-controller profile", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_ne!(m.cpu.read_u32(WINDOW).unwrap(), 0);
    assert_eq!(
        m.cpu.read_u32(STACK_BEFORE).unwrap(),
        m.cpu.read_u32(STACK_AFTER).unwrap(),
        "SDK stdcall cleanup"
    );
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8).unwrap(),
            case.expected,
            "{} case {index}",
            case.api
        );
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(),
            MARKER,
            "{} LastError",
            case.api
        );
    }
    for (base, expected) in buffers {
        let mut actual = vec![0; expected.len()];
        m.cpu.read(base, &mut actual).unwrap();
        assert_eq!(actual, expected, "guarded output at {base:#x}");
    }
    let mut readonly = [0; 800];
    m.cpu.read(READ_ONLY, &mut readonly).unwrap();
    assert_eq!(readonly, [0x77; 800]);
    assert_eq!(m.sys.contexts.len(), 1, "no capture/timer worker");
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|diag| diag.acceptance_eligible(RunMode::Strict))
    );
}

#[test]
fn no_configured_controller_family_matches_the_sdk_personality_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        profile(cpu);
    }
    profile(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn joystick_exports_have_typed_sdk_signatures_and_real_dynamic_availability() {
    let registry = registry();
    for (name, argc, level) in [
        ("joyGetNumDevs", 0, ImplementationLevel::State),
        ("joyGetDevCapsA", 3, ImplementationLevel::State),
        ("joyGetDevCapsW", 3, ImplementationLevel::State),
        ("joyGetPos", 2, ImplementationLevel::SafeFail),
        ("joyGetPosEx", 2, ImplementationLevel::SafeFail),
        ("joySetCapture", 4, ImplementationLevel::SafeFail),
        ("joyReleaseCapture", 1, ImplementationLevel::SafeFail),
    ] {
        assert_eq!(registry.level("winmm.dll", name), Some(level));
        assert!(registry.dynamic_available("winmm.dll", name));
        assert_eq!(
            registry.get("winmm.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
    }
    assert!(registry.signature_conflicts().is_empty());
}
