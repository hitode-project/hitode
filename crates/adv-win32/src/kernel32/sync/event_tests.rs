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
        let (module, name) = name.split_once('!').unwrap_or((K32, name));
        let address = self.0.resolve_import(module, name).unwrap();
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
fn native_counted_names_share_event_identity_mode_inheritance_and_last_error_with_win32() {
    let mut f = Fixture::new();
    let name = f.name(0x100, "native-event", false);
    let native = f.name(0x200, r"\Sessions\0\BaseNamedObjects\native-event", true);
    let length = r"\Sessions\0\BaseNamedObjects\native-event"
        .encode_utf16()
        .count() as u16
        * 2;
    // A counted native name has no readable terminator requirement.
    f.0.cpu
        .write(native + u32::from(length), &0xeeeeu16.to_le_bytes())
        .unwrap();
    let string = DATA + 0x400;
    f.0.cpu
        .write(
            string,
            &[length.to_le_bytes(), length.to_le_bytes()].concat(),
        )
        .unwrap();
    f.0.cpu.write_u32(string + 4, native).unwrap();
    let attributes = DATA + 0x440;
    for (i, value) in [24, 0, string, 2, 0, 0].into_iter().enumerate() {
        f.0.cpu.write_u32(attributes + i as u32 * 4, value).unwrap();
    }
    let output = DATA + 0x80;
    assert_eq!(
        f.call(
            "ntdll.dll!NtCreateEvent",
            &[output, EVENT_ALL_ACCESS, attributes, 0, 1]
        ),
        0
    );
    let original = f.0.cpu.read_u32(output).unwrap();
    assert_ne!(original, 0);
    assert_eq!(f.0.sys.objects.handle_flags(original), Some(1));
    let alias = f.call("CreateEventA", &[0, 0, 0, name]);
    assert_ne!(alias, original);
    assert_eq!(f.error(), 183);
    assert!(f.0.sys.objects.same_object(original, alias));
    assert_eq!(f.wait(alias), WAIT_OBJECT_0);
    assert_eq!(
        f.wait(alias),
        WAIT_OBJECT_0,
        "the existing manual-reset mode wins"
    );
    f.call("SetLastError", &[0x12345678]);
    f.0.cpu.write_u32(attributes + 12, 0).unwrap();
    f.0.cpu.write_u32(output, 0xabcdef01).unwrap();
    assert_eq!(
        f.call(
            "ntdll.dll!NtCreateEvent",
            &[output, EVENT_ALL_ACCESS, attributes, 1, 0]
        ),
        0xc0000035
    );
    assert_eq!(f.0.cpu.read_u32(output).unwrap(), 0xabcdef01);
    f.0.cpu.write_u32(attributes + 12, 0x82).unwrap();
    assert_eq!(
        f.call(
            "ntdll.dll!NtCreateEvent",
            &[output, EVENT_ALL_ACCESS, attributes, 1, 0]
        ),
        0x40000000
    );
    let reopened = f.0.cpu.read_u32(output).unwrap();
    assert!(f.0.sys.objects.same_object(original, reopened));
    assert_eq!(f.0.sys.objects.handle_flags(reopened), Some(1));
    f.0.cpu.write_u32(attributes + 12, 2).unwrap();
    assert_eq!(
        f.call(
            "ntdll.dll!NtOpenEvent",
            &[output, EVENT_ALL_ACCESS, attributes]
        ),
        0
    );
    let opened = f.0.cpu.read_u32(output).unwrap();
    assert!(f.0.sys.objects.same_object(original, opened));
    let previous = DATA + 0x90;
    assert_eq!(
        f.call("ntdll.dll!NtResetEvent", &[opened, DATA + 0x2000]),
        0xc0000005
    );
    assert_eq!(
        f.wait(original),
        WAIT_OBJECT_0,
        "output faults do not change the signal"
    );
    assert_eq!(f.call("ntdll.dll!NtResetEvent", &[opened, previous]), 0);
    assert_eq!(f.0.cpu.read_u32(previous).unwrap(), 1);
    assert_eq!(f.wait(original), WAIT_TIMEOUT);
    assert_eq!(f.call("ntdll.dll!NtSetEvent", &[alias, previous]), 0);
    assert_eq!(f.0.cpu.read_u32(previous).unwrap(), 0);
    assert_eq!(f.wait(opened), WAIT_OBJECT_0);
    assert_eq!(
        f.error(),
        0x12345678,
        "NT entries leave LastError untouched"
    );
    for handle in [original, alias, reopened, opened] {
        assert_eq!(f.call("CloseHandle", &[handle]), 1);
    }
    assert_eq!(
        f.call(
            "ntdll.dll!NtOpenEvent",
            &[output, EVENT_ALL_ACCESS, attributes]
        ),
        0xc0000034
    );
}

