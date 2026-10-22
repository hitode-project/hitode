//! No configured IME means no fabricated HIMC, conversion success or output data.
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
const READONLY: u32 = 0x430000;
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
    m: Machine<B>,
    code: Asm,
    next: u32,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for (name, argc) in [
            ("ImmGetContext", 1),
            ("ImmGetConversionStatus", 3),
            ("ImmSetConversionStatus", 3),
            ("ImmGetOpenStatus", 1),
            ("ImmSetOpenStatus", 2),
            ("ImmAssociateContext", 2),
            ("ImmReleaseContext", 2),
        ] {
            assert_eq!(
                registry.get("imm32.dll", name).unwrap().conv,
                Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available("imm32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ime-context-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = 932;
        Win32::install(&mut m, config).unwrap();
        m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        m.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        m.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        m.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        m.cpu.write(READONLY, &[0x77; 8]).unwrap();
        Self {
            m,
            code: Asm::new(CODE),
            next: DATA,
        }
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let p = self.next;
        self.next += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.next < DATA + 0x10000);
        self.m.cpu.write(p, bytes).unwrap();
        p
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("imm32.dll", name));
        let function = self.m.resolve_import(module, name).unwrap();
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
        let out = self.bytes(&[0; 4]);
        self.code.call(function).mov_mem_eax(out);
        out
    }
    fn checked(&mut self, name: &str, args: &[Arg]) -> (u32, u32) {
        self.call("kernel32.dll!SetLastError", &[V(MARKER)]);
        let value = self.call(name, args);
        let error = self.call("kernel32.dll!GetLastError", &[]);
        (value, error)
    }
    fn value(&self, p: u32) -> u32 {
        self.m.cpu.read_u32(p).unwrap()
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.m.cpu.write(CODE, &self.code.bytes).unwrap();
        self.m.cpu.region(CODE, self.code.bytes.len());
        self.m.spawn("no IME guest", CODE, &[], 65536).unwrap();
        let result = self.m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn assert_result(&self, result: (u32, u32), value: u32, error: u32) {
        assert_eq!((self.value(result.0), self.value(result.1)), (value, error));
    }
}

fn lifecycle<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let class = f.bytes(
        &"Static"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let window = f.call(
        "user32.dll!CreateWindowExW",
        &[
            V(0),
            V(class),
            V(0),
            V(0x80000000),
            V(0),
            V(0),
            V(1),
            V(1),
            V(0),
            V(0),
            V(0),
            V(0),
        ],
    );
    let event = f.call("kernel32.dll!CreateEventA", &[V(0), V(1), V(1), V(0)]);
    let context = f.checked("ImmGetContext", &[S(window)]);
    let invalid_window = f.checked("ImmGetContext", &[V(0)]);
    let conversion = f.bytes(&0xcccccccc_u32.to_le_bytes());
    let sentence = f.bytes(&0xdddddddd_u32.to_le_bytes());
    let get = f.checked(
        "ImmGetConversionStatus",
        &[S(context.0), V(conversion), V(sentence)],
    );
    let open = f.checked("ImmGetOpenStatus", &[S(context.0)]);
    let set = f.checked("ImmSetConversionStatus", &[S(context.0), V(0), V(0)]);
    let close = f.checked("ImmSetOpenStatus", &[S(context.0), V(0)]);
    let unsupported_open = f.checked("ImmSetOpenStatus", &[S(context.0), V(1)]);
    let disabled = f.checked("ImmAssociateContext", &[S(window), V(0)]);
    let disabled_again = f.checked("ImmGetContext", &[S(window)]);
    let other_handle = f.checked("ImmAssociateContext", &[S(window), S(event)]);
    let release = f.checked("ImmReleaseContext", &[S(window), S(event)]);
    let release_null = f.checked("ImmReleaseContext", &[V(0), V(0)]);
    let still_event = f.call("kernel32.dll!WaitForSingleObject", &[S(event), V(0)]);
    let still_window = f.call("user32.dll!IsWindow", &[S(window)]);
    let closed = f.call("kernel32.dll!CloseHandle", &[S(event)]);
    let destroyed = f.call("user32.dll!DestroyWindow", &[S(window)]);
    let stale_context = f.checked("ImmGetContext", &[S(window)]);
    let stale_associate = f.checked("ImmAssociateContext", &[S(window), V(0)]);
    f.run();
    assert_ne!(f.value(window), 0);
    assert_ne!(f.value(event), 0);
    for r in [
        context,
        invalid_window,
        get,
        open,
        disabled_again,
        stale_context,
        stale_associate,
    ] {
        f.assert_result(r, 0, MARKER);
    }
    for r in [set, close, unsupported_open, other_handle] {
        f.assert_result(r, 0, 6);
    }
    f.assert_result(disabled, 0, 0);
    f.assert_result(release, 1, MARKER);
    f.assert_result(release_null, 1, MARKER);
    assert_eq!(f.value(still_event), 0);
    assert_eq!(f.value(still_window), 1);
    assert_eq!(f.value(closed), 1);
    assert_eq!(f.value(destroyed), 1);
    assert_eq!(f.value(conversion), 0xcccccccc);
    assert_eq!(f.value(sentence), 0xdddddddd);
    assert_eq!(f.m.ext.get::<Win32>().codec.acp, 932);
    let diagnostics = f.m.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    for api in [
        "ImmGetContext",
        "ImmGetConversionStatus",
        "ImmGetOpenStatus",
        "ImmSetConversionStatus",
        "ImmSetOpenStatus",
        "ImmAssociateContext",
    ] {
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|(name, why)| name == &format!("imm32.dll!{api}")
                    && why.contains("host composition"))
        );
    }
}

