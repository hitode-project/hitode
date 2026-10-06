//! Charset queries follow the same selected, realized font as text metrics.
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
const READONLY: u32 = 0x420000;

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
enum Arg {
    Value(u32),
    Saved(u32),
}
use Arg::{Saved as S, Value as V};

struct Fixture<B: TestCpu> {
    machine: Machine<B>,
    code: Asm,
    data: u32,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B, cp: u32) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for (name, argc) in [("GetTextCharset", 1), ("GetTextCharsetInfo", 3)] {
            assert_eq!(
                registry.get("gdi32.dll", name).unwrap().conv,
                Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available("gdi32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/text-charset-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = cp;
        Win32::install(&mut machine, config).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write(READONLY, &[0xcd; 24]).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            data: DATA,
        }
    }
    fn allocate(&mut self, bytes: &[u8]) -> u32 {
        let pointer = self.data;
        self.data += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.data < DATA + 0x10000);
        self.machine.cpu.write(pointer, bytes).unwrap();
        pointer
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("gdi32.dll", name));
        let function = self.machine.resolve_import(module, name).unwrap();
        for argument in args.iter().rev() {
            match *argument {
                V(value) => {
                    self.code.push_imm(value);
                }
                S(pointer) => {
                    self.code.mov_eax_mem(pointer).push_eax();
                }
            }
        }
        let output = self.allocate(&[0; 4]);
        self.code.call(function).mov_mem_eax(output);
        output
    }
    fn error(&mut self) -> u32 {
        self.call("kernel32.dll!GetLastError", &[])
    }
    fn font(&mut self, face: &str, charset: u8) -> u32 {
        let mut logical = [0; 60];
        logical[..4].copy_from_slice(&(-24_i32).to_le_bytes());
        logical[16..20].copy_from_slice(&400_i32.to_le_bytes());
        logical[23] = charset;
        logical[28..28 + face.len()].copy_from_slice(face.as_bytes());
        let pointer = self.allocate(&logical);
        self.call("CreateFontIndirectA", &[V(pointer)])
    }
    fn run(&mut self) {
        self.code.bytes.push(0xb8);
        self.code.bytes.extend_from_slice(&0_u32.to_le_bytes());
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("font charset guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn bytes(&self, pointer: u32, size: usize) -> Vec<u8> {
        let mut output = vec![0; size];
        self.machine.cpu.read(pointer, &mut output).unwrap();
        output
    }
}

#[cfg(windows)]
fn native_charset(face: &str, charset: u8) -> u32 {
    use windows_sys::Win32::Graphics::Gdi as gdi;
    // These two wingdi.h declarations are gated on Globalization in
    // windows-sys; the small native oracle only uses a NULL signature.
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetTextCharset(dc: gdi::HDC) -> i32;
        fn GetTextCharsetInfo(dc: gdi::HDC, signature: *mut std::ffi::c_void, flags: u32) -> i32;
    }
    let mut logical = gdi::LOGFONTW {
        lfHeight: -24,
        lfWeight: 400,
        lfCharSet: charset,
        ..Default::default()
    };
    for (slot, unit) in logical.lfFaceName.iter_mut().zip(face.encode_utf16()) {
        *slot = unit;
    }
    // SAFETY: this self-authored font only uses an owned memory DC, creates no
    // window, and restores the original font before disposing both objects.
    unsafe {
        let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
        assert!(!dc.is_null());
        let font = gdi::CreateFontIndirectW(&logical);
        assert!(!font.is_null());
        let old = gdi::SelectObject(dc, font);
        assert!(!old.is_null());
        let charset = GetTextCharset(dc);
        assert_eq!(charset, GetTextCharsetInfo(dc, std::ptr::null_mut(), 0));
        let mut metrics = gdi::TEXTMETRICA::default();
        assert_ne!(gdi::GetTextMetricsA(dc, &mut metrics), 0);
        assert_eq!(charset as u8, metrics.tmCharSet);
        assert!(!gdi::SelectObject(dc, old).is_null());
        assert_ne!(gdi::DeleteObject(font), 0);
        assert_ne!(gdi::DeleteDC(dc), 0);
        charset as u32
    }
}

