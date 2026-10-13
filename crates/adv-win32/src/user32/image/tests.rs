//! Guest LoadImage calls and common USER image-owner observations.
mod copying;
mod icon_info;

use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const RESOURCE: u32 = 0x600000;
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn dib(size: u32, pixel: [u8; 4]) -> Vec<u8> {
    let stride = size.div_ceil(32) * 4;
    let mut bytes = vec![0; (40 + size * size * 4 + size * stride) as usize];
    put32(&mut bytes, 0, 40);
    put32(&mut bytes, 4, size);
    put32(&mut bytes, 8, size * 2);
    put16(&mut bytes, 12, 1);
    put16(&mut bytes, 14, 32);
    for p in bytes[40..(40 + size * size * 4) as usize].chunks_exact_mut(4) {
        p.copy_from_slice(&pixel);
    }
    bytes
}
fn ico() -> Vec<u8> {
    // An unsupported large PNG entry must not obstruct an exact small DIB.
    let images = [
        (256, b"\x89PNG\r\n\x1a\n".to_vec()),
        (16, dib(16, [255, 0, 0, 128])),
        (32, dib(32, [0, 255, 0, 255])),
    ];
    let mut bytes = vec![0; 6 + images.len() * 16];
    put16(&mut bytes, 2, 1);
    put16(&mut bytes, 4, images.len() as u16);
    for (n, (size, image)) in images.iter().enumerate() {
        let row = 6 + n * 16;
        bytes[row] = *size as u8;
        bytes[row + 1] = *size as u8;
        put16(&mut bytes, row + 4, 1);
        put16(&mut bytes, row + 6, 32);
        put32(&mut bytes, row + 8, image.len() as u32);
        let offset = bytes.len() as u32;
        put32(&mut bytes, row + 12, offset);
        bytes.extend_from_slice(image);
    }
    bytes
}
fn repeated_payload_ico() -> Vec<u8> {
    let image = dib(32, [0, 255, 0, 255]);
    let mut bytes = vec![0; 6 + 4096 * 16];
    put16(&mut bytes, 2, 1);
    put16(&mut bytes, 4, 4096);
    let offset = bytes.len() as u32;
    for index in 0..4096 {
        let row = 6 + index * 16;
        bytes[row] = 32;
        bytes[row + 1] = 32;
        put16(&mut bytes, row + 4, 1);
        put16(&mut bytes, row + 6, 32);
        put32(&mut bytes, row + 8, image.len() as u32);
        put32(&mut bytes, row + 12, offset);
    }
    bytes.extend_from_slice(&image);
    bytes
}
enum Arg {
    Value(u32),
    Memory(u32),
}
use Arg::{Memory as M, Value as I};
struct Fixture {
    machine: Machine<JitCpu>,
    data: u32,
    code: Asm,
}
impl Fixture {
    fn new(jit: bool) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/load-image-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let game = root.join("game");
        let data = root.join("data");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(data.join("overlay")).unwrap();
        // Base is malformed; only the case-insensitive overlay has the real ICO.
        std::fs::write(game.join("図像.ico"), b"bad base image").unwrap();
        std::fs::write(data.join("overlay/図像.ico"), ico()).unwrap();
        std::fs::write(game.join("bad.ico"), b"bad").unwrap();
        std::fs::write(game.join("repeated.ico"), repeated_payload_ico()).unwrap();
        let mut registry = Registry::default();
        crate::register(&mut registry);
        for api in ["LoadImageA", "LoadImageW"] {
            assert_eq!(
                registry.get("user32.dll", api).unwrap().conv,
                adv_abi::Conv::Stdcall { argc: 6 }
            );
            assert!(registry.dynamic_available("user32.dll", api));
        }
        for api in ["LoadCursorW", "LoadIconA", "LoadIconW"] {
            assert_eq!(
                registry.get("user32.dll", api).unwrap().conv,
                adv_abi::Conv::Stdcall { argc: 2 }
            );
            assert!(registry.dynamic_available("user32.dll", api));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut config = crate::Win32Config::new(game.canonicalize().unwrap(), "fixture.exe", data);
        config.os.codepage = 932;
        Win32::install(&mut machine, config).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        Self {
            machine,
            data: DATA,
            code: Asm::new(CODE),
        }
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let address = self.data;
        self.data += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.data < DATA + 4096);
        self.machine.cpu.write(address, bytes).unwrap();
        address
    }
    fn text(&mut self, text: &str, wide: bool) -> u32 {
        let bytes = if wide {
            text.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        } else {
            let mut bytes = self.machine.ext.get::<Win32>().codec.encode(text);
            bytes.push(0);
            bytes
        };
        self.bytes(&bytes)
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("user32.dll", name));
        let entry = self.machine.resolve_import(module, name).unwrap();
        for argument in args.iter().rev() {
            match argument {
                I(value) => {
                    self.code.push_imm(*value);
                }
                M(pointer) => {
                    self.code.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
        let output = self.bytes(&[0; 4]);
        self.code.call(entry).mov_mem_eax(output);
        output
    }
    fn load(
        &mut self,
        wide: bool,
        instance: u32,
        name: u32,
        width: u32,
        height: u32,
        flags: u32,
    ) -> u32 {
        self.call(
            if wide { "LoadImageW" } else { "LoadImageA" },
            &[I(instance), I(name), I(1), I(width), I(height), I(flags)],
        )
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine
            .cpu
            .executor_mut()
            .add_region(Region::new(CODE, self.code.bytes.len()))
            .unwrap();
        self.machine
            .spawn("LoadImage guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn image(&self, result: u32) -> &IconImage {
        self.machine
            .ext
            .get::<Win32>()
            .user
            .objects
            .image(&self.machine.sys.objects, self.value(result))
            .unwrap()
    }
    fn resource(&mut self) -> u32 {
        use adv_os::module::{Lifetime, Module, ModuleKind, Modules};
        use adv_pe::{ResourceData, ResourceDir, ResourceEntry, ResourceNode};
        self.machine.cpu.map(RESOURCE, 0x10000, Prot::READ).unwrap();
        let mut group = vec![0; 34];
        put16(&mut group, 2, 1);
        put16(&mut group, 4, 2);
        let mut icons = Vec::new();
        for (index, size) in [16_u32, 32].into_iter().enumerate() {
            let bytes = dib(size, [0, 0, 255, 255]);
            let row = 6 + index * 14;
            group[row] = size as u8;
            group[row + 1] = size as u8;
            put16(&mut group, row + 4, 1);
            put16(&mut group, row + 6, 32);
            put32(&mut group, row + 8, bytes.len() as u32);
            put16(&mut group, row + 12, 201 + index as u16);
            icons.push((
                ResId::Id(201 + index as u16),
                bytes,
                0x2000 + index as u32 * 0x2000,
            ));
        }
        let mut directory_offset = 0x1000;
        let mut entry = |id, bytes: Vec<u8>, rva| {
            self.machine.cpu.write(RESOURCE + rva, &bytes).unwrap();
            self.machine
                .cpu
                .write_u32(RESOURCE + directory_offset, rva)
                .unwrap();
            self.machine
                .cpu
                .write_u32(RESOURCE + directory_offset + 4, bytes.len() as u32)
                .unwrap();
            let data = ResourceData {
                entry_rva: directory_offset,
                rva,
                size: bytes.len() as u32,
                codepage: 0,
            };
            directory_offset += 16;
            ResourceEntry {
                id,
                node: ResourceNode::Dir(ResourceDir {
                    entries: vec![ResourceEntry {
                        id: ResId::Id(0x409),
                        node: ResourceNode::Data(data),
                    }],
                }),
            }
        };
        let icon_entries = icons
            .into_iter()
            .map(|(id, bytes, rva)| entry(id, bytes, rva))
            .collect();
        let group_entry = entry(ResId::Id(101), group, 0x6000);
        let mut cursor_group = vec![0; 20];
        put16(&mut cursor_group, 2, 2);
        put16(&mut cursor_group, 4, 1);
        put16(&mut cursor_group, 6, 16);
        put16(&mut cursor_group, 8, 16);
        put16(&mut cursor_group, 10, 1);
        put16(&mut cursor_group, 12, 32);
        put32(&mut cursor_group, 14, 1128);
        put16(&mut cursor_group, 18, 203);
        let cursor_entry = entry(ResId::Id(301), cursor_group, 0x7000);
        let mut cursor_name = cursor_entry.clone();
        cursor_name.id = ResId::Name("図像".into());
        let mut named = group_entry.clone();
        named.id = ResId::Name("図像".into());
        let resources = ResourceDir {
            entries: vec![
                ResourceEntry {
                    id: ResId::Id(12),
                    node: ResourceNode::Dir(ResourceDir {
                        entries: vec![cursor_entry, cursor_name],
                    }),
                },
                ResourceEntry {
                    id: ResId::Id(3),
                    node: ResourceNode::Dir(ResourceDir {
                        entries: icon_entries,
                    }),
                },
                ResourceEntry {
                    id: ResId::Id(14),
                    node: ResourceNode::Dir(ResourceDir {
                        entries: vec![group_entry, named],
                    }),
                },
            ],
        };
        self.machine.ext.get_mut::<Modules>().add(Module {
            name: "owned-image.dll".into(),
            path: "owned-image.dll".into(),
            kind: ModuleKind::Image,
            base: RESOURCE,
            size: 0x10000,
            is_dll: true,
            entry: 0,
            stack_reserve: 0,
            exports_by_name: Default::default(),
            exports_by_ordinal: Default::default(),
            resources: Some(resources),
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime::default(),
        });
        RESOURCE
    }
}

#[test]
fn file_images_share_ansi_unicode_overlay_decoder_and_real_dimensions_without_sharing_handles() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let a = test.text("図像.ICO", false);
        let w = test.text("図像.ico", true);
        let file = test.call(
            "kernel32.dll!CreateFileW",
            &[I(w), I(0xc0000000), I(7), I(0), I(3), I(0), I(0)],
        );
        let mapping = test.call(
            "kernel32.dll!CreateFileMappingW",
            &[M(file), I(0), I(4), I(0), I(0), I(0)],
        );
        let view = test.call(
            "kernel32.dll!MapViewOfFile",
            &[M(mapping), I(2), I(0), I(0), I(0)],
        );
        let pixel_offset = decode::dword(&ico(), 6 + 16 + 12).unwrap() + 40 + 15 * 16 * 4;
        test.code.mov_eax_mem(view).add_eax_imm(pixel_offset);
        test.code.bytes.extend_from_slice(&[0xc7, 0x00]);
        test.code
            .bytes
            .extend_from_slice(&0x80776655_u32.to_le_bytes());
        let first = test.load(true, CODE, w, 16, 16, FROM_FILE);
        let second = test.load(false, 0xdeadbeef, a, 16, 16, FROM_FILE | SHARED);
        let third = test.load(true, 0, w, 16, 16, FROM_FILE | SHARED);
        let rectangle = test.load(true, 0, w, 10, 20, FROM_FILE);
        let one_zero = test.load(true, 0, w, 0, 8, FROM_FILE);
        let default = test.load(true, 0, w, 0, 0, FROM_FILE | DEFAULT_SIZE);
        test.call("kernel32.dll!UnmapViewOfFile", &[M(view)]);
        test.call("kernel32.dll!CloseHandle", &[M(mapping)]);
        test.call("kernel32.dll!CloseHandle", &[M(file)]);
        test.run();
        assert_eq!(
            (test.image(first).width, test.image(first).height),
            (16, 16)
        );
        assert_ne!(test.value(view), 0);
        assert_eq!(&test.image(first).pixels[..4], &[0x77, 0x66, 0x55, 128]);
        assert_eq!(test.image(first).mask[0], 0);
        assert_eq!(
            (test.image(rectangle).width, test.image(rectangle).height),
            (10, 20)
        );
        assert_eq!(
            (test.image(one_zero).width, test.image(one_zero).height),
            (256, 8)
        );
        assert_eq!(
            (test.image(default).width, test.image(default).height),
            (32, 32)
        );
        assert_eq!(&test.image(default).pixels[..4], &[0, 255, 0, 255]);
        let handles = [first, second, third].map(|p| test.value(p));
        assert!(handles.iter().all(|&h| h != 0));
        assert_eq!(
            handles
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        assert!(
            test.machine
                .ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .is_empty()
        );
    }
}

#[test]
fn resource_ids_names_shared_cache_and_destroy_use_the_same_user_object_lifetime() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let module = test.resource();
        let name_a = test.text("図像", false);
        let name_w = test.text("図像", true);
        let baseline = test.machine.sys.objects.object_count();
        let private = test.load(true, module, 101, 16, 16, 0);
        let shared = test.load(false, module, 101, 32, 32, SHARED);
        let again = test.load(true, module, 101, 16, 16, SHARED);
        let named_a = test.load(false, module, name_a, 16, 16, 0);
        let named_w = test.load(true, module, name_w, 32, 32, 0);
        let cursor_a = test.call("LoadImageA", &[I(module), I(301), I(2), I(16), I(16), I(0)]);
        let cursor_w = test.call("LoadImageW", &[I(module), I(301), I(2), I(16), I(16), I(0)]);
        let cursor_old = test.call("LoadCursorA", &[I(module), I(301)]);
        test.call("DestroyCursor", &[M(cursor_a)]);
        test.call("DestroyIcon", &[M(private)]);
        test.call("DestroyIcon", &[M(named_a)]);
        test.call("DestroyIcon", &[M(named_w)]);
        test.call("DestroyIcon", &[M(shared)]);
        let after_destroy = test.load(true, module, 101, 16, 16, SHARED);
        test.run();
        assert_ne!(test.value(cursor_a), 0);
        assert_eq!(test.value(cursor_a), test.value(cursor_w));
        assert_eq!(test.value(cursor_a), test.value(cursor_old));
        assert_ne!(test.value(private), test.value(shared));
        assert_eq!(test.value(shared), test.value(again));
        assert_eq!(test.value(shared), test.value(after_destroy));
        assert_eq!(
            (test.image(shared).width, test.image(shared).height),
            (32, 32)
        );
        for result in [private, named_a, named_w] {
            assert_ne!(test.value(result), 0);
            assert!(
                test.machine
                    .ext
                    .get::<Win32>()
                    .user
                    .objects
                    .image(&test.machine.sys.objects, test.value(result))
                    .is_none()
            );
        }
        assert_eq!(test.machine.sys.objects.object_count(), baseline + 2); // One guest thread and one shared image.
    }
}