#[test]
fn unsupported_native_event_attributes_and_output_faults_publish_no_handle_or_object() {
    let mut f = Fixture::new();
    let attributes = DATA + 0x400;
    let output = DATA + 0x80;
    let baseline = f.0.sys.objects.object_count();
    f.0.cpu.write_u32(output, 0xabcdef01).unwrap();
    f.call("SetLastError", &[0x12345678]);
    for (fields, desired, kind, status) in [
        ([24, 1, 0, 0, 0, 0], EVENT_ALL_ACCESS, 0, 0xc00000bb),
        ([24, 0, 0, 0, 1, 0], EVENT_ALL_ACCESS, 0, 0xc00000bb),
        ([24, 0, 0, 0x40, 0, 0], EVENT_ALL_ACCESS, 0, 0xc00000bb),
        ([24, 0, 0, 0, 0, 0], 0x100000, 0, 0xc00000bb),
        ([24, 0, 0, 0, 0, 0], EVENT_ALL_ACCESS, 2, 0xc000000d),
    ] {
        for (i, value) in fields.into_iter().enumerate() {
            f.0.cpu.write_u32(attributes + i as u32 * 4, value).unwrap();
        }
        assert_eq!(
            f.call(
                "ntdll.dll!NtCreateEvent",
                &[output, desired, attributes, kind, 0]
            ),
            status
        );
        assert_eq!(f.0.cpu.read_u32(output).unwrap(), 0xabcdef01);
        assert_eq!(f.0.sys.objects.object_count(), baseline);
    }
    assert_eq!(
        f.call(
            "ntdll.dll!NtCreateEvent",
            &[DATA + 0x2000, EVENT_ALL_ACCESS, 0, 0, 1]
        ),
        0xc0000005
    );
    assert_eq!(f.0.sys.objects.object_count(), baseline);
    assert_eq!(f.error(), 0x12345678);
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

#[test]
fn remaining_mutex_and_semaphore_fronts_report_modified_native_entries() {
    use adv_abi::{ApiDiagnostics, RunMode};
    let cases: &[(&str, &str, &[u32])] = &[
        ("CreateSemaphoreA", "NtCreateSemaphore", &[0, 0, 1, 0]),
        ("CreateSemaphoreW", "NtCreateSemaphore", &[0, 0, 1, 0]),
        ("ReleaseSemaphore", "NtReleaseSemaphore", &[0, 1, 0]),
        ("CreateMutexA", "NtCreateMutant", &[0, 0, 0]),
        ("CreateMutexW", "NtCreateMutant", &[0, 0, 0]),
        ("ReleaseMutex", "NtReleaseMutant", &[0]),
        ("OpenMutexA", "NtOpenMutant", &[0x1f0001, 0, 0]),
        ("OpenMutexW", "NtOpenMutant", &[0x1f0001, 0, 0]),
    ];
    for &(front, native, args) in cases {
        let mut f = Fixture::new();
        crate::Win32::prepare_process_infrastructure(&mut f.0).unwrap();
        f.call(front, args);
        assert!(
            f.0.ext
                .get_or_default::<ApiDiagnostics>()
                .acceptance_eligible(RunMode::Strict),
            "{front}"
        );
        let target = {
            let m = &mut f.0;
            let mut env = Env {
                context: 0,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            };
            crate::native::entry(&mut env, native).unwrap()
        };
        // Authored replacement bytes only test whether the host bypass is visible.
        f.0.cpu.write(target, &[0xcc]).unwrap();
        f.call(front, args);
        let diagnostics = f.0.ext.get::<ApiDiagnostics>();
        assert_eq!(diagnostics.structural_mismatches, 1, "{front}");
        assert!(!diagnostics.acceptance_eligible(RunMode::Strict), "{front}");
    }
}
