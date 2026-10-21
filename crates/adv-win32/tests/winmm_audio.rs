//! Authored no-WaveOut/no-MCI/no-mixer-device requests; no audio device or game is used.
use adv_abi::{ApiDiagnostics, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const RESULTS: u32 = DATA + 0x800;
const MARKER: u32 = 0x1234_5678;
const CALLBACK: u32 = CODE + 0xe00;
const CALLBACK_COUNT: u32 = DATA + 0x700;
const STACK_BEFORE: u32 = DATA + 0x704;
const STACK_AFTER: u32 = DATA + 0x708;

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut m = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/winmm-audio",
            "fixture.exe",
            "/nonexistent/winmm-audio-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    let mut callback = Asm::new(CALLBACK);
    callback.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [CALLBACK_COUNT]
    callback
        .bytes
        .extend_from_slice(&CALLBACK_COUNT.to_le_bytes());
    callback.ret_n(20);
    m.cpu.write(CALLBACK, &callback.bytes).unwrap();
    m
}
fn start() -> Asm {
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(STACK_BEFORE);
    code
}
fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], index: u32) {
    code.push_imm(MARKER)
        .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(m.resolve_import("winmm.dll", name).unwrap())
        .mov_mem_eax(RESULTS + index * 8);
    code.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(RESULTS + index * 8 + 4);
}
fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(STACK_AFTER);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.start_main(CODE, CODE, 65536, "fixture.exe").unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.sys.contexts.len(),
        1,
        "no waveform/MCI worker may be fabricated"
    );
    assert_eq!(
        m.cpu.read_u32(CALLBACK_COUNT).unwrap(),
        0,
        "no open/close/done callback may be fabricated"
    );
    assert_eq!(
        m.cpu.read_u32(STACK_BEFORE).unwrap(),
        m.cpu.read_u32(STACK_AFTER).unwrap()
    );
    assert_eq!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .map_or(0, |d| d.stub_calls),
        0
    );
}
fn result<B: Backend>(m: &Machine<B>, index: u32, expected: u32) {
    assert_eq!(
        m.cpu.read_u32(RESULTS + index * 8).unwrap(),
        expected,
        "call {index}"
    );
    assert_eq!(
        m.cpu.read_u32(RESULTS + index * 8 + 4).unwrap(),
        MARKER,
        "MMRESULT must not replace LastError"
    );
}
fn bytes<B: Backend>(m: &Machine<B>, address: u32, count: usize) -> Vec<u8> {
    let mut bytes = vec![0; count];
    m.cpu.read(address, &mut bytes).unwrap();
    bytes
}
fn pcm() -> Vec<u8> {
    [
        1u16.to_le_bytes().as_slice(),
        2u16.to_le_bytes().as_slice(),
        44100u32.to_le_bytes().as_slice(),
        176400u32.to_le_bytes().as_slice(),
        4u16.to_le_bytes().as_slice(),
        16u16.to_le_bytes().as_slice(),
    ]
    .concat()
}