#[test]
fn image_failures_have_checked_names_native_file_errors_and_explicit_unsupported_warnings() {
    let mut test = Fixture::new(false);
    let path = test.text("図像.ico", true);
    let missing = test.text("missing.ico", false);
    let bad = test.text("bad.ico", true);
    let repeated = test.text("repeated.ico", true);
    let baseline = test.machine.sys.objects.object_count();
    let mut results = Vec::new();
    for (wide, inst, name, ty, width, flags, error) in [
        (true, 0, path, 1, u32::MAX, FROM_FILE, 87),
        (false, 0, missing, 1, 16, FROM_FILE, 2),
        (true, 0, bad, 1, 16, FROM_FILE, 0),
        (true, 0, repeated, 1, 16, FROM_FILE, 0),
        (true, 0, 0xdead0000, 1, 16, FROM_FILE, 998),
        (true, 0, path, 1, 16, FROM_FILE | 1, 50),
        (true, 0, path, 0, 16, FROM_FILE, 50),
        (true, 0, path, 2, 16, FROM_FILE, 50),
        (true, 0xdead0000, 101, 1, 16, 0, 126),
    ] {
        test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
        let result = test.call(
            if wide { "LoadImageW" } else { "LoadImageA" },
            &[I(inst), I(name), I(ty), I(width), I(16), I(flags)],
        );
        let last_error = test.call("kernel32.dll!GetLastError", &[]);
        results.push((result, last_error, error));
    }
    let opened = test.call(
        "kernel32.dll!CreateFileW",
        &[I(path), I(0x80000000), I(0), I(0), I(3), I(0), I(0)],
    );
    let sharing = test.load(true, 0, path, 16, 16, FROM_FILE);
    let sharing_error = test.call("kernel32.dll!GetLastError", &[]);
    test.call("kernel32.dll!CloseHandle", &[M(opened)]);
    test.run();
    assert_eq!((test.value(sharing), test.value(sharing_error)), (0, 32));
    for (result, error, expected) in results {
        assert_eq!((test.value(result), test.value(error)), (0, expected));
    }
    assert_eq!(test.machine.sys.objects.object_count(), baseline + 1);
    let diagnostic = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostic.stub_calls, 0);
    assert_eq!(diagnostic.warnings.len(), 4);
    assert!(
        diagnostic
            .warnings
            .keys()
            .all(|key| key.0 == "user32.dll!LoadImageW")
    );
}

