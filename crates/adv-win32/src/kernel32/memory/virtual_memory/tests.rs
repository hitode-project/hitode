use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg},
};
use adv_machine::{Clock, Machine, Object, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const DATA: u32 = 0x410000;
const REMOTE: u32 = 0x20000000;
const COUNT: u32 = DATA + 0x100;
const INPUT: u32 = DATA + 0x200;
const OUTPUT: u32 = DATA + 0x300;
const INFO: u32 = DATA + 0x400;
const OLD: u32 = DATA + 0x500;

fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    m.sys.set_process_identity(pid).unwrap();
    let path = std::env::temp_dir().join("adv-process-memory-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&path, "fixture.exe", &path)).unwrap();
    m.spawn("memory tests", PARK_ADDR, &[], GRANULE).unwrap();
    assert!(m.sys.vm.reserve_at(DATA, PAGE));
    m.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA + 0x800);
    m
}
fn env<B: Backend>(m: &mut Machine<B>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn args<B: Backend>(m: &mut Machine<B>, values: &[u32]) {
    for (i, value) in values.iter().enumerate() {
        m.cpu
            .write_u32(DATA + 0x804 + i as u32 * 4, *value)
            .unwrap();
    }
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("unexpected deferred memory operation"),
    }
}
fn services<B: Backend>(parent: &mut Machine<B>, child: &mut Machine<B>) -> processes::Services {
    let s = processes::Services::default();
    s.register(&parent.sys.process_identity);
    s.register(&child.sys.process_identity);
    parent.ext.insert(s.clone());
    child.ext.insert(s.clone());
    s
}
fn handle<B: Backend>(parent: &mut Machine<B>, child: &Machine<B>, access: u32) -> u32 {
    parent.sys.objects.insert(Object::Process {
        identity: child.sys.process_identity.clone(),
        access,
    })
}
fn resolve<B: Backend>(
    outcome: Outcome,
    parent: &mut Machine<B>,
    child: &mut Machine<B>,
    s: &processes::Services,
) -> u32 {
    let Outcome::HostRequest { then } = outcome else {
        panic!("remote memory must yield the host service boundary")
    };
    let processes::Request::Visit { pid, context, task } = s.pop().unwrap() else {
        panic!("wrong request")
    };
    assert_eq!(pid, child.sys.pid);
    assert_eq!(context, None);
    task(Ok(&mut env(child)));
    assert!(!s.pending());
    returned(then(&mut env(parent)))
}
fn word<B: Backend>(m: &Machine<B>, address: u32) -> u32 {
    m.cpu.read_u32(address).unwrap()
}
fn bytes<B: Backend>(m: &Machine<B>, address: u32, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    m.cpu.read(address, &mut bytes).unwrap();
    bytes
}

