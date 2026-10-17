//! Authored DirectSound callbacks. No host audio, native device query or game.
use adv_abi::{ApiDiagnostics, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Env, InitCall, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CALLBACK: u32 = CODE + 0x200;
const INNER: u32 = CODE + 0x600;
const GUID: u32 = DATA;
const DESCRIPTION: u32 = DATA + 4;
const MODULE: u32 = DATA + 8;
const CONTEXT: u32 = DATA + 12;
const COUNT: u32 = DATA + 16;
const INNER_COUNT: u32 = DATA + 20;
const HRESULT: u32 = DATA + 24;
const NESTED_HRESULT: u32 = DATA + 28;
const CREATE_HRESULT: u32 = DATA + 32;
const DEVICE: u32 = DATA + 36;
const STACK_BEFORE: u32 = DATA + 40;
const STACK_AFTER: u32 = DATA + 44;
const INNER_DESCRIPTION: u32 = DATA + 48;
const INNER_MODULE: u32 = DATA + 52;
const INNER_CONTEXT: u32 = DATA + 56;
const RELEASE_COUNT: u32 = DATA + 60;
const TEXT: u32 = DATA + 0x100;
const MODULE_TEXT: u32 = DATA + 0x180;
const INNER_TEXT: u32 = DATA + 0x200;
const INNER_MODULE_TEXT: u32 = DATA + 0x280;

struct Baseline {
    heap_marker: u32,
    objects: usize,
}

fn capture_baseline(env: &mut Env<'_>) -> Outcome {
    let heaps = env.ext.get_mut::<Heaps>();
    let pointer = heaps.alloc(env.cpu, &mut env.sys.vm, 0, 16, false).unwrap();
    heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
    env.ext.insert(Baseline {
        heap_marker: pointer,
        objects: env.sys.objects.object_count(),
    });
    Outcome::Return(0)
}

fn registry(installed: bool) -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.add(
        "<dsound-enumeration-fixture>",
        "capture_baseline",
        Conv::Stdcall { argc: 0 },
        capture_baseline,
    );
    if installed {
        adv_dx::dsound::register(&mut registry);
    } else {
        adv_dx::dsound::register_unavailable(&mut registry);
    }
    registry
}
fn machine(jit: bool, installed: bool, enabled: bool, codepage: u32) -> Machine<JitCpu> {
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry(installed),
        Clock::virtual_time(),
    )
    .unwrap();
    let mut config = Win32Config::new(
        "/nonexistent/dsound-enum",
        "fixture.exe",
        "/nonexistent/dsound-enum-data",
    );
    config.os.codepage = codepage;
    Win32::install(&mut m, config).unwrap();
    if installed {
        adv_dx::dsound::install(&mut m).unwrap();
        m.ext.get_mut::<adv_dx::dsound::Sound>().enabled = enabled;
    }
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m
}
fn increment(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0xff, 0x05]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}
fn capture(code: &mut Asm, offset: u8, output: u32) {
    code.mov_eax_esp(offset).mov_mem_eax(output);
}
fn copy(m: &mut Machine<JitCpu>, code: &mut Asm, source: u32, output: u32, wide: bool) {
    code.mov_eax_mem(source).push_eax().push_imm(output).call(
        m.resolve_import("kernel32.dll", if wide { "lstrcpyW" } else { "lstrcpyA" })
            .unwrap(),
    );
}
fn heap_marker(m: &mut Machine<JitCpu>) -> u32 {
    let heaps = m.ext.get_mut::<Heaps>();
    let pointer = heaps
        .alloc(&mut m.cpu, &mut m.sys.vm, 0, 16, false)
        .unwrap();
    heaps.free(&mut m.cpu, &mut m.sys.vm, 0, pointer);
    pointer
}
fn run(m: &mut Machine<JitCpu>, code: Asm) -> (RunResult, u32, usize) {
    m.cpu.write(CODE, &code.bytes).unwrap();
    // start_main schedules Win32's startup gate; execution creates its persistent
    // loader mutex. Sample after that gate releases, before the fixture entry.
    let checkpoint = m
        .resolve_import("<dsound-enumeration-fixture>", "capture_baseline")
        .unwrap();
    m.sys.process_ready.push(InitCall {
        target: checkpoint,
        args: vec![],
        what: "DirectSound test baseline after process startup".into(),
        failure_exit: None,
        begin: None,
    });
    m.start_main(CODE, CODE, 65536, "fixture.exe").unwrap();
    let result = m.run_virtual(1_000_000);
    let baseline = m.ext.get::<Baseline>();
    (result, baseline.heap_marker, baseline.objects)
}
fn text(m: &Machine<JitCpu>, output: u32, wide: bool) -> String {
    if wide {
        let mut units = Vec::new();
        for index in 0..64 {
            let unit = m.cpu.read_u16(output + index * 2).unwrap();
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        String::from_utf16(&units).unwrap()
    } else {
        m.ext
            .get::<Win32>()
            .codec
            .decode(&m.cpu.read_cstr(output, 64).unwrap())
    }
}
fn enumeration(jit: bool, wide: bool, keep_going: u32, codepage: u32) {
    let mut m = machine(jit, true, true, codepage);
    let mut inner = Asm::new(INNER);
    increment(&mut inner, INNER_COUNT);
    capture(&mut inner, 8, INNER_DESCRIPTION);
    capture(&mut inner, 12, INNER_MODULE);
    capture(&mut inner, 16, INNER_CONTEXT);
    copy(&mut m, &mut inner, INNER_DESCRIPTION, INNER_TEXT, true);
    copy(&mut m, &mut inner, INNER_MODULE, INNER_MODULE_TEXT, true);
    inner
        .mov_mem_imm(DATA + 0x70, 0)
        .mov_eax_mem(DATA + 0x70)
        .ret_n(16);
    m.cpu.write(INNER, &inner.bytes).unwrap();

    let mut callback = Asm::new(CALLBACK);
    increment(&mut callback, COUNT);
    capture(&mut callback, 4, GUID);
    capture(&mut callback, 8, DESCRIPTION);
    capture(&mut callback, 12, MODULE);
    capture(&mut callback, 16, CONTEXT);
    callback
        .push_imm(1)
        .call(m.resolve_import("kernel32.dll", "Sleep").unwrap());
    callback
        .push_imm(0x1122_3344)
        .push_imm(INNER)
        .call(
            m.resolve_import("dsound.dll", "DirectSoundEnumerateW")
                .unwrap(),
        )
        .mov_mem_eax(NESTED_HRESULT);
    // The outer strings must remain intact after waiting and nested enumeration.
    copy(&mut m, &mut callback, DESCRIPTION, TEXT, wide);
    copy(&mut m, &mut callback, MODULE, MODULE_TEXT, wide);
    // The NULL GUID supplied to the callback must identify a real default device.
    callback
        .push_imm(0)
        .push_imm(DEVICE)
        .push_imm(0)
        .call(m.resolve_import("dsound.dll", "DirectSoundCreate").unwrap())
        .mov_mem_eax(CREATE_HRESULT);
    callback
        .mov_eax_mem(DEVICE)
        .push_eax()
        .mov_eax_ptr_eax()
        .add_eax_imm(8)
        .mov_eax_ptr_eax();
    callback.bytes.extend_from_slice(&[0xff, 0xd0]); // device->Release()
    callback
        .mov_mem_eax(RELEASE_COUNT)
        .mov_mem_imm(DATA + 0x70, keep_going)
        .mov_eax_mem(DATA + 0x70)
        .ret_n(16);
    m.cpu.write(CALLBACK, &callback.bytes).unwrap();

    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(STACK_BEFORE)
        .push_imm(0xfedc_ba98)
        .push_imm(CALLBACK)
        .call(
            m.resolve_import(
                "dsound.dll",
                if wide {
                    "DirectSoundEnumerateW"
                } else {
                    "DirectSoundEnumerateA"
                },
            )
            .unwrap(),
        )
        .mov_mem_eax(HRESULT);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(STACK_AFTER);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let (result, marker, objects) = run(&mut m, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(INNER_COUNT).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(GUID).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(CONTEXT).unwrap(), 0xfedc_ba98);
    assert_eq!(m.cpu.read_u32(INNER_CONTEXT).unwrap(), 0x1122_3344);
    for output in [HRESULT, NESTED_HRESULT, CREATE_HRESULT] {
        assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    }
    assert_eq!(m.cpu.read_u32(RELEASE_COUNT).unwrap(), 0);
    assert_eq!(text(&m, TEXT, wide), "Primary Sound Driver");
    assert_eq!(text(&m, MODULE_TEXT, wide), "dsound.dll");
    assert_eq!(text(&m, INNER_TEXT, true), "Primary Sound Driver");
    assert_eq!(text(&m, INNER_MODULE_TEXT, true), "dsound.dll");
    assert_ne!(
        m.cpu.read_u32(DESCRIPTION).unwrap(),
        m.cpu.read_u32(INNER_DESCRIPTION).unwrap()
    );
    assert_eq!(
        m.cpu.read_u32(STACK_BEFORE).unwrap(),
        m.cpu.read_u32(STACK_AFTER).unwrap()
    );
    assert_eq!(m.sys.contexts.len(), 1);
    assert_eq!(m.sys.objects.object_count(), objects);
    assert_eq!(
        heap_marker(&mut m),
        marker,
        "enumeration scratch must not leak guest heap blocks"
    );
    assert_eq!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .map_or(0, |d| d.stub_calls),
        0
    );
}