#[test]
fn load_cursor_w_keeps_system_identity_and_shares_checked_unicode_resource_state_with_ansi() {
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let module = test.resource();
        let name_a = test.text("図像", false);
        let name_w = test.text("図像", true);
        let ordinal = test.text("#301", true);
        let missing = test.text("missing", true);
        let baseline = test.machine.sys.objects.object_count();
        let system_w = test.call("LoadCursorW", &[I(0), I(32512)]);
        let system_a = test.call("LoadCursorA", &[I(0), I(32512)]);
        let selected = test.call("SetCursor", &[M(system_w)]);
        let current = test.call("GetCursor", &[]);
        test.call("DestroyCursor", &[M(system_w)]);
        let system_again = test.call("LoadCursorW", &[I(0), I(32512)]);
        let named_w = test.call("LoadCursorW", &[I(module), I(name_w)]);
        let named_a = test.call("LoadCursorA", &[I(module), I(name_a)]);
        let named_image = test.call(
            "LoadImageW",
            &[I(module), I(name_w), I(2), I(0), I(0), I(0)],
        );
        let id = test.call("LoadCursorW", &[I(module), I(301)]);
        let string_id = test.call("LoadCursorW", &[I(module), I(ordinal)]);
        let bad = test.call("LoadCursorW", &[I(module), I(0xdead0000)]);
        let bad_error = test.call("kernel32.dll!GetLastError", &[]);
        let absent = test.call("LoadCursorW", &[I(module), I(missing)]);
        let absent_error = test.call("kernel32.dll!GetLastError", &[]);
        let invalid_system = test.call("LoadCursorW", &[I(0), I(0)]);
        let invalid_system_error = test.call("kernel32.dll!GetLastError", &[]);
        test.call("DestroyCursor", &[M(named_w)]);
        test.call("DestroyCursor", &[M(id)]);
        test.run();
        assert_ne!(test.value(system_w), 0);
        assert_eq!(test.value(system_w), test.value(system_a));
        assert_eq!(test.value(selected), 0); // SetCursor returns the previous cursor.
        assert_eq!(test.value(system_w), test.value(current));
        assert_eq!(test.value(system_w), test.value(system_again));
        assert_ne!(test.value(named_w), 0);
        assert_eq!(test.value(named_w), test.value(named_a));
        assert_eq!(test.value(named_w), test.value(named_image));
        assert_eq!(test.value(id), test.value(string_id));
        assert_eq!((test.value(bad), test.value(bad_error)), (0, 998));
        assert_eq!((test.value(absent), test.value(absent_error)), (0, 1814));
        assert_eq!(
            (test.value(invalid_system), test.value(invalid_system_error)),
            (0, 1814)
        );
        assert_eq!(test.machine.sys.objects.object_count(), baseline + 2); // Retained stock cursor and guest thread.
        let diagnostic = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostic.stub_calls, 0);
        assert!(
            diagnostic
                .warnings
                .keys()
                .any(|key| key.0 == "user32.dll!LoadCursorW"
                    && key.1.contains("headless resource identity"))
        );
    }
}

