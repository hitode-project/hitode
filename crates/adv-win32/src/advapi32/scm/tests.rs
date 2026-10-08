//! Authored calls through the registered x86 ABI and owned fixture catalogs.
use super::*;
use adv_abi::{ApiDiagnostics, ImplementationLevel};
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_os::scm::{Catalog, Configuration, SERVICE_READ, Service, Status};
use adv_unicorn::UnicornCpu;
use std::sync::Arc;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x450000;
const MARKER: u32 = 0x13572468;
#[derive(Clone, Copy)]
enum Arg {
    Imm(u32),
    Load(u32),
}
use Arg::{Imm as I, Load as L};

struct Fixture<B: Backend> {
    machine: Machine<B>,
    code: Asm,
    next: u32,
}
impl<B: Backend> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        for (name, count) in [
            ("OpenSCManagerA", 3),
            ("OpenSCManagerW", 3),
            ("OpenServiceA", 3),
            ("OpenServiceW", 3),
            ("EnumServicesStatusExA", 10),
            ("EnumServicesStatusExW", 10),
            ("QueryServiceConfigA", 4),
            ("QueryServiceConfigW", 4),
            ("CloseServiceHandle", 1),
        ] {
            assert_eq!(
                registry.level("advapi32.dll", name),
                Some(ImplementationLevel::Implement)
            );
            assert_eq!(
                registry.get("advapi32.dll", name).unwrap().conv,
                Conv::Stdcall { argc: count }
            );
            assert!(registry.dynamic_available("advapi32.dll", name));
        }
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        // This fixture only queries in-memory SCM data; no guest file is opened.
        let mut config =
            crate::Win32Config::new("/nonexistent/scm", "fixture.exe", "/nonexistent/scm-data");
        config.os.codepage = 936;
        crate::Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x40000, Prot::RW).unwrap();
        machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write(READ_ONLY, &[0xa5; 4096]).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: DATA,
        }
    }
    fn blob(&mut self, bytes: &[u8]) -> u32 {
        let at = self.next;
        self.next += (bytes.len().max(4) as u32 + 3) & !3;
        assert!(self.next < DATA + 0x40000);
        self.machine.cpu.write(at, bytes).unwrap();
        at
    }
    fn word(&mut self, value: u32) -> u32 {
        self.blob(&value.to_le_bytes())
    }
    fn w(&mut self, text: &str) -> u32 {
        self.blob(
            &text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
    }
    fn a(&mut self, text: &str) -> u32 {
        let mut bytes = self.machine.ext.get::<Win32>().codec.encode(text);
        bytes.push(0);
        self.blob(&bytes)
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        self.call_in("advapi32.dll", name, args)
    }
    fn call_in(&mut self, module: &str, name: &str, args: &[Arg]) -> u32 {
        let target = self.machine.resolve_import(module, name).unwrap();
        let set_error = self
            .machine
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap();
        let get_error = self
            .machine
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap();
        let result = self.blob(&[0xcc; 16]);
        self.code
            .push_imm(MARKER)
            .call(set_error)
            .bytes
            .extend_from_slice(&[0x89, 0xe0]);
        self.code.mov_mem_eax(result + 8);
        for &arg in args.iter().rev() {
            match arg {
                I(value) => {
                    self.code.push_imm(value);
                }
                L(pointer) => {
                    self.code.mov_eax_mem(pointer).push_eax();
                }
            }
        }
        self.code
            .call(target)
            .mov_mem_eax(result)
            .call(get_error)
            .mov_mem_eax(result + 4)
            .bytes
            .extend_from_slice(&[0x89, 0xe0]);
        self.code.mov_mem_eax(result + 12);
        result
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.spawn("owned SCM", CODE, &[], 65536).unwrap();
        let result = self.machine.run_virtual(3_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        if let Some(diagnostics) = self.machine.ext.try_get::<ApiDiagnostics>() {
            assert_eq!(diagnostics.stub_calls, 0);
            assert!(diagnostics.gaps.is_empty());
        }
    }
    fn value(&self, at: u32) -> u32 {
        self.machine.cpu.read_u32(at).unwrap()
    }
    fn expect(&self, result: u32, value: u32, error: u32) {
        assert_eq!(self.value(result), value, "result at {result:#x}");
        assert_eq!(self.value(result + 4), error, "LastError at {result:#x}");
        assert_eq!(
            self.value(result + 8),
            self.value(result + 12),
            "stdcall cleanup at {result:#x}"
        );
    }
    fn read_w(&self, pointer: u32) -> String {
        String::from_utf16(
            &WideStr(GuestPtr::new(pointer))
                .read(&self.machine.cpu, 4096)
                .unwrap(),
        )
        .unwrap()
    }
    fn read_a(&self, pointer: u32) -> Vec<u8> {
        AnsiStr(GuestPtr::new(pointer))
            .read(&self.machine.cpu, 4096)
            .unwrap()
    }
}
fn owned(name: &str, kind: u32, state: u32, group: &str) -> Service {
    Service {
        name: name.into(),
        allowed_access: SERVICE_READ,
        status: Status {
            service_type: kind,
            current_state: state,
            controls_accepted: 0,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint: 0,
            wait_hint: 0,
            process_id: 0,
            service_flags: 0,
        },
        configuration: Configuration {
            service_type: kind,
            start_type: 3,
            error_control: 1,
            binary_path: "C:\\Fixture\\owned.exe".into(),
            load_order_group: group.into(),
            tag_id: 42,
            dependencies: vec!["DepOne".into(), "+OwnedGroup".into()],
            account_name: "LocalSystem".into(),
            display_name: format!("{name} 服务"),
        },
    }
}

fn empty_and_native_boundaries<B: Backend>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let missing = f.w("OwnedMissing");
    let empty = f.w("");
    let active = f.w("ServicesActive");
    let dot = f.w(".");
    let remote = f.w("remote.invalid");
    let manager = f.call("OpenSCManagerW", &[I(dot), I(active), I(4)]);
    let connect = f.call("OpenSCManagerA", &[I(0), I(0), I(0)]);
    let read = f.call("OpenSCManagerW", &[I(0), I(0), I(0x80000000)]);
    let bad_database = f.call("OpenSCManagerW", &[I(0), I(empty), I(4)]);
    let denied = f.call("OpenSCManagerW", &[I(0), I(0), I(0x40)]);
    let write = f.call("OpenSCManagerA", &[I(0), I(0), I(2)]);
    let unsupported = f.call("OpenSCManagerW", &[I(remote), I(0), I(4)]);
    let needed = f.word(MARKER);
    let count = f.word(MARKER);
    let resume = f.word(0);
    let no_rows = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(0),
            I(0),
            I(needed),
            I(count),
            I(resume),
            I(0),
        ],
    );
    let needed_group = f.word(MARKER);
    let count_group = f.word(MARKER);
    let group = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(0),
            I(0),
            I(needed_group),
            I(count_group),
            I(0),
            I(missing),
        ],
    );
    let mut errors = Vec::new();
    for (handle, level, types, state, expected, zero_outputs) in [
        (L(manager), 1, 0x30, 3, 124, false),
        (I(0), 0, 0x30, 3, 6, false),
        (L(manager), 0, 0, 3, 87, true),
        (L(manager), 0, 0x30, 0, 87, true),
        (L(connect), 0, 0x30, 3, 5, true),
    ] {
        let n = f.word(MARKER);
        let c = f.word(MARKER);
        let r = f.call(
            "EnumServicesStatusExA",
            &[
                handle,
                I(level),
                I(types),
                I(state),
                I(0),
                I(0),
                I(n),
                I(c),
                I(0),
                I(0),
            ],
        );
        errors.push((r, expected, n, c, zero_outputs));
    }
    let n_null = f.word(MARKER);
    let required_null = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(0),
            I(0),
            I(n_null),
            I(0),
            I(0),
            I(0),
        ],
    );
    let absent = f.call("OpenServiceW", &[L(connect), I(missing), I(1)]);
    let empty_name = f.call("OpenServiceW", &[L(manager), I(empty), I(1)]);
    let null_name = f.call("OpenServiceW", &[L(manager), I(0), I(1)]);
    let config_needed = f.word(MARKER);
    let wrong_kind = f.call(
        "QueryServiceConfigW",
        &[L(manager), I(0), I(0), I(config_needed)],
    );
    let close_kernel = f.call_in("kernel32.dll", "CloseHandle", &[L(manager)]);
    let wait_kernel = f.call_in("kernel32.dll", "WaitForSingleObject", &[L(manager), I(0)]);
    let duplicate_out = f.word(MARKER);
    let duplicate = f.call_in(
        "kernel32.dll",
        "DuplicateHandle",
        &[
            I(u32::MAX),
            L(manager),
            I(u32::MAX),
            I(duplicate_out),
            I(0),
            I(0),
            I(2),
        ],
    );
    let closed = f.call("CloseServiceHandle", &[L(manager)]);
    let closed_twice = f.call("CloseServiceHandle", &[L(manager)]);
    let closed_connect = f.call("CloseServiceHandle", &[L(connect)]);
    let closed_read = f.call("CloseServiceHandle", &[L(read)]);
    f.run();
    for result in [manager, connect, read] {
        assert_ne!(f.value(result), 0);
        assert_eq!(f.value(result + 4), 0);
    }
    f.expect(bad_database, 0, 123);
    f.expect(denied, 0, 5);
    f.expect(write, 0, 5);
    f.expect(unsupported, 0, 120);
    f.expect(no_rows, 1, MARKER);
    assert_eq!(
        (f.value(needed), f.value(count), f.value(resume)),
        (0, 0, 0)
    );
    f.expect(group, 0, 1060);
    assert_eq!((f.value(needed_group), f.value(count_group)), (0, 0));
    for (r, e, n, c, cleared) in errors {
        f.expect(r, 0, e);
        assert_eq!(
            (f.value(n), f.value(c)),
            if cleared { (0, 0) } else { (MARKER, MARKER) }
        );
    }
    f.expect(required_null, 0, 487);
    assert_eq!(f.value(n_null), MARKER);
    f.expect(absent, 0, 1060);
    f.expect(empty_name, 0, 123);
    f.expect(null_name, 0, 487);
    f.expect(wrong_kind, 0, 6);
    assert_eq!(f.value(config_needed), MARKER);
    f.expect(close_kernel, 0, 6);
    f.expect(wait_kernel, u32::MAX, 6);
    f.expect(duplicate, 0, 6);
    for result in [closed, closed_connect, closed_read] {
        f.expect(result, 1, MARKER);
    }
    f.expect(closed_twice, 0, 6);
    assert_eq!(
        f.machine
            .ext
            .get::<Scm>()
            .handle_count(&f.machine.sys.objects),
        0
    );
}

