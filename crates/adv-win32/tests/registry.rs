//! Authored programs call the real registered guest ABI. No native registry
//! access, host keys, installed configuration or game-specific values are used.
use adv_machine::{Clock, Machine, Registry, RunResult};
use adv_win32::{Codec, Win32, Win32Config};
use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x430000;
const HKCU: u32 = 0x80000001;

#[derive(Clone, Copy)]
enum Arg {
    Imm(u32),
    Load(u32),
}
use Arg::{Imm as I, Load as L};

struct Fixture {
    machine: Machine<UnicornCpu>,
    asm: Asm,
    next: u32,
}
impl Fixture {
    fn new(cp: u32) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self::with_data(
            cp,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../target/registry-fixture/{}-{unique}",
                std::process::id()
            )),
        )
    }
    fn with_data(cp: u32, data: std::path::PathBuf) -> Self {
        let mut reg = Registry::default();
        adv_win32::register(&mut reg);
        let mut machine = Machine::new(
            UnicornCpu::new(adv_machine::layout::PARK_ADDR).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut cfg = Win32Config::new("/nonexistent/registry-fixture", "registry.exe", data);
        cfg.codepage = cp;
        Win32::install(&mut machine, cfg).unwrap();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
        Self {
            machine,
            asm: Asm::new(CODE),
            next: DATA,
        }
    }
    fn blob(&mut self, bytes: &[u8]) -> u32 {
        let at = self.next;
        self.next += (bytes.len().max(4) as u32 + 3) & !3;
        assert!(self.next < DATA + 0x10000);
        self.machine.cpu.write(at, bytes).unwrap();
        at
    }
    fn word(&mut self, value: u32) -> u32 {
        self.blob(&value.to_le_bytes())
    }
    fn a(&mut self, text: &str, cp: u32) -> u32 {
        let mut bytes = Codec::new(cp).encode(text);
        bytes.push(0);
        self.blob(&bytes)
    }
    fn w(&mut self, text: &str) -> u32 {
        let bytes: Vec<_> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        self.blob(&bytes)
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let target = self.machine.resolve_import("advapi32.dll", name).unwrap();
        for &arg in args.iter().rev() {
            match arg {
                I(v) => {
                    self.asm.push_imm(v);
                }
                L(p) => {
                    self.asm.mov_eax_mem(p).push_eax();
                }
            }
        }
        let result = self.word(0xcccccccc);
        self.asm.call(target).mov_mem_eax(result);
        result
    }
    fn create(&mut self, path: u32, wide: bool, access: u32) -> (u32, u32, u32) {
        let key = self.word(0);
        let disposition = self.word(0);
        let rc = self.call(
            if wide {
                "RegCreateKeyExW"
            } else {
                "RegCreateKeyExA"
            },
            &[
                I(HKCU),
                I(path),
                I(0),
                I(0),
                I(0),
                I(access),
                I(0),
                I(key),
                I(disposition),
            ],
        );
        (rc, key, disposition)
    }
    fn run(&mut self) {
        let exit = self
            .machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        self.asm.push_imm(0).call(exit);
        self.machine.cpu.write(CODE, &self.asm.bytes).unwrap();
        self.machine
            .spawn("registry fixture", CODE, &[], 0x10000)
            .unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
    }
    fn get(&self, at: u32) -> u32 {
        self.machine.cpu.read_u32(at).unwrap()
    }
    fn bytes(&self, at: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.machine.cpu.read(at, &mut bytes).unwrap();
        bytes
    }
}

