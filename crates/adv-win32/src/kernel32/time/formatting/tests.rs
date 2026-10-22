use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NAME: u32 = DATA + 0x1000;
const ST: u32 = DATA + 0x1100;
const PICTURE: u32 = DATA + 0x1200;
const OUTPUT: u32 = DATA + 0x1400;
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        crate::Win32Config::new(".", "fixture.exe", "target/closure-format-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    for (at, text) in [
        (NAME, "ja-JP"),
        (NAME + 32, "en-US"),
        (PICTURE, "dddd MMMM d"),
        (PICTURE + 64, "HH:mm:ss tt"),
    ] {
        m.cpu
            .write(
                at,
                &text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    m.cpu
        .write(
            ST,
            &[2024u16, 2, 0, 29, 13, 4, 5, 0]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu
        .write(
            ST + 32,
            &[2023u16, 2, 0, 29, 13, 4, 5, 0]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(OUTPUT, &[0xcd; 512]).unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    for (i, (name, args, text)) in [
        (
            "GetDateFormatEx",
            vec![NAME, 0, ST, 0, OUTPUT, 32, 0],
            Some("2024/02/29"),
        ),
        (
            "GetDateFormatEx",
            vec![NAME + 32, 0, ST, PICTURE, OUTPUT + 64, 32, 0],
            Some("Thursday February 29"),
        ),
        (
            "GetDateFormatEx",
            vec![NAME, 0, ST, PICTURE, OUTPUT + 128, 32, 0],
            Some("木曜日 2月 29"),
        ),
        (
            "GetTimeFormatEx",
            vec![NAME, 0, ST, PICTURE + 64, OUTPUT + 192, 32],
            Some("13:04:05 午後"),
        ),
        (
            "GetDateFormatW",
            vec![0x411, 0, ST + 32, 0, OUTPUT + 256, 32],
            None,
        ),
        (
            "GetDateFormatEx",
            vec![NAME, 3, ST, 0, OUTPUT + 256, 32, 0],
            None,
        ),
        (
            "GetDateFormatEx",
            vec![NAME, 0, ST, 0, OUTPUT + 256, 5, 0],
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for &a in args.iter().rev() {
            code.push_imm(a);
        }
        code.call(m.resolve_import("kernel32.dll", name).unwrap())
            .mov_mem_eax(DATA + i as u32 * 4);
        expected.push(text.map_or(0, |t| t.encode_utf16().count() as u32 + 1));
    }
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("locale date formatting", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, value) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
            value,
            "row {i}"
        );
    }
    for (i, text) in [
        "2024/02/29",
        "Thursday February 29",
        "木曜日 2月 29",
        "13:04:05 午後",
    ]
    .into_iter()
    .enumerate()
    {
        let expected = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut bytes = vec![0; expected.len()];
        m.cpu.read(OUTPUT + i as u32 * 64, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    assert_eq!(m.cpu.read_u32(OUTPUT + 256).unwrap(), 0xcdcdcdcd);
}
#[test]
// spec: SYS-NLS-3
fn named_and_legacy_formatting_share_localized_gregorian_data_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