fn empty_or_invalid(jit: bool, installed: bool, enabled: bool) {
    let mut m = machine(jit, installed, enabled, 932);
    let mut callback = Asm::new(CALLBACK);
    increment(&mut callback, COUNT);
    callback.ret_n(16);
    m.cpu.write(CALLBACK, &callback.bytes).unwrap();
    let mut code = Asm::new(CODE);
    for (index, (name, callback, expected)) in [
        ("DirectSoundEnumerateA", CALLBACK, 0),
        ("DirectSoundEnumerateW", CALLBACK, 0),
        ("DirectSoundEnumerateA", 0, 0x8007_0057),
        ("DirectSoundEnumerateW", DATA, 0x8007_0057),
    ]
    .into_iter()
    .enumerate()
    {
        code.push_imm(0)
            .push_imm(callback)
            .call(m.resolve_import("dsound.dll", name).unwrap())
            .mov_mem_eax(DATA + 0x80 + index as u32 * 4);
        m.cpu
            .write_u32(DATA + 0xa0 + index as u32 * 4, expected)
            .unwrap();
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(matches!(run(&mut m, code).0, RunResult::Exited(0)));
    for index in 0..4 {
        assert_eq!(
            m.cpu.read_u32(DATA + 0x80 + index * 4).unwrap(),
            m.cpu.read_u32(DATA + 0xa0 + index * 4).unwrap()
        );
    }
    assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 0);
    assert_eq!(m.sys.contexts.len(), 1);
    assert_eq!(m.ext.get::<ApiDiagnostics>().stub_calls, 0);
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(name, _)| name.starts_with("DirectSoundEnumerate"))
    );
}

