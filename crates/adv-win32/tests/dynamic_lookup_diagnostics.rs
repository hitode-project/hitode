//! Genuine guest GPA calls over authored DLLs; diagnostics never turn probes into calls.
use adv_abi::{ApiDiagnostics, DynamicLookupKind, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{
    FileSource, ModuleKind, Process,
    synthetic::{Export, Target},
};
use adv_pe::ExportTarget;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INIT_COUNT: u32 = DATA;

struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}
struct Image(Vec<u8>);
impl FileSource for Image {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        Some(self.0.clone())
    }
}

#[derive(Clone, Copy)]
enum Arg {
    Imm(u32),
    Stored(u32),
}
use Arg::{Imm as I, Stored as S};
struct Call {
    result: u32,
    caller: u32,
}
struct Test<B: Backend> {
    machine: Machine<B>,
    asm: Asm,
    next: u32,
    root: std::path::PathBuf,
}
impl<B: Backend> Test<B> {
    fn new(cpu: B, label: &str, mode: RunMode) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/dynamic-lookup-diagnostics")
            .join(format!("{}-{label}-{mode:?}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        registry.set_mode(mode);
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(&root, "owned.exe", root.join("overlay")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
        Self {
            machine,
            asm: Asm::new(CODE),
            next: DATA + 256,
            root,
        }
    }
    fn alloc(&mut self, size: u32) -> u32 {
        let result = self.next;
        self.next += size.next_multiple_of(4);
        assert!(self.next < DATA + 16384);
        result
    }
    fn text(&mut self, text: &str) -> u32 {
        let pointer = self.alloc(text.len() as u32 + 1);
        self.machine
            .cpu
            .write(pointer, format!("{text}\0").as_bytes())
            .unwrap();
        pointer
    }
    fn call(&mut self, name: &str, arguments: &[Arg]) -> Call {
        let function = self.machine.resolve_import("kernel32.dll", name).unwrap();
        for argument in arguments.iter().rev() {
            match argument {
                I(value) => {
                    self.asm.push_imm(*value);
                }
                S(pointer) => {
                    self.asm.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
        self.asm.call(function);
        let caller = self.asm.here();
        let result = self.alloc(4);
        self.asm.mov_mem_eax(result);
        Call { result, caller }
    }
    fn probe(&mut self, handle: Arg, name: Arg) -> (Call, Call) {
        self.call("SetLastError", &[I(0x1234)]);
        let result = self.call("GetProcAddress", &[handle, name]);
        let error = self.call("GetLastError", &[]);
        (result, error)
    }
    fn host(&mut self, name: &str) -> u32 {
        let id = self.machine.load(&Empty, name).unwrap();
        adv_os::modules(&self.machine.ext).get(id).base
    }
    fn image(&mut self, name: &str, base: u32, exports: &[Export], entry: bool) -> u32 {
        let mut bytes = adv_os::synthetic::build(base, name, exports).unwrap().bytes;
        if entry {
            let pe = adv_pe::Pe::parse(&bytes).unwrap();
            let ExportTarget::Rva(entry) = pe
                .exports()
                .unwrap()
                .into_iter()
                .find(|e| e.name.as_deref() == Some("DllMain"))
                .unwrap()
                .target
            else {
                panic!("initializer")
            };
            bytes[0x98 + 16..0x98 + 20].copy_from_slice(&entry.to_le_bytes());
        }
        std::fs::write(self.root.join(name), &bytes).unwrap();
        base
    }
    fn load_image(&mut self, name: &str, base: u32, exports: &[Export]) -> u32 {
        self.image(name, base, exports, false);
        let bytes = std::fs::read(self.root.join(name)).unwrap();
        let id = self.machine.load(&Image(bytes), name).unwrap();
        assert_eq!(
            adv_os::modules(&self.machine.ext).get(id).kind,
            ModuleKind::Image
        );
        adv_os::modules(&self.machine.ext).get(id).base
    }
    fn run(&mut self) {
        self.call("ExitProcess", &[I(0)]);
        self.machine.cpu.write(CODE, &self.asm.bytes).unwrap();
        self.machine.spawn("owned", CODE, &[], 65536).unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        if let Some(diagnostics) = self.machine.ext.try_get::<ApiDiagnostics>() {
            assert_eq!(diagnostics.stub_calls, 0);
            assert!(diagnostics.gaps.is_empty());
            assert!(diagnostics.acceptance_eligible(RunMode::Strict));
        }
    }
    fn value(&self, call: &Call) -> u32 {
        self.machine.cpu.read_u32(call.result).unwrap()
    }
}
fn export(name: &str, target: Target) -> Export {
    Export {
        name: Some(name.into()),
        ordinal: None,
        target,
    }
}

fn optional<B: Backend>(cpu: B, label: &str, mode: RunMode) {
    let mut test = Test::new(cpu, label, mode);
    let kernel = test.host("kernel32.dll");
    let comctl = test.host("comctl32.dll");
    let d2d = test.host("d2d1.dll");
    let known = test.text("FlsAlloc");
    let optional = test.text("AuthoredOptionalFeatureThatDoesNotExist");
    let implemented = test.text("TlsAlloc");
    let probes = [
        test.probe(I(kernel), I(known)),
        test.probe(I(kernel), I(optional)),
        test.probe(I(kernel), I(0xffff)),
        test.probe(I(0xdeadbeef), I(known)),
        test.probe(I(comctl), I(2)),
    ];
    let success = test.probe(I(kernel), I(implemented));
    let reviewed_proxy = test.probe(I(d2d), I(1));
    test.image(
        "data.dll",
        0x500000,
        &[export("DataOnly", Target::Code(vec![0xc3]))],
        false,
    );
    let data_name = test.text("data.dll");
    let data_module = test.call("LoadLibraryExA", &[I(data_name), I(0), I(2)]);
    let data_probe = test.probe(S(data_module.result), I(known));
    test.run();
    for (i, (result, error)) in probes.iter().enumerate() {
        assert_eq!(test.value(result), 0);
        assert_eq!(test.value(error), if i == 3 { 126 } else { 127 });
    }
    assert_ne!(test.value(&success.0), 0);
    assert_eq!(test.value(&success.1), 0x1234);
    assert_ne!(test.value(&reviewed_proxy.0), 0);
    assert_eq!(test.value(&reviewed_proxy.1), 0x1234);
    assert_ne!(test.value(&data_module), 0);
    assert_eq!(test.value(&data_probe.0), 0);
    assert_eq!(test.value(&data_probe.1), 126);
    let records = &test
        .machine
        .ext
        .get::<ApiDiagnostics>()
        .dynamic_lookup_failures;
    assert_eq!(records.len(), 6);
    for (i, kind) in [
        DynamicLookupKind::Unimplemented,
        DynamicLookupKind::MissingExport,
        DynamicLookupKind::MissingExport,
        DynamicLookupKind::InvalidModule,
        DynamicLookupKind::Unimplemented,
        DynamicLookupKind::InvalidModule,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(records[i].kind, kind);
        assert_eq!(
            records[i].caller,
            if i == 5 {
                data_probe.0.caller
            } else {
                probes[i].0.caller
            }
        );
        assert_eq!(records[i].context, 0);
        assert!(records[i].sequence > 0);
        if i > 0 {
            assert!(records[i].sequence > records[i - 1].sequence);
        }
    }
    assert_eq!(records[0].module, "kernel32.dll");
    assert_eq!(records[0].name, "FlsAlloc");
    assert_eq!(records[0].failure_module.as_deref(), Some("kernel32.dll"));
    assert_eq!(records[0].failure_name.as_deref(), Some("FlsAlloc"));
    assert_eq!(records[2].name, "#65535");
    assert_eq!(records[3].module, "<invalid-hmodule>");
    assert_eq!(records[4].module, "comctl32.dll");
    assert_eq!(records[4].name, "#2");
    assert_eq!(records[5].module, "data.dll");
    assert!(
        test.machine
            .sys
            .thunks
            .iter()
            .filter(|(_, thunk)| thunk.name == "FlsAlloc")
            .all(|(_, thunk)| !thunk.requested)
    );
}

#[test]
fn optional_probes_keep_returns_last_error_and_acceptance_on_all_backends_and_modes() {
    for mode in [RunMode::Strict, RunMode::Discovery] {
        optional(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            "optional-interpreter",
            mode,
        );
        optional(JitCpu::new(PARK_ADDR, true).unwrap(), "optional-jit", mode);
        optional(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            "optional-unicorn",
            mode,
        );
    }
}

#[test]
fn guest_system_named_dll_missing_sdk_export_is_a_guest_missing_export() {
    let mut test = Test::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        "guest-proxy",
        RunMode::Strict,
    );
    let guest = test.load_image(
        "kernel32.dll",
        0x500000,
        &[export("Existing", Target::Code(vec![0xc3]))],
    );
    test.host("C:\\Windows\\system32\\kernel32.dll");
    let name = test.text("FlsAlloc");
    let probe = test.probe(I(guest), I(name));
    test.run();
    assert_eq!(test.value(&probe.0), 0);
    assert_eq!(test.value(&probe.1), 127);
    let records = &test
        .machine
        .ext
        .get::<ApiDiagnostics>()
        .dynamic_lookup_failures;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, DynamicLookupKind::MissingExport);
    assert_eq!(records[0].caller, probe.0.caller);
}

#[test]
fn a_live_guest_hook_on_an_unimplemented_sdk_export_still_resolves_without_a_failure_record() {
    for jit in [false, true] {
        let mut test = Test::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            &format!("inline-hook-{jit}"),
            RunMode::Strict,
        );
        let host = test.host("kernel32.dll");
        let id = adv_os::modules(&test.machine.ext).by_handle(host).unwrap();
        let ExportTarget::Rva(rva) =
            adv_os::modules(&test.machine.ext).get(id).exports_by_name["FlsAlloc"]
        else {
            panic!("host diagnostic entry")
        };
        test.machine
            .cpu
            .write(host + rva, &[0xb8, 42, 0, 0, 0, 0xc2, 4, 0])
            .unwrap();
        let name = test.text("FlsAlloc");
        let probe = test.probe(I(host), I(name));
        test.run();
        assert_eq!(test.value(&probe.0), host + rva);
        assert_eq!(test.value(&probe.1), 0x1234);
        assert_eq!(
            test.machine
                .ext
                .try_get::<ApiDiagnostics>()
                .map_or(0, |d| d.dynamic_lookup_failures.len()),
            0
        );
    }
}

#[test]
fn failed_forwarders_keep_requested_and_target_names_and_rollback_guest_dependencies() {
    let mut test = Test::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        "forwarders",
        RunMode::Strict,
    );
    let proxy = test.load_image(
        "proxy.dll",
        0x500000,
        &[
            export(
                "NeedSDK",
                Target::Forwarder("kernel32.FlsAlloc".into()),
            ),
            export(
                "NeedGuest",
                Target::Forwarder("owned.MissingFeature".into()),
            ),
            export("Cycle", Target::Forwarder("proxy.Cycle".into())),
        ],
    );
    test.image(
        "owned.dll",
        0x600000,
        &[export("Existing", Target::Code(vec![0xc3]))],
        false,
    );
    let mut probes = Vec::new();
    for name in ["NeedSDK", "NeedGuest", "Cycle"] {
        let text = test.text(name);
        probes.push(test.probe(I(proxy), I(text)));
    }
    test.run();
    for (result, error) in &probes {
        assert_eq!(test.value(result), 0);
        assert_eq!(test.value(error), 127);
    }
    let records = &test
        .machine
        .ext
        .get::<ApiDiagnostics>()
        .dynamic_lookup_failures;
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].kind, DynamicLookupKind::Unimplemented);
    assert_eq!(records[0].module, "proxy.dll");
    assert_eq!(records[0].name, "NeedSDK");
    assert_eq!(records[0].failure_module.as_deref(), Some("kernel32.dll"));
    assert_eq!(records[0].failure_name.as_deref(), Some("FlsAlloc"));
    assert_eq!(records[1].kind, DynamicLookupKind::MissingExport);
    assert_eq!(records[1].failure_module.as_deref(), Some("owned.dll"));
    assert_eq!(records[1].failure_name.as_deref(), Some("MissingFeature"));
    assert_eq!(records[2].kind, DynamicLookupKind::ResolutionFailed);
    assert!(records[2].reason.contains("forwarder cycle"));
    assert!(records[2].failure_module.is_none());
    for (record, probe) in records.iter().zip(&probes) {
        assert_eq!(record.caller, probe.0.caller);
    }
    assert!(
        adv_os::modules(&test.machine.ext)
            .by_name("owned.dll")
            .is_none()
    );
    assert!(
        adv_os::modules(&test.machine.ext)
            .get(adv_os::modules(&test.machine.ext).by_handle(proxy).unwrap())
            .lifetime
            .dependencies
            .is_empty()
    );
}

