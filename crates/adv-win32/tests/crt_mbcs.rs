//! Authored guest calls share the CRT's independent multibyte code-page state.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TEXT: u32 = DATA + 1024;
const DEST: u32 = DATA + 2048;
const CALLBACK: u32 = CODE + 2048;

fn machine(jit: bool) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    let mut cfg = Win32Config::new(
        "/nonexistent/crt-mbcs",
        "fixture.exe",
        "/nonexistent/crt-mbcs-data",
    );
    cfg.os.codepage = 932;
    Win32::install(&mut m, cfg).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(TEXT, &[0xa1, 0x81, 0x81, 0x41, 0]).unwrap();
    m.cpu.write(TEXT + 16, &[65, 0x81, 0]).unwrap();
    m.cpu.write(DEST, &[0xcc; 64]).unwrap();
    m
}
fn invoke(
    m: &mut Machine<JitCpu>,
    a: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    result: u32,
) {
    let module = if module.starts_with("api-ms-win-crt-")
        && !include_str!("../../../data/abi/crt-exports.tsv")
            .lines()
            .any(|l| l.starts_with(&format!("{module}\t{name}\t")))
    {
        "ucrtbase.dll"
    } else {
        module
    };
    for &arg in args.iter().rev() {
        a.push_imm(arg);
    }
    a.call(m.resolve_import(module, name).unwrap())
        .add_esp(args.len() as u8 * 4)
        .mov_mem_eax(result);
}
fn run(m: &mut Machine<JitCpu>, a: Asm) -> RunResult {
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("multibyte CRT fixture", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn clean(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}

// spec: PERS-CRT-3, PERS-CRT-4
#[test]
fn cp932_and_cp936_traversal_counts_context_and_copy_share_all_version_fronts() {
    for jit in [false, true] {
        for module in [
            "msvcrt20.dll",
            "msvcrt40.dll",
            "msvcrt.dll",
            "msvcr70.dll",
            "msvcr71.dll",
            "msvcr80.dll",
            "msvcr90.dll",
            "msvcr100.dll",
            "msvcr110.dll",
            "msvcr120.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-multibyte-l1-1-0.dll",
        ] {
            for cp in [932, 936] {
                let mut m = machine(jit);
                let mut a = Asm::new(CODE);
                let calls = [
                    ("_setmbcp", vec![cp]),
                    ("_getmbcp", vec![]),
                    ("_mbslen", vec![TEXT]),
                    ("_mbsinc", vec![TEXT]),
                    ("_mbsnextc", vec![TEXT]),
                    ("_mbsdec", vec![TEXT, TEXT + 4]),
                    ("_mbsnbcnt", vec![TEXT, 2]),
                    ("_mbsnccnt", vec![TEXT, 3]),
                    ("_mbsninc", vec![TEXT, 2]),
                    ("_ismbslead", vec![TEXT, TEXT + 1]),
                    ("_ismbstrail", vec![TEXT, TEXT + 1]),
                    ("_ismbblead", vec![0xa1]),
                    ("_ismbbtrail", vec![0xfd]),
                    ("_ismbbtrail", vec![0x7f]),
                    ("_ismbblead", vec![0x181]),
                    ("_mbsnbcpy", vec![DEST, TEXT, 3]),
                    ("_mbsnbcpy", vec![DEST + 16, TEXT + 16, 5]),
                ];
                for (i, (name, args)) in calls.iter().enumerate() {
                    invoke(&mut m, &mut a, module, name, args, DATA + i as u32 * 4);
                }
                a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
                let result = run(&mut m, a);
                assert!(
                    matches!(result, RunResult::Exited(0)),
                    "{module} CP{cp} {jit}: {result:?}"
                );
                let expected = if cp == 932 {
                    [
                        0,
                        cp,
                        3,
                        TEXT + 1,
                        0xa1,
                        TEXT + 3,
                        3,
                        2,
                        TEXT + 3,
                        u32::MAX,
                        0,
                        0,
                        0,
                        0,
                        0,
                        DEST,
                        DEST + 16,
                    ]
                } else {
                    [
                        0,
                        cp,
                        2,
                        TEXT + 2,
                        0xa181,
                        TEXT + 2,
                        4,
                        1,
                        TEXT + 4,
                        0,
                        u32::MAX,
                        1,
                        1,
                        0,
                        0,
                        DEST,
                        DEST + 16,
                    ]
                };
                for (i, e) in expected.iter().enumerate() {
                    assert_eq!(
                        m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
                        *e,
                        "{module} CP{cp} {}",
                        calls[i].0
                    );
                }
                let mut bytes = [0; 4];
                m.cpu.read(DEST, &mut bytes).unwrap();
                assert_eq!(
                    bytes,
                    if cp == 932 {
                        [0xa1, 0x81, 0x81, 0xcc]
                    } else {
                        [0xa1, 0x81, 0, 0xcc]
                    }
                );
                let mut bytes = [1; 5];
                m.cpu.read(DEST + 16, &mut bytes).unwrap();
                assert_eq!(bytes, [65, 0, 0, 0, 0]);
                clean(&m);
            }
        }
    }
}

// spec: PERS-CRT-3
#[test]
fn codepage_controls_preserve_crt_ownership_api_aliases_ctype_and_win32_acp() {
    for jit in [false, true] {
        let mut m = machine(jit);
        m.cpu.write(TEXT + 64, b"Japanese.932\0").unwrap();
        let mut a = Asm::new(CODE);
        let calls = [
            ("msvcr90.dll", "_setmbcp", vec![936], 0),
            ("ucrtbase.dll", "_getmbcp", vec![], 932),
            ("api-ms-win-crt-locale-l1-1-0.dll", "_setmbcp", vec![0], 0),
            ("ucrtbase.dll", "_getmbcp", vec![], 0),
            (
                "api-ms-win-crt-multibyte-l1-1-0.dll",
                "_ismbblead",
                vec![0x81],
                0,
            ),
            ("msvcr90.dll", "_getmbcp", vec![], 936),
            ("msvcr90.dll", "_setmbcp", vec![1252], 0),
            ("msvcr90.dll", "_getmbcp", vec![], 0),
            ("msvcr90.dll", "_mbslen", vec![TEXT], 4),
            ("msvcr90.dll", "setlocale", vec![2, TEXT + 64], 0),
            ("msvcr90.dll", "_getmbcp", vec![], 0),
            ("msvcr90.dll", "_setmbcp", vec![(-4i32) as u32], 0),
            ("msvcr90.dll", "_getmbcp", vec![], 932),
            ("msvcr90.dll", "_setmbcp", vec![65001], u32::MAX),
            ("msvcr90.dll", "_getmbcp", vec![], 932),
            ("msvcr90.dll", "_setmbcp", vec![123456], u32::MAX),
            ("msvcr90.dll", "_getmbcp", vec![], 932),
            (
                "api-ms-win-crt-locale-l1-1-0.dll",
                "_setmbcp",
                vec![65001],
                0,
            ),
            ("ucrtbase.dll", "_getmbcp", vec![], 0),
            (
                "api-ms-win-crt-multibyte-l1-1-0.dll",
                "_ismbblead",
                vec![0x81],
                0,
            ),
            ("ucrtbase.dll", "_setmbcp", vec![(-3i32) as u32], 0),
            ("ucrtbase.dll", "_getmbcp", vec![], 932),
        ];
        for (i, (module, name, args, _)) in calls.iter().enumerate() {
            invoke(&mut m, &mut a, module, name, args, DATA + i as u32 * 4);
        }
        a.call(m.resolve_import("kernel32.dll", "GetACP").unwrap())
            .mov_mem_eax(DATA + 128);
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for (i, (_, name, _, expected)) in calls.iter().enumerate() {
            let value = m.cpu.read_u32(DATA + i as u32 * 4).unwrap();
            if *name == "setlocale" {
                assert_ne!(value, 0);
            } else {
                assert_eq!(value, *expected, "{i} {name}");
            }
        }
        assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 932);
        clean(&m);
    }
}

// spec: PERS-CRT-4
#[test]
fn limits_and_dangling_leads_do_not_read_unmapped_tails_or_invent_characters() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let end = DATA + 4095;
        m.cpu.write(end, &[0x81]).unwrap();
        let mut a = Asm::new(CODE);
        for (i, (name, args)) in [
            ("_mbsnccnt", vec![end, 1]),
            ("_mbsnbcpy", vec![DEST, end, 1]),
            ("_ismbslead", vec![end, end]),
            ("_mbsnbcnt", vec![0, 0]),
            ("_mbsnccnt", vec![0xdead0000, 0]),
            ("_mbsninc", vec![0, 5]),
            ("_mbsnbcpy", vec![0, 0, 0]),
            ("_mbslen", vec![TEXT + 17]),
            ("_mbsinc", vec![TEXT + 17]),
            ("_mbsnextc", vec![TEXT + 17]),
            ("_mbsnbcnt", vec![TEXT + 17, 1]),
            ("_ismbstrail", vec![TEXT + 17, TEXT + 18]),
        ]
        .iter()
        .enumerate()
        {
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                name,
                args,
                DATA + i as u32 * 4,
            );
        }
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for (i, expected) in [
            0,
            DEST,
            u32::MAX,
            0,
            0,
            0,
            0,
            0,
            TEXT + 18,
            0x81,
            0,
            u32::MAX,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
                expected,
                "{i}"
            );
        }
        assert_eq!(m.cpu.read_u8(DEST).unwrap(), 0);
        assert_eq!(m.cpu.read_u8(DEST + 1).unwrap(), 0xcc);
        clean(&m);
    }
}

