//! Real selected-font Unicode kerning pairs and checked guest publication.
#![cfg(windows)]
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError},
    Graphics::Gdi as gdi,
};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CROSS: u32 = 0x460000;
const READONLY: u32 = 0x470000;
const LOGICAL: u32 = DATA;
const ZERO_LOGICAL: u32 = DATA + 128;
const PATH: u32 = DATA + 256;
const FULL: u32 = DATA + 16384;
const PARTIAL: u32 = DATA + 512;
const MARKER: u32 = 0x12345678;

fn logical(face: &str, height: i32) -> [u8; 92] {
    let mut bytes = [0; 92];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    bytes[16..20].copy_from_slice(&400i32.to_le_bytes());
    bytes[23] = 1;
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
fn native_pairs() -> Vec<u8> {
    let mut request = gdi::LOGFONTW {
        lfHeight: -32,
        lfWeight: 400,
        lfCharSet: 1,
        ..Default::default()
    };
    for (slot, unit) in request.lfFaceName.iter_mut().zip("Arial".encode_utf16()) {
        *slot = unit;
    }
    // SAFETY: authored private DC/font reference, all storage is initialized and
    // every native selection is restored before releasing its owned handles.
    unsafe {
        let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
        let font = gdi::CreateFontIndirectW(&request);
        assert!(!dc.is_null() && !font.is_null());
        let old = gdi::SelectObject(dc, font);
        SetLastError(MARKER);
        let count = gdi::GetKerningPairsW(dc, 0, std::ptr::null_mut());
        assert!(
            count > 4 && count < 16384,
            "native reference has a bounded real kerning table"
        );
        assert_eq!(GetLastError(), MARKER);
        let mut pairs = vec![gdi::KERNINGPAIR::default(); count as usize];
        assert_eq!(gdi::GetKerningPairsW(dc, count, pairs.as_mut_ptr()), count);
        assert!(
            pairs.iter().any(|pair| pair.iKernAmount < 0),
            "signed adjustments are observable"
        );
        gdi::SelectObject(dc, old);
        gdi::DeleteObject(font);
        gdi::DeleteDC(dc);
        pairs
            .into_iter()
            .flat_map(|pair| {
                let mut bytes = [0; 8];
                bytes[..2].copy_from_slice(&pair.wFirst.to_le_bytes());
                bytes[2..4].copy_from_slice(&pair.wSecond.to_le_bytes());
                bytes[4..].copy_from_slice(&pair.iKernAmount.to_le_bytes());
                bytes
            })
            .collect()
    }
}
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
    fn new(cpu: B, backend: &str, count: u32) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        assert_eq!(
            registry.get("gdi32.dll", "GetKerningPairsW").unwrap().conv,
            Conv::Stdcall { argc: 3 }
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/kerning-pairs-w-tests")
            .join(format!("{}-{backend}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("zero.ttf"),
            include_bytes!("../../adv-device/tests/fixtures/font-resource.ttf"),
        )
        .unwrap();
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(&root, "kerning.exe", root.join("overlay")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 262144, Prot::RW).unwrap();
        machine.cpu.map(CROSS, 8192, Prot::RW).unwrap();
        machine.cpu.write(CROSS, &[0xcd; 8192]).unwrap();
        machine.cpu.protect(CROSS + 4096, 4096, Prot::READ).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write(READONLY, &[0xcd; 16]).unwrap();
        machine.cpu.write(LOGICAL, &logical("Arial", -32)).unwrap();
        machine
            .cpu
            .write(ZERO_LOGICAL, &logical("ADV Resource Fixture", -40))
            .unwrap();
        machine
            .cpu
            .write(
                PATH,
                &"zero.ttf"
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        machine
            .cpu
            .write(FULL, &vec![0xcd; (count as usize + 2) * 8])
            .unwrap();
        machine.cpu.write(PARTIAL, &[0xcd; 128]).unwrap();
        machine
            .cpu
            .write(DATA + 1024, &u16::from(b'B').to_le_bytes())
            .unwrap();
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
    fn pairs(&mut self, dc: Arg, count: u32, output: u32) -> (u32, u32) {
        self.call("kernel32.dll", "SetLastError", &[V(MARKER)]);
        let result = self.gdi("GetKerningPairsW", &[dc, V(count), V(output)]);
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
            .spawn("Unicode kerning guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(2_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
}
fn contract<B: TestCpu>(cpu: B, backend: &str, expected: &[u8]) {
    let count = (expected.len() / 8) as u32;
    let mut f = Fixture::new(cpu, backend, count);
    let owners = f.machine.sys.objects.object_count();
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 1200);
    let dc = f.gdi("CreateCompatibleDC", &[V(0)]);
    let font = f.gdi("CreateFontIndirectW", &[V(LOGICAL)]);
    let old = f.gdi("SelectObject", &[S(dc), S(font)]);
    let total = f.pairs(S(dc), 0, 0);
    let ignored_capacity = f.pairs(S(dc), u32::MAX, 0);
    let full = f.pairs(S(dc), count + 2, FULL);
    let partial = f.pairs(S(dc), 2, PARTIAL);
    let zero_capacity = f.pairs(S(dc), 0, PARTIAL + 64);
    let invalid_dc = [f.pairs(V(0), 0, 0), f.pairs(V(u32::MAX), 0, 0)];
    let guards = [
        f.pairs(S(dc), 2, READONLY),
        f.pairs(S(dc), 2, CROSS + 4096 - 8),
        f.pairs(S(dc), u32::MAX, PARTIAL + 64),
        f.pairs(S(dc), 1, u32::MAX - 3),
        f.pairs(S(dc), adv_device::text::MAX_KERNING_PAIRS + 1, PARTIAL + 64),
    ];
    f.gdi("SelectObject", &[S(dc), S(old)]);
    f.gdi("DeleteObject", &[S(font)]);
    let added = f.gdi("AddFontResourceExW", &[V(PATH), V(0x10), V(0)]);
    let no_kern_font = f.gdi("CreateFontIndirectW", &[V(ZERO_LOGICAL)]);
    f.gdi("SelectObject", &[S(dc), S(no_kern_font)]);
    let zero_total = f.pairs(S(dc), 0, 0);
    let zero_copied = f.pairs(S(dc), 4, PARTIAL + 64);
    let extent = f.gdi(
        "GetTextExtentPoint32W",
        &[S(dc), V(DATA + 1024), V(1), V(DATA + 1040)],
    );
    f.gdi("SelectObject", &[S(dc), S(old)]);
    f.gdi("DeleteObject", &[S(no_kern_font)]);
    let removed = f.gdi("RemoveFontResourceExW", &[V(PATH), V(0x10), V(0)]);
    f.gdi("DeleteDC", &[S(dc)]);
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 1204);
    f.run();
    for pair in [total, ignored_capacity, full] {
        assert_eq!(f.value(pair.0), count);
        assert_eq!(f.value(pair.1), MARKER);
    }
    assert_eq!(f.value(partial.0), 2);
    assert_eq!(f.value(partial.1), MARKER);
    assert!(
        f.bytes(FULL, expected.len()) == expected,
        "{backend}: complete selected-font pairs differ"
    );
    assert!(
        f.bytes(PARTIAL, 16) == expected[..16],
        "{backend}: truncated pair prefix differs"
    );
    assert!(
        f.bytes(FULL + count * 8, 16)
            .iter()
            .all(|&byte| byte == 0xcd)
    );
    assert!(f.bytes(PARTIAL + 16, 112).iter().all(|&byte| byte == 0xcd));
    for pair in guards.into_iter().chain([zero_capacity]) {
        assert_eq!(f.value(pair.0), 0);
        assert_eq!(f.value(pair.1), 87);
    }
    for pair in invalid_dc.into_iter().chain([zero_total, zero_copied]) {
        assert_eq!(f.value(pair.0), 0);
        assert_eq!(f.value(pair.1), MARKER);
    }
    assert_eq!(f.value(added), 1);
    assert_eq!(f.value(removed), 1);
    assert_eq!(f.value(extent), 1);
    assert_eq!(
        f.value(DATA + 1040),
        20,
        "the zero-pair font is the authored face"
    );
    assert!(f.bytes(CROSS, 8192).iter().all(|&byte| byte == 0xcd));
    assert!(f.bytes(READONLY, 16).iter().all(|&byte| byte == 0xcd));
    assert_eq!(
        f.value(DATA + 1200),
        f.value(DATA + 1204),
        "stdcall restores the guest stack"
    );
    assert_eq!(adv_win32::gdi32::font::cache_stats(&f.machine).fonts, 0);
    assert_eq!(f.machine.sys.objects.object_count(), owners + 1);
    assert!(
        f.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, reason)| {
                api == "gdi32.dll!GetKerningPairsW" && reason.contains("budget")
            }),
        "an oversized profile is explicitly reported as unavailable"
    );
}
#[test]
fn selected_unicode_kerning_count_copy_partial_zero_and_guards_match_three_guest_backends() {
    let expected = native_pairs();
    contract(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn", &expected);
    contract(JitCpu::new(PARK_ADDR, false).unwrap(), "jit", &expected);
    contract(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        "interpreter",
        &expected,
    );
}