fn isolation<B: Backend>(make: impl Fn() -> B) {
    let mut parent = machine(make(), 0x1000);
    let mut child = machine(make(), 0x2000);
    let s = services(&mut parent, &mut child);
    let h = handle(
        &mut parent,
        &child,
        VM_OPERATION | VM_READ | VM_WRITE | QUERY_INFORMATION,
    );
    assert_eq!(
        allocate(
            &mut env(&mut parent),
            REMOTE,
            2 * PAGE,
            MEM_RESERVE | MEM_COMMIT,
            4
        )
        .unwrap(),
        REMOTE
    );
    parent.cpu.write(REMOTE, b"parent").unwrap();
    args(
        &mut parent,
        &[h, REMOTE, 2 * PAGE, MEM_RESERVE | MEM_COMMIT, 4],
    );
    let result = virtual_alloc_ex(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), REMOTE);
    assert_eq!(bytes(&child, REMOTE, 6), [0; 6]);
    parent.cpu.write(INPUT, b"child!").unwrap();
    args(&mut parent, &[h, REMOTE, INPUT, 6, COUNT]);
    let result = write_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
    assert_eq!(word(&parent, COUNT), 6);
    assert_eq!(bytes(&parent, REMOTE, 6), b"parent");
    assert_eq!(bytes(&child, REMOTE, 6), b"child!");
    args(&mut parent, &[h, REMOTE, OUTPUT, 6, COUNT]);
    let result = read_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
    assert_eq!(bytes(&parent, OUTPUT, 6), b"child!");
    assert_eq!(word(&parent, COUNT), 6);
    args(&mut parent, &[h, REMOTE, INFO, 28]);
    let result = virtual_query_ex(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 28);
    assert_eq!(word(&parent, INFO), REMOTE);
    assert_eq!(word(&parent, INFO + 12), 2 * PAGE);
    assert_eq!(word(&parent, INFO + 16), MEM_COMMIT);
    args(&mut parent, &[h, REMOTE, 2 * PAGE, 2, OLD]);
    let result = virtual_protect_ex(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
    assert_eq!(word(&parent, OLD), 4);
    assert!(!child.cpu.accessible(REMOTE, PAGE, Prot::WRITE));
    assert!(parent.cpu.accessible(REMOTE, PAGE, Prot::WRITE));
    args(&mut parent, &[h, REMOTE, INPUT, 6, COUNT]);
    let result = write_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent)), NOACCESS);
    assert_eq!(word(&parent, COUNT), 0);
    assert_eq!(bytes(&child, REMOTE, 6), b"child!");
    for protection in [0x10, 0x20] {
        args(&mut parent, &[h, REMOTE, 2 * PAGE, protection, OLD]);
        let result = virtual_protect_ex(&mut env(&mut parent));
        assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
        parent.cpu.write(INPUT, b"patch!").unwrap();
        args(&mut parent, &[h, REMOTE, INPUT, 6, COUNT]);
        let result = write_process_memory(&mut env(&mut parent));
        assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
        assert_eq!(word(&parent, COUNT), 6);
        assert_eq!(bytes(&child, REMOTE, 6), b"patch!");
        assert_eq!(page_protection(&env(&mut child), REMOTE), Some(protection));
        assert!(child.cpu.accessible(REMOTE, PAGE, Prot::EXEC));
        assert!(!child.cpu.accessible(REMOTE, PAGE, Prot::WRITE));
    }
    args(&mut parent, &[h, REMOTE, 0, MEM_RELEASE]);
    let result = virtual_free_ex(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
    assert_eq!(child.sys.vm.containing(REMOTE), None);
    assert_eq!(bytes(&parent, REMOTE, 6), b"parent");
}

