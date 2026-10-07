use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, Reg},
};
use adv_machine::{
    Clock, Machine,
    layout::{PAGE, PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;

const DATA: u32 = 0x410000;
const SECURITY: u32 = DATA + 0x100;
const PATH: u32 = DATA + 0x200;
const WPATH: u32 = DATA + 0x300;
const OUT: u32 = DATA + 0x400;

fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    m.sys.set_process_identity(pid).unwrap();
    let root = std::env::temp_dir().join(format!(
        "adv-handle-inheritance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/test.bin"), b"fixture").unwrap();
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
    )
    .unwrap();
    m.spawn("handle tests", PARK_ADDR, &[], 65536).unwrap();
    m.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA + 0x800);
    m.cpu.write(PATH, b"test.bin\0").unwrap();
    let wide: Vec<_> = "test.bin\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(WPATH, &wide).unwrap();
    for (offset, word) in [(0, 12), (4, 0), (8, 1)] {
        m.cpu.write_u32(SECURITY + offset, word).unwrap();
    }
    m
}
fn env<B: Backend>(m: &mut Machine<B>, thunk: u32) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    }
}
fn call<B: Backend>(m: &mut Machine<B>, name: &str, args: &[u32]) -> Outcome {
    let address = m.resolve_import(K32, name).unwrap();
    let thunk = (address - THUNK_BASE) / THUNK_STRIDE;
    let f = m.sys.thunks.get(thunk).f;
    for (i, value) in args.iter().enumerate() {
        m.cpu
            .write_u32(DATA + 0x804 + i as u32 * 4, *value)
            .unwrap();
    }
    (f.handler)(&mut env(m, thunk))
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("unexpected deferred handle API"),
    }
}