fn wave_boundary<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    m.cpu.write(DATA, &[0x77; 52]).unwrap();
    m.cpu.write(DATA + 0x100, &pcm()).unwrap();
    m.cpu.write_u32(DATA + 0x200, 0xfeed_face).unwrap();
    let header = DATA + 0x300;
    m.cpu.write(header, &[0x55; 32]).unwrap();
    let mut code = start();
    call(&mut m, &mut code, "waveOutGetNumDevs", &[], 0);
    call(
        &mut m,
        &mut code,
        "waveOutGetDevCapsA",
        &[u32::MAX, DATA, 52],
        1,
    );
    call(&mut m, &mut code, "waveOutGetDevCapsA", &[0, DATA, 52], 2);
    call(
        &mut m,
        &mut code,
        "waveOutOpen",
        &[DATA + 0x200, u32::MAX, DATA + 0x100, CALLBACK, 0, 0x30000],
        3,
    );
    let headers = [
        "waveOutPrepareHeader",
        "waveOutUnprepareHeader",
        "waveOutWrite",
    ];
    for (i, name) in headers.into_iter().enumerate() {
        call(&mut m, &mut code, name, &[0, header, 32], i as u32 + 4);
    }
    for (i, name) in [
        "waveOutPause",
        "waveOutRestart",
        "waveOutReset",
        "waveOutClose",
    ]
    .into_iter()
    .enumerate()
    {
        call(&mut m, &mut code, name, &[0xdead_beef], i as u32 + 7);
    }
    // Invalid handles take precedence over unused/inaccessible header arguments.
    call(
        &mut m,
        &mut code,
        "waveOutPrepareHeader",
        &[0xdead_beef, READ_ONLY, 0],
        11,
    );
    call(&mut m, &mut code, "waveOutWrite", &[0, 0xdead_beef, 0], 12);
    run(&mut m, code);
    result(&m, 0, 0);
    result(&m, 1, 6);
    result(&m, 2, 2);
    result(&m, 3, 6);
    for index in 4..=12 {
        result(&m, index, 5);
    }
    assert_eq!(bytes(&m, DATA, 52), vec![0x77; 52]);
    assert_eq!(m.cpu.read_u32(DATA + 0x200).unwrap(), 0);
    assert_eq!(bytes(&m, header, 32), vec![0x55; 32]);
    let diagnostics = m.ext.get::<ApiDiagnostics>();
    for name in [
        "waveOutGetNumDevs",
        "waveOutGetDevCapsA",
        "waveOutOpen",
        "waveOutPrepareHeader",
        "waveOutUnprepareHeader",
        "waveOutWrite",
        "waveOutPause",
        "waveOutRestart",
        "waveOutReset",
        "waveOutClose",
    ] {
        assert!(diagnostics.warnings.keys().any(|(api, _)| api == name));
    }
}

fn wave_parameters<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let legacy = READ_ONLY + 4096 - 16;
    m.cpu.write(legacy, &pcm()).unwrap();
    let extended = DATA + 0x100;
    let mut format = pcm();
    format[0..2].copy_from_slice(&3u16.to_le_bytes());
    format.extend_from_slice(&4096u16.to_le_bytes());
    m.cpu.write(extended, &format).unwrap();
    m.cpu.write(READ_ONLY, &[0x66; 52]).unwrap();
    for i in 0..8 {
        m.cpu.write_u32(DATA + 0x300 + i * 4, 0xfeed_face).unwrap();
    }
    let opens = [
        (0, legacy, 1, 6),
        (DATA + 0x300, legacy, 1, 6),
        (DATA + 0x304, legacy, 0, 6),
        (0, legacy, 0, 11),
        (READ_ONLY, legacy, 0, 11),
        (DATA + 0x308, 0, 0, 11),
        (DATA + 0x30c, extended, 0, 11),
        (DATA + 0x310, legacy, 0x40000, 11),
    ];
    let mut code = start();
    for (index, &(output, format, flags, _)) in opens.iter().enumerate() {
        call(
            &mut m,
            &mut code,
            "waveOutOpen",
            &[output, u32::MAX, format, 0, 0, flags],
            index as u32,
        );
    }
    call(
        &mut m,
        &mut code,
        "waveOutGetDevCapsA",
        &[u32::MAX, 0, 0],
        8,
    );
    call(
        &mut m,
        &mut code,
        "waveOutGetDevCapsA",
        &[u32::MAX, READ_ONLY, 52],
        9,
    );
    run(&mut m, code);
    for (index, &(_, _, _, error)) in opens.iter().enumerate() {
        result(&m, index as u32, error);
    }
    result(&m, 8, 0);
    result(&m, 9, 11);
    assert_eq!(m.cpu.read_u32(DATA + 0x304).unwrap(), 0);
    for offset in [0x300, 0x308, 0x30c, 0x310] {
        assert_eq!(m.cpu.read_u32(DATA + offset).unwrap(), 0xfeed_face);
    }
    assert_eq!(bytes(&m, READ_ONLY, 52), vec![0x66; 52]);
}