fn populated_catalog<B: Backend>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let services = [
        owned("Alpha", 0x10, 1, "OwnedGroup"),
        owned("Beta", 1, 4, "OwnedGroup"),
        owned("Zulu", 0x10, 1, ""),
    ];
    let alpha = services[0].clone();
    f.machine.ext.insert(Scm::with_catalog(Arc::new(
        Catalog::from_services(services.to_vec()).unwrap(),
    )));
    let manager = f.call("OpenSCManagerW", &[I(0), I(0), I(4)]);
    let group = f.w("ownedGROUP");
    let blank = f.w("");
    let total_needed = f.word(MARKER);
    let total_count = f.word(MARKER);
    let resume = f.word(0);
    let probe = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(0),
            I(0),
            I(total_needed),
            I(total_count),
            I(resume),
            I(0),
        ],
    );
    let alpha_size = 44
        + (alpha.name.encode_utf16().count()
            + alpha.configuration.display_name.encode_utf16().count()
            + 2)
            * 2;
    let first_buffer = f.blob(&vec![0x5a; alpha_size + 8]);
    let first_needed = f.word(MARKER);
    let first_count = f.word(MARKER);
    let first = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(first_buffer),
            I(alpha_size as u32),
            I(first_needed),
            I(first_count),
            I(resume),
            I(0),
        ],
    );
    let second_buffer = f.blob(&[0x5a; 512]);
    let second_needed = f.word(MARKER);
    let second_count = f.word(MARKER);
    let second = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(second_buffer),
            I(512),
            I(second_needed),
            I(second_count),
            I(resume),
            I(0),
        ],
    );
    let driver_buffer = f.blob(&[0x5a; 512]);
    let driver_needed = f.word(MARKER);
    let driver_count = f.word(MARKER);
    let driver = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(1),
            I(1),
            I(driver_buffer),
            I(512),
            I(driver_needed),
            I(driver_count),
            I(0),
            I(group),
        ],
    );
    let a_buffer = f.blob(&[0x5a; 512]);
    let a_needed = f.word(MARKER);
    let a_count = f.word(MARKER);
    let ansi = f.call(
        "EnumServicesStatusExA",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(2),
            I(a_buffer),
            I(512),
            I(a_needed),
            I(a_count),
            I(0),
            I(blank),
        ],
    );
    let service_name = f.a("aLpHa");
    let service = f.call("OpenServiceA", &[L(manager), I(service_name), I(1)]);
    let denied_service = f.call("OpenServiceA", &[L(manager), I(service_name), I(2)]);
    let config_needed = f.word(MARKER);
    let config_probe = f.call(
        "QueryServiceConfigW",
        &[L(service), I(0), I(0), I(config_needed)],
    );
    let short_buffer = f.blob(&[0x5a; 512]);
    let short_needed = f.word(MARKER);
    let short = f.call(
        "QueryServiceConfigW",
        &[L(service), I(short_buffer), I(35), I(short_needed)],
    );
    let closed_manager = f.call("CloseServiceHandle", &[L(manager)]);
    let config_buffer = f.blob(&[0x5a; 1024]);
    let success_needed = f.word(MARKER);
    let config = f.call(
        "QueryServiceConfigW",
        &[L(service), I(config_buffer), I(1024), I(success_needed)],
    );
    let config_a_buffer = f.blob(&[0x5a; 1024]);
    let config_a_needed = f.word(MARKER);
    let config_ansi = f.call(
        "QueryServiceConfigA",
        &[L(service), I(config_a_buffer), I(1024), I(config_a_needed)],
    );
    let closed_service = f.call("CloseServiceHandle", &[L(service)]);
    let invalid_after = f.call(
        "QueryServiceConfigW",
        &[L(service), I(0), I(0), I(config_needed)],
    );
    f.run();
    f.expect(probe, 0, 234);
    assert_eq!(f.value(total_count), 0);
    f.expect(first, 0, 234);
    assert_eq!(f.value(first_count), 1);
    assert_eq!(f.read_w(f.value(first_buffer)), "Alpha");
    assert_eq!(f.value(first_buffer + alpha_size as u32), 0x5a5a5a5a);
    f.expect(second, 1, MARKER);
    assert_eq!(
        (
            f.value(second_count),
            f.value(second_needed),
            f.value(resume)
        ),
        (1, 0, 0)
    );
    assert_eq!(f.read_w(f.value(second_buffer)), "Zulu");
    assert_eq!(
        f.value(total_needed),
        alpha_size as u32 + f.value(first_needed)
    );
    f.expect(driver, 1, MARKER);
    assert_eq!(f.value(driver_count), 1);
    assert_eq!(f.read_w(f.value(driver_buffer)), "Beta");
    for (word, expected) in services[1].status.words().into_iter().enumerate() {
        assert_eq!(f.value(driver_buffer + 8 + word as u32 * 4), expected);
    }
    f.expect(ansi, 1, MARKER);
    assert_eq!(f.value(a_count), 1);
    assert_eq!(f.read_a(f.value(a_buffer)), b"Zulu");
    assert_eq!(
        f.read_a(f.value(a_buffer + 4)),
        f.machine
            .ext
            .get::<Win32>()
            .codec
            .encode(&services[2].configuration.display_name)
    );
    assert_ne!(f.value(service), 0);
    f.expect(denied_service, 0, 5);
    f.expect(config_probe, 0, 122);
    f.expect(short, 0, 122);
    assert_eq!(f.value(short_needed), f.value(config_needed));
    assert_eq!(f.value(short_buffer), 0x5a5a5a5a);
    f.expect(closed_manager, 1, MARKER);
    f.expect(config, 1, MARKER);
    assert_eq!(f.value(success_needed), f.value(config_needed));
    for (offset, expected) in [
        (12, &alpha.configuration.binary_path),
        (16, &alpha.configuration.load_order_group),
        (28, &alpha.configuration.account_name),
        (32, &alpha.configuration.display_name),
    ] {
        let pointer = f.value(config_buffer + offset);
        assert!((config_buffer + 36..config_buffer + f.value(success_needed)).contains(&pointer));
        assert_eq!(&f.read_w(pointer), expected);
    }
    let dependencies = f.value(config_buffer + 24);
    let mut expected = alpha
        .configuration
        .dependencies
        .iter()
        .flat_map(|s| s.encode_utf16().chain([0]))
        .collect::<Vec<_>>();
    expected.push(0);
    for (i, expected) in expected.iter().enumerate() {
        assert_eq!(
            f.machine.cpu.read_u16(dependencies + i as u32 * 2).unwrap(),
            *expected
        );
    }
    assert_eq!(
        (
            f.value(config_buffer),
            f.value(config_buffer + 4),
            f.value(config_buffer + 8),
            f.value(config_buffer + 20)
        ),
        (0x10, 3, 1, 42)
    );
    f.expect(config_ansi, 1, MARKER);
    assert_eq!(
        f.read_a(f.value(config_a_buffer + 32)),
        f.machine
            .ext
            .get::<Win32>()
            .codec
            .encode(&alpha.configuration.display_name)
    );
    f.expect(closed_service, 1, MARKER);
    f.expect(invalid_after, 0, 6);
    assert_eq!(
        f.machine
            .ext
            .get::<Scm>()
            .handle_count(&f.machine.sys.objects),
        0
    );
}