#[test]
fn nested_case_insensitive_keys_distinct_handles_close_and_machine_isolation() {
    let mut f = Fixture::new(932);
    let path = f.a("Software\\Registry Fixture\\Nested", 932);
    let out = f.word(0xabcdef01);
    let missing = f.call(
        "RegOpenKeyExA",
        &[I(HKCU), I(path), I(0), I(0xf0019), I(out)],
    );
    let (created, key, disposition) = f.create(path, false, 0xf003f);
    let alternate = f.w("sOFTWARE\\rEGISTRY fIXTURE\\nESTED");
    let (recreated, duplicate, existing) = f.create(alternate, true, 1);
    let parent = f.a("Software\\Registry Fixture", 932);
    let parent_handle = f.word(0);
    let open_parent = f.call("RegOpenKeyA", &[I(HKCU), I(parent), I(parent_handle)]);
    let nested = f.a("nested", 932);
    let relative = f.word(0);
    let open_relative = f.call(
        "RegOpenKeyExA",
        &[L(parent_handle), I(nested), I(0), I(1), I(relative)],
    );
    let root = f.word(0);
    let open_root = f.call("RegOpenKeyExW", &[I(HKCU), I(0), I(0), I(1), I(root)]);
    let closed = f.call("RegCloseKey", &[L(key)]);
    let twice = f.call("RegCloseKey", &[L(key)]);
    let size = f.word(0);
    let stale = f.call(
        "RegQueryValueExW",
        &[L(key), I(0), I(0), I(0), I(0), I(size)],
    );
    let duplicate_still_open = f.call(
        "RegQueryValueExW",
        &[L(duplicate), I(0), I(0), I(0), I(0), I(size)],
    );
    let root_close = f.call("RegCloseKey", &[I(HKCU)]);
    f.run();
    assert_eq!(f.get(missing), 2);
    for rc in [
        created,
        recreated,
        open_parent,
        open_relative,
        open_root,
        closed,
        root_close,
    ] {
        assert_eq!(f.get(rc), 0);
    }
    assert_eq!((f.get(disposition), f.get(existing)), (1, 2));
    assert_ne!(f.get(key), f.get(duplicate));
    assert_ne!(f.get(parent_handle), f.get(relative));
    assert_eq!(f.get(root), HKCU);
    assert_eq!(
        (f.get(twice), f.get(stale), f.get(duplicate_still_open)),
        (6, 6, 2)
    );
    let mut isolated = Fixture::new(932);
    let path = isolated.a("Software\\Registry Fixture\\Nested", 932);
    let out = isolated.word(0);
    let rc = isolated.call("RegOpenKeyExA", &[I(HKCU), I(path), I(0), I(1), I(out)]);
    isolated.run();
    assert_eq!(isolated.get(rc), 2);
}

#[test]
fn ansi_and_wide_share_values_types_exact_lengths_and_capacity_contract() {
    for cp in [932, 936] {
        let mut f = Fixture::new(cp);
        let path = f.w("Software\\漢字\\Fixture");
        let (created, key, _) = f.create(path, true, 3);
        let mut checks = Vec::new();
        for (kind, text) in [(1, "漢字\0"), (2, "%TEMP%\\漢字\0"), (7, "一\0二\0\0")] {
            let value_name = format!("value{kind}");
            let name_a = f.a(&value_name, cp);
            let name_w = f.w(&value_name.to_uppercase());
            let ansi = Codec::new(cp).encode(text);
            let wide: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let data = f.blob(&ansi);
            let set = f.call(
                "RegSetValueExA",
                &[
                    L(key),
                    I(name_a),
                    I(0),
                    I(kind),
                    I(data),
                    I(ansi.len() as u32),
                ],
            );
            for (api, query_name, expected) in [
                ("RegQueryValueExA", name_a, ansi),
                ("RegQueryValueExW", name_w, wide),
            ] {
                let kind_out = f.word(0);
                let size_only = f.word(0);
                let size_query = f.call(
                    api,
                    &[L(key), I(query_name), I(0), I(kind_out), I(0), I(size_only)],
                );
                let short = f.blob(&[0x55; 64]);
                let short_size = f.word(1);
                let more = f.call(
                    api,
                    &[
                        L(key),
                        I(query_name),
                        I(0),
                        I(kind_out),
                        I(short),
                        I(short_size),
                    ],
                );
                let output = f.blob(&[0xa5; 64]);
                let capacity = f.word(64);
                let query = f.call(
                    api,
                    &[
                        L(key),
                        I(query_name),
                        I(0),
                        I(kind_out),
                        I(output),
                        I(capacity),
                    ],
                );
                checks.push((
                    set, size_query, more, query, kind_out, kind, size_only, short_size, output,
                    capacity, short, expected,
                ));
            }
        }
        let binary = f.blob(&[0, 0xff, 0x80, 0x01]);
        let set_default = f.call(
            "RegSetValueExW",
            &[L(key), I(0), I(0), I(4), I(binary), I(4)],
        );
        let default_bytes = f.blob(&[0xcc; 8]);
        let default_size = f.word(8);
        let default_type = f.word(0);
        let query_default = f.call(
            "RegQueryValueExA",
            &[
                L(key),
                I(0),
                I(0),
                I(default_type),
                I(default_bytes),
                I(default_size),
            ],
        );
        let raw_name = f.w("unterminated");
        let raw_data = f.blob(b"A\0B\0");
        let set_raw = f.call(
            "RegSetValueExW",
            &[L(key), I(raw_name), I(0), I(1), I(raw_data), I(4)],
        );
        let raw_output = f.blob(&[0xcc; 8]);
        let raw_size = f.word(8);
        let query_raw = f.call(
            "RegQueryValueExW",
            &[L(key), I(raw_name), I(0), I(0), I(raw_output), I(raw_size)],
        );
        f.run();
        assert_eq!(f.get(created), 0);
        for (
            set,
            size_query,
            more,
            query,
            kind_out,
            kind,
            size_only,
            short_size,
            output,
            capacity,
            short,
            expected,
        ) in checks
        {
            for rc in [set, size_query, query] {
                assert_eq!(f.get(rc), 0);
            }
            assert_eq!(f.get(more), 234);
            assert_eq!(f.get(kind_out), kind);
            for out in [size_only, short_size, capacity] {
                assert_eq!(f.get(out), expected.len() as u32);
            }
            assert_eq!(f.bytes(short, 64), vec![0x55; 64]);
            assert_eq!(f.bytes(output, expected.len()), expected);
            assert_eq!(f.bytes(output + expected.len() as u32, 1), [0xa5]);
        }
        for rc in [set_default, query_default, set_raw, query_raw] {
            assert_eq!(f.get(rc), 0);
        }
        assert_eq!((f.get(default_type), f.get(default_size)), (4, 4));
        assert_eq!(
            f.bytes(default_bytes, 8),
            [0, 0xff, 0x80, 1, 0xcc, 0xcc, 0xcc, 0xcc]
        );
        assert_eq!(f.get(raw_size), 4);
        assert_eq!(
            f.bytes(raw_output, 8),
            [b'A', 0, b'B', 0, 0xcc, 0xcc, 0xcc, 0xcc]
        );
    }
}