#[test]
fn load_icon_stock_shares_ansi_unicode_image_identity_without_aliasing_cursor_or_claiming_pixels() {
    // Owned native LoadIcon/LoadImage probe: stock A/W identity survives
    // DestroyIcon and is distinct from the same-numbered stock cursor.
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let baseline = test.machine.sys.objects.object_count();
        test.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
        let wide = test.call("LoadIconW", &[I(0), I(32516)]); // Actual IDI_INFORMATION path.
        let last_error = test.call("kernel32.dll!GetLastError", &[]);
        let ansi = test.call("LoadIconA", &[I(0), I(32516)]);
        let image = test.load(true, 0, 32516, 0, 0, DEFAULT_SIZE | SHARED);
        let cursor = test.call("LoadCursorW", &[I(0), I(32516)]);
        test.call("DestroyIcon", &[M(wide)]);
        let again = test.call("LoadIconW", &[I(0), I(32516)]);
        test.run();
        let handle = test.value(wide);
        assert_ne!(handle, 0);
        for result in [ansi, image, again] {
            assert_eq!(test.value(result), handle);
        }
        assert_eq!(test.value(last_error), 0x12345678);
        let cursor = test.value(cursor);
        assert_eq!(
            cursor,
            super::super::window::SYSTEM_OBJECT_BASE + (32516 & 0xfff) * 4
        );
        assert_ne!(handle, cursor);
        let objects = &test.machine.ext.get::<Win32>().user.objects;
        assert!(objects.contains(&test.machine.sys.objects, handle));
        assert!(objects.contains(&test.machine.sys.objects, cursor));
        assert!(objects.image(&test.machine.sys.objects, handle).is_none());
        assert!(test.machine.sys.objects.get(handle).is_none());
        assert_eq!(test.machine.sys.objects.object_count(), baseline + 3); // Two stocks and one thread.
        let diagnostic = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostic.stub_calls, 0);
        assert_eq!(diagnostic.warnings.len(), 3);
        for api in ["LoadIconA", "LoadIconW", "LoadImageW"] {
            assert!(diagnostic.warnings.keys().any(|key| {
                key.0 == format!("user32.dll!{api}")
                    && key.1.contains("stock pixel data is unavailable")
            }));
        }
    }
}