fn attributes<B: Backend>(make: impl Fn() -> B) {
    let mut m = machine(make(), 0x1000);
    for name in [
        "CreateEventA",
        "CreateEventW",
        "CreateMutexA",
        "CreateMutexW",
        "CreateSemaphoreA",
        "CreateSemaphoreW",
        "CreateWaitableTimerA",
        "CreateWaitableTimerW",
    ] {
        let args = if name.starts_with("CreateEvent") {
            vec![SECURITY, 0, 0, 0]
        } else if name.starts_with("CreateSemaphore") {
            vec![SECURITY, 0, 1, 0]
        } else {
            vec![SECURITY, 0, 0]
        };
        let h = returned(call(&mut m, name, &args));
        assert_ne!(h, 0, "{name}");
        assert_eq!(m.sys.objects.handle_flags(h), Some(INHERIT), "{name}");
    }
    for (name, path) in [("CreateFileA", PATH), ("CreateFileW", WPATH)] {
        let h = returned(call(
            &mut m,
            name,
            &[path, 0x80000000, 7, SECURITY, 3, 0, 0],
        ));
        assert_ne!(h, u32::MAX);
        assert_eq!(m.sys.objects.handle_flags(h), Some(INHERIT));
    }
    let h = returned(call(
        &mut m,
        "CreateThread",
        &[SECURITY, 65536, PARK_ADDR, 0, 4, 0],
    ));
    assert_ne!(h, 0);
    assert_eq!(m.sys.objects.handle_flags(h), Some(INHERIT));
    let mapping = returned(call(
        &mut m,
        "CreateFileMappingA",
        &[u32::MAX, SECURITY, 4, 0, PAGE, PATH],
    ));
    assert_ne!(mapping, 0);
    assert_eq!(m.sys.objects.handle_flags(mapping), Some(INHERIT));
    let another = returned(call(
        &mut m,
        "CreateFileMappingW",
        &[u32::MAX, 0, 4, 0, PAGE, WPATH],
    ));
    assert_ne!(another, 0);
    assert_eq!(m.sys.objects.handle_flags(another), Some(0));
    for (name, path, inherit) in [
        ("OpenFileMappingA", PATH, 1),
        ("OpenFileMappingW", WPATH, 0),
    ] {
        let opened = returned(call(&mut m, name, &[4, inherit, path]));
        assert_ne!(opened, 0);
        assert_eq!(m.sys.objects.handle_flags(opened), Some(inherit));
    }
    // Fresh duplicates have independent flags and ignore the source's close protection.
    assert_eq!(
        returned(call(&mut m, "SetHandleInformation", &[h, u32::MAX, 3])),
        1
    );
    assert_eq!(returned(call(&mut m, "GetHandleInformation", &[h, OUT])), 1);
    assert_eq!(m.cpu.read_u32(OUT).unwrap(), 3);
    assert_eq!(returned(call(&mut m, "CloseHandle", &[h])), 0);
    assert_eq!(crate::util::last_error(&env(&mut m, 0)), 6);
    assert_eq!(
        returned(call(
            &mut m,
            "DuplicateHandle",
            &[CURRENT_PROCESS, h, CURRENT_PROCESS, OUT, 0, 0, 3]
        )),
        1
    );
    let plain = m.cpu.read_u32(OUT).unwrap();
    assert_eq!(m.sys.objects.handle_flags(plain), Some(0));
    assert_eq!(
        m.sys.objects.handle_flags(h),
        Some(3),
        "protected source remains open even with DUPLICATE_CLOSE_SOURCE"
    );
    assert_eq!(
        returned(call(
            &mut m,
            "DuplicateHandle",
            &[CURRENT_PROCESS, h, CURRENT_PROCESS, OUT, 0, 1, 2]
        )),
        1
    );
    let inherited = m.cpu.read_u32(OUT).unwrap();
    assert_eq!(m.sys.objects.handle_flags(inherited), Some(1));
    assert_eq!(
        returned(call(&mut m, "SetHandleInformation", &[h, INHERIT, 0])),
        1
    );
    assert_eq!(m.sys.objects.handle_flags(h), Some(2));
    assert_eq!(
        returned(call(&mut m, "SetHandleInformation", &[h, 4, 4])),
        1
    );
    assert_eq!(m.sys.objects.handle_flags(h), Some(2));
    assert_eq!(
        returned(call(&mut m, "SetHandleInformation", &[h, 2, 0])),
        1
    );
    assert_eq!(returned(call(&mut m, "CloseHandle", &[h])), 1);
    assert_eq!(m.sys.objects.handle_flags(h), None);
    let before = m.sys.objects.object_count();
    // Cross-page SECURITY_ATTRIBUTES fail before a file is opened or a thread spawned.
    for (name, args) in [
        (
            "CreateFileA",
            vec![PATH, 0x80000000, 7, DATA + PAGE - 8, 3, 0, 0],
        ),
        ("CreateEventA", vec![DATA + PAGE - 8, 0, 0, 0]),
        (
            "CreateThread",
            vec![DATA + PAGE - 8, 65536, PARK_ADDR, 0, 4, 0],
        ),
    ] {
        let failed = returned(call(&mut m, name, &args));
        assert_eq!(
            failed,
            if name.starts_with("CreateFile") {
                u32::MAX
            } else {
                0
            }
        );
        assert_eq!(crate::util::last_error(&env(&mut m, 0)), 998);
        assert_eq!(m.sys.objects.object_count(), before);
    }
    m.cpu.write_u32(SECURITY + 4, 1).unwrap();
    assert_eq!(
        returned(call(&mut m, "CreateEventA", &[SECURITY, 0, 0, 0])),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m, 0)), 50);
    assert_eq!(m.sys.objects.object_count(), before);
    m.cpu.protect(DATA, PAGE, Prot::READ).unwrap();
    let mut e = env(&mut m, 0);
    assert_eq!(
        returned(finish_duplicate(&mut e, Ok(plain), OUT, 0x1000)),
        0
    );
    assert_eq!(e.sys.objects.handle_flags(plain), None);
    assert_eq!(e.sys.objects.handle_flags(inherited), Some(1));
}

