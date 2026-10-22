//! File-backed native oracle: target/engine-loop/20261007-resume/profile-native/final.jsonl.
//! Authored kernel32-only probe, SHA256
//! 0F1AB13ED31D40F8CCC9BD0C3C5A1C9131B22B35FA4C1E92D8C2FA4ADAFDF948.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::path::PathBuf;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = 0x420000;

pub(super) fn fixture<B: Backend>(cpu: B, name: &str) -> (Machine<B>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/profile-string-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for (name, argc) in [
        ("GetPrivateProfileStringA", 6),
        ("GetPrivateProfileStringW", 6),
        ("GetPrivateProfileIntA", 4),
        ("GetPrivateProfileIntW", 4),
        ("GetProfileIntA", 3),
        ("GetProfileIntW", 3),
        ("GetProfileStringA", 5),
        ("GetProfileStringW", 5),
    ] {
        assert!(registry.dynamic_available(K32, name));
        assert_eq!(
            registry.get(K32, name).unwrap().conv,
            Conv::Stdcall { argc }
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    machine.cpu.map(OUT, 0x20000, Prot::RW).unwrap();
    (machine, root)
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
pub(super) fn wide<B: Backend>(machine: &mut Machine<B>, pointer: u32, text: &str) {
    let bytes: Vec<_> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}
pub(super) fn ansi<B: Backend>(machine: &mut Machine<B>, pointer: u32, bytes: &[u8]) {
    machine.cpu.write(pointer, &[bytes, &[0]].concat()).unwrap();
}
pub(super) fn bytes<B: Backend>(machine: &Machine<B>, pointer: u32, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    machine.cpu.read(pointer, &mut bytes).unwrap();
    bytes
}
pub(super) fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn invoke<B: Backend>(machine: &mut Machine<B>, values: [u32; 6], wide: bool) -> u32 {
    let before = machine.sys.objects.object_count();
    for (index, value) in values.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    let outcome = if wide {
        get_w_ABI.handler
    } else {
        get_a_ABI.handler
    }(&mut env(machine));
    assert_eq!(
        machine.sys.objects.object_count(),
        before,
        "temporary FileObj must close"
    );
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("profile queries return synchronously"),
    }
}
pub(super) fn error<B: Backend>(machine: &mut Machine<B>) -> u32 {
    crate::util::last_error(&env(machine))
}
fn call(code: &mut Asm, api: u32, values: &[u32], record: u32, get_error: u32) {
    for &value in values.iter().rev() {
        code.push_imm(value);
    }
    code.call(api).mov_mem_eax(record);
    code.call(get_error).mov_mem_eax(record + 4);
}

pub(super) fn profile_call<B: Backend>(machine: &mut Machine<B>, name: &str, args: &[u32]) -> u32 {
    let before = machine.sys.objects.object_count();
    for (index, value) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    let address = machine.resolve_import(K32, name).unwrap();
    let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let handler = machine.sys.thunks.get(index).f.handler;
    let Outcome::Return(result) = handler(&mut env(machine)) else {
        panic!("synchronous profile query")
    };
    assert_eq!(
        machine.sys.objects.object_count(),
        before,
        "profile query must release temporary FileObj"
    );
    result
}

#[test]
fn integer_conversion_and_win_ini_wrappers_share_decoding_and_file_lifetime() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "integer-contract");
    machine
        .spawn("integer query", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    let contents = [&[0xff, 0xfe], utf16("[S]\r\nempty=\r\nbad=abc\r\nminus=-2\r\nhex=0x2a\r\nzero=010\r\npartial=12tail\r\nplus=+3\r\nquoted=\" -2 \"\r\noverflow=4294967296\r\n").as_slice()].concat();
    std::fs::write(root.join("game/numbers.ini"), &contents).unwrap();
    for unicode in [false, true] {
        let api = if unicode {
            "GetPrivateProfileIntW"
        } else {
            "GetPrivateProfileIntA"
        };
        for (pointer, text) in [(DATA + 0x400, "S"), (DATA + 0x800, ".\\numbers.ini")] {
            if unicode {
                wide(&mut machine, pointer, text);
            } else {
                ansi(&mut machine, pointer, text.as_bytes());
            }
        }
        for (key, expected, last_error) in [
            ("missing", 2, 2),
            ("empty", 2, 0),
            ("bad", 0, 0),
            ("minus", (-2_i32) as u32, 0),
            ("hex", 42, 0),
            ("zero", 10, 0),
            ("partial", 12, 0),
            ("plus", 3, 0),
            ("quoted", (-2_i32) as u32, 0),
            ("overflow", 0, 0),
        ] {
            if unicode {
                wide(&mut machine, DATA + 0x500, key);
            } else {
                ansi(&mut machine, DATA + 0x500, key.as_bytes());
            }
            set_last_error(&mut env(&mut machine), 0x12345678);
            assert_eq!(
                profile_call(
                    &mut machine,
                    api,
                    &[DATA + 0x400, DATA + 0x500, 2, DATA + 0x800]
                ),
                expected
            );
            assert_eq!(error(&mut machine), last_error);
        }
    }
    wide(&mut machine, DATA + 0x400, "settings");
    wide(&mut machine, DATA + 0x500, "value");
    assert_eq!(
        profile_call(
            &mut machine,
            "GetProfileIntW",
            &[DATA + 0x400, DATA + 0x500, (-7_i32) as u32]
        ),
        (-7_i32) as u32
    );
    assert_eq!(error(&mut machine), 2);
    let win_ini = machine
        .ext
        .get::<Win32>()
        .vfs
        .write_path("C:\\Windows\\win.ini")
        .unwrap();
    std::fs::write(&win_ini, b"[settings]\r\nvalue=42\r\n").unwrap();
    for unicode in [false, true] {
        for (pointer, text) in [
            (DATA + 0x400, "settings"),
            (DATA + 0x500, "value"),
            (DATA + 0x600, "fallback"),
        ] {
            if unicode {
                wide(&mut machine, pointer, text);
            } else {
                ansi(&mut machine, pointer, text.as_bytes());
            }
        }
        let int_api = if unicode {
            "GetProfileIntW"
        } else {
            "GetProfileIntA"
        };
        assert_eq!(
            profile_call(&mut machine, int_api, &[DATA + 0x400, DATA + 0x500, 2]),
            42
        );
        assert_eq!(error(&mut machine), 0);
        let string_api = if unicode {
            "GetProfileStringW"
        } else {
            "GetProfileStringA"
        };
        assert_eq!(
            profile_call(
                &mut machine,
                string_api,
                &[DATA + 0x400, DATA + 0x500, DATA + 0x600, OUT, 8]
            ),
            2
        );
        assert_eq!(
            bytes(&machine, OUT, if unicode { 6 } else { 3 }),
            if unicode {
                utf16("42\0")
            } else {
                b"42\0".to_vec()
            }
        );
        assert_eq!(error(&mut machine), 0);
    }
    assert_eq!(
        std::fs::read(root.join("game/numbers.ini")).unwrap(),
        contents
    );
    assert_eq!(
        std::fs::read(win_ini).unwrap(),
        b"[settings]\r\nvalue=42\r\n"
    );
}

fn actual_guest<B: Backend>(cpu: B, name: &str) {
    let (mut machine, root) = fixture(cpu, name);
    let ansi_contents = [b"[S]\r\nk=".as_slice(), &[0x93, 0x8c, 0x8b, 0x9e], b"\r\n"].concat();
    let wide_contents = [&[0xff, 0xfe], utf16("[S]\r\n日本=東京\r\n").as_slice()].concat();
    std::fs::write(root.join("game/ansi.ini"), &ansi_contents).unwrap();
    std::fs::write(root.join("game/unicode.ini"), &wide_contents).unwrap();
    std::fs::write(root.join("game/overlay.ini"), b"[S]\r\nk=old\r\n").unwrap();
    wide(&mut machine, DATA + 0x400, "S");
    wide(&mut machine, DATA + 0x410, "k");
    wide(&mut machine, DATA + 0x430, ".\\ansi.ini");
    ansi(&mut machine, DATA + 0x480, b"S");
    ansi(&mut machine, DATA + 0x490, &[0x93, 0xfa, 0x96, 0x7b]); // 日本, CP932.
    ansi(&mut machine, DATA + 0x500, b".\\unicode.ini");
    ansi(&mut machine, DATA + 0x600, b"S");
    ansi(&mut machine, DATA + 0x610, b"k");
    ansi(&mut machine, DATA + 0x620, b"overlay");
    ansi(&mut machine, DATA + 0x640, b".\\overlay.ini");
    wide(&mut machine, DATA + 0x700, ".\\overlay.ini");
    machine.cpu.write(OUT, &[0xcc; 16]).unwrap();
    let read_a = machine
        .resolve_import(K32, "GetPrivateProfileStringA")
        .unwrap();
    let read_w = machine
        .resolve_import(K32, "GetPrivateProfileStringW")
        .unwrap();
    let write_a = machine
        .resolve_import(K32, "WritePrivateProfileStringA")
        .unwrap();
    let get_error = machine.resolve_import(K32, "GetLastError").unwrap();
    let exit = machine.resolve_import(K32, "ExitProcess").unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 0x50);
    // A real request of 65532 UTF16 units: output range is 131064 bytes.
    call(
        &mut code,
        read_w,
        &[DATA + 0x400, DATA + 0x410, 0, OUT, 65532, DATA + 0x430],
        DATA + 0x80,
        get_error,
    );
    call(
        &mut code,
        read_a,
        &[
            DATA + 0x480,
            DATA + 0x490,
            0,
            DATA + 0x2000,
            16,
            DATA + 0x500,
        ],
        DATA + 0x88,
        get_error,
    );
    call(
        &mut code,
        write_a,
        &[DATA + 0x600, DATA + 0x610, DATA + 0x620, DATA + 0x640],
        DATA + 0x90,
        get_error,
    );
    call(
        &mut code,
        read_w,
        &[
            DATA + 0x400,
            DATA + 0x410,
            0,
            DATA + 0x2200,
            16,
            DATA + 0x700,
        ],
        DATA + 0x98,
        get_error,
    );
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x54);
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("profile stdcall", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x50).unwrap(),
        machine.cpu.read_u32(DATA + 0x54).unwrap()
    );
    assert_eq!(
        bytes(&machine, DATA + 0x80, 16),
        [2u32, 0, 4, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x90).unwrap(), 1);
    assert_eq!(
        bytes(&machine, DATA + 0x98, 8),
        [7u32, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        bytes(&machine, OUT, 8),
        [utf16("東京\0"), vec![0xcc; 2]].concat()
    );
    assert_eq!(
        bytes(&machine, DATA + 0x2000, 5),
        [0x93, 0x8c, 0x8b, 0x9e, 0]
    );
    assert_eq!(bytes(&machine, DATA + 0x2200, 16), utf16("overlay\0"));
    assert_eq!(
        std::fs::read(root.join("game/ansi.ini")).unwrap(),
        ansi_contents
    );
    assert_eq!(
        std::fs::read(root.join("game/unicode.ini")).unwrap(),
        wide_contents
    );
    assert_eq!(
        std::fs::read(root.join("game/overlay.ini")).unwrap(),
        b"[S]\r\nk=old\r\n"
    );
    assert_eq!(
        std::fs::read(root.join("data/overlay/overlay.ini")).unwrap(),
        b"[S]\r\nk=overlay\r\n"
    );
}
#[test]
fn actual_x86_aw_codepages_large_wide_capacity_and_overlay_write_read_share_the_file_owner() {
    actual_guest(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    actual_guest(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
}

#[test]
fn section_names_aw_preserve_native_lists_truncation_and_empty_files() {
    for unicode in [false, true] {
        let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "section-names");
        std::fs::write(
            root.join("game/sections.ini"),
            b"[one]\r\na=b\r\n[two]\r\nx=y\r\n[ONE]\r\nz=v\r\n",
        )
        .unwrap();
        std::fs::write(root.join("game/empty.ini"), []).unwrap();
        let stride = if unicode { 2 } else { 1 };
        if unicode {
            wide(&mut machine, DATA + 0x800, ".\\sections.ini");
        } else {
            ansi(&mut machine, DATA + 0x800, b".\\sections.ini");
        }
        let name = if unicode {
            "GetPrivateProfileSectionNamesW"
        } else {
            "GetPrivateProfileSectionNamesA"
        };
        let api = machine.resolve_import(K32, name).unwrap();
        let get_error = machine.resolve_import(K32, "GetLastError").unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut code,
            api,
            &[OUT, 32, DATA + 0x800],
            DATA + 0x80,
            get_error,
        );
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine
            .spawn("section names stdcall", CODE, &[], 65536)
            .unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        assert_eq!(machine.cpu.read_u32(DATA + 0x80).unwrap(), 12);
        assert_eq!(machine.cpu.read_u32(DATA + 0x84).unwrap(), 0);
        let expected = if unicode {
            utf16("one\0two\0ONE\0\0")
        } else {
            b"one\0two\0ONE\0\0".to_vec()
        };
        assert_eq!(bytes(&machine, OUT, expected.len()), expected);
        for (capacity, expected, count) in [
            (0, b"".as_slice(), 0),
            (1, b"\0", 0),
            (2, b"\0\0", 0),
            (3, b"o\0\0", 1),
            (5, b"one\0\0", 3),
            (6, b"one\0\0\0", 4),
        ] {
            machine.cpu.write(OUT, &[0xcc; 32]).unwrap();
            let result = get(
                &mut env(&mut machine),
                [0, 0, 0, OUT, capacity, DATA + 0x800],
                unicode,
                true,
                if unicode {
                    "kernel32.dll!GetPrivateProfileSectionNamesW"
                } else {
                    "kernel32.dll!GetPrivateProfileSectionNamesA"
                },
            );
            assert!(matches!(result, Outcome::Return(n) if n == count));
            assert_eq!(error(&mut machine), 234);
            let expected: Vec<_> = if unicode {
                expected
                    .iter()
                    .flat_map(|b| u16::from(*b).to_le_bytes())
                    .collect()
            } else {
                expected.to_vec()
            };
            assert_eq!(
                bytes(&machine, OUT, expected.len() + stride),
                [expected, vec![0xcc; stride]].concat()
            );
        }
        if unicode {
            wide(&mut machine, DATA + 0x800, ".\\empty.ini");
        } else {
            ansi(&mut machine, DATA + 0x800, b".\\empty.ini");
        }
        for capacity in [0, 1, 2, 32] {
            machine.cpu.write(OUT, &[0xcc; 8]).unwrap();
            let result = get(
                &mut env(&mut machine),
                [0, 0, 0, OUT, capacity, DATA + 0x800],
                unicode,
                true,
                if unicode {
                    "kernel32.dll!GetPrivateProfileSectionNamesW"
                } else {
                    "kernel32.dll!GetPrivateProfileSectionNamesA"
                },
            );
            assert!(matches!(result, Outcome::Return(0)));
            assert_eq!(error(&mut machine), if capacity < 2 { 234 } else { 0 });
            assert_eq!(
                bytes(&machine, OUT, stride),
                vec![if capacity == 0 { 0xcc } else { 0 }; stride]
            );
            assert_eq!(
                bytes(&machine, OUT + stride as u32, stride),
                vec![0xcc; stride]
            );
        }
    }
}

