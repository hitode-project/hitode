//! Legacy IME requests borrow GlobalAlloc blocks and report the shared absent provider.
use adv_abi::{ApiDiagnostics, Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, Reg},
};
use adv_machine::{
    Clock, Env, Machine, Outcome,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_os::{Heaps, global::Globals};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const MARKER: u32 = 0x12345678;

struct Fixture<B: Backend> {
    machine: Machine<B>,
    stack: u32,
}

impl<B: Backend> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for name in ["SendIMEMessageExA", "SendIMEMessageExW"] {
            assert_eq!(
                registry.get("user32.dll", name).unwrap().conv,
                Conv::Stdcall { argc: 2 }
            );
            assert_eq!(
                registry.level("user32.dll", name),
                Some(ImplementationLevel::SafeFail)
            );
            assert!(registry.dynamic_available("user32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ime-legacy-tests");
        Win32::install(&mut machine, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
        machine.ext.get_or_default::<ApiDiagnostics>();
        machine
            .spawn("legacy IME caller", PARK_ADDR, &[], 65536)
            .unwrap();
        let stack = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .unwrap();
        machine.cpu.set_reg(Reg::Esp, stack);
        Self { machine, stack }
    }

    fn call(&mut self, api: &str, args: &[u32]) -> u32 {
        let (module, name) = api.split_once('!').unwrap_or(("kernel32.dll", api));
        let address = self.machine.resolve_import(module, name).unwrap();
        let thunk = (address - THUNK_BASE) / THUNK_STRIDE;
        for (index, value) in args.iter().enumerate() {
            self.machine
                .cpu
                .write_u32(self.stack + 4 + index as u32 * 4, *value)
                .unwrap();
        }
        let handler = self.machine.sys.thunks.get(thunk).f.handler;
        let mut env = Env {
            context: 0,
            cpu: &mut self.machine.cpu,
            sys: &mut self.machine.sys,
            ext: &mut self.machine.ext,
            thunk,
        };
        match handler(&mut env) {
            Outcome::Return(value) => value,
            _ => panic!("unexpected {api} outcome"),
        }
    }

    fn checked(&mut self, api: &str, args: &[u32], error: u32) {
        self.call("SetLastError", &[MARKER]);
        assert_eq!(self.call(api, args), 0);
        assert_eq!(self.call("GetLastError", &[]), error);
    }

    fn words(&self, pointer: u32) -> [u32; 8] {
        std::array::from_fn(|index| {
            self.machine
                .cpu
                .read_u32(pointer + index as u32 * 4)
                .unwrap()
        })
    }

    fn fill(&mut self, pointer: u32, words: [u32; 8]) {
        for (index, value) in words.into_iter().enumerate() {
            self.machine
                .cpu
                .write_u32(pointer + index as u32 * 4, value)
                .unwrap();
        }
    }
}

fn absent_provider<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    let window = t.machine.ext.get::<Win32>().user.desktop;
    // SDK ime.h and the public SendIMEMessageEx contract use a 32-byte x86
    // IMESTRUCT and IME_RS_NOIME in wParam. No nested payload is needed when
    // the provider is absent, including the IME_SETOPEN teardown request.
    // https://learn.microsoft.com/en-us/windows/win32/api/ime/nf-ime-sendimemessageexa
    for flags in [0x40, 0x42] {
        let handle = t.call("GlobalAlloc", &[flags, 32]);
        assert_ne!(handle, 0);
        let pointer = t.call("GlobalLock", &[handle]);
        assert_ne!(pointer, 0);
        assert_eq!(handle == pointer, flags == 0x40);
        let heap_owner = t.machine.ext.get::<Heaps>().owner(pointer);
        assert!(heap_owner.is_some());
        let before = t
            .machine
            .ext
            .get::<Globals>()
            .get(&t.machine.sys.objects, handle)
            .unwrap()
            .clone();
        for function in [4, 5, 0x19, 0xdeadbeef] {
            for api in [
                "user32.dll!SendIMEMessageExA",
                "user32.dll!SendIMEMessageExW",
            ] {
                let mut words = [function, 0, 17, 19, 23, u32::MAX, u32::MAX - 7, 0];
                t.fill(pointer, words);
                t.checked(api, &[window, handle], MARKER);
                words[1] = 2;
                assert_eq!(t.words(pointer), words);
                let after = t
                    .machine
                    .ext
                    .get::<Globals>()
                    .get(&t.machine.sys.objects, handle)
                    .unwrap();
                assert_eq!(
                    (after.pointer, after.size, after.locks, after.clipboard),
                    (before.pointer, before.size, before.locks, before.clipboard)
                );
                assert_eq!(t.machine.ext.get::<Heaps>().owner(pointer), heap_owner);
            }
        }
        assert_eq!(t.call("GlobalSize", &[handle]), 32);
        assert_eq!(t.call("GlobalFlags", &[handle]), u32::from(before.locks));
        t.call("GlobalUnlock", &[handle]);
        assert_eq!(t.call("GlobalFree", &[handle]), 0);
        assert!(
            t.machine
                .ext
                .get::<Globals>()
                .get(&t.machine.sys.objects, handle)
                .is_none()
        );
    }
    assert_eq!(t.machine.ext.get::<Win32>().user.windows.len(), 1);
    assert_eq!(t.machine.sys.contexts.len(), 1);
    let diagnostics = t.machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    for api in [
        "user32.dll!SendIMEMessageExA",
        "user32.dll!SendIMEMessageExW",
    ] {
        assert!(diagnostics.warnings.iter().any(|((name, reason), count)| {
            name == api
                && reason.contains("provider")
                && reason.contains("subfunction 0x4")
                && *count == 2
        }));
    }
}

#[test]
fn legacy_ime_failures_write_noime_without_consuming_global_memory_or_contexts() {
    absent_provider(UnicornCpu::new(PARK_ADDR).unwrap());
    absent_provider(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn malformed<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    let window = t.machine.ext.get::<Win32>().user.desktop;
    let event = t.call("CreateEventA", &[0, 1, 0, 0]);
    for api in [
        "user32.dll!SendIMEMessageExA",
        "user32.dll!SendIMEMessageExW",
    ] {
        for invalid in [0, 0x700000, u32::MAX, event] {
            t.checked(api, &[window, invalid], 6);
        }
    }
    assert_eq!(t.call("CloseHandle", &[event]), 1);
    let discarded = t.call("GlobalAlloc", &[2, 0]);
    t.checked("user32.dll!SendIMEMessageExA", &[window, discarded], 157);
    assert_eq!(t.call("GlobalFree", &[discarded]), 0);
    let short = t.call("GlobalAlloc", &[0x42, 16]);
    let short_pointer = t.call("GlobalLock", &[short]);
    t.machine.cpu.write(short_pointer, &[0x77; 16]).unwrap();
    t.checked("user32.dll!SendIMEMessageExW", &[window, short], 87);
    let mut bytes = [0; 16];
    t.machine.cpu.read(short_pointer, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 16]);
    assert_eq!(t.call("GlobalFree", &[short]), 0);
    let handle = t.call("GlobalAlloc", &[0x42, 32]);
    let pointer = t.call("GlobalLock", &[handle]);
    let original = [4, 0, 17, 19, 23, u32::MAX, u32::MAX - 7, 0];
    t.fill(pointer, original);
    // A movable allocation's locked data pointer is not its HGLOBAL.
    t.checked("user32.dll!SendIMEMessageExA", &[window, pointer], 6);
    assert_eq!(t.words(pointer), original);
    let page = pointer & !4095;
    t.machine.cpu.protect(page, 4096, Prot::READ).unwrap();
    for api in [
        "user32.dll!SendIMEMessageExA",
        "user32.dll!SendIMEMessageExW",
    ] {
        t.checked(api, &[window, handle], 998);
        assert_eq!(t.words(pointer), original);
    }
    t.machine.cpu.protect(page, 4096, Prot::RW).unwrap();
    assert_eq!(t.call("GlobalFree", &[handle]), 0);
    assert_eq!(t.call("imm32.dll!ImmGetContext", &[window]), 0);
}

#[test]
fn legacy_ime_validates_global_handles_sizes_and_permissions_before_writing_errors() {
    malformed(UnicornCpu::new(PARK_ADDR).unwrap());
    malformed(JitCpu::new(PARK_ADDR, false).unwrap());
}