fn mci_boundary<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let block = DATA + 0x100;
    m.cpu.write(block, &[0; 20]).unwrap();
    m.cpu.write_u32(block + 4, 0xfeed_face).unwrap();
    m.cpu.write_u32(block + 8, DATA + 0x200).unwrap();
    m.cpu.write(DATA + 0x200, b"waveaudio\0").unwrap();
    m.cpu.write(READ_ONLY, &[0x66; 32]).unwrap();
    let output = DATA + 0x300;
    m.cpu.write(output, &[0x77; 256]).unwrap();
    let mut code = start();
    call(
        &mut m,
        &mut code,
        "mciSendCommandA",
        &[0, 0x803, 0x2000 | 1, block],
        0,
    );
    call(
        &mut m,
        &mut code,
        "mciSendCommandA",
        &[1, 0x804, 1, 0xdead_beef],
        1,
    );
    call(&mut m, &mut code, "mciSendCommandA", &[0, 0x803, 0, 0], 2);
    call(
        &mut m,
        &mut code,
        "mciSendCommandA",
        &[0, 0x803, 0, READ_ONLY],
        3,
    );
    call(
        &mut m,
        &mut code,
        "mciGetErrorStringA",
        &[306, output, 128],
        4,
    );
    call(
        &mut m,
        &mut code,
        "mciGetErrorStringA",
        &[257, output + 128, 5],
        5,
    );
    call(
        &mut m,
        &mut code,
        "mciGetErrorStringA",
        &[297, output + 136, 1],
        6,
    );
    call(
        &mut m,
        &mut code,
        "mciGetErrorStringA",
        &[306, output + 144, 0],
        7,
    );
    call(&mut m, &mut code, "mciGetErrorStringA", &[306, 0, 128], 8);
    call(
        &mut m,
        &mut code,
        "mciGetErrorStringA",
        &[306, READ_ONLY, 32],
        9,
    );
    call(
        &mut m,
        &mut code,
        "mciGetErrorStringA",
        &[0x1234, output + 152, 32],
        10,
    );
    run(&mut m, code);
    for (index, error) in [306, 257, 297, 273, 1, 1, 1, 0, 0, 0, 0]
        .into_iter()
        .enumerate()
    {
        result(&m, index as u32, error);
    }
    assert_eq!(m.cpu.read_u32(block + 4).unwrap(), 0);
    assert_eq!(
        m.cpu.read_cstr(output, 128).unwrap(),
        b"The specified MCI device is not installed."
    );
    assert_eq!(
        bytes(&m, output + 128, 6),
        [b'T', b'h', b'e', b' ', 0, 0x77]
    );
    assert_eq!(bytes(&m, output + 136, 2), [0, 0x77]);
    assert_eq!(bytes(&m, output + 144, 32), vec![0x77; 32]);
    assert_eq!(bytes(&m, READ_ONLY, 32), vec![0x66; 32]);
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api == "mciSendCommandA")
    );
}