#[test]
fn handle_permissions_invalid_ranges_and_options_do_not_fake_success() {
    let mut f = Fixture::new(932);
    let path = f.a("Software\\PermissionFixture", 932);
    let (created, writer, _) = f.create(path, false, 2);
    let reader = f.word(0);
    let opened = f.call("RegOpenKeyExA", &[I(HKCU), I(path), I(0), I(1), I(reader)]);
    let bytes = f.blob(&[3, 4, 5, 6]);
    let size = f.word(4);
    let deny_set = f.call(
        "RegSetValueExA",
        &[L(reader), I(0), I(0), I(3), I(bytes), I(4)],
    );
    let deny_query = f.call(
        "RegQueryValueExA",
        &[L(writer), I(0), I(0), I(0), I(0), I(size)],
    );
    let set = f.call(
        "RegSetValueExA",
        &[L(writer), I(0), I(0), I(3), I(bytes), I(4)],
    );
    let invalid_capacity = f.call(
        "RegQueryValueExA",
        &[L(reader), I(0), I(0), I(0), I(bytes), I(0)],
    );
    let bad_output = f.call(
        "RegQueryValueExA",
        &[L(reader), I(0), I(0), I(0), I(READ_ONLY), I(size)],
    );
    let child = f.a("child", 932);
    let child_handle = f.word(0);
    // The default writable key ACL allows this, even though the handle is Query-only.
    let create_from_reader = f.call(
        "RegCreateKeyExA",
        &[
            L(reader),
            I(child),
            I(0),
            I(0),
            I(0),
            I(3),
            I(0),
            I(child_handle),
            I(0),
        ],
    );
    let huge = f.call(
        "RegSetValueExA",
        &[L(writer), I(0), I(0), I(3), I(bytes), I(u32::MAX)],
    );
    let invalid_source = f.call("RegSetValueExA", &[L(writer), I(0), I(0), I(3), I(0), I(4)]);
    let explicit64 = f.word(0);
    let no64 = f.call(
        "RegOpenKeyExA",
        &[I(HKCU), I(path), I(0), I(0x101), I(explicit64)],
    );
    let impossible = f.a("Software\\NotCreated", 932);
    let no_output = f.call(
        "RegCreateKeyExA",
        &[
            I(HKCU),
            I(impossible),
            I(0),
            I(0),
            I(0),
            I(3),
            I(0),
            I(READ_ONLY),
            I(0),
        ],
    );
    let ordinary_out = f.word(0);
    let not_created = f.call(
        "RegOpenKeyExA",
        &[I(HKCU), I(impossible), I(0), I(1), I(ordinary_out)],
    );
    f.run();
    for rc in [created, opened, set, create_from_reader] {
        assert_eq!(f.get(rc), 0);
    }
    assert_eq!((f.get(deny_set), f.get(deny_query)), (5, 5));
    assert_eq!(f.get(invalid_capacity), 87);
    assert_eq!(
        (f.get(bad_output), f.get(invalid_source), f.get(no_output)),
        (998, 998, 998)
    );
    assert_eq!(f.get(huge), 8);
    assert_eq!(f.get(no64), 50);
    assert_eq!(f.get(not_created), 2);
}