#[test]
fn native_defaults_quotes_duplicates_and_small_enumeration_capacities_match() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "native-contract");
    machine.spawn("query", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    std::fs::write(root.join("game/query.ini"), b"[S]\r\na=  \" x \"\r\nb=' y '\r\ndupe=first\r\nDUPE=second\r\nnovalue\r\nempty=\r\nspaces=  xx  \t\r\n[S]\r\nnew=late\r\n[empty]\r\n[two]\r\nk=last\r\n").unwrap();
    for unicode in [false, true] {
        let stride = if unicode { 2 } else { 1 };
        for (ptr, text) in [
            (DATA + 0x400, "S"),
            (DATA + 0x600, "  def \t "),
            (DATA + 0x800, ".\\query.ini"),
        ] {
            if unicode {
                wide(&mut machine, ptr, text);
            } else {
                ansi(&mut machine, ptr, text.as_bytes());
            }
        }
        for (key, expected, last_error) in [
            ("a", " x ", 0),
            ("b", " y ", 0),
            ("dupe", "first", 0),
            ("new", "  def \t", 2),
            ("novalue", "  def \t", 2),
            ("spaces", "xx", 0),
            ("missing", "  def \t", 2),
        ] {
            if unicode {
                wide(&mut machine, DATA + 0x500, key);
            } else {
                ansi(&mut machine, DATA + 0x500, key.as_bytes());
            }
            machine.cpu.write(OUT, &[0xcc; 64]).unwrap();
            let count = invoke(
                &mut machine,
                [
                    DATA + 0x400,
                    DATA + 0x500,
                    DATA + 0x600,
                    OUT,
                    32,
                    DATA + 0x800,
                ],
                unicode,
            );
            assert_eq!(count as usize, expected.len(), "{key}, wide={unicode}");
            assert_eq!(error(&mut machine), last_error);
            let expected = if unicode {
                utf16(&format!("{expected}\0"))
            } else {
                [expected.as_bytes(), &[0]].concat()
            };
            assert_eq!(
                bytes(&machine, OUT, expected.len() + stride),
                [expected, vec![0xcc; stride]].concat()
            );
        }
        for (capacity, expected, count) in [
            (0, vec![], 0),
            (1, vec![0], 0),
            (2, vec![0, 0], 0),
            (3, vec![b'S', 0, 0], 1),
            (4, vec![b'S', 0, 0, 0], 2),
        ] {
            machine.cpu.write(OUT, &[0xcc; 32]).unwrap();
            // app=NULL ignores key; enumeration must not dereference this bad key.
            assert_eq!(
                invoke(
                    &mut machine,
                    [0, 0xdead0000, 0, OUT, capacity, DATA + 0x800],
                    unicode
                ),
                count
            );
            assert_eq!(error(&mut machine), 234);
            let expected = if unicode {
                expected
                    .into_iter()
                    .flat_map(|b| u16::from(b).to_le_bytes())
                    .collect::<Vec<_>>()
            } else {
                expected
            };
            assert_eq!(
                bytes(&machine, OUT, expected.len() + stride),
                [expected, vec![0xcc; stride]].concat()
            );
        }
        let key_list = "a\0b\0dupe\0DUPE\0empty\0spaces\0\0";
        assert_eq!(
            invoke(
                &mut machine,
                [DATA + 0x400, 0, 0xdead0000, OUT, 64, DATA + 0x800],
                unicode
            ),
            27
        );
        assert_eq!(error(&mut machine), 0);
        let expected = if unicode {
            utf16(key_list)
        } else {
            key_list.as_bytes().to_vec()
        };
        assert_eq!(bytes(&machine, OUT, expected.len()), expected);
        if unicode {
            wide(&mut machine, DATA + 0x400, "empty");
        } else {
            ansi(&mut machine, DATA + 0x400, b"empty");
        }
        machine.cpu.write(OUT, &[0xcc; 8]).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                [DATA + 0x400, 0, 0xdead0000, OUT, 4, DATA + 0x800],
                unicode
            ),
            0
        );
        assert_eq!(error(&mut machine), 0);
        assert_eq!(
            bytes(&machine, OUT, stride * 2),
            [vec![0; stride], vec![0xcc; stride]].concat()
        );
        // nSize=0 permits a NULL output; a missing value retains FILE_NOT_FOUND.
        assert_eq!(
            invoke(
                &mut machine,
                [DATA + 0x400, DATA + 0x500, 0, 0, 0, DATA + 0x800],
                unicode
            ),
            0
        );
        assert_eq!(error(&mut machine), 2);
    }
}