#[test]
fn remote_apis_isolate_identical_addresses_on_every_executor() {
    for jit in [false, true] {
        isolation(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    isolation(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn reservation<B: Backend>(make: impl Fn() -> B) {
    let mut m = machine(make(), 0x1000);
    assert_eq!(
        allocate(&mut env(&mut m), REMOTE + 3, 3 * PAGE, MEM_RESERVE, 4).unwrap(),
        REMOTE
    );
    assert!(!m.cpu.accessible(REMOTE, 1, Prot::default()));
    assert_eq!(
        query(&mut env(&mut m), REMOTE + PAGE + 8).unwrap(),
        [
            REMOTE + PAGE,
            REMOTE,
            4,
            3 * PAGE,
            MEM_RESERVE,
            0,
            MEM_PRIVATE
        ]
    );
    assert_eq!(
        allocate(&mut env(&mut m), REMOTE + PAGE + 3, 1, MEM_COMMIT, 0x20).unwrap(),
        REMOTE + PAGE
    );
    assert_eq!(
        query(&mut env(&mut m), REMOTE).unwrap()[3..],
        [PAGE, MEM_RESERVE, 0, MEM_PRIVATE]
    );
    assert_eq!(
        query(&mut env(&mut m), REMOTE + PAGE).unwrap()[3..],
        [PAGE, MEM_COMMIT, 0x20, MEM_PRIVATE]
    );
    m.cpu.write(REMOTE + PAGE, b"preserve").unwrap();
    allocate(&mut env(&mut m), REMOTE + PAGE, PAGE, MEM_COMMIT, 2).unwrap();
    assert_eq!(bytes(&m, REMOTE + PAGE, 8), b"preserve");
    assert!(!m.cpu.accessible(REMOTE + PAGE, 1, Prot::WRITE));
    assert_eq!(
        protect(&mut env(&mut m), REMOTE + PAGE, 2 * PAGE, 4)
            .unwrap_err()
            .code,
        INVALID_ADDRESS
    );
    assert!(m.cpu.accessible(REMOTE + PAGE, PAGE, Prot::READ));
    assert!(!m.cpu.accessible(REMOTE + PAGE, PAGE, Prot::WRITE));
    assert_eq!(
        release(&mut env(&mut m), REMOTE + PAGE, PAGE, MEM_DECOMMIT).unwrap(),
        1
    );
    assert_eq!(query(&mut env(&mut m), REMOTE).unwrap()[3], 4 * PAGE);
    assert_eq!(query(&mut env(&mut m), REMOTE).unwrap()[4], MEM_RESERVE);
    allocate(&mut env(&mut m), REMOTE + PAGE, PAGE, MEM_COMMIT, 4).unwrap();
    assert_eq!(bytes(&m, REMOTE + PAGE, 8), [0; 8]);
    assert_eq!(
        release(&mut env(&mut m), REMOTE + PAGE, 0, MEM_RELEASE)
            .unwrap_err()
            .code,
        err::INVALID_PARAMETER
    );
    assert_eq!(
        release(&mut env(&mut m), REMOTE, PAGE, MEM_RELEASE)
            .unwrap_err()
            .code,
        err::INVALID_PARAMETER
    );
    release(&mut env(&mut m), REMOTE, 0, MEM_RELEASE).unwrap();
    let info = query(&mut env(&mut m), REMOTE).unwrap();
    assert_eq!(info, [REMOTE, 0, 0, 0x7e000000 - REMOTE, MEM_FREE, 0, 0]);
    assert_eq!(
        allocate(&mut env(&mut m), REMOTE, PAGE, MEM_COMMIT, 4)
            .unwrap_err()
            .code,
        INVALID_ADDRESS
    );
    // Reservation extent is page aligned, and top-down placement remains a
    // granule-aligned allocation base.
    let top = allocate(&mut env(&mut m), 0, PAGE, MEM_RESERVE | MEM_TOP_DOWN, 4).unwrap();
    assert_eq!(top, DYN_END - GRANULE);
    assert_eq!(m.sys.vm.region(top), Some(PAGE));
}

#[test]
fn reservations_commit_decommit_and_query_the_actual_page_states() {
    for jit in [false, true] {
        reservation(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    reservation(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn failures<B: Backend>(make: impl Fn() -> B) {
    let mut parent = machine(make(), 0x1000);
    let mut child = machine(make(), 0x2000);
    let s = services(&mut parent, &mut child);
    let h = handle(
        &mut parent,
        &child,
        VM_OPERATION | VM_READ | VM_WRITE | QUERY_INFORMATION,
    );
    allocate(&mut env(&mut child), REMOTE, 2 * PAGE, MEM_RESERVE, 4).unwrap();
    allocate(&mut env(&mut child), REMOTE, PAGE, MEM_COMMIT, 4).unwrap();
    child.cpu.write(REMOTE + PAGE - 4, b"tail").unwrap();
    parent.cpu.write(OUTPUT, &[0xa5; 8]).unwrap();
    parent.cpu.write(INPUT, b"changed!").unwrap();
    args(&mut parent, &[h, REMOTE + PAGE - 4, OUTPUT, 8, COUNT]);
    let result = read_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent)), PARTIAL_COPY);
    assert_eq!(word(&parent, COUNT), 0);
    assert_eq!(bytes(&parent, OUTPUT, 8), [0xa5; 8]);
    args(&mut parent, &[h, REMOTE + PAGE - 4, INPUT, 8, COUNT]);
    let result = write_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 0);
    assert_eq!(word(&parent, COUNT), 4);
    assert_eq!(bytes(&child, REMOTE + PAGE - 4, 4), b"tail");
    // Invalid caller output is rejected before any remote protection change.
    args(&mut parent, &[h, REMOTE, PAGE, 2, DATA + PAGE - 2]);
    assert_eq!(returned(virtual_protect_ex(&mut env(&mut parent))), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent)), NOACCESS);
    assert!(!s.pending());
    assert!(child.cpu.accessible(REMOTE, PAGE, Prot::WRITE));
    args(&mut parent, &[h, REMOTE, INPUT, 8, DATA + PAGE - 2]);
    assert_eq!(returned(write_process_memory(&mut env(&mut parent))), 0);
    assert!(!s.pending());
    // The valid caller prefix must not be partially populated on a bad buffer.
    parent.cpu.write(DATA + PAGE - 4, &[0xa5; 4]).unwrap();
    args(&mut parent, &[h, REMOTE, DATA + PAGE - 4, 8, COUNT]);
    assert_eq!(returned(read_process_memory(&mut env(&mut parent))), 0);
    assert_eq!(bytes(&parent, DATA + PAGE - 4, 4), [0xa5; 4]);
    assert_eq!(word(&parent, COUNT), 0);
    assert!(!s.pending());
    for (address, size, expected) in [
        (u32::MAX - 3, 8, PARTIAL_COPY),
        (REMOTE, MAX_TRANSFER + 1, 8),
    ] {
        args(&mut parent, &[h, address, INPUT, size, COUNT]);
        assert_eq!(returned(write_process_memory(&mut env(&mut parent))), 0);
        assert_eq!(crate::util::last_error(&env(&mut parent)), expected);
        assert_eq!(word(&parent, COUNT), 0);
        assert!(!s.pending());
    }
    for access in [VM_WRITE, VM_OPERATION, VM_READ] {
        let restricted = handle(&mut parent, &child, access);
        args(&mut parent, &[restricted, REMOTE, INPUT, 8, COUNT]);
        assert_eq!(returned(write_process_memory(&mut env(&mut parent))), 0);
        assert_eq!(
            crate::util::last_error(&env(&mut parent)),
            err::ACCESS_DENIED
        );
        assert!(!s.pending());
    }
    for bad in [
        0,
        0x1234,
        parent.sys.objects.insert(Object::Thread { ctx: 0 }),
    ] {
        args(&mut parent, &[bad, REMOTE, INPUT, 8, COUNT]);
        assert_eq!(returned(read_process_memory(&mut env(&mut parent))), 0);
        assert_eq!(
            crate::util::last_error(&env(&mut parent)),
            err::INVALID_HANDLE
        );
        assert!(!s.pending());
    }
    let read_only = handle(&mut parent, &child, VM_READ);
    args(&mut parent, &[read_only, REMOTE, INFO, 28]);
    assert_eq!(returned(virtual_query_ex(&mut env(&mut parent))), 0);
    assert_eq!(
        crate::util::last_error(&env(&mut parent)),
        err::ACCESS_DENIED
    );
    let denied = 0x430000;
    assert!(parent.sys.vm.reserve_at(denied, 2 * PAGE));
    parent.cpu.map(denied, PAGE, Prot::READ).unwrap();
    parent.cpu.map(denied + PAGE, PAGE, Prot::EXEC).unwrap();
    parent.cpu.write(denied, &[0xa5; 8]).unwrap();
    parent.cpu.write(denied + PAGE, b"changed!").unwrap();
    args(&mut parent, &[h, REMOTE, denied, 8, COUNT]);
    assert_eq!(returned(read_process_memory(&mut env(&mut parent))), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent)), NOACCESS);
    assert_eq!(bytes(&parent, denied, 8), [0xa5; 8]);
    assert!(!s.pending());
    args(&mut parent, &[h, REMOTE, denied + PAGE, 8, COUNT]);
    assert_eq!(returned(write_process_memory(&mut env(&mut parent))), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent)), NOACCESS);
    assert!(!s.pending());
    args(&mut parent, &[u32::MAX, INPUT, OUTPUT, 8, COUNT]);
    assert_eq!(returned(read_process_memory(&mut env(&mut parent))), 1);
    assert_eq!(bytes(&parent, OUTPUT, 8), b"changed!");
    assert!(!s.pending());
    // Zero-length copies still validate handles and optional output, but neither
    // target nor source pointer needs to address a page.
    args(&mut parent, &[h, u32::MAX, 0, 0, COUNT]);
    let result = write_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
    assert_eq!(word(&parent, COUNT), 0);
}