#[test]
fn root_refresh_empty_create_unicode_writes_and_zero_length_values() {
    let mut f = Fixture::new(1252);
    let key_name = f.w("Software\\École");
    let (create, key, _) = f.create(key_name, true, 3);
    let case_name = f.a("software\\éCOLE", 1252);
    let alias = f.word(0);
    let open = f.call(
        "RegOpenKeyExA",
        &[I(HKCU), I(case_name), I(0), I(1), I(alias)],
    );
    let default_type = f.word(0);
    let zero_size = f.word(0xffffffff);
    let set_zero = f.call("RegSetValueExW", &[L(key), I(0), I(0), I(3), I(0), I(0)]);
    let query_zero = f.call(
        "RegQueryValueExA",
        &[L(alias), I(0), I(0), I(default_type), I(0), I(zero_size)],
    );
    let type_only = f.call(
        "RegQueryValueExW",
        &[L(alias), I(0), I(0), I(default_type), I(0), I(0)],
    );
    let named = f.w("UnicodeValue");
    let wide_data: Vec<_> = "été\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let data = f.blob(&wide_data);
    let set_wide = f.call(
        "RegSetValueExW",
        &[
            L(key),
            I(named),
            I(0),
            I(1),
            I(data),
            I(wide_data.len() as u32),
        ],
    );
    let ansi_name = f.a("unicodevalue", 1252);
    let ansi_output = f.blob(&[0xcc; 16]);
    let ansi_size = f.word(16);
    let query_ansi = f.call(
        "RegQueryValueExA",
        &[
            L(alias),
            I(ansi_name),
            I(0),
            I(0),
            I(ansi_output),
            I(ansi_size),
        ],
    );
    let empty = f.a("", 1252);
    let (empty_create, root_handle, existing) = f.create(empty, false, 1);
    let null_create_out = f.word(0);
    let null_create = f.call(
        "RegCreateKeyExW",
        &[
            I(HKCU),
            I(0),
            I(0),
            I(0),
            I(0),
            I(1),
            I(0),
            I(null_create_out),
            I(0),
        ],
    );
    let invalid_reserved = f.call("RegSetValueExW", &[L(key), I(0), I(1), I(3), I(0), I(0)]);
    f.run();
    for rc in [
        create,
        open,
        set_zero,
        query_zero,
        type_only,
        set_wide,
        query_ansi,
        empty_create,
    ] {
        assert_eq!(f.get(rc), 0);
    }
    assert_eq!((f.get(zero_size), f.get(default_type)), (0, 3));
    assert_eq!(f.get(ansi_size), 4);
    assert_eq!(f.bytes(ansi_output, 5), [0xe9, b't', 0xe9, 0, 0xcc]);
    assert_ne!(
        f.get(root_handle),
        HKCU,
        "Create empty creates a new real handle"
    );
    assert_eq!(f.get(existing), 2);
    assert_eq!((f.get(null_create), f.get(invalid_reserved)), (87, 87));
}