#[test]
fn security_attributes_flags_and_duplicate_aliases_work_on_all_executors() {
    for jit in [false, true] {
        attributes(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    attributes(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn duplication_rollback<B: Backend>(make: impl Fn() -> B, unmap: bool) {
    let mut parent = machine(make(), 0x1000);
    let mut child = machine(make(), 0x2000);
    let services = processes::Services::default();
    services.register(&parent.sys.process_identity);
    services.register(&child.sys.process_identity);
    parent.ext.insert(services.clone());
    child.ext.insert(services.clone());
    let target = parent.sys.objects.insert(Object::Process {
        identity: child.sys.process_identity.clone(),
        access: 0x1fffff,
    });
    parent.cpu.write_u32(OUT, 0xa5a5a5a5).unwrap();
    let before = child.sys.objects.object_count();
    let outcome = call(
        &mut parent,
        "DuplicateHandle",
        &[CURRENT_PROCESS, CURRENT_PROCESS, target, OUT, 0, 1, 2],
    );
    let Outcome::HostRequest { then } = outcome else {
        panic!("missing duplication boundary")
    };
    if unmap {
        parent.cpu.unmap(DATA, PAGE).unwrap();
    } else {
        parent.cpu.protect(DATA, PAGE, Prot::READ).unwrap();
    }
    let processes::Request::Visit { pid, task, .. } = services.pop().unwrap() else {
        panic!("wrong request")
    };
    assert_eq!(pid, child.sys.pid);
    task(Ok(&mut env(&mut child, 0)));
    assert_eq!(child.sys.objects.object_count(), before + 1);
    let Outcome::HostRequest { then } = then(&mut env(&mut parent, 0)) else {
        panic!("failed publication must yield a remote rollback")
    };
    let processes::Request::Visit { pid, task, .. } = services.pop().unwrap() else {
        panic!("wrong rollback request")
    };
    assert_eq!(pid, child.sys.pid);
    task(Ok(&mut env(&mut child, 0)));
    assert_eq!(returned(then(&mut env(&mut parent, 0))), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent, 0)), 998);
    assert_eq!(child.sys.objects.object_count(), before);
    assert!(child.sys.objects.inheritable_handles().next().is_none());
    if !unmap {
        assert_eq!(parent.cpu.read_u32(OUT).unwrap(), 0xa5a5a5a5);
    }
    assert!(!services.pending());
}

#[test]
fn failed_remote_duplicate_publication_closes_the_target_alias() {
    for jit in [false, true] {
        for unmap in [false, true] {
            duplication_rollback(|| JitCpu::new(PARK_ADDR, jit).unwrap(), unmap);
        }
    }
    for unmap in [false, true] {
        duplication_rollback(|| UnicornCpu::new(PARK_ADDR).unwrap(), unmap);
    }
}

fn waits<B: Backend>(make: impl Fn() -> B) {
    let mut m = machine(make(), 0x1000);
    let process = adv_machine::process::ProcessIdentity::new(0x2000);
    process.completion.complete(0);
    let denied = m.sys.objects.insert(Object::Process {
        identity: process.clone(),
        access: 0x400,
    });
    let allowed = m.sys.objects.insert(Object::Process {
        identity: process,
        access: 0x100000,
    });
    let thread = m.sys.thread_identity(0).unwrap();
    thread.completion.complete(0);
    let thread_denied = m.sys.objects.insert(Object::RemoteThread {
        identity: thread.clone(),
        access: 2,
    });
    let thread_allowed = m.sys.objects.insert(Object::RemoteThread {
        identity: thread,
        access: 0x100000,
    });
    for handle in [denied, thread_denied] {
        assert_eq!(
            returned(call(&mut m, "WaitForSingleObject", &[handle, 0])),
            u32::MAX
        );
        assert_eq!(crate::util::last_error(&env(&mut m, 0)), 5);
    }
    for handle in [allowed, thread_allowed] {
        assert_eq!(
            returned(call(&mut m, "WaitForSingleObject", &[handle, 0])),
            0
        );
    }
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: true,
    });
    for handle in [denied, thread_denied] {
        m.cpu.write_u32(OUT, event).unwrap();
        m.cpu.write_u32(OUT + 4, handle).unwrap();
        assert_eq!(
            returned(call(&mut m, "WaitForMultipleObjects", &[2, OUT, 0, 0])),
            u32::MAX
        );
        assert_eq!(crate::util::last_error(&env(&mut m, 0)), 5);
        assert!(
            matches!(
                m.sys.objects.get(event),
                Some(Object::Event { signaled: true, .. })
            ),
            "failed rights validation must not consume an auto-reset event"
        );
    }
}

