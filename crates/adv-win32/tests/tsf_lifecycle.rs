//! Authored guest TSF lifecycle: shared identities, class vtables and owners.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{
    Win32, Win32Config,
    ole32::tsf::{CLSID_THREAD, IID_THREAD},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const SHIM: u32 = CODE + 3072;
const CLASS: u32 = DATA;
const IID: u32 = DATA + 32;
const UNKNOWN: u32 = DATA + 64;
const MANAGER: u32 = DATA + 128;
const MANAGER_TWO: u32 = DATA + 132;
const DOC: u32 = DATA + 136;
const DOC_TWO: u32 = DATA + 140;
const TOP: u32 = DATA + 144;
const BASE: u32 = DATA + 148;
const CLIENT: u32 = DATA + 152;
const CLIENT_TWO: u32 = DATA + 156;
const CONTEXT: u32 = DATA + 160;
const COOKIE: u32 = DATA + 164;
const DOC_RETURNED: u32 = DATA + 168;
const QUERY: u32 = DATA + 172;
const BAD_CONTEXT: u32 = DATA + 176;
const BAD_COOKIE: u32 = DATA + 180;
const TABLE: u32 = DATA + 184;
const TABLE_TWO: u32 = DATA + 188;
const ORIGINAL: u32 = DATA + 192;
const HOOK_COUNT: u32 = DATA + 196;

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
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        adv_win32::compat::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        for (interface, slots) in [
            ("ITfThreadMgr", 14),
            ("ITfDocumentMgr", 9),
            ("ITfContext", 18),
        ] {
            let signatures: Vec<_> = adv_abi::signature::com_signatures()
                .iter()
                .filter(|signature| signature.module == interface)
                .collect();
            assert_eq!(signatures.len(), slots);
            for signature in signatures {
                let method = format!("{interface}::{}", signature.name);
                assert_eq!(
                    registry.get("msctf.dll", &method).unwrap().conv,
                    signature.conv.unwrap()
                );
            }
        }
        assert_eq!(
            registry
                .get("msctf.dll", "ITfDocumentMgr::CreateContext")
                .unwrap()
                .conv,
            Conv::Stdcall { argc: 6 }
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tsf-lifecycle-tests")
            .join(format!("{}-{backend}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(&root, "tsf.exe", root.join("overlay")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.write(CLASS, &CLSID_THREAD).unwrap();
        machine.cpu.write(IID, &IID_THREAD).unwrap();
        machine.cpu.write(UNKNOWN, &adv_os::com::IUNKNOWN).unwrap();
        // Increment the shared-vtable hook counter, then tail-call the original
        // CreateContext without changing its six-argument stdcall stack.
        let mut shim = vec![0xff, 0x05];
        shim.extend_from_slice(&HOOK_COUNT.to_le_bytes());
        shim.extend_from_slice(&[0xff, 0x25]);
        shim.extend_from_slice(&ORIGINAL.to_le_bytes());
        machine.cpu.write(SHIM, &shim).unwrap();
        machine.cpu.region(SHIM, shim.len());
        Self {
            machine,
            code: Asm::new(CODE),
            next: DATA + 2048,
        }
    }
    fn push(&mut self, args: &[Arg]) {
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
    }
    fn result(&mut self) -> u32 {
        let output = self.next;
        self.next += 4;
        self.code.mov_mem_eax(output);
        output
    }
    fn api(&mut self, name: &str, args: &[Arg]) -> u32 {
        let address = self.machine.resolve_import("ole32.dll", name).unwrap();
        self.push(args);
        self.code.call(address);
        self.result()
    }
    fn method(&mut self, pointer: u32, slot: u8, args: &[Arg]) -> u32 {
        self.push(args);
        self.code.mov_eax_mem(pointer).push_eax().mov_eax_ptr_eax();
        self.code.bytes.extend_from_slice(&[0xff, 0x50, slot * 4]);
        self.result()
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn run(&mut self) {
        assert!(self.code.bytes.len() < 3072);
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("TSF guest lifecycle", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(2_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
}
fn contract<B: TestCpu>(cpu: B, backend: &str) {
    let mut f = Fixture::new(cpu, backend);
    let owners = f.machine.sys.objects.object_count();
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 512);
    f.api("CoInitialize", &[V(0)]);
    let created = f.api(
        "CoCreateInstance",
        &[V(CLASS), V(0), V(1), V(IID), V(MANAGER)],
    );
    let created_again = f.api(
        "CoCreateInstance",
        &[V(CLASS), V(0), V(1), V(IID), V(MANAGER_TWO)],
    );
    let inactive = f.method(MANAGER, 4, &[]);
    let null_activate = f.method(MANAGER, 3, &[V(0)]);
    let null_document = f.method(MANAGER, 5, &[V(0)]);
    let document = f.method(MANAGER, 5, &[V(DOC)]);
    let top = f.method(DOC, 6, &[V(TOP)]);
    let base = f.method(DOC, 7, &[V(BASE)]);
    f.code.mov_eax_mem(TOP).mov_mem_eax(DATA + 524);
    let default_document = f.method(TOP, 16, &[V(DOC_RETURNED)]);
    f.method(DOC_RETURNED, 2, &[]);
    let active = f.method(MANAGER, 3, &[V(CLIENT)]);
    let active_again = f.method(MANAGER_TWO, 3, &[V(CLIENT_TWO)]);
    f.code
        .mov_mem_imm(BAD_CONTEXT, 0x12345678)
        .mov_mem_imm(BAD_COOKIE, 0xa5a5a5a5);
    let invalid_owner = f.method(DOC, 3, &[V(0), V(0), V(0), V(BAD_CONTEXT), V(BAD_COOKIE)]);
    // The same class table is visible in a later document, after the first
    // temporary document installed a hook on CreateContext.
    f.code.mov_eax_mem(DOC).mov_eax_ptr_eax().mov_mem_eax(TABLE);
    f.code
        .bytes
        .extend_from_slice(&[0x8b, 0x48, 0x0c, 0x89, 0xc8]); // original slot into eax
    f.code.mov_mem_eax(ORIGINAL).mov_eax_mem(TABLE);
    f.code.bytes.extend_from_slice(&[0xc7, 0x40, 0x0c]);
    f.code.bytes.extend_from_slice(&SHIM.to_le_bytes());
    let document_two = f.method(MANAGER, 5, &[V(DOC_TWO)]);
    f.code
        .mov_eax_mem(DOC_TWO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(TABLE_TWO);
    let context = f.method(DOC_TWO, 3, &[S(CLIENT), V(0), V(0), V(CONTEXT), V(COOKIE)]);
    let context_document = f.method(CONTEXT, 16, &[V(DOC_RETURNED)]);
    // Save which document the ephemeral context belongs to before reusing output.
    f.code.mov_eax_mem(DOC_RETURNED).mov_mem_eax(DATA + 516);
    f.method(DOC_RETURNED, 2, &[]);
    let unsupported_store = f.method(
        DOC,
        3,
        &[S(CLIENT), V(0), V(DATA), V(BAD_CONTEXT), V(BAD_COOKIE)],
    );
    let deactivated = f.method(MANAGER, 4, &[]);
    let deactivated_again = f.method(MANAGER_TWO, 4, &[]);
    let underflow = f.method(MANAGER, 4, &[]);
    let document_released = f.method(DOC, 2, &[]);
    let retired_document = f.method(TOP, 16, &[V(DOC_RETURNED)]);
    let live_unknown = f.method(TOP, 0, &[V(UNKNOWN), V(QUERY)]);
    f.method(QUERY, 2, &[]);
    f.method(TOP, 2, &[]);
    f.method(BASE, 2, &[]);
    let manager_release = f.method(MANAGER, 2, &[]);
    let manager_final_release = f.method(MANAGER_TWO, 2, &[]);
    let retired_manager = f.method(
        DOC_TWO,
        3,
        &[S(CLIENT), V(0), V(0), V(BAD_CONTEXT), V(BAD_COOKIE)],
    );
    let remaining_top = f.method(DOC_TWO, 6, &[V(TOP)]);
    f.method(TOP, 2, &[]);
    f.method(CONTEXT, 2, &[]);
    let final_document = f.method(DOC_TWO, 2, &[]);
    f.api("CoUninitialize", &[]);
    f.code.bytes.extend_from_slice(&[0x89, 0xe0]);
    f.code.mov_mem_eax(DATA + 520);
    f.run();
    for value in [
        created,
        created_again,
        document,
        top,
        base,
        default_document,
        active,
        active_again,
        document_two,
        context,
        context_document,
        deactivated,
        deactivated_again,
        live_unknown,
        remaining_top,
    ] {
        assert_eq!(f.value(value), 0, "{backend} HRESULT at {value:#x}");
    }
    for value in [inactive, underflow] {
        assert_eq!(f.value(value), 0x8000ffff);
    }
    for value in [null_activate, invalid_owner] {
        assert_eq!(f.value(value), 0x80070057);
    }
    assert_eq!(f.value(null_document), 0x80004003);
    for value in [unsupported_store, retired_document, retired_manager] {
        assert_eq!(f.value(value), 0x80004001);
    }
    assert_eq!(
        f.value(MANAGER),
        f.value(MANAGER_TWO),
        "same-thread controlling identity"
    );
    assert_eq!(f.value(CLIENT), f.value(CLIENT_TWO));
    assert_ne!(f.value(CLIENT), 0);
    assert_eq!(
        f.value(TABLE),
        f.value(TABLE_TWO),
        "document class vtable survives object lifetime"
    );
    assert_eq!(
        f.value(HOOK_COUNT),
        3,
        "a later document calls the installed class hook"
    );
    assert_eq!(f.value(DATA + 516), f.value(DOC_TWO));
    assert_eq!(
        f.value(DATA + 524),
        f.value(BASE),
        "default top and base have one identity"
    );
    assert_ne!(
        f.value(DATA + 524),
        f.value(CONTEXT),
        "CreateContext returns a separate identity"
    );
    assert_eq!(f.value(COOKIE), 0, "NULL text store exposes no edit cookie");
    assert_eq!(f.value(BAD_CONTEXT), 0);
    assert_eq!(f.value(BAD_COOKIE), 0);
    assert_eq!(
        f.value(DOC_RETURNED),
        0,
        "a retired parent is not republished"
    );
    assert_eq!(f.value(manager_release), 1);
    for value in [document_released, manager_final_release, final_document] {
        assert_eq!(f.value(value), 0);
    }
    assert_eq!(
        f.value(DATA + 512),
        f.value(DATA + 520),
        "stdcall virtual methods restore ESP"
    );
    assert_eq!(
        f.machine.sys.objects.object_count(),
        owners + 1,
        "only the fixture thread remains"
    );
}
#[test]
fn thread_document_context_ownership_and_class_hooks_survive_three_guest_backends() {
    contract(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    contract(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
    contract(JitCpu::new(PARK_ADDR, true).unwrap(), "interpreter");
}