fn selection<B: TestCpu>(cpu: B, cp: u32) {
    let mut test = Fixture::new(cpu, cp);
    let dc = test.call("CreateCompatibleDC", &[V(0)]);
    let stock = test.call("GetStockObject", &[V(13)]);
    let stock_before = test.call("GetTextCharset", &[S(dc)]);
    let mut results = Vec::new();
    for (face, charset) in [
        ("Arial", 0),
        ("MS Gothic", 128),
        ("SimSun", 134),
        ("Arial", 1),
    ] {
        let font = test.font(face, charset);
        test.call("SelectObject", &[S(dc), S(font)]);
        test.call("kernel32.dll!SetLastError", &[V(0x1234)]);
        let query = test.call("GetTextCharset", &[S(dc)]);
        let info = test.call("GetTextCharsetInfo", &[S(dc), V(0), V(0)]);
        let error = test.error();
        let metrics = test.allocate(&[0; 56]);
        let success = test.call("GetTextMetricsA", &[S(dc), V(metrics)]);
        test.call("SelectObject", &[S(dc), S(stock)]);
        let deleted = test.call("DeleteObject", &[S(font)]);
        results.push((face, charset, query, info, error, metrics, success, deleted));
    }
    let stock_after = test.call("GetTextCharset", &[S(dc)]);
    let deleted_dc = test.call("DeleteDC", &[S(dc)]);
    let stale = test.call("GetTextCharset", &[S(dc)]);
    let stale_error = test.error();
    test.run();
    assert_eq!(test.value(stock_before), test.value(stock_after));
    for (face, charset, query, info, error, metrics, success, deleted) in results {
        assert_eq!(test.value(success), 1);
        let realized = u32::from(test.bytes(metrics, 56)[52]);
        assert_eq!((test.value(query), test.value(info)), (realized, realized));
        assert_eq!(test.value(error), 0x1234);
        assert_eq!(test.value(deleted), 1);
        #[cfg(windows)]
        assert_eq!(
            realized,
            native_charset(face, charset),
            "cp={cp} face={face} logical charset={charset}"
        );
        #[cfg(not(windows))]
        let _ = (face, charset);
    }
    assert_eq!(
        (
            test.value(deleted_dc),
            test.value(stale),
            test.value(stale_error)
        ),
        (1, 1, 6)
    );
    let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}

#[test]
fn charset_queries_follow_selected_realized_fonts_and_ignore_guest_process_acp() {
    for cp in [1252, 936] {
        selection(UnicornCpu::new(PARK_ADDR).unwrap(), cp);
        selection(JitCpu::new(PARK_ADDR, false).unwrap(), cp);
        selection(JitCpu::new(PARK_ADDR, true).unwrap(), cp);
    }
}

fn failures<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 932);
    let dc = test.call("CreateCompatibleDC", &[V(0)]);
    let font = test.font("Arial", 0);
    let signature = test.allocate(&[0xcc; 24]);
    let unsupported = test.call("GetTextCharsetInfo", &[S(dc), V(signature), V(0)]);
    let unsupported_error = test.error();
    let readonly = test.call("GetTextCharsetInfo", &[S(dc), V(READONLY), V(0)]);
    let readonly_error = test.error();
    let reserved = test.call("GetTextCharsetInfo", &[S(dc), V(0), V(1)]);
    let reserved_error = test.error();
    let mut invalid = Vec::new();
    for argument in [V(0), V(0xdeadbeef), S(font)] {
        let charset = test.call("GetTextCharset", &[argument]);
        let error = test.error();
        invalid.push((charset, error));
    }
    let bad_dc_first = test.call("GetTextCharsetInfo", &[V(0), V(0xfffffff0), V(0)]);
    let bad_dc_error = test.error();
    test.call("DeleteObject", &[S(font)]);
    test.call("DeleteDC", &[S(dc)]);
    test.run();
    assert_eq!(
        (test.value(unsupported), test.value(unsupported_error)),
        (1, 50)
    );
    assert_eq!((test.value(readonly), test.value(readonly_error)), (1, 998));
    assert_eq!((test.value(reserved), test.value(reserved_error)), (1, 87));
    for (query, error) in invalid {
        assert_eq!((test.value(query), test.value(error)), (1, 6));
    }
    assert_eq!((test.value(bad_dc_first), test.value(bad_dc_error)), (1, 6));
    assert_eq!(test.bytes(signature, 24), vec![0xcc; 24]);
    assert_eq!(test.bytes(READONLY, 24), vec![0xcd; 24]);
    let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(
        diagnostics
            .warnings
            .keys()
            .any(|(api, reason)| api == "gdi32.dll!GetTextCharsetInfo"
                && reason.contains("coverage"))
    );
}

#[test]
fn charset_queries_reject_invalid_dc_and_unsupported_signature_without_publishing_bytes() {
    failures(UnicornCpu::new(PARK_ADDR).unwrap());
    failures(JitCpu::new(PARK_ADDR, false).unwrap());
    failures(JitCpu::new(PARK_ADDR, true).unwrap());
}