fn mci_strings<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let commands: [&[u8]; 4] = [
        b"close SiglusMCIMovie wait",
        b"seek SiglusMCIMovie to start",
        b"open \"op.wav\" type waveaudio alias SiglusMCIMovie wait",
        b"status SiglusMCIMovie mode wait",
    ];
    let mut at = DATA + 0x100;
    let mut pointers = Vec::new();
    for command in commands {
        m.cpu.write(at, command).unwrap();
        m.cpu.write(at + command.len() as u32, &[0]).unwrap();
        pointers.push(at);
        at += 0x60;
    }
    let wide: Vec<u8> = "close SiglusMCIMovie wait\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(DATA + 0x400, &wide).unwrap();
    let output = DATA + 0x500;
    m.cpu.write(output, &[0x77; 64]).unwrap();
    let mut code = start();
    for (index, &pointer) in pointers.iter().enumerate() {
        call(
            &mut m,
            &mut code,
            "mciSendStringA",
            &[pointer, output, 64, 0],
            index as u32,
        );
    }
    call(
        &mut m,
        &mut code,
        "mciSendStringW",
        &[DATA + 0x400, output, 32, 0],
        4,
    );
    call(&mut m, &mut code, "mciSendStringA", &[0, output, 64, 0], 5);
    run(&mut m, code);
    // Unknown alias, unknown verb, a device type that is not installed in any
    // build (its alias is not kept), then the W front end and a missing
    // command string.
    for (index, error) in [263, 261, 306, 263, 263, 267].into_iter().enumerate() {
        result(&m, index as u32, error);
    }
    assert_eq!(bytes(&m, output, 64), vec![0x77; 64]);
}

