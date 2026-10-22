use super::*;
use crate::{Win32, Win32Config};
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NAME: u32 = DATA + 0x1000;
const OUTPUT: u32 = DATA + 0x1100;
const COUNT: u32 = DATA + 0x1300;
fn run<B: Backend>(cpu: B, cp: u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut cfg = Win32Config::new(".", "fixture.exe", "target/locale-closure-tests");
    cfg.os.codepage = cp;
    Win32::install(&mut m, cfg).unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    for (offset, text) in [
        (0, "Ja-jP"),
        (32, "zh-CN"),
        (64, "invalid-tag"),
        (96, "ja"),
        (128, ""),
    ] {
        m.cpu
            .write(
                NAME + offset,
                &text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    m.cpu.write(OUTPUT, &[0xcd; 256]).unwrap();
    m.cpu.write(NAME + 160, &[0x82, 0xa0, b'A', 0]).unwrap();
    m.cpu
        .write(NAME + 176, &[0x42, 0x30, 0x41, 0, 0, 0])
        .unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut call = |name: &str, args: &[u32], value: u32| {
        for &arg in args.iter().rev() {
            code.push_imm(arg);
        }
        code.call(m.resolve_import("kernel32.dll", name).unwrap())
            .mov_mem_eax(DATA + expected.len() as u32 * 4);
        expected.push(value);
    };
    for (pointer, flags, value) in [
        (NAME, 0, 0x411),
        (NAME + 32, 0, 0x804),
        (NAME + 64, 0, 0),
        (NAME + 96, 0, 0x411),
        (NAME + 96, 0x08000000, 0x11),
        (NAME + 128, 0, 0x7f),
        (0, 0, if cp == 936 { 0x804 } else { 0x411 }),
    ] {
        call("LocaleNameToLCID", &[pointer, flags], value);
    }
    call("IsValidLocaleName", &[NAME], 1);
    call("IsValidLocaleName", &[NAME + 64], 0);
    call("LCIDToLocaleName", &[0x411, OUTPUT, 0, 0], 6);
    call("LCIDToLocaleName", &[0x411, OUTPUT, 5, 0], 0);
    call("LCIDToLocaleName", &[0x411, OUTPUT, 6, 0], 6);
    call("GetUserDefaultLocaleName", &[OUTPUT + 32, 85], 6);
    call("GetLocaleInfoEx", &[NAME, 0x20001004, OUTPUT + 64, 2], 2);
    call("GetLocaleInfoW", &[0x411, 0x20001004, OUTPUT + 68, 2], 2);
    call("GetLocaleInfoA", &[0x409, 0x59, OUTPUT + 72, 8], 3);
    call(
        "GetStringTypeExA",
        &[0x411, 1, NAME + 160, u32::MAX, OUTPUT + 96],
        1,
    );
    call(
        "GetStringTypeExW",
        &[0x411, 1, NAME + 176, u32::MAX, OUTPUT + 112],
        1,
    );
    call(
        "GetStringTypeExW",
        &[0x411, 7, NAME + 176, 1, OUTPUT + 128],
        0,
    );
    const CALLBACK: u32 = CODE + 0x1000;
    const STOP: u32 = CODE + 0x1100;
    const EXTENDED: u32 = CODE + 0x1200;
    call("EnumSystemLocalesA", &[CALLBACK, 1], 1);
    call("EnumSystemLocalesW", &[STOP, 1], 1);
    call("EnumSystemLocalesEx", &[EXTENDED, 0, 0x12345678, 0], 1);
    call("EnumCalendarInfoA", &[CALLBACK, 0x411, 1, 5], 1);
    call("EnumSystemLocalesEx", &[EXTENDED, 0, 0, 1], 0);
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    for (address, extended, stop) in [
        (CALLBACK, false, false),
        (STOP, false, true),
        (EXTENDED, true, false),
    ] {
        let mut cb = Asm::new(address);
        cb.mov_eax_mem(COUNT).add_eax_imm(1).mov_mem_eax(COUNT);
        if extended {
            cb.mov_eax_esp(8)
                .mov_mem_eax(COUNT + 4)
                .mov_eax_esp(12)
                .mov_mem_eax(COUNT + 8);
        }
        cb.push_imm(1)
            .call(sleep)
            .mov_mem_imm(COUNT + 12, u32::from(!stop))
            .mov_eax_mem(COUNT + 12)
            .ret_n(if extended { 12 } else { 4 });
        m.cpu.write(address, &cb.bytes).unwrap();
    }
    m.spawn("locale callbacks", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, value) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
            value,
            "row {i}"
        );
    }
    let expected_types = [
        adv_os::nls::ctype1(0x3042),
        adv_os::nls::ctype1(65),
        adv_os::nls::ctype1(0),
    ]
    .into_iter()
    .flat_map(u16::to_le_bytes)
    .collect::<Vec<_>>();
    for at in [OUTPUT + 96, OUTPUT + 112] {
        let mut bytes = [0; 8];
        m.cpu.read(at, &mut bytes).unwrap();
        assert_eq!(&bytes[..6], expected_types);
        assert_eq!(&bytes[6..], &[0xcd; 2]);
    }
    assert_eq!(m.cpu.read_u32(OUTPUT + 128).unwrap(), 0xcdcdcdcd);
    assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 12);
    assert_eq!(m.cpu.read_u32(COUNT + 4).unwrap(), 0x21);
    assert_eq!(m.cpu.read_u32(COUNT + 8).unwrap(), 0x12345678);
    assert_eq!(m.cpu.read_u32(OUTPUT + 64).unwrap(), 932);
    assert_eq!(m.cpu.read_u32(OUTPUT + 68).unwrap(), 932);
    let mut bytes = [0; 12];
    m.cpu.read(OUTPUT, &mut bytes).unwrap();
    assert_eq!(
        bytes,
        "ja-JP\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
            .as_slice()
    );
    let mut ansi = [0; 3];
    m.cpu.read(OUTPUT + 72, &mut ansi).unwrap();
    assert_eq!(&ansi, b"en\0");
}
#[test]
// spec: SYS-NLS-1, SYS-NLS-2
fn locale_names_information_and_yielding_enumeration_share_profile_on_three_backends() {
    for cp in [932, 936] {
        for jit in [false, true] {
            let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
            cpu.set_async_compilation(false);
            run(cpu, cp);
        }
        run(UnicornCpu::new(PARK_ADDR).unwrap(), cp);
    }
}