#[test]
fn checked_outputs_encoding_failures_and_unicode_writes_preserve_source_files() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "failures");
    machine.spawn("query", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    let wide_contents = [&[0xff, 0xfe], utf16("[S]\r\nk=value\r\n").as_slice()].concat();
    std::fs::write(root.join("game/unicode.ini"), &wide_contents).unwrap();
    wide(&mut machine, DATA + 0x400, "S");
    wide(&mut machine, DATA + 0x500, "k");
    wide(&mut machine, DATA + 0x800, ".\\unicode.ini");
    machine.cpu.write(OUT, &[0xcc; 8192]).unwrap();
    machine.cpu.protect(OUT + 4096, 4096, Prot::READ).unwrap();
    for (pointer, capacity) in [
        (OUT + 4094, 2),
        (OUT + 4096, 16),
        (0, 16),
        (0xfffffffc, 4),
        (OUT, 0x80000000),
    ] {
        assert_eq!(
            invoke(
                &mut machine,
                [
                    DATA + 0x400,
                    DATA + 0x500,
                    0,
                    pointer,
                    capacity,
                    DATA + 0x800
                ],
                true
            ),
            0
        );
        assert_eq!(error(&mut machine), 998);
        assert_eq!(bytes(&machine, OUT, 8192), [0xcc; 8192]);
    }
    assert_eq!(
        invoke(
            &mut machine,
            [0xdead0000, DATA + 0x500, 0, OUT, 16, DATA + 0x800],
            true
        ),
        0
    );
    assert_eq!(error(&mut machine), 998);
    assert_eq!(bytes(&machine, OUT, 16), [0xcc; 16]);
    // A found value does not read the unused default, and W/UTF16LE needs no ACP.
    machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(437);
    assert_eq!(
        invoke(
            &mut machine,
            [
                DATA + 0x400,
                DATA + 0x500,
                0xdead0000,
                OUT,
                16,
                DATA + 0x800
            ],
            true
        ),
        5
    );
    assert_eq!(error(&mut machine), 0);
    for (ptr, text) in [
        (DATA + 0x1000, b"S".as_slice()),
        (DATA + 0x1100, b"k"),
        (DATA + 0x1200, b".\\unicode.ini"),
        (DATA + 0x1300, b"replacement"),
    ] {
        ansi(&mut machine, ptr, text);
    }
    machine.cpu.write(OUT, &[0xcc; 16]).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            [DATA + 0x1000, DATA + 0x1100, 0, OUT, 16, DATA + 0x1200],
            false
        ),
        0
    );
    assert_eq!(error(&mut machine), 50);
    assert_eq!(bytes(&machine, OUT, 16), [0xcc; 16]);
    machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(932);
    for (contents, last_error) in [
        ([0xfe, 0xff, 0, 91].to_vec(), 50),
        ([0xff, 0xfe, 91].to_vec(), 1113),
    ] {
        std::fs::write(root.join("game/unicode.ini"), contents).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                [DATA + 0x400, DATA + 0x500, 0, OUT, 16, DATA + 0x800],
                true
            ),
            0
        );
        assert_eq!(error(&mut machine), last_error);
        assert_eq!(bytes(&machine, OUT, 16), [0xcc; 16]);
    }
    std::fs::write(root.join("game/unicode.ini"), &wide_contents).unwrap();
    for (index, value) in [DATA + 0x1000, DATA + 0x1100, DATA + 0x1300, DATA + 0x1200]
        .iter()
        .enumerate()
    {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    assert!(matches!(
        super::super::write_private_profile_string_a(&mut env(&mut machine)),
        Outcome::Return(1)
    ));
    assert_eq!(error(&mut machine), 0);
    assert_eq!(
        std::fs::read(root.join("game/unicode.ini")).unwrap(),
        wide_contents
    );
    let written = std::fs::read(root.join("data/overlay/unicode.ini")).unwrap();
    assert_eq!(
        written,
        [&[0xff, 0xfe], utf16("[S]\r\nk=replacement\r\n").as_slice()].concat()
    );
}