#[test]
fn failed_initializer_keeps_original_gpa_caller_and_existing_rollback_return_contract() {
    for jit in [false, true] {
        let mut test = Test::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            &format!("initializer-{jit}"),
            RunMode::Strict,
        );
        let proxy = test.load_image(
            "proxy.dll",
            0x500000,
            &[export("Entry", Target::Forwarder("reject.Entry".into()))],
        );
        let mut body = vec![0xff, 0x05]; // Count both PROCESS_ATTACH and rollback PROCESS_DETACH.
        body.extend_from_slice(&INIT_COUNT.to_le_bytes());
        body.extend_from_slice(&[0x31, 0xc0, 0xc2, 12, 0]);
        test.image(
            "reject.dll",
            0x600000,
            &[
                export("DllMain", Target::Code(body)),
                export("Entry", Target::Code(vec![0xc3])),
            ],
            true,
        );
        let name = test.text("Entry");
        let probe = test.probe(I(proxy), I(name));
        test.run();
        assert_eq!(test.value(&probe.0), 0);
        assert_eq!(test.value(&probe.1), 127);
        assert_eq!(test.machine.cpu.read_u32(INIT_COUNT).unwrap(), 2);
        let records = &test
            .machine
            .ext
            .get::<ApiDiagnostics>()
            .dynamic_lookup_failures;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, DynamicLookupKind::InitializationFailed);
        assert_eq!(records[0].module, "proxy.dll");
        assert_eq!(records[0].name, "Entry");
        assert_eq!(records[0].caller, probe.0.caller);
        assert_eq!(records[0].context, 0);
        assert!(records[0].failure_module.is_none());
        assert!(
            adv_os::modules(&test.machine.ext)
                .by_name("reject.dll")
                .is_none()
        );
    }
}