#[test]
fn remote_memory_failures_check_rights_pointers_counts_and_partial_copies() {
    for jit in [false, true] {
        failures(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    failures(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn aliased_counts<B: Backend>(make: impl Fn() -> B) {
    let mut parent = machine(make(), 0x1000);
    let mut child = machine(make(), 0x2000);
    let s = services(&mut parent, &mut child);
    let h = handle(&mut parent, &child, VM_OPERATION | VM_READ | VM_WRITE);
    allocate(
        &mut env(&mut child),
        REMOTE,
        PAGE,
        MEM_RESERVE | MEM_COMMIT,
        4,
    )
    .unwrap();
    let original = [0xaa, 0xbb, 0xcc, 0xdd, 0x11, 0x22, 0x33, 0x44];
    parent.cpu.write(INPUT, &original).unwrap();
    args(&mut parent, &[h, REMOTE, INPUT, 8, INPUT]);
    let result = write_process_memory(&mut env(&mut parent));
    assert_eq!(resolve(result, &mut parent, &mut child, &s), 1);
    assert_eq!(bytes(&child, REMOTE, 8), original);
    assert_eq!(word(&parent, INPUT), 8);
    parent.cpu.write(INPUT, &original).unwrap();
    args(&mut parent, &[u32::MAX, INPUT, OUTPUT, 8, INPUT]);
    assert_eq!(returned(read_process_memory(&mut env(&mut parent))), 1);
    assert_eq!(bytes(&parent, OUTPUT, 8), original);
    assert_eq!(word(&parent, INPUT), 8);
}

#[test]
fn count_outputs_may_alias_write_buffers_and_current_process_read_sources() {
    for jit in [false, true] {
        aliased_counts(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    aliased_counts(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

/// Keep the selected failure/count and debugger-write behavior checked against
/// native Windows rather than relying only on two guest implementations.
#[cfg(windows)]
#[test]
fn native_windows_confirms_partial_counts_recommit_and_executable_writes() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(address: *mut c_void, size: usize, ty: u32, protection: u32)
        -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, ty: u32) -> i32;
        fn VirtualProtect(address: *mut c_void, size: usize, protection: u32, old: *mut u32)
        -> i32;
        fn ReadProcessMemory(
            process: *mut c_void,
            address: *mut c_void,
            buffer: *mut u8,
            size: usize,
            copied: *mut usize,
        ) -> i32;
        fn WriteProcessMemory(
            process: *mut c_void,
            address: *mut c_void,
            buffer: *const u8,
            size: usize,
            copied: *mut usize,
        ) -> i32;
        fn GetLastError() -> u32;
    }
    struct Reservation(*mut c_void);
    impl Drop for Reservation {
        fn drop(&mut self) {
            unsafe {
                VirtualFree(self.0, 0, MEM_RELEASE);
            }
        }
    }
    // SAFETY: All host buffers are sized to the requested transfers. The native
    // reservation is owned here, released on every exit, and never dereferenced
    // unless that first page is committed with at least read permission.
    unsafe {
        let region = Reservation(VirtualAlloc(
            std::ptr::null_mut(),
            2 * PAGE as usize,
            MEM_RESERVE,
            4,
        ));
        assert!(!region.0.is_null());
        let first = VirtualAlloc(region.0, PAGE as usize, MEM_COMMIT, 4);
        assert_eq!(first, region.0);
        let tail = first.cast::<u8>().add(PAGE as usize - 4);
        std::ptr::copy_nonoverlapping(b"tail".as_ptr(), tail, 4);
        let process = (-1isize) as *mut c_void;
        let mut copied = 0;
        assert_eq!(
            WriteProcessMemory(process, tail.cast(), b"changed!".as_ptr(), 8, &mut copied),
            0
        );
        assert_eq!(GetLastError(), PARTIAL_COPY);
        assert_eq!(copied, 4);
        assert_eq!(std::slice::from_raw_parts(tail, 4), b"tail");
        let mut output = [0xa5; 8];
        assert_eq!(
            ReadProcessMemory(process, tail.cast(), output.as_mut_ptr(), 8, &mut copied),
            0
        );
        assert_eq!(GetLastError(), PARTIAL_COPY);
        assert_eq!(copied, 0);
        assert_eq!(output, [0xa5; 8]);
        assert_eq!(VirtualAlloc(first, PAGE as usize, MEM_COMMIT, 2), first);
        assert_eq!(std::slice::from_raw_parts(tail, 4), b"tail");
        assert_eq!(
            WriteProcessMemory(process, first, b"code".as_ptr(), 4, &mut copied),
            0
        );
        assert_eq!(GetLastError(), NOACCESS);
        assert_eq!(copied, 0);
        assert_eq!(VirtualAlloc(first, PAGE as usize, MEM_COMMIT, 0x20), first);
        assert_ne!(
            WriteProcessMemory(process, first, b"code".as_ptr(), 4, &mut copied),
            0
        );
        assert_eq!(copied, 4);
        let mut previous = 0;
        assert_ne!(VirtualProtect(first, PAGE as usize, 4, &mut previous), 0);
        assert_eq!(
            previous, 0x20,
            "debugger writes restore executable protection"
        );
    }
}
