//! Class factories and multi-interface activation reuse installed constructors and the registry.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CLASS: u32 = DATA + 0x1000;
const THREAD: u32 = CLASS + 16;
const UNKNOWN: u32 = THREAD + 16;
const FACTORY_IID: u32 = UNKNOWN + 16;
const BAD: u32 = FACTORY_IID + 16;
const FACTORY: u32 = BAD + 16;
const OBJECT: u32 = FACTORY + 16;
const ALIAS: u32 = OBJECT + 16;
const RESULTS: u32 = DATA + 0x1200;
const PATH: u32 = DATA + 0x2000;
const NAME: u32 = PATH + 256;
const GUID_TEXT: u32 = NAME + 256;
const KEY: u32 = GUID_TEXT + 256;
const PROGID_OUT: u32 = KEY + 16;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    L(u32),
}
use Arg::{I, L};
struct Fixture<B: Backend> {
    m: Machine<B>,
    code: Asm,
    expected: Vec<(String, u32)>,
}
impl<B: Backend> Fixture<B> {
    fn push(&mut self, args: &[Arg]) {
        for arg in args.iter().rev() {
            match arg {
                I(value) => {
                    self.code.push_imm(*value);
                }
                L(pointer) => {
                    self.code.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
    }
    fn call(&mut self, dll: &str, name: &str, args: &[Arg], value: u32) {
        let target = self.m.resolve_import(dll, name).unwrap();
        self.push(args);
        self.code
            .call(target)
            .mov_mem_eax(DATA + self.expected.len() as u32 * 4);
        self.expected.push((name.into(), value));
    }
    fn method(&mut self, object: u32, slot: u32, args: &[Arg], value: u32) {
        self.push(args);
        self.code.mov_eax_mem(object).push_eax().mov_eax_ptr_eax();
        self.code.bytes.extend([0xff, 0x50, (slot * 4) as u8]);
        self.code.mov_mem_eax(DATA + self.expected.len() as u32 * 4);
        self.expected.push((format!("COM slot {slot}"), value));
    }
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-com-activation-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    for (address, value) in [
        (CLASS, adv_win32::ole32::tsf::CLSID_THREAD),
        (THREAD, adv_win32::ole32::tsf::IID_THREAD),
        (UNKNOWN, adv_os::com::IUNKNOWN),
        (
            FACTORY_IID,
            [1, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46],
        ),
        (BAD, [0x55; 16]),
    ] {
        m.cpu.write(address, &value).unwrap();
    }
    for (i, iid) in [UNKNOWN, THREAD, BAD].into_iter().enumerate() {
        m.cpu.write_u32(RESULTS + i as u32 * 12, iid).unwrap();
    }
    for (address, text) in [
        (PATH, "Closure.Automation.1\\CLSID"),
        (NAME, "closure.automation.1"),
        (GUID_TEXT, "{529a9e6b-6587-4f23-ab9e-9c7d683e3c50}"),
    ] {
        m.cpu
            .write(
                address,
                &text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    let mut f = Fixture {
        m,
        code: Asm::new(CODE),
        expected: Vec::new(),
    };
    f.call("ole32.dll", "CoInitialize", &[I(0)], 0);
    f.call(
        "ole32.dll",
        "CoGetClassObject",
        &[I(CLASS), I(1), I(0), I(FACTORY_IID), I(FACTORY)],
        0,
    );
    f.method(FACTORY, 0, &[I(UNKNOWN), I(ALIAS)], 0);
    f.method(ALIAS, 2, &[], 1);
    f.method(FACTORY, 3, &[I(0), I(THREAD), I(OBJECT)], 0);
    f.method(FACTORY, 4, &[I(1)], 0);
    f.method(FACTORY, 4, &[I(0)], 0);
    f.call(
        "ole32.dll",
        "CoCreateInstanceEx",
        &[I(CLASS), I(0), I(1), I(0), I(3), I(RESULTS)],
        0x80012,
    );
    f.call(
        "ole32.dll",
        "CoGetClassObject",
        &[I(BAD), I(1), I(0), I(FACTORY_IID), I(DATA + 0x800)],
        0x80040154,
    );
    f.call(
        "ole32.dll",
        "CoGetClassObject",
        &[I(CLASS), I(1), I(0), I(BAD), I(DATA + 0x804)],
        0x80004002,
    );
    f.call(
        "advapi32.dll",
        "RegCreateKeyExW",
        &[
            I(0x80000000),
            I(PATH),
            I(0),
            I(0),
            I(0),
            I(0xf003f),
            I(0),
            I(KEY),
            I(0),
        ],
        0,
    );
    f.call(
        "advapi32.dll",
        "RegSetValueExW",
        &[L(KEY), I(0), I(0), I(1), I(GUID_TEXT), I(78)],
        0,
    );
    f.call("ole32.dll", "CLSIDFromProgID", &[I(NAME), I(PROGID_OUT)], 0);
    f.call("advapi32.dll", "RegCloseKey", &[L(KEY)], 0);
    // Preserve method-visible aliases, then release every acquired interface.
    for (pointer, slot) in [
        (RESULTS + 4, 2),
        (RESULTS + 16, 2),
        (OBJECT, 2),
        (FACTORY, 2),
    ] {
        // Manager caches share identity; its externally visible count follows the same owner.
        let value = if pointer == RESULTS + 4 {
            2
        } else if pointer == RESULTS + 16 {
            1
        } else {
            0
        };
        f.method(pointer, slot, &[], value);
    }
    f.call("ole32.dll", "CoUninitialize", &[], 0);
    f.code
        .mov_mem_imm(DATA + 0x808, 0)
        .mov_eax_mem(DATA + 0x808)
        .ret();
    f.m.cpu.write(CODE, &f.code.bytes).unwrap();
    f.m.spawn("COM activation closure", CODE, &[], 65536)
        .unwrap();
    let result = f.m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (name, value)) in f.expected.into_iter().enumerate() {
        assert_eq!(
            f.m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
            value,
            "{name} row {i}"
        );
    }
    assert_eq!(
        f.m.cpu.read_u32(FACTORY).unwrap(),
        f.m.cpu.read_u32(ALIAS).unwrap()
    );
    assert_eq!(
        f.m.cpu.read_u32(RESULTS + 4).unwrap(),
        f.m.cpu.read_u32(RESULTS + 16).unwrap()
    );
    for (offset, value) in [(8, 0), (20, 0), (28, 0), (32, 0x80004002)] {
        assert_eq!(
            f.m.cpu.read_u32(RESULTS + offset).unwrap(),
            value,
            "MULTI_QI {offset}"
        );
    }
    let mut class = [0; 16];
    f.m.cpu.read(PROGID_OUT, &mut class).unwrap();
    assert_eq!(class, adv_win32::ole32::tsf::CLSID_THREAD);
    assert_eq!(f.m.cpu.read_u32(DATA + 0x800).unwrap(), 0);
    assert_eq!(f.m.cpu.read_u32(DATA + 0x804).unwrap(), 0);
}
#[test]
// spec: SYS-COM-3
fn factory_multi_interface_activation_and_progid_share_real_owners_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