#[test]
fn default_enumeration_is_synchronous_reentrant_and_creatable_for_ansi_and_utf16() {
    for jit in [false, true] {
        for wide in [false, true] {
            for keep_going in [0, 1] {
                enumeration(jit, wide, keep_going, 932);
            }
        }
        for codepage in [936, 65001] {
            enumeration(jit, false, 1, codepage);
        }
    }
}
#[test]
fn unavailable_and_disabled_audio_enumerate_no_devices_and_reject_bad_callbacks() {
    for jit in [false, true] {
        empty_or_invalid(jit, false, false);
        empty_or_invalid(jit, true, false);
    }
}
#[test]
fn enumeration_descriptors_match_installed_and_unavailable_registration() {
    for installed in [false, true] {
        let registry = registry(installed);
        assert!(registry.signature_conflicts().is_empty());
        for name in ["DirectSoundEnumerateA", "DirectSoundEnumerateW"] {
            assert!(registry.dynamic_available("dsound.dll", name));
            assert_eq!(
                registry.level("dsound.dll", name),
                Some(if installed {
                    ImplementationLevel::Implement
                } else {
                    ImplementationLevel::State
                })
            );
        }
        assert!(registry.dynamic_available("dsound.dll", "GetDeviceID"));
        assert_eq!(
            registry.level("dsound.dll", "GetDeviceID"),
            Some(if installed {
                ImplementationLevel::Implement
            } else {
                ImplementationLevel::SafeFail
            })
        );
    }
}

// spec: PERS-EXPORT-1
#[test]
fn device_ids_name_one_playback_device_that_create_opens() {
    const SOURCE: u32 = DATA + 0x300;
    const ID: u32 = DATA + 0x340;
    const RESULTS: u32 = DATA + 0x380;
    let default = |index: u8| {
        let mut guid = [
            0x00, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b,
            0x5c, 0x03,
        ];
        guid[0] = index;
        guid
    };
    for jit in [false, true] {
        let mut m = machine(jit, true, true, 932);
        // DSDEVID_DefaultPlayback, then DSDEVID_DefaultCapture.
        m.cpu.write(SOURCE, &default(0)).unwrap();
        m.cpu.write(SOURCE + 16, &default(1)).unwrap();
        let get = m.resolve_import("dsound.dll", "GetDeviceID").unwrap();
        let create = m.resolve_import("dsound.dll", "DirectSoundCreate").unwrap();
        let mut code = Asm::new(CODE);
        // The alias, the identity itself, a capture device and no source.
        for (n, (source, dest)) in [
            (SOURCE, ID),
            (ID, ID + 16),
            (SOURCE + 16, ID + 32),
            (0, ID + 32),
        ]
        .into_iter()
        .enumerate()
        {
            code.push_imm(dest)
                .push_imm(source)
                .call(get)
                .mov_mem_eax(RESULTS + 4 * n as u32);
        }
        code.push_imm(0)
            .push_imm(DEVICE)
            .push_imm(ID)
            .call(create)
            .mov_mem_eax(RESULTS + 16);
        code.ret();
        let (res, _, _) = run(&mut m, code);
        assert!(matches!(res, RunResult::Exited(_)), "{res:?}");
        let results: Vec<u32> = (0..5)
            .map(|n| m.cpu.read_u32(RESULTS + 4 * n).unwrap())
            .collect();
        assert_eq!(results, [0, 0, 0x8878_0078, 0x8007_0057, 0]);
        let mut ids = [0; 32];
        m.cpu.read(ID, &mut ids).unwrap();
        assert_eq!(ids[..16], ids[16..]);
        assert_ne!(ids[..16], default(0));
        assert_ne!(ids[..16], [0; 16]);
        assert_ne!(m.cpu.read_u32(DEVICE).unwrap(), 0);
    }
}
