//! Fixed Unicode LOGFONT input reaches the existing GDI owner and font matcher.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config, gdi32::GdiObj};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = 0x430000;
const NOACCESS: u32 = 0x440000;
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
    fn new(cpu: B, cp: u32) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        assert_eq!(
            registry
                .get("gdi32.dll", "CreateFontIndirectW")
                .unwrap()
                .conv,
            Conv::Stdcall { argc: 1 }
        );
        assert!(registry.dynamic_available("gdi32.dll", "CreateFontIndirectW"));
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/font-creation-w-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = cp;
        Win32::install(&mut machine, config).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.map(NOACCESS, 4096, Prot(0)).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: DATA,
        }
    }
    fn allocate(&mut self, bytes: &[u8]) -> u32 {
        let pointer = self.next;
        self.next += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.next < DATA + 0xf000);
        self.machine.cpu.write(pointer, bytes).unwrap();
        pointer
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("gdi32.dll", name));
        let entry = self.machine.resolve_import(module, name).unwrap();
        for &arg in args.iter().rev() {
            match arg {
                V(v) => {
                    self.code.push_imm(v);
                }
                S(p) => {
                    self.code.mov_eax_mem(p).push_eax();
                }
            }
        }
        let result = self.allocate(&[0; 4]);
        self.code.call(entry).mov_mem_eax(result);
        result
    }
    fn value(&self, p: u32) -> u32 {
        self.machine.cpu.read_u32(p).unwrap()
    }
    fn bytes(&self, p: u32, n: usize) -> Vec<u8> {
        let mut bytes = vec![0; n];
        self.machine.cpu.read(p, &mut bytes).unwrap();
        bytes
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("Unicode logical font guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        self.code = Asm::new(CODE);
    }
}