#[test]
// spec: WIN32-USER-13
fn no_provider_disable_queries_and_native_release_share_live_user_and_object_owners() {
    lifecycle(UnicornCpu::new(PARK_ADDR).unwrap());
    lifecycle(JitCpu::new(PARK_ADDR, false).unwrap());
    lifecycle(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn outputs<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let before = f.m.sys.objects.object_count();
    let conversion = f.bytes(&0xcccccccc_u32.to_le_bytes());
    let sentence = f.bytes(&0xdddddddd_u32.to_le_bytes());
    let mut results = Vec::new();
    for context in [0, 0x1234] {
        for (a, b) in [
            (conversion, sentence),
            (conversion, READONLY),
            (READONLY, sentence),
            (conversion, 0x600000),
            (0xfffffffe, 0),
            (conversion, conversion),
        ] {
            results.push((
                f.checked("ImmGetConversionStatus", &[V(context), V(a), V(b)]),
                if context == 0 { MARKER } else { 6 },
            ));
        }
        results.push((
            f.checked("ImmGetOpenStatus", &[V(context)]),
            if context == 0 { MARKER } else { 6 },
        ));
        results.push((
            f.checked(
                "ImmSetConversionStatus",
                &[V(context), V(u32::MAX), V(u32::MAX)],
            ),
            6,
        ));
    }
    f.run();
    for (result, error) in results {
        f.assert_result(result, 0, error);
    }
    assert_eq!(f.value(conversion), 0xcccccccc);
    assert_eq!(f.value(sentence), 0xdddddddd);
    let mut readonly = [0; 8];
    f.m.cpu.read(READONLY, &mut readonly).unwrap();
    assert_eq!(readonly, [0x77; 8]);
    assert_eq!(f.m.sys.objects.object_count(), before + 1);
}

#[test]
fn null_or_invalid_himc_precedes_both_output_ranges_and_never_publishes_partial_status() {
    outputs(UnicornCpu::new(PARK_ADDR).unwrap());
    outputs(JitCpu::new(PARK_ADDR, false).unwrap());
    outputs(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn composition<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let buffer = f.bytes(&[0xcc; 64]);
    let mut results = Vec::new();
    for context in [0, 0x1234] {
        let getter = if context == 0 { MARKER } else { 6 };
        for name in ["ImmGetCompositionStringA", "ImmGetCompositionStringW"] {
            for index in [8, 0x800] {
                results.push((
                    f.checked(name, &[V(context), V(index), V(buffer), V(64)]),
                    getter,
                ));
            }
        }
        for name in [
            "ImmGetCompositionWindow",
            "ImmGetCompositionFontA",
            "ImmGetCompositionFontW",
        ] {
            results.push((f.checked(name, &[V(context), V(buffer)]), getter));
        }
        for name in [
            "ImmSetCompositionWindow",
            "ImmSetCandidateWindow",
            "ImmSetCompositionFontA",
            "ImmSetCompositionFontW",
        ] {
            results.push((f.checked(name, &[V(context), V(READONLY)]), 6));
        }
        results.push((
            f.checked("ImmNotifyIME", &[V(context), V(0x15), V(4), V(0)]),
            6,
        ));
    }
    f.run();
    for (result, error) in results {
        f.assert_result(result, 0, error);
    }
    let mut untouched = [0; 64];
    f.m.cpu.read(buffer, &mut untouched).unwrap();
    assert_eq!(untouched, [0xcc; 64]);
    let diagnostics = f.m.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(
        diagnostics
            .warnings
            .keys()
            .any(|(name, _)| name == "imm32.dll!ImmGetCompositionStringA")
    );
}

#[test]
// spec: WIN32-USER-13
fn composition_and_window_fronts_fail_without_a_context_and_never_touch_buffers() {
    composition(UnicornCpu::new(PARK_ADDR).unwrap());
    composition(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn installed_layout<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let layout = f.call("user32.dll!GetKeyboardLayout", &[V(0)]);
    let mut results = vec![f.checked("ImmIsIME", &[S(layout)])];
    for hkl in [0, 0x04090409, 0xe0010411, u32::MAX] {
        results.push(f.checked("ImmIsIME", &[V(hkl)]));
    }
    f.run();
    for result in results {
        f.assert_result(result, 0, MARKER);
    }
    assert!(f.m.ext.get::<adv_abi::ApiDiagnostics>().warnings.is_empty());
}
#[test]
// spec: WIN32-USER-13
fn non_ime_and_unknown_layouts_preserve_last_error_without_contexts() {
    installed_layout(UnicornCpu::new(PARK_ADDR).unwrap());
    installed_layout(JitCpu::new(PARK_ADDR, false).unwrap());
    installed_layout(JitCpu::new(PARK_ADDR, true).unwrap());
}
