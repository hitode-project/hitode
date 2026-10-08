//! Named event aliases share the controller's existing signal and lifetime owner.
use super::*;
use adv_abi::{Conv, ImplementationLevel};
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const DATA: u32 = 0x410000;
const EVENT_ALL_ACCESS: u32 = 0x001f0003;

struct Fixture(Machine<JitCpu>);
impl Fixture {
    fn new() -> Self {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        for name in ["OpenEventA", "OpenEventW"] {
            assert_eq!(
                registry.get(K32, name).unwrap().conv,
                Conv::Stdcall { argc: 3 }
            );
            assert_eq!(
                registry.level(K32, name),
                Some(ImplementationLevel::Implement)
            );
            assert!(registry.dynamic_available(K32, name));
        }
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/open-event-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "authored.exe", &root),
        )
        .unwrap();
        machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
        machine
            .spawn("event caller", PARK_ADDR, &[], 65536)
            .unwrap();
        Self(machine)
    }
    fn name(&mut self, offset: u32, name: &str, wide: bool) -> u32 {
        let bytes = if wide {
            name.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        } else {
            [name.as_bytes(), &[0]].concat()
        };
        self.0.cpu.write(DATA + offset, &bytes).unwrap();
        DATA + offset
    }
    fn call(&mut self, name: &str, args: &[u32]) -> u32 {
        let address = self.0.resolve_import(K32, name).unwrap();
        let thunk = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let handler = self.0.sys.thunks.get(thunk).f.handler;
        self.0.cpu.set_reg(Reg::Esp, DATA + 0x1000);
        for (index, &arg) in args.iter().enumerate() {
            self.0
                .cpu
                .write_u32(DATA + 0x1004 + index as u32 * 4, arg)
                .unwrap();
        }
        let Outcome::Return(value) = handler(&mut Env {
            context: self.0.sys.current,
            cpu: &mut self.0.cpu,
            sys: &mut self.0.sys,
            ext: &mut self.0.ext,
            thunk,
        }) else {
            panic!("event API must return immediately");
        };
        value
    }
    fn error(&mut self) -> u32 {
        self.call("GetLastError", &[])
    }
    fn wait(&mut self, handle: u32) -> u32 {
        self.call("WaitForSingleObject", &[handle, 0])
    }
}

#[test]
fn open_event_aw_aliases_share_reset_mode_signal_lifetime_and_handle_inheritance() {
    for manual in [false, true] {
        let mut f = Fixture::new();
        let name = f.name(0x100, "authored-event", false);
        let wide = f.name(0x200, "Local\\authored-event", true);
        let baseline = f.0.sys.objects.object_count();
        let original = f.call("CreateEventA", &[0, u32::from(manual), 0, name]);
        assert_ne!(original, 0);
        let ansi = f.call("OpenEventA", &[EVENT_ALL_ACCESS, 0, name]);
        let unicode = f.call("OpenEventW", &[0x10000000, 1, wide]);
        assert_ne!(ansi, 0);
        assert_ne!(unicode, 0);
        assert_ne!(ansi, original);
        assert_ne!(unicode, ansi);
        assert!(f.0.sys.objects.same_object(original, ansi));
        assert!(f.0.sys.objects.same_object(original, unicode));
        assert_eq!(f.0.sys.objects.handle_flags(original), Some(0));
        assert_eq!(f.0.sys.objects.handle_flags(ansi), Some(0));
        assert_eq!(f.0.sys.objects.handle_flags(unicode), Some(1));
        assert_eq!(f.wait(unicode), WAIT_TIMEOUT);
        assert_eq!(f.call("SetEvent", &[ansi]), 1);
        assert_eq!(f.wait(unicode), WAIT_OBJECT_0);
        assert_eq!(
            f.wait(original),
            if manual { WAIT_OBJECT_0 } else { WAIT_TIMEOUT }
        );
        assert_eq!(f.call("ResetEvent", &[unicode]), 1);
        assert_eq!(f.wait(original), WAIT_TIMEOUT);
        assert_eq!(f.call("CloseHandle", &[original]), 1);
        let reopened = f.call("OpenEventA", &[EVENT_ALL_ACCESS, 0, name]);
        assert_ne!(reopened, 0);
        assert!(f.0.sys.objects.same_object(unicode, reopened));
        assert_eq!(f.call("SetEvent", &[reopened]), 1);
        assert_eq!(f.wait(ansi), WAIT_OBJECT_0);
        for handle in [ansi, unicode, reopened] {
            assert_eq!(f.call("CloseHandle", &[handle]), 1);
        }
        assert_eq!(f.call("OpenEventA", &[EVENT_ALL_ACCESS, 0, name]), 0);
        assert_eq!(f.error(), err::FILE_NOT_FOUND);
        assert_eq!(f.0.sys.objects.object_count(), baseline);
    }
}

#[test]
fn open_event_uses_case_sensitive_shared_namespace_and_rejects_wrong_types_or_access() {
    let mut f = Fixture::new();
    let name = f.name(0x100, "event-name", false);
    let wrong_case = f.name(0x200, "Event-name", false);
    let global = f.name(0x300, "Global\\event-name", true);
    let mutex_name = f.name(0x400, "mutex-name", true);
    assert_eq!(f.call("OpenEventA", &[EVENT_ALL_ACCESS, 0, name]), 0);
    assert_eq!(f.error(), err::FILE_NOT_FOUND);
    let event = f.call("CreateEventA", &[0, 1, 1, name]);
    let mutex = f.call("CreateMutexW", &[0, 0, mutex_name]);
    assert_ne!(event, 0);
    assert_ne!(mutex, 0);
    let objects = f.0.sys.objects.object_count();
    let handles = f.0.sys.objects.alias_count(adv_machine::Namespace::DEFAULT);
    for (api, pointer, error) in [
        ("OpenEventA", wrong_case, err::FILE_NOT_FOUND),
        ("OpenEventW", global, err::FILE_NOT_FOUND),
        ("OpenEventW", mutex_name, err::INVALID_HANDLE),
    ] {
        assert_eq!(f.call(api, &[EVENT_ALL_ACCESS, 0, pointer]), 0);
        assert_eq!(f.error(), error);
    }
    assert_eq!(f.call("OpenEventA", &[0x00100000, 1, name]), 0);
    assert_eq!(f.error(), 50);
    assert_eq!(f.0.sys.objects.object_count(), objects);
    assert_eq!(
        f.0.sys.objects.alias_count(adv_machine::Namespace::DEFAULT),
        handles
    );
    assert_eq!(
        f.wait(event),
        WAIT_OBJECT_0,
        "failed opens preserve the signal"
    );
    assert_eq!(f.0.sys.objects.handle_flags(event), Some(0));
    assert_eq!(f.call("CloseHandle", &[event]), 1);
    assert_eq!(f.call("CloseHandle", &[mutex]), 1);
}