#[test]
fn load_icon_module_decodes_default_size_shares_resource_cache_and_checks_names_before_allocation()
{
    for jit in [false, true] {
        let mut test = Fixture::new(jit);
        let module = test.resource();
        let name_a = test.text("図像", false);
        let name_w = test.text("図像", true);
        let ordinal = test.text("#101", true);
        let missing = test.text("missing", true);
        let malformed = test.bytes(&[0x00, 0xd8, 0, 0]);
        test.machine.cpu.write(DATA + 4094, &[b'X', 0]).unwrap();
        let baseline = test.machine.sys.objects.object_count();
        let wide = test.call("LoadIconW", &[I(module), I(101)]);
        let ansi = test.call("LoadIconA", &[I(module), I(101)]);
        let id_string = test.call("LoadIconW", &[I(module), I(ordinal)]);
        let image = test.load(true, module, 101, 16, 16, SHARED);
        let named_a = test.call("LoadIconA", &[I(module), I(name_a)]);
        let named_w = test.call("LoadIconW", &[I(module), I(name_w)]);
        test.call("DestroyIcon", &[M(wide)]);
        let again = test.call("LoadIconW", &[I(module), I(101)]);
        let mut failures = Vec::new();
        for (instance, name, error) in [
            (module, 0xdead0000, 998),
            (module, DATA + 4094, 998),
            (module, malformed, 1113),
            (module, missing, 1814),
            (0xdead0000, 101, 126),
            (0, 0, 1814),
        ] {
            let result = test.call("LoadIconW", &[I(instance), I(name)]);
            let last_error = test.call("kernel32.dll!GetLastError", &[]);
            failures.push((result, last_error, error));
        }
        test.run();
        for result in [ansi, id_string, image, again] {
            assert_eq!(test.value(result), test.value(wide));
        }
        assert_ne!(test.value(wide), test.value(named_w));
        assert_eq!(test.value(named_a), test.value(named_w));
        for result in [wide, named_a] {
            let image = test.image(result);
            // Guest SM_CXICON/SM_CYICON are 32; native host DPI is independent.
            assert_eq!((image.width, image.height), (32, 32));
            assert!(image.pixels.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
            assert!(image.mask.iter().all(|&p| p == 0));
        }
        for (result, last_error, expected) in failures {
            assert_eq!((test.value(result), test.value(last_error)), (0, expected));
        }
        assert_eq!(test.machine.sys.objects.object_count(), baseline + 3); // Two cached icons and one thread.
        let diagnostic = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostic.stub_calls, 0);
        assert_eq!(diagnostic.warnings.len(), 1);
        assert!(
            diagnostic.warnings.keys().any(|key| {
                key.0 == "user32.dll!LoadIconW" && key.1.contains("unpaired UTF-16")
            })
        );
    }
}
