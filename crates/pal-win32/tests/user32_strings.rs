//! Real guest API calls: explicit counts, ACP, DBCS boundaries and protected ranges.
use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::{Clock, Machine, Registry, RunResult};
use pal_win32::{Win32, Win32Config};

fn convert(name: &str, cp: u32, input: &[u8], count: u32, writable: bool) -> (u32, Vec<u8>) {
    const CODE: u32 = 0x400000;
    const DATA: u32 = 0x410000;
    const RESULT: u32 = 0x420000;
    let mut registry = Registry::default();
    pal_win32::register(&mut registry);
    let mut m = Machine::new(
        UnicornCpu::new(pal_machine::layout::PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let mut cfg = Win32Config::new(".", "case-fixture.exe", "target/case-fixture");
    cfg.codepage = cp;
    Win32::install(&mut m, cfg).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu
        .map(DATA, 8192, if writable { Prot::RW } else { Prot::READ })
        .unwrap();
    m.cpu.map(RESULT, 4096, Prot::RW).unwrap();
    m.cpu.write(DATA, input).unwrap();
    let proc = m.resolve_import("user32.dll", name).unwrap();
    let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(count)
        .push_imm(DATA)
        .call(proc)
        .mov_mem_eax(RESULT)
        .push_imm(0)
        .call(exit);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("main", CODE, &[], 0x10000).unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    let mut output = vec![0; input.len()];
    m.cpu.read(DATA, &mut output).unwrap();
    (m.cpu.read_u32(RESULT).unwrap(), output)
}

#[test]
fn counts_do_not_stop_at_nul_and_do_not_touch_suffix() {
    assert_eq!(
        convert("CharUpperBuffA", 932, b"ab\0cd!suffix", 5, true),
        (5, b"AB\0CD!suffix".to_vec())
    );
    assert_eq!(
        convert("CharLowerBuffA", 936, b"AB\0CD!SUFFIX", 5, true),
        (5, b"ab\0cd!SUFFIX".to_vec())
    );
    assert_eq!(
        convert("CharUpperBuffA", 932, b"abc", 0, true),
        (0, b"abc".to_vec())
    );
}

#[test]
fn dbcs_trails_and_chunk_boundaries_are_not_ascii_letters() {
    for cp in [932, 936] {
        let mut input = vec![b'a'; 4095];
        input.extend_from_slice(&[0x81, 0x61, b'z', 0x81]);
        let mut expected = vec![b'A'; 4095];
        expected.extend_from_slice(&[0x81, 0x61, b'Z', 0x81]);
        assert_eq!(
            convert("CharUpperBuffA", cp, &input, input.len() as u32, true),
            (input.len() as u32, expected)
        );
        let codec = pal_win32::Codec::new(cp);
        let input = codec.encode("ａｂαя漢字");
        let expected = codec.encode("ＡＢΑЯ漢字");
        assert_eq!(
            convert("CharUpperBuffA", cp, &input, input.len() as u32, true),
            (input.len() as u32, expected)
        );
    }
}

#[test]
fn invalid_ranges_do_not_partially_mutate() {
    for (count, writable) in [(3, false), (8193, true), (u32::MAX, true)] {
        assert_eq!(
            convert("CharUpperBuffA", 932, b"abc", count, writable),
            (0, b"abc".to_vec())
        );
    }
    assert_eq!(
        convert("CharUpperBuffW", 932, b"a\0b\0", u32::MAX, true),
        (0, b"a\0b\0".to_vec())
    );
}

#[cfg(windows)]
#[test]
fn wide_case_matches_native_windows_for_mixed_text() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CharUpperBuffW(text: *mut u16, len: u32) -> u32;
        fn CharLowerBuffW(text: *mut u16, len: u32) -> u32;
    }
    let mut units: Vec<u16> = "aZ\0éÖßıİΣσςЯяＡｚ日本語".encode_utf16().collect();
    units.extend([0xd800, 0xdc00, 0xdfff]);
    for (name, upper) in [("CharUpperBuffW", true), ("CharLowerBuffW", false)] {
        let mut reference = units.clone();
        // SAFETY: pointer covers exactly reference.len() initialized UTF-16 units.
        let count = unsafe {
            if upper {
                CharUpperBuffW(reference.as_mut_ptr(), reference.len() as u32)
            } else {
                CharLowerBuffW(reference.as_mut_ptr(), reference.len() as u32)
            }
        };
        let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        let expected: Vec<u8> = reference
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(
            convert(name, 932, &bytes, units.len() as u32, true),
            (count, expected),
            "{name}"
        );
    }
}