#[test]
// spec: MEDIA-MCI-3
fn mci_strings_share_device_names_and_report_parser_errors() {
    mci_strings(JitCpu::new(PARK_ADDR, false).unwrap());
    mci_strings(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn waveform_exports_state_and_safe_fail_levels_match_the_absent_front() {
    let registry = registry();
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.level("winmm.dll", "waveOutGetNumDevs"),
        Some(ImplementationLevel::State)
    );
    // MCI digital video commands are implemented; wave output stays absent.
    for name in ["mciGetErrorStringA", "mciSendCommandA"] {
        assert_eq!(
            registry.level("winmm.dll", name),
            Some(ImplementationLevel::Implement)
        );
    }
    for name in [
        "waveOutGetDevCapsA",
        "waveOutOpen",
        "waveOutPrepareHeader",
        "waveOutUnprepareHeader",
        "waveOutWrite",
        "waveOutPause",
        "waveOutRestart",
        "waveOutReset",
        "waveOutClose",
    ] {
        assert_eq!(
            registry.level("winmm.dll", name),
            Some(ImplementationLevel::SafeFail)
        );
        assert!(registry.dynamic_available("winmm.dll", name));
    }
}
#[test]
fn no_wave_device_or_completion_is_fabricated_and_headers_stay_unchanged() {
    wave_boundary(JitCpu::new(PARK_ADDR, false).unwrap());
    wave_boundary(JitCpu::new(PARK_ADDR, true).unwrap());
    wave_boundary(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn waveform_open_queries_and_invalid_outputs_or_formats_preserve_their_contract() {
    wave_parameters(JitCpu::new(PARK_ADDR, false).unwrap());
    wave_parameters(JitCpu::new(PARK_ADDR, true).unwrap());
    wave_parameters(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn mci_no_device_errors_have_checked_bounded_text_and_no_notifications() {
    mci_boundary(JitCpu::new(PARK_ADDR, false).unwrap());
    mci_boundary(JitCpu::new(PARK_ADDR, true).unwrap());
    mci_boundary(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn midi_boundary<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let mut code = start();
    m.cpu.write(DATA, &[0x77; 512]).unwrap();
    m.cpu.write(READ_ONLY, &[0x66; 32]).unwrap();
    m.cpu.write_u32(DATA + 128, 0).unwrap();
    m.cpu.write_u32(DATA + 132, u32::MAX).unwrap();
    m.cpu.write_u32(DATA + 136, 0x1234).unwrap();
    m.cpu.write_u32(DATA + 140, 0x1234).unwrap();
    call(&mut m, &mut code, "midiOutGetNumDevs", &[], 0);
    call(
        &mut m,
        &mut code,
        "midiOutGetDevCapsA",
        &[0, DATA + 256, 52],
        1,
    );
    call(
        &mut m,
        &mut code,
        "midiOutGetDevCapsA",
        &[u32::MAX, DATA + 256, 52],
        2,
    );
    call(&mut m, &mut code, "midiOutGetDevCapsA", &[0, 0, 0], 3);
    call(
        &mut m,
        &mut code,
        "midiOutGetDevCapsA",
        &[0, READ_ONLY, 52],
        4,
    );
    call(
        &mut m,
        &mut code,
        "midiStreamOpen",
        &[DATA, DATA + 128, 1, CALLBACK, MARKER, 0x30000],
        5,
    );
    call(
        &mut m,
        &mut code,
        "midiStreamOpen",
        &[DATA + 4, DATA + 132, 1, 0, 0, 0],
        6,
    );
    call(
        &mut m,
        &mut code,
        "midiStreamOpen",
        &[DATA + 8, DATA + 136, 0, 0, 0, 0],
        7,
    );
    call(
        &mut m,
        &mut code,
        "midiStreamOpen",
        &[READ_ONLY, DATA + 140, 1, 0, 0, 0],
        8,
    );
    call(
        &mut m,
        &mut code,
        "midiStreamOpen",
        &[DATA + 12, READ_ONLY, 1, 0, 0, 0],
        9,
    );
    run(&mut m, code);
    for (index, error) in [0, 2, 6, 0, 11, 2, 6, 11, 11, 11].into_iter().enumerate() {
        result(&m, index as u32, error);
    }
    for offset in [0, 4, 8, 12] {
        assert_eq!(m.cpu.read_u32(DATA + offset).unwrap(), 0);
    }
    assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0x8000_0000);
    assert_eq!(m.cpu.read_u32(DATA + 132).unwrap(), u32::MAX);
    assert_eq!(m.cpu.read_u32(DATA + 136).unwrap(), 0x1234);
    assert_eq!(m.cpu.read_u32(DATA + 140).unwrap(), 0x1234);
    assert_eq!(bytes(&m, DATA + 256, 52), vec![0x77; 52]);
    assert_eq!(bytes(&m, READ_ONLY, 32), vec![0x66; 32]);
    assert_eq!(
        registry().level("winmm.dll", "midiStreamOpen"),
        Some(ImplementationLevel::SafeFail)
    );
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
// spec: MEDIA-AUDIO-5
fn midi_absent_device_count_caps_and_stream_errors_preserve_buffers_and_callbacks() {
    midi_boundary(JitCpu::new(PARK_ADDR, false).unwrap());
    midi_boundary(JitCpu::new(PARK_ADDR, true).unwrap());
    midi_boundary(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn midi_absent_operations<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    m.cpu.write(DATA, &[0x77; 512]).unwrap();
    let mut code = start();
    let mut cases: Vec<(&str, Vec<u32>, u32)> = vec![
        ("midiInGetNumDevs", vec![], 0),
        ("midiOutOpen", vec![DATA, 0, CALLBACK, MARKER, 0x30000], 2),
        ("midiOutOpen", vec![DATA + 4, u32::MAX, 0, 0, 0], 6),
        (
            "midiInOpen",
            vec![DATA + 8, 0, CALLBACK, MARKER, 0x30020],
            2,
        ),
        ("midiInOpen", vec![DATA + 12, u32::MAX, 0, 0, 0], 2),
        ("midiOutOpen", vec![READ_ONLY, 0, 0, 0, 0], 11),
        ("midiInOpen", vec![0, 0, 0, 0, 0], 11),
        ("midiOutOpen", vec![DATA, 0, 0, 0, 1], 10),
        ("midiInOpen", vec![DATA + 8, 0, 0, 0, 0x40000], 10),
        ("midiInGetDevCapsA", vec![0, DATA + 128, 44], 2),
        ("midiInGetDevCapsW", vec![u32::MAX, DATA + 128, 76], 2),
        ("midiInGetDevCapsA", vec![0, 0, 0], 0),
        ("midiInGetDevCapsW", vec![0, READ_ONLY, 76], 11),
    ];
    cases.push((
        "midiConnect",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiDisconnect",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiInAddBuffer",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiInClose", vec![0xdead_beef], 5));
    cases.push(("midiInGetID", vec![0xdead_beef, 0xdead_beef], 5));
    cases.push((
        "midiInMessage",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiInPrepareHeader",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiInReset", vec![0xdead_beef], 5));
    cases.push(("midiInStart", vec![0xdead_beef], 5));
    cases.push(("midiInStop", vec![0xdead_beef], 5));
    cases.push((
        "midiInUnprepareHeader",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiOutCacheDrumPatches",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiOutCachePatches",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiOutClose", vec![0xdead_beef], 5));
    cases.push(("midiOutGetID", vec![0xdead_beef, 0xdead_beef], 5));
    cases.push(("midiOutGetVolume", vec![0xdead_beef, 0xdead_beef], 2));
    cases.push((
        "midiOutLongMsg",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiOutMessage",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiOutPrepareHeader",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiOutReset", vec![0xdead_beef], 5));
    cases.push(("midiOutSetVolume", vec![0xdead_beef, 0xdead_beef], 2));
    cases.push(("midiOutShortMsg", vec![0xdead_beef, 0xdead_beef], 5));
    cases.push((
        "midiOutUnprepareHeader",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiStreamClose", vec![0xdead_beef], 5));
    cases.push((
        "midiStreamOut",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiStreamPause", vec![0xdead_beef], 5));
    cases.push((
        "midiStreamPosition",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push((
        "midiStreamProperty",
        vec![0xdead_beef, 0xdead_beef, 0xdead_beef],
        5,
    ));
    cases.push(("midiStreamRestart", vec![0xdead_beef], 5));
    cases.push(("midiStreamStop", vec![0xdead_beef], 5));
    for (index, (name, args, _)) in cases.iter().enumerate() {
        call(&mut m, &mut code, name, args, index as u32);
    }
    run(&mut m, code);
    for (index, (_, _, expected)) in cases.iter().enumerate() {
        result(&m, index as u32, *expected);
    }
    assert_eq!(bytes(&m, DATA, 16), vec![0; 16]);
    assert_eq!(bytes(&m, DATA + 16, 496), vec![0x77; 496]);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
// spec: MEDIA-AUDIO-5
fn midi_input_output_and_stream_families_share_absent_devices() {
    midi_absent_operations(JitCpu::new(PARK_ADDR, false).unwrap());
    midi_absent_operations(JitCpu::new(PARK_ADDR, true).unwrap());
    midi_absent_operations(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[cfg(windows)]
#[test]
fn midi_native_stream_failure_clears_handle_and_tags_device_after_count_validation() {
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn midiStreamOpen(
            output: *mut usize,
            device: *mut u32,
            count: u32,
            callback: usize,
            instance: usize,
            flags: u32,
        ) -> u32;
    }
    unsafe {
        for (count, expected) in [(0, 11), (2, 11), (1, 2)] {
            let mut output = 0xfedcba98;
            let mut device = 0x7ffffffe;
            assert_eq!(
                midiStreamOpen(&mut output, &mut device, count, 0, 0, 0),
                expected
            );
            assert_eq!(output, 0);
            assert_eq!(device, if count == 1 { 0xfffffffe } else { 0x7ffffffe });
        }
    }
}

fn mixer_boundary<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    // Structures whose cbStruct is right, and one whose size is wrong.
    m.cpu.write_u32(DATA, 0x118).unwrap(); // MIXERLINEW
    m.cpu.write_u32(DATA + 0x200, 0xa8).unwrap(); // MIXERLINEA
    m.cpu.write_u32(DATA + 0x300, 0x18).unwrap(); // controls / details
    m.cpu.write_u32(DATA + 0x400, 0x17).unwrap();
    let mut code = start();
    let calls: [(&str, [u32; 3], u32); 13] = [
        ("mixerGetNumDevs", [0; 3], 0),
        ("mixerGetDevCapsW", [0, DATA + 0x500, 0x50], 2),
        ("mixerGetDevCapsA", [0, 0, 0x30], 11),
        ("mixerGetLineInfoW", [0, DATA, 1], 2),
        ("mixerGetLineInfoW", [0, DATA, 0x8000_0001], 5),
        ("mixerGetLineInfoW", [0, DATA + 0x200, 1], 11),
        ("mixerGetLineInfoA", [0, DATA + 0x200, 0], 2),
        ("mixerGetLineControlsW", [0, DATA + 0x300, 0], 2),
        ("mixerGetLineControlsA", [0, 0, 0], 11),
        ("mixerGetControlDetailsW", [0, DATA + 0x300, 0x8000_0000], 5),
        ("mixerGetControlDetailsA", [0, DATA + 0x400, 0], 11),
        ("mixerSetControlDetails", [0, DATA + 0x300, 0], 2),
        ("mixerOpen", [DATA + 0x600, 0, 0], 6),
    ];
    for (index, (name, args, _)) in calls.iter().enumerate() {
        let args: &[u32] = match *name {
            "mixerGetNumDevs" => &[],
            "mixerOpen" => &[args[0], 0, 0, 0, 0],
            _ => args,
        };
        call(&mut m, &mut code, name, args, index as u32);
    }
    run(&mut m, code);
    for (index, (_, _, expected)) in calls.iter().enumerate() {
        result(&m, index as u32, *expected);
    }
    assert_eq!(bytes(&m, DATA + 0x500, 0x50), vec![0; 0x50]);
}
#[test]
fn mixer_queries_without_a_device_fail_before_touching_their_structures() {
    mixer_boundary(JitCpu::new(PARK_ADDR, false).unwrap());
    mixer_boundary(JitCpu::new(PARK_ADDR, true).unwrap());
    mixer_boundary(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn midi_error_descriptions<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    m.cpu.write(DATA, &[0x77; 512]).unwrap();
    let mut code = start();
    let mut expected = Vec::new();
    for (n, name) in [
        "midiOutGetErrorTextA",
        "midiOutGetErrorTextW",
        "midiInGetErrorTextA",
        "midiInGetErrorTextW",
    ]
    .into_iter()
    .enumerate()
    {
        let output = DATA + n as u32 * 64;
        for (error, pointer, capacity, result) in [
            (5, output + 4, 16, 0),
            (5, output + 40, 1, 0),
            (5, 0, 0, 0),
            (0xffff, 0, 0, 0),
            (5, READ_ONLY, 16, 11),
            (0xffff, output + 48, 4, 9),
        ] {
            call(
                &mut m,
                &mut code,
                name,
                &[error, pointer, capacity],
                expected.len() as u32,
            );
            expected.push(result);
        }
    }
    run(&mut m, code);
    for (index, expected) in expected.into_iter().enumerate() {
        result(&m, index as u32, expected);
    }
    for n in 0..4 {
        let output = DATA + n * 64;
        let mut expected = vec![0x77; 64];
        let message = b"The specified d\0";
        let (encoded, terminator) = if n % 2 == 0 {
            (message.to_vec(), vec![0])
        } else {
            (
                message
                    .iter()
                    .flat_map(|b| u16::from(*b).to_le_bytes())
                    .collect(),
                vec![0, 0],
            )
        };
        expected[4..4 + encoded.len()].copy_from_slice(&encoded);
        expected[40..40 + terminator.len()].copy_from_slice(&terminator);
        assert_eq!(bytes(&m, output, 64), expected);
    }
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
// spec: MEDIA-AUDIO-6
fn midi_error_text_is_bounded_terminated_and_shared_by_input_and_output() {
    midi_error_descriptions(JitCpu::new(PARK_ADDR, false).unwrap());
    midi_error_descriptions(JitCpu::new(PARK_ADDR, true).unwrap());
    midi_error_descriptions(UnicornCpu::new(PARK_ADDR).unwrap());
}