// spec: PERS-CRT-4
#[test]
fn null_arguments_enter_modern_invalid_handlers_and_readonly_copy_destinations_fail() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let mut callback = Asm::new(CALLBACK);
        callback.bytes.extend_from_slice(&[0xff, 0x05]);
        callback
            .bytes
            .extend_from_slice(&(DATA + 128).to_le_bytes());
        callback.ret();
        m.cpu.write(CALLBACK, &callback.bytes).unwrap();
        let mut a = Asm::new(CODE);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "_set_invalid_parameter_handler",
            &[CALLBACK],
            DATA,
        );
        invoke(&mut m, &mut a, "msvcr90.dll", "_mbsinc", &[0], DATA + 4);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "_ismbslead",
            &[TEXT, 0],
            DATA + 8,
        );
        invoke(&mut m, &mut a, "msvcr90.dll", "_errno", &[], DATA + 12);
        a.mov_eax_ptr_eax().mov_mem_eax(DATA + 16);
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 2);
        assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 22);
        let mut m = machine(jit);
        m.cpu.map(0x420000, 4096, Prot::READ).unwrap();
        m.cpu.write(0x420000, &[0xcc; 4]).unwrap();
        let mut a = Asm::new(CODE);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "_mbsnbcpy",
            &[0x420000, TEXT, 4],
            DATA,
        );
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(m.cpu.read_u8(0x420000).unwrap(), 0xcc);
        let mut m = machine(jit);
        m.cpu.map(0x420000, 4096, Prot::READ).unwrap();
        m.cpu.write(0x420000, &[0xcc; 4]).unwrap();
        let mut a = Asm::new(CODE);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "_splitpath",
            &[TEXT, 0x420000, 0, 0, 0],
            DATA,
        );
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(m.cpu.read_u8(0x420000).unwrap(), 0xcc);
    }
}

