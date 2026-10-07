//! File font resources exercise real guest calls, callbacks and owner cleanup.
#![cfg(windows)]

use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CALLBACK: u32 = CODE + 3072;
const FONT: &[u8] = include_bytes!("../../adv-device/tests/fixtures/font-resource.ttf");
const PATH_A: u32 = DATA;
const PATH_W: u32 = DATA + 128;
const QUERY: u32 = DATA + 256;
const COUNTER: u32 = DATA + 512;
const FONT_LOGICAL: u32 = DATA + 768;
const EXTENT: u32 = DATA + 1024;
const B: u32 = DATA + 1040;

trait TestCpu: Backend {
    fn region(&mut self, _: u32, _: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn region(&mut self, start: u32, length: usize) {
        self.executor_mut()
            .add_region(Region::new(start, length))
            .unwrap();
    }
}
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Saved(u32),
}
use Arg::{Saved as S, Value as V};

struct Fixture<B: TestCpu> {
    machine: Machine<B>,
    code: Asm,
    next: u32,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B, backend: &str) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/font-resource-ex-tests")
            .join(format!("{}-{backend}", std::process::id()));
        std::fs::create_dir_all(root.join("Fonts")).unwrap();
        std::fs::write(root.join("Fonts/字形.ttf"), FONT).unwrap();
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for name in [
            "AddFontResourceExA",
            "AddFontResourceExW",
            "RemoveFontResourceExA",
            "RemoveFontResourceExW",
        ] {
            assert_eq!(
                registry.get("gdi32.dll", name).unwrap().conv,
                Conv::Stdcall { argc: 3 }
            );
            assert!(registry.dynamic_available("gdi32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let mut config = Win32Config::new(&root, "font-resource-ex.exe", root.join("overlay"));
        config.os.codepage = 936;
        Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let path = machine.ext.get::<Win32>().codec.encode("Fonts\\字形.ttf");
        machine
            .cpu
            .write(PATH_A, &path.into_iter().chain([0]).collect::<Vec<_>>())
            .unwrap();
        let wide = "c:/gAmE/Fonts/./字形.ttf"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        machine.cpu.write(PATH_W, &wide).unwrap();
        let mut query = [0; 92];
        query[23] = 1; // DEFAULT_CHARSET: discover this file's actual repertoire.
        for (slot, unit) in query[28..]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip("ADV Resource Fixture".encode_utf16())
        {
            *slot = unit.to_le_bytes();
        }
        machine.cpu.write(QUERY, &query).unwrap();
        query[..4].copy_from_slice(&(-40i32).to_le_bytes());
        query[16..20].copy_from_slice(&400i32.to_le_bytes());
        machine.cpu.write(FONT_LOGICAL, &query).unwrap();
        machine
            .cpu
            .write(B, &u16::from(b'B').to_le_bytes())
            .unwrap();
        // inc dword ptr [COUNTER]; mov eax,1; ret 16. The guest callback sees
        // copied ENUMLOGFONTEXW/TEXTMETRIC data and continues the enumeration.
        let mut callback = vec![0xff, 0x05];
        callback.extend_from_slice(&COUNTER.to_le_bytes());
        callback.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc2, 16, 0]);
        machine.cpu.write(CALLBACK, &callback).unwrap();
        machine.cpu.region(CALLBACK, callback.len());
        Self {
            machine,
            code: Asm::new(CODE),
            next: DATA + 2048,
        }
    }
    fn call(&mut self, module: &str, name: &str, args: &[Arg]) -> u32 {
        let address = self.machine.resolve_import(module, name).unwrap();
        for &arg in args.iter().rev() {
            match arg {
                V(value) => {
                    self.code.push_imm(value);
                }
                S(pointer) => {
                    self.code.mov_eax_mem(pointer).push_eax();
                }
            }
        }
        let output = self.next;
        self.next += 4;
        self.code.call(address).mov_mem_eax(output);
        output
    }
    fn gdi(&mut self, name: &str, args: &[Arg]) -> u32 {
        self.call("gdi32.dll", name, args)
    }
    fn enumerate(&mut self, dc: u32) -> u32 {
        self.code.mov_mem_imm(COUNTER, 0);
        self.gdi(
            "EnumFontFamiliesExW",
            &[S(dc), V(QUERY), V(CALLBACK), V(0), V(0)],
        );
        let output = self.next;
        self.next += 4;
        self.code.mov_eax_mem(COUNTER).mov_mem_eax(output);
        output
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("file font guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(2_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
}

fn contract<B: TestCpu>(cpu: B, backend: &str) {
    let mut f = Fixture::new(cpu, backend);
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    f.code.mov_mem_eax(DATA + 1536);
    let dc = f.gdi("CreateCompatibleDC", &[V(0)]);
    let baseline = f.enumerate(dc);
    let add_w = f.gdi("AddFontResourceExW", &[V(PATH_W), V(0x10), V(0)]);
    let visible = f.enumerate(dc);
    let add_a = f.gdi("AddFontResourceExA", &[V(PATH_A), V(0x10), V(1)]);
    let wrong_owner = f.gdi("RemoveFontResourceW", &[V(PATH_W)]);
    let wrong_visible_flags = f.gdi("RemoveFontResourceExW", &[V(PATH_W), V(0x30), V(0)]);
    let font = f.gdi("CreateFontIndirectW", &[V(FONT_LOGICAL)]);
    let old = f.gdi("SelectObject", &[S(dc), S(font)]);
    let extent = f.gdi("GetTextExtentPoint32W", &[S(dc), V(B), V(1), V(EXTENT)]);
    let remove_a = f.gdi("RemoveFontResourceExA", &[V(PATH_A), V(0x10), V(1)]);
    let still_visible = f.enumerate(dc);
    f.gdi("SelectObject", &[S(dc), S(old)]);
    f.gdi("DeleteObject", &[S(font)]);
    let remove_w = f.gdi("RemoveFontResourceExW", &[V(PATH_W), V(0x10), V(0)]);
    let removed = f.enumerate(dc);
    let hidden_add = f.gdi("AddFontResourceExA", &[V(PATH_A), V(0x30), V(0)]);
    let hidden = f.enumerate(dc);
    let wrong_visibility = f.gdi("RemoveFontResourceExW", &[V(PATH_W), V(0x10), V(0)]);
    let hidden_remove = f.gdi("RemoveFontResourceExW", &[V(PATH_W), V(0x30), V(0)]);
    let missing_remove = f.gdi("RemoveFontResourceExA", &[V(PATH_A), V(0x30), V(0)]);
    let unsupported = f.gdi("AddFontResourceExW", &[V(PATH_W), V(0x50), V(0)]);
    let unsupported_error = f.call("kernel32.dll", "GetLastError", &[]);
    let legacy_add = f.gdi("AddFontResourceA", &[V(PATH_A)]);
    let legacy_visible = f.enumerate(dc);
    let legacy_remove = f.gdi("RemoveFontResourceExW", &[V(PATH_W), V(0), V(0)]);
    let final_enum = f.enumerate(dc);
    f.gdi("DeleteDC", &[S(dc)]);
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 1540);
    f.run();
    for value in [
        add_w,
        add_a,
        remove_a,
        remove_w,
        hidden_add,
        hidden_remove,
        legacy_add,
        legacy_remove,
        extent,
    ] {
        assert_eq!(f.value(value), 1, "{backend} result at {value:#x}");
    }
    for value in [
        baseline,
        wrong_owner,
        wrong_visible_flags,
        removed,
        hidden,
        wrong_visibility,
        missing_remove,
        unsupported,
        final_enum,
    ] {
        assert_eq!(f.value(value), 0, "{backend} result at {value:#x}");
    }
    for value in [visible, still_visible, legacy_visible] {
        assert!(f.value(value) > 0, "{backend} enumeration at {value:#x}");
    }
    assert_eq!(
        f.value(EXTENT),
        20,
        "authored B has a 500-unit advance at 40 pixels"
    );
    assert_eq!(f.value(unsupported_error), 87);
    assert_eq!(
        f.value(DATA + 1536),
        f.value(DATA + 1540),
        "stdcall restores ESP across callbacks"
    );
    assert_eq!(adv_win32::gdi32::font::cache_stats(&f.machine).fonts, 0);
}

#[test]
fn file_resource_visibility_references_and_vfs_paths_survive_three_guest_backends() {
    contract(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    contract(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
    contract(JitCpu::new(PARK_ADDR, true).unwrap(), "interpreter");
}