#[test]
fn process_and_remote_thread_waits_require_synchronize_before_acquiring_objects() {
    for jit in [false, true] {
        waits(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    waits(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn thread_timing_failure_and_group_affinity_preserve_outputs_and_check_access() {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), 0x1000);
    let identity = m.sys.thread_identity(0).unwrap();
    let query = m.sys.objects.insert(Object::RemoteThread {
        identity: identity.clone(),
        access: 0x800,
    });
    let set = m.sys.objects.insert(Object::RemoteThread {
        identity,
        access: 0x20,
    });
    let pseudo = u32::MAX - 1;
    for (handle, error) in [(pseudo, 50), (query, 50), (set, 5), (0, 6)] {
        m.cpu.write(OUT, &[0xa5; 32]).unwrap();
        assert_eq!(
            returned(call(
                &mut m,
                "GetThreadTimes",
                &[handle, OUT, OUT + 8, OUT + 16, OUT + 24]
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m, 0)), error);
        let mut bytes = [0; 32];
        m.cpu.read(OUT, &mut bytes).unwrap();
        assert_eq!(bytes, [0xa5; 32]);
    }
    for handle in [pseudo, query] {
        assert_eq!(
            returned(call(&mut m, "GetThreadGroupAffinity", &[handle, OUT])),
            1
        );
        assert_eq!(m.cpu.read_u32(OUT).unwrap(), 1);
        assert_eq!(m.cpu.read_u32(OUT + 4).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(OUT + 8).unwrap(), 0);
    }
    assert_eq!(
        returned(call(&mut m, "GetThreadGroupAffinity", &[set, OUT])),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m, 0)), 5);
    assert_eq!(
        returned(call(&mut m, "SetThreadGroupAffinity", &[query, OUT, 0])),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m, 0)), 5);
    for handle in [pseudo, set] {
        assert_eq!(
            returned(call(&mut m, "SetThreadGroupAffinity", &[handle, OUT, OUT])),
            1
        );
        assert_eq!(
            returned(call(&mut m, "SetThreadGroupAffinity", &[handle, OUT, 0])),
            1
        );
    }
    m.cpu.write_u32(OUT, 0).unwrap();
    assert_eq!(
        returned(call(&mut m, "SetThreadGroupAffinity", &[pseudo, OUT, 0])),
        1
    );
    for words in [[2, 0, 0], [1, 1, 0], [1, 0x10000, 0], [1, 0, 1]] {
        for (index, word) in words.into_iter().enumerate() {
            m.cpu.write_u32(OUT + index as u32 * 4, word).unwrap();
        }
        m.cpu.write(OUT + 16, &[0xa5; 12]).unwrap();
        assert_eq!(
            returned(call(
                &mut m,
                "SetThreadGroupAffinity",
                &[pseudo, OUT, OUT + 16]
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m, 0)), 87);
        let mut bytes = [0; 12];
        m.cpu.read(OUT + 16, &mut bytes).unwrap();
        assert_eq!(bytes, [0xa5; 12]);
    }
}

#[test]
fn processor_topology_query_sizes_filtering_and_number_share_one_profile() {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), 0x1000);
    m.cpu.write_u32(OUT, u32::MAX).unwrap();
    assert_eq!(
        returned(call(&mut m, "GetCurrentProcessorNumberEx", &[OUT])),
        0
    );
    assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0);
    let length = OUT + 0x200;
    for (relation, expected) in [(0, 44), (1, 44), (3, 44), (4, 76), (0xffff, 208)] {
        m.cpu.write_u32(length, 0).unwrap();
        assert_eq!(
            returned(call(
                &mut m,
                "GetLogicalProcessorInformationEx",
                &[relation, 0, length]
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m, 0)), 122);
        assert_eq!(m.cpu.read_u32(length).unwrap(), expected);
        m.cpu.write_u32(length, expected - 1).unwrap();
        m.cpu.write(OUT, &[0xa5; 208]).unwrap();
        assert_eq!(
            returned(call(
                &mut m,
                "GetLogicalProcessorInformationEx",
                &[relation, OUT, length]
            )),
            0
        );
        assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0xa5a5a5a5);
        assert_eq!(
            returned(call(
                &mut m,
                "GetLogicalProcessorInformationEx",
                &[relation, OUT, length]
            )),
            1
        );
        let mut offset = 0;
        while offset < expected {
            let kind = m.cpu.read_u32(OUT + offset).unwrap();
            assert!(relation == 0xffff || kind == relation);
            let size = m.cpu.read_u32(OUT + offset + 4).unwrap();
            assert_eq!(size, if kind == 4 { 76 } else { 44 });
            let mask_offset = if kind == 4 { 72 } else { 32 };
            assert_eq!(m.cpu.read_u32(OUT + offset + mask_offset).unwrap(), 1);
            offset += size;
        }
        assert_eq!(offset, expected);
    }
    m.cpu.write_u32(length, 999).unwrap();
    assert_eq!(
        returned(call(
            &mut m,
            "GetLogicalProcessorInformationEx",
            &[99, OUT, length]
        )),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m, 0)), 87);
    assert_eq!(m.cpu.read_u32(length).unwrap(), 999);
}