// spec: PERS-CRT-3, PERS-CRT-4
#[test]
fn ordinary_and_secure_path_splits_use_the_explicit_multibyte_codepage() {
    for jit in [false, true] {
        for cp in [932, 936] {
            let mut m = machine(jit);
            m.cpu
                .write(
                    TEXT + 128,
                    &[
                        67, 58, 92, 0xa1, 92, 102, 105, 108, 101, 46, 116, 120, 116, 0,
                    ],
                )
                .unwrap();
            let mut a = Asm::new(CODE);
            invoke(&mut m, &mut a, "msvcr90.dll", "_setmbcp", &[cp], DATA);
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "_splitpath",
                &[TEXT + 128, DEST, DEST + 64, DEST + 128, DEST + 192],
                DATA + 4,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "_splitpath_s",
                &[
                    TEXT + 128,
                    DEST + 256,
                    64,
                    DEST + 320,
                    64,
                    DEST + 384,
                    64,
                    DEST + 448,
                    64,
                ],
                DATA + 8,
            );
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0);
            let expected: &[u8] = if cp == 932 {
                b"file\0"
            } else {
                &[0xa1, 92, 102, 105, 108, 101, 0]
            };
            for offset in [128, 384] {
                let mut bytes = vec![0; expected.len()];
                m.cpu.read(DEST + offset, &mut bytes).unwrap();
                assert_eq!(bytes, expected);
            }
            clean(&m);
        }
    }
}
