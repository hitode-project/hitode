//! Authored Unicode metric calls use one selected-font owner and checked output.
#![cfg(windows)]

use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
use windows_sys::Win32::Graphics::Gdi as gdi;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = 0x430000;
const CROSS: u32 = 0x440000;
const LOG: u32 = DATA + 2048;
const MARKER: u32 = 0x12345678;

trait TestCpu: Backend {
    fn region(&mut self, _: u32, _: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn region(&mut self, start: u32, size: usize) {
        self.executor_mut()
            .add_region(Region::new(start, size))
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
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        assert_eq!(
            registry.get("gdi32.dll", "GetTextMetricsW").unwrap().conv,
            Conv::Stdcall { argc: 2 }
        );
        assert!(registry.dynamic_available("gdi32.dll", "GetTextMetricsW"));
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/font-metrics-w-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        // W metrics must be independent of the guest ACP as well as host ACP.
        config.os.codepage = 932;
        Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write(READONLY, &[0xcd; 60]).unwrap();
        machine.cpu.map(CROSS, 8192, Prot::RW).unwrap();
        machine.cpu.write(CROSS, &[0xcd; 8192]).unwrap();
        machine.cpu.protect(CROSS + 4096, 4096, Prot::READ).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: LOG,
        }
    }
    fn call(&mut self, module: &str, name: &str, args: &[Arg]) -> u32 {
        let entry = self.machine.resolve_import(module, name).unwrap();
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
        let result = self.next;
        self.next += 4;
        self.code.call(entry).mov_mem_eax(result);
        result
    }
    fn gdi(&mut self, name: &str, args: &[Arg]) -> u32 {
        self.call("gdi32.dll", name, args)
    }
    fn failure(&mut self, dc: Arg, output: u32) -> (u32, u32) {
        self.call("kernel32.dll", "SetLastError", &[V(MARKER)]);
        let result = self.gdi("GetTextMetricsW", &[dc, V(output)]);
        let error = self.call("kernel32.dll", "GetLastError", &[]);
        (result, error)
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn bytes(&self, pointer: u32, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        self.machine.cpu.read(pointer, &mut bytes).unwrap();
        bytes
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("selected Unicode metric guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
}

fn logical() -> [u8; 92] {
    let mut bytes = [0; 92];
    bytes[..4].copy_from_slice(&(-24i32).to_le_bytes());
    bytes[16..20].copy_from_slice(&400i32.to_le_bytes());
    bytes[23] = 128; // SHIFTJIS_CHARSET, not the host system charset.
    for (slot, unit) in bytes[28..]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip("MS Gothic".encode_utf16())
    {
        *slot = unit.to_le_bytes();
    }
    bytes
}

fn native_metrics() -> [u8; 60] {
    assert_eq!(std::mem::size_of::<gdi::TEXTMETRICW>(), 60);
    assert_eq!(std::mem::offset_of!(gdi::TEXTMETRICW, tmFirstChar), 44);
    assert_eq!(std::mem::offset_of!(gdi::TEXTMETRICW, tmCharSet), 56);
    let mut font = gdi::LOGFONTW {
        lfHeight: -24,
        lfWeight: 400,
        lfCharSet: 128,
        ..Default::default()
    };
    for (slot, unit) in font.lfFaceName.iter_mut().zip("MS Gothic".encode_utf16()) {
        *slot = unit;
    }
    // SAFETY: independent authored LOGFONTW, an owned memory DC and complete
    // host output storage; no desktop/window DC is acquired. Restore first.
    let metric = unsafe {
        let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
        assert!(!dc.is_null());
        let font = gdi::CreateFontIndirectW(&font);
        assert!(!font.is_null());
        let old = gdi::SelectObject(dc, font);
        assert!(!old.is_null());
        let mut metric = gdi::TEXTMETRICW::default();
        assert_ne!(gdi::GetTextMetricsW(dc, &mut metric), 0);
        assert!(!gdi::SelectObject(dc, old).is_null());
        assert_ne!(gdi::DeleteObject(font), 0);
        assert_ne!(gdi::DeleteDC(dc), 0);
        metric
    };
    let mut bytes = [0; 60];
    for (index, value) in [
        metric.tmHeight,
        metric.tmAscent,
        metric.tmDescent,
        metric.tmInternalLeading,
        metric.tmExternalLeading,
        metric.tmAveCharWidth,
        metric.tmMaxCharWidth,
        metric.tmWeight,
        metric.tmOverhang,
        metric.tmDigitizedAspectX,
        metric.tmDigitizedAspectY,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (index, value) in [
        metric.tmFirstChar,
        metric.tmLastChar,
        metric.tmDefaultChar,
        metric.tmBreakChar,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[44 + index * 2..46 + index * 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes[52..57].copy_from_slice(&[
        metric.tmItalic,
        metric.tmUnderlined,
        metric.tmStruckOut,
        metric.tmPitchAndFamily,
        metric.tmCharSet,
    ]);
    bytes
}

fn contract<B: TestCpu>(cpu: B, expected: &[u8; 60]) {
    let mut f = Fixture::new(cpu);
    f.machine.cpu.write(DATA, &logical()).unwrap();
    let wide = DATA + 128;
    let narrow = DATA + 256;
    let untouched = DATA + 384;
    f.machine.cpu.write(wide, &[0xcd; 64]).unwrap();
    f.machine.cpu.write(narrow, &[0xcd; 60]).unwrap();
    f.machine.cpu.write(untouched, &[0xcd; 60]).unwrap();
    let owners = f.machine.sys.objects.object_count();
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 1024);
    let dc = f.gdi("CreateCompatibleDC", &[V(0)]);
    let font = f.gdi("CreateFontIndirectW", &[V(DATA)]);
    let old = f.gdi("SelectObject", &[S(dc), S(font)]);
    f.call("kernel32.dll", "SetLastError", &[V(MARKER)]);
    let wr = f.gdi("GetTextMetricsW", &[S(dc), V(wide)]);
    let we = f.call("kernel32.dll", "GetLastError", &[]);
    let ar = f.gdi("GetTextMetricsA", &[S(dc), V(narrow)]);
    let ae = f.call("kernel32.dll", "GetLastError", &[]);
    let null_dc = f.failure(V(0), untouched);
    let null_output = f.failure(S(dc), 0);
    let invalid_dc = f.failure(V(u32::MAX), untouched);
    let guards = [
        f.failure(S(dc), READONLY),
        f.failure(S(dc), CROSS + 4096 - 58),
        f.failure(S(dc), u32::MAX - 20),
    ];
    let restored = f.gdi("SelectObject", &[S(dc), S(old)]);
    let deleted_font = f.gdi("DeleteObject", &[S(font)]);
    let deleted_dc = f.gdi("DeleteDC", &[S(dc)]);
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 1028);
    f.run();
    assert_ne!(f.value(dc), 0);
    assert_ne!(f.value(font), 0);
    assert_eq!(f.value(wr), 1);
    assert_eq!(f.value(ar), 1);
    assert_eq!(f.value(we), MARKER);
    assert_eq!(f.value(ae), MARKER);
    assert_eq!(f.bytes(wide, 60), expected);
    assert_eq!(f.bytes(wide + 60, 4), [0xcd; 4]);
    assert_eq!(f.bytes(narrow, 44), &expected[..44]);
    assert_eq!(f.bytes(narrow + 48, 5), &expected[52..57]);
    assert_eq!(f.bytes(narrow + 56, 4), [0xcd; 4]);
    for pair in [null_dc, null_output] {
        assert_eq!(f.value(pair.0), 0);
        assert_eq!(f.value(pair.1), MARKER);
    }
    assert_eq!(f.value(invalid_dc.0), 0);
    assert_eq!(f.value(invalid_dc.1), 6);
    for pair in guards {
        assert_eq!(f.value(pair.0), 0);
        assert_eq!(f.value(pair.1), 87);
    }
    assert_eq!(f.bytes(untouched, 60), [0xcd; 60]);
    assert_eq!(f.bytes(READONLY, 60), [0xcd; 60]);
    assert_eq!(f.bytes(CROSS, 8192), [0xcd; 8192]);
    assert_eq!(f.value(DATA + 1024), f.value(DATA + 1028));
    assert_eq!(f.value(restored), f.value(font));
    assert_eq!(f.value(deleted_font), 1);
    assert_eq!(f.value(deleted_dc), 1);
    assert!(
        f.machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&f.machine.sys.objects, f.value(font))
            .is_none()
    );
    assert!(
        f.machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&f.machine.sys.objects, f.value(dc))
            .is_none()
    );
    assert_eq!(f.machine.sys.objects.object_count(), owners + 1); // Known fixture thread.
    assert_eq!(adv_win32::gdi32::font::cache_stats(&f.machine).fonts, 1);
}

#[test]
fn unicode_metrics_share_selected_font_and_publish_exact_wide_layout_atomically() {
    let expected = native_metrics(); // One independent memory-DC differential.
    assert!(u16::from_le_bytes(expected[46..48].try_into().unwrap()) > 255);
    contract(UnicornCpu::new(PARK_ADDR).unwrap(), &expected);
    contract(JitCpu::new(PARK_ADDR, false).unwrap(), &expected);
    contract(JitCpu::new(PARK_ADDR, true).unwrap(), &expected);
}