fn logical(face: &str) -> [u8; 92] {
    let mut bytes = [0; 92];
    for (offset, value) in [(0, 16_i32), (4, 8), (16, 400)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[23] = 128;
    bytes[24] = 4;
    bytes[26] = 6;
    bytes[27] = 1;
    for (slot, unit) in bytes[28..]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(face.encode_utf16())
    {
        *slot = unit.to_le_bytes();
    }
    bytes
}

#[cfg(windows)]
fn native_measurement(input: &[u8; 92], text: &[u16]) -> ([u8; 56], [u8; 8]) {
    use windows_sys::Win32::Graphics::Gdi as gdi;
    assert_eq!(std::mem::size_of::<gdi::LOGFONTW>(), 92);
    let i = |at| i32::from_le_bytes(input[at..at + 4].try_into().unwrap());
    let mut logical = gdi::LOGFONTW {
        lfHeight: i(0),
        lfWidth: i(4),
        lfEscapement: i(8),
        lfOrientation: i(12),
        lfWeight: i(16),
        lfItalic: input[20],
        lfUnderline: input[21],
        lfStrikeOut: input[22],
        lfCharSet: input[23],
        lfOutPrecision: input[24],
        lfClipPrecision: input[25],
        lfQuality: input[26],
        lfPitchAndFamily: input[27],
        ..Default::default()
    };
    for (slot, bytes) in logical
        .lfFaceName
        .iter_mut()
        .zip(input[28..].as_chunks::<2>().0)
    {
        *slot = u16::from_le_bytes(*bytes);
    }
    // SAFETY: only self-authored input and an owned memory DC; no window or
    // desktop DC is acquired. The old selection is restored before both frees.
    unsafe {
        let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
        assert!(!dc.is_null());
        let font = gdi::CreateFontIndirectW(&logical);
        assert!(!font.is_null());
        let old = gdi::SelectObject(dc, font);
        assert!(!old.is_null());
        let mut m = gdi::TEXTMETRICA::default();
        assert_ne!(gdi::GetTextMetricsA(dc, &mut m), 0);
        let mut size = windows_sys::Win32::Foundation::SIZE::default();
        assert_ne!(
            gdi::GetTextExtentPoint32W(dc, text.as_ptr(), text.len() as i32, &mut size),
            0
        );
        assert!(!gdi::SelectObject(dc, old).is_null());
        assert_ne!(gdi::DeleteObject(font), 0);
        assert_ne!(gdi::DeleteDC(dc), 0);
        let mut packed = [0; 56];
        for (index, v) in [
            m.tmHeight,
            m.tmAscent,
            m.tmDescent,
            m.tmInternalLeading,
            m.tmExternalLeading,
            m.tmAveCharWidth,
            m.tmMaxCharWidth,
            m.tmWeight,
            m.tmOverhang,
            m.tmDigitizedAspectX,
            m.tmDigitizedAspectY,
        ]
        .into_iter()
        .enumerate()
        {
            packed[index * 4..index * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        packed[44..53].copy_from_slice(&[
            m.tmFirstChar,
            m.tmLastChar,
            m.tmDefaultChar,
            m.tmBreakChar,
            m.tmItalic,
            m.tmUnderlined,
            m.tmStruckOut,
            m.tmPitchAndFamily,
            m.tmCharSet,
        ]);
        let mut extent = [0; 8];
        extent[..4].copy_from_slice(&size.cx.to_le_bytes());
        extent[4..].copy_from_slice(&size.cy.to_le_bytes());
        (packed, extent)
    }
}

fn metrics<B: TestCpu>(cpu: B, expected: Option<([u8; 56], [u8; 8])>) {
    let mut f = Fixture::new(cpu, 1252); // Wide face and SHIFTJIS charset are independent of ACP.
    let before = f.machine.sys.objects.object_count();
    let input = logical("MS ゴシック");
    f.machine.cpu.write(READONLY, &input).unwrap();
    let dc = f.call("CreateCompatibleDC", &[V(0)]);
    f.call("kernel32.dll!SetLastError", &[V(MARKER)]);
    let font = f.call("CreateFontIndirectW", &[V(READONLY)]);
    let error = f.call("kernel32.dll!GetLastError", &[]);
    let old = f.call("SelectObject", &[S(dc), S(font)]);
    let selected_delete = f.call("DeleteObject", &[S(font)]);
    let output = f.allocate(&[0xcc; 56]);
    let got = f.call("GetTextMetricsA", &[S(dc), V(output)]);
    let charset = f.call("GetTextCharset", &[S(dc)]);
    let units = [65_u16, 98, 0x3042];
    let text = f.allocate(
        &units
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let extent = f.allocate(&[0xcc; 8]);
    let measured = f.call("GetTextExtentPoint32W", &[S(dc), V(text), V(3), V(extent)]);
    f.call("SelectObject", &[S(dc), S(old)]);
    let deleted = f.call("DeleteObject", &[S(font)]);
    let stale = f.call("GetObjectA", &[S(font), V(60), V(0)]);
    let deleted_dc = f.call("DeleteDC", &[S(dc)]);
    f.run();
    assert_ne!(f.value(font), 0);
    assert_eq!(f.value(error), MARKER);
    assert_eq!(f.value(selected_delete), 0);
    assert_eq!(
        (
            f.value(got),
            f.value(measured),
            f.value(deleted),
            f.value(deleted_dc)
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(f.value(stale), 0);
    let actual = f.bytes(output, 56);
    assert_eq!(f.value(charset), u32::from(actual[52]));
    if let Some((metrics, size)) = expected {
        assert_eq!(actual, metrics);
        assert_eq!(f.bytes(extent, 8), size);
    }
    assert_eq!(f.bytes(READONLY, 92), input);
    assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    assert!(
        f.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
}

#[test]
fn fixed_wide_face_and_charset_realize_the_same_native_memory_font() {
    #[cfg(windows)]
    let expected = Some(native_measurement(
        &logical("MS ゴシック"),
        &[65, 98, 0x3042],
    ));
    #[cfg(not(windows))]
    let expected = None;
    metrics(UnicornCpu::new(PARK_ADDR).unwrap(), expected);
    metrics(JitCpu::new(PARK_ADDR, false).unwrap(), expected);
    metrics(JitCpu::new(PARK_ADDR, true).unwrap(), expected);
}

fn guards<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu, 932);
    let before = f.machine.sys.objects.object_count();
    let short = DATA + 0x10000 - 91;
    f.machine.cpu.write(short, &logical("Arial")[..91]).unwrap();
    let mut bad = logical("Arial");
    bad[28..30].copy_from_slice(&0xd800_u16.to_le_bytes());
    bad[30..32].fill(0);
    let invalid = f.allocate(&bad);
    let mut results = Vec::new();
    for pointer in [0, NOACCESS, short, 0xfffffff0, invalid] {
        f.call("kernel32.dll!SetLastError", &[V(MARKER)]);
        let value = f.call("CreateFontIndirectW", &[V(pointer)]);
        let error = f.call("kernel32.dll!GetLastError", &[]);
        results.push((pointer, value, error));
    }
    f.run();
    for (pointer, value, error) in results {
        assert_eq!(f.value(value), 0);
        assert_eq!(
            f.value(error),
            if pointer == invalid { 1113 } else { MARKER }
        );
    }
    assert_eq!(f.bytes(invalid, 92), bad);
    assert_eq!(f.bytes(short, 91), logical("Arial")[..91]);
    assert_eq!(f.machine.sys.objects.object_count(), before + 1);
    assert!(
        f.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, reason)| api == "gdi32.dll!CreateFontIndirectW"
                && reason.contains("unpaired UTF-16"))
    );
}

#[test]
fn invalid_fixed_input_cannot_publish_font_or_modify_guest_memory() {
    guards(UnicornCpu::new(PARK_ADDR).unwrap());
    guards(JitCpu::new(PARK_ADDR, false).unwrap());
    guards(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn snapshot<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu, 932);
    let before = f.machine.sys.objects.object_count();
    let face = "MS ゴシック";
    let input = logical(face);
    let source = f.allocate(&input);
    let wide = f.call("CreateFontIndirectW", &[V(source)]);
    f.code.mov_mem_imm(source + 28, 0); // Source lifetime does not own the font name.
    let mut ansi = [0; 60];
    ansi[..28].copy_from_slice(&input[..28]);
    let bytes = f.machine.ext.get::<Win32>().codec.encode(face);
    ansi[28..28 + bytes.len()].copy_from_slice(&bytes);
    let ansi_source = f.allocate(&ansi);
    let narrow = f.call("CreateFontIndirectA", &[V(ansi_source)]);
    let a = f.allocate(&[0xcc; 60]);
    let w = f.allocate(&[0xcc; 60]);
    let a_count = f.call("GetObjectA", &[S(narrow), V(60), V(a)]);
    let w_count = f.call("GetObjectA", &[S(wide), V(60), V(w)]);
    // Exactly 92 readable bytes end at a page boundary, without a face NUL.
    let bounded = DATA + 0x10000 - 92;
    let full = logical(&"Q".repeat(32));
    f.machine.cpu.write(bounded, &full).unwrap();
    let full_font = f.call("CreateFontIndirectW", &[V(bounded)]);
    f.run();
    assert_eq!((f.value(a_count), f.value(w_count)), (60, 60));
    assert_eq!(f.bytes(a, 60), ansi);
    assert_eq!(f.bytes(w, 60), ansi);
    for (handle, expected) in [
        (f.value(wide), face.to_owned()),
        (f.value(narrow), face.to_owned()),
        (f.value(full_font), "Q".repeat(32)),
    ] {
        let Some(GdiObj::Font(font)) = f
            .machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&f.machine.sys.objects, handle)
        else {
            panic!("font owner missing")
        };
        assert_eq!(font.face, expected);
        assert_eq!(&font.raw[..28], &input[..28]);
        assert_eq!(font.charset, 128);
    }
    assert_eq!(f.machine.sys.objects.object_count(), before + 4);
    let mut deleted = Vec::new();
    for result in [wide, narrow, full_font] {
        deleted.push(f.call("DeleteObject", &[S(result)]));
    }
    f.run();
    for result in deleted {
        assert_eq!(f.value(result), 1);
    }
    assert_eq!(f.machine.sys.objects.object_count(), before + 2); // Two retained guest threads only.
    assert!(
        f.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
}

#[test]
fn unicode_snapshot_shares_a_owner_and_keeps_fixed_face_boundary() {
    snapshot(UnicornCpu::new(PARK_ADDR).unwrap());
    snapshot(JitCpu::new(PARK_ADDR, false).unwrap());
    snapshot(JitCpu::new(PARK_ADDR, true).unwrap());
}