#[test]
fn enumeration_converts_acp_names_and_data_but_metadata_uses_unicode_units() {
    for cp in [932, 936] {
        let mut f = Fixture::new(cp);
        let path = f.w("Software\\Enumeration Fixture");
        let (created, key, _) = f.create(path, true, 0xf003f);
        let child_name = f.w("漢字");
        let child_class = f.w("種類");
        let child = f.word(0);
        let child_created = f.call(
            "RegCreateKeyExW",
            &[
                L(key),
                I(child_name),
                I(0),
                I(child_class),
                I(0),
                I(0xf003f),
                I(0),
                I(child),
                I(0),
            ],
        );
        let data_name = f.w("漢字");
        let data = f.w("漢字");
        let set = f.call(
            "RegSetValueExW",
            &[L(key), I(data_name), I(0), I(1), I(data), I(6)],
        );
        let mut checks = Vec::new();
        for wide in [false, true] {
            let name = f.blob(&[0xcc; 32]);
            let class = f.blob(&[0xcc; 32]);
            let name_size = f.word(16);
            let class_size = f.word(16);
            let enum_key = f.call(
                if wide {
                    "RegEnumKeyExW"
                } else {
                    "RegEnumKeyExA"
                },
                &[
                    L(key),
                    I(0),
                    I(name),
                    I(name_size),
                    I(0),
                    I(class),
                    I(class_size),
                    I(0),
                ],
            );
            let value_name = f.blob(&[0xcc; 32]);
            let value_name_size = f.word(16);
            let short_data = f.blob(&[0xcc; 32]);
            let size = f.word(1);
            let kind = f.word(0);
            let enum_value = f.call(
                if wide {
                    "RegEnumValueW"
                } else {
                    "RegEnumValueA"
                },
                &[
                    L(key),
                    I(0),
                    I(value_name),
                    I(value_name_size),
                    I(0),
                    I(kind),
                    I(short_data),
                    I(size),
                ],
            );
            let counts: Vec<_> = (0..7).map(|_| f.word(0)).collect();
            let info = f.call(
                if wide {
                    "RegQueryInfoKeyW"
                } else {
                    "RegQueryInfoKeyA"
                },
                &[
                    L(key),
                    I(0),
                    I(0),
                    I(0),
                    I(counts[0]),
                    I(counts[1]),
                    I(counts[2]),
                    I(counts[3]),
                    I(counts[4]),
                    I(counts[5]),
                    I(counts[6]),
                    I(0),
                ],
            );
            let encode = |s: &str| {
                if wide {
                    s.encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>()
                } else {
                    let mut bytes = Codec::new(cp).encode(s);
                    bytes.push(0);
                    bytes
                }
            };
            checks.push((
                enum_key,
                name,
                name_size,
                class,
                class_size,
                enum_value,
                value_name,
                value_name_size,
                short_data,
                size,
                kind,
                info,
                counts,
                encode("漢字"),
                encode("種類"),
                wide,
            ));
        }
        let end_size = f.word(16);
        let end_buffer = f.blob(&[0xcc; 32]);
        let end = f.call(
            "RegEnumKeyExW",
            &[
                L(key),
                I(1),
                I(end_buffer),
                I(end_size),
                I(0),
                I(0),
                I(0),
                I(0),
            ],
        );
        f.run();
        for rc in [created, child_created, set] {
            assert_eq!(f.get(rc), 0);
        }
        for (
            enum_key,
            name,
            name_size,
            class,
            class_size,
            enum_value,
            value_name,
            value_name_size,
            short_data,
            size,
            kind,
            info,
            counts,
            expected_name,
            expected_class,
            wide,
        ) in checks
        {
            for rc in [enum_key, info] {
                assert_eq!(f.get(rc), 0);
            }
            let unit = if wide { 2 } else { 1 };
            assert_eq!(f.get(name_size), (expected_name.len() / unit - 1) as u32);
            assert_eq!(f.get(class_size), (expected_class.len() / unit - 1) as u32);
            assert_eq!(f.bytes(name, expected_name.len()), expected_name);
            assert_eq!(f.bytes(class, expected_class.len()), expected_class);
            assert_eq!(f.get(enum_value), 234);
            assert_eq!(f.bytes(value_name, expected_name.len()), expected_name);
            assert_eq!(
                f.get(value_name_size),
                (expected_name.len() / unit - 1) as u32
            );
            assert_eq!(f.get(size), expected_name.len() as u32);
            assert_eq!(f.get(kind), 1);
            assert_eq!(f.bytes(short_data, 32), [0xcc; 32]);
            assert_eq!(
                counts.into_iter().map(|p| f.get(p)).collect::<Vec<_>>(),
                [1, 2, 2, 1, 2, 6, 20]
            );
        }
        assert_eq!(f.get(end), 259);
        assert_eq!(f.bytes(end_buffer, 32), [0xcc; 32]);
    }
}