fn checked_publication<B: Backend>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.machine.ext.insert(Scm::with_catalog(Arc::new(
        Catalog::from_services(vec![owned("Owned", 0x10, 1, "")]).unwrap(),
    )));
    let manager = f.call("OpenSCManagerW", &[I(0), I(0), I(4)]);
    let name = f.w("Owned");
    let service = f.call("OpenServiceW", &[L(manager), I(name), I(1)]);
    let limited = f.call("OpenServiceW", &[L(manager), I(name), I(0)]);
    let needed = f.word(MARKER);
    let count = f.word(MARKER);
    let resume = f.word(0);
    let enum_fault = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(READ_ONLY),
            I(4096),
            I(needed),
            I(count),
            I(resume),
            I(0),
        ],
    );
    let bad_needed_count = f.word(MARKER);
    let bad_needed = f.call(
        "EnumServicesStatusExW",
        &[
            L(manager),
            I(0),
            I(0x30),
            I(3),
            I(0),
            I(0),
            I(READ_ONLY),
            I(bad_needed_count),
            I(0),
            I(0),
        ],
    );
    let config_needed = f.word(MARKER);
    let config_fault = f.call(
        "QueryServiceConfigW",
        &[L(service), I(READ_ONLY), I(4096), I(config_needed)],
    );
    let size_only_needed = f.word(MARKER);
    let size_only = f.call(
        "QueryServiceConfigW",
        &[L(service), I(u32::MAX - 3), I(0), I(size_only_needed)],
    );
    let denied = f.call(
        "QueryServiceConfigW",
        &[L(limited), I(0), I(0), I(config_needed)],
    );
    let close_manager = f.call("CloseServiceHandle", &[L(manager)]);
    let close_service = f.call("CloseServiceHandle", &[L(service)]);
    let close_limited = f.call("CloseServiceHandle", &[L(limited)]);
    f.run();
    f.expect(enum_fault, 0, 998);
    assert_eq!(
        (f.value(needed), f.value(count), f.value(resume)),
        (0, 0, 0)
    );
    f.expect(bad_needed, 0, 998);
    assert_eq!(f.value(bad_needed_count), MARKER);
    f.expect(config_fault, 0, 998);
    assert_eq!(f.value(config_needed), MARKER);
    f.expect(size_only, 0, 122);
    assert!(f.value(size_only_needed) > 36);
    f.expect(denied, 0, 5);
    assert_eq!(f.value(READ_ONLY), 0xa5a5a5a5);
    for result in [close_manager, close_service, close_limited] {
        f.expect(result, 1, MARKER);
    }
}

#[test]
fn registered_scm_empty_database_matches_native_readonly_boundaries_and_kernel_rejection() {
    empty_and_native_boundaries(UnicornCpu::new(PARK_ADDR).unwrap());
    empty_and_native_boundaries(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    empty_and_native_boundaries(jit);
}
#[test]
fn registered_scm_publishes_actual_owned_rows_pages_and_query_configuration() {
    populated_catalog(UnicornCpu::new(PARK_ADDR).unwrap());
    populated_catalog(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    populated_catalog(jit);
}
#[test]
fn registered_scm_checks_output_storage_before_payload_publication() {
    checked_publication(UnicornCpu::new(PARK_ADDR).unwrap());
    checked_publication(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    checked_publication(jit);
}