#[test]
fn enumeration_permissions_short_outputs_and_deleted_handle_identity_are_preserved() {
    let mut f = Fixture::new(932);
    let path = f.w("Software\\Deletion Fixture");
    let (created, key, _) = f.create(path, true, 0xf003f);
    let child_name = f.w("Child");
    let child = f.word(0);
    let child_created = f.call(
        "RegCreateKeyExW",
        &[
            L(key),
            I(child_name),
            I(0),
            I(0),
            I(0),
            I(0xf003f),
            I(0),
            I(child),
            I(0),
        ],
    );
    let name = f.blob(&[0xcc; 32]);
    let cap = f.word(5);
    let class_cap = f.word(9);
    let short = f.call(
        "RegEnumKeyExW",
        &[
            L(key),
            I(0),
            I(name),
            I(cap),
            I(0),
            I(0),
            I(class_cap),
            I(0),
        ],
    );
    let good_cap = f.word(16);
    let bad_name = f.call(
        "RegEnumKeyExW",
        &[
            L(key),
            I(0),
            I(READ_ONLY),
            I(good_cap),
            I(0),
            I(0),
            I(0),
            I(0),
        ],
    );
    let bad_capacity = f.call(
        "RegEnumKeyExW",
        &[L(key), I(0), I(name), I(READ_ONLY), I(0), I(0), I(0), I(0)],
    );
    let query_only = f.word(0);
    let opened = f.call("RegOpenKeyExW", &[L(key), I(0), I(0), I(1), I(query_only)]);
    let deny_enum = f.call(
        "RegEnumKeyExW",
        &[
            L(query_only),
            I(0),
            I(name),
            I(good_cap),
            I(0),
            I(0),
            I(0),
            I(0),
        ],
    );
    let deny_delete = f.call("RegDeleteValueW", &[L(query_only), I(0)]);
    let nonleaf = f.call("RegDeleteKeyW", &[I(HKCU), I(path)]);
    let deleted = f.call("RegDeleteKeyW", &[L(query_only), I(child_name)]);
    let stale = f.call(
        "RegQueryInfoKeyW",
        &[
            L(child),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
        ],
    );
    let replacement = f.word(0);
    let recreated = f.call(
        "RegCreateKeyExW",
        &[
            L(key),
            I(child_name),
            I(0),
            I(0),
            I(0),
            I(1),
            I(0),
            I(replacement),
            I(0),
        ],
    );
    let still_stale = f.call(
        "RegOpenKeyExW",
        &[L(child), I(0), I(0), I(1), I(query_only)],
    );
    let closed = f.call("RegCloseKey", &[L(child)]);
    f.run();
    for rc in [created, child_created, opened, deleted, recreated, closed] {
        assert_eq!(f.get(rc), 0);
    }
    assert_eq!(f.get(short), 234);
    assert_eq!((f.get(cap), f.get(class_cap)), (5, 0));
    assert_eq!(f.bytes(name, 32), [0xcc; 32]);
    assert_eq!((f.get(bad_name), f.get(bad_capacity)), (998, 998));
    assert_eq!(
        (f.get(deny_enum), f.get(deny_delete), f.get(nonleaf)),
        (5, 5, 5)
    );
    assert_eq!((f.get(stale), f.get(still_stale)), (1018, 1018));
    assert_ne!(f.get(child), f.get(replacement));
}

#[test]
fn nested_creation_updates_the_immediate_parent_timestamp() {
    let mut f = Fixture::new(932);
    let path = f.w("Software");
    let (created, software, _) = f.create(path, true, 0xf003f);
    let software_before = f.blob(&[0; 8]);
    let root_before = f.blob(&[0; 8]);
    let mut results = vec![created];
    for (key, time) in [(L(software), software_before), (I(HKCU), root_before)] {
        results.push(f.call(
            "RegQueryInfoKeyW",
            &[
                key,
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(time),
            ],
        ));
    }
    let sleep = f.machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    f.asm.push_imm(1).call(sleep);
    let child = f.w("Software\\Child");
    let (child_created, _, _) = f.create(child, true, 0xf003f);
    results.push(child_created);
    let software_after = f.blob(&[0; 8]);
    let root_after = f.blob(&[0; 8]);
    for (key, time) in [(L(software), software_after), (I(HKCU), root_after)] {
        results.push(f.call(
            "RegQueryInfoKeyW",
            &[
                key,
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(0),
                I(time),
            ],
        ));
    }
    f.run();
    for result in results {
        assert_eq!(f.get(result), 0);
    }
    let time = |ptr| u64::from_le_bytes(f.bytes(ptr, 8).try_into().unwrap());
    assert!(time(software_after) > time(software_before));
    assert_eq!(time(root_after), time(root_before));
}
