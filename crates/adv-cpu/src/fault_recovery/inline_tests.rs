//! Synthetic registry, checkpoint, TLS, and same-frame signal recovery checks.
use super::*;
use std::process::Command;
use std::sync::{Arc, Barrier};

fn site(host_pc: usize) -> Site {
    Site {
        host_pc,
        recovery_pc: host_pc + 8,
        guest_pc: 20,
        next_pc: 24,
        width: 4,
        write: false,
    }
}

#[test]
fn exact_lookup_validation_and_retirement() {
    let registry = Registry::new();
    let first = site(0x1010);
    let second = site(0x1030);
    let id = registry
        .publish(0x1000, 0x100, vec![first, second])
        .unwrap();
    assert_eq!(registry.lookup(first.host_pc), Some(first));
    assert_eq!(registry.lookup(second.host_pc), Some(second));
    for pc in [0, 0xfff, 0x1000, 0x1011, 0x1020, 0x10ff, 0x1100] {
        assert_eq!(registry.lookup(pc), None);
    }
    assert!(registry.publish(0x1000, 0x100, vec![first]).is_err());
    registry.unregister(id);
    assert_eq!(registry.lookup(first.host_pc), None);
    assert!(registry.publish(0x1000, 0x100, vec![first, first]).is_err());
    assert!(
        registry
            .publish(0x1000, 0x100, vec![second, first])
            .is_err()
    );
    for width in [0, 3, 8, 16] {
        assert!(
            registry
                .publish(0x1000, 0x100, vec![Site { width, ..first }])
                .is_err()
        );
    }
    for invalid in [
        Site {
            host_pc: 0xfff,
            ..first
        },
        Site {
            host_pc: 0x1100,
            ..first
        },
        Site {
            recovery_pc: 0xfff,
            ..first
        },
        Site {
            recovery_pc: 0x1100,
            ..first
        },
    ] {
        assert!(registry.publish(0x1000, 0x100, vec![invalid]).is_err());
    }
    assert!(registry.publish(usize::MAX - 1, 4, vec![]).is_err());
    assert!(registry.publish(0, 0x100, vec![]).is_err());
    assert!(registry.publish(0x1000, 0, vec![]).is_err());
    let empty = registry.publish(0x1000, 0x100, vec![]).unwrap();
    assert_eq!(registry.lookup(0x1010), None);
    registry.unregister(empty);
    for width in [1, 2, 4] {
        let id = registry
            .publish(0x1000, 0x100, vec![Site { width, ..first }])
            .unwrap();
        registry.unregister(id);
    }
    let too_many = (0..=MAX_SITES)
        .map(|index| site(0x1000 + index * 16))
        .collect();
    assert!(
        registry
            .publish(0x1000, (MAX_SITES + 1) * 16 + 16, too_many)
            .is_err()
    );
}

#[test]
fn retirement_keeps_an_already_loaded_snapshot_alive() {
    let registry = Arc::new(Registry::new());
    let expected = site(0x1010);
    let id = registry.publish(0x1000, 0x100, vec![expected]).unwrap();
    let reader = registry.read();
    let writer_registry = Arc::clone(&registry);
    let writer = std::thread::spawn(move || writer_registry.unregister(id));
    // Replacement happens before the writer's grace period. Hold a reader
    // across that replacement and inspect precisely the retired allocation.
    while registry.snapshot.load(Ordering::SeqCst) == reader.snapshot {
        std::thread::yield_now();
    }
    assert_eq!(unsafe { &(*reader.snapshot).sites }[0], expected);
    assert_eq!(registry.lookup(expected.host_pc), None);
    drop(reader);
    writer.join().unwrap();
    assert_eq!(registry.readers.load(Ordering::SeqCst), 0);
}

#[test]
fn concurrent_writers_and_readers_preserve_exact_copies() {
    let registry = Arc::new(Registry::new());
    let permanent = site(0x1010);
    let permanent_id = registry.publish(0x1000, 0x100, vec![permanent]).unwrap();
    let barrier = Arc::new(Barrier::new(6));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..4096 {
                    assert_eq!(registry.lookup(permanent.host_pc), Some(permanent));
                    for pc in [0x2010, 0x3010] {
                        if let Some(found) = registry.lookup(pc) {
                            assert_eq!(found, site(pc));
                        }
                        assert_eq!(registry.lookup(pc + 1), None);
                    }
                }
            });
        }
        for start in [0x2000, 0x3000] {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..128 {
                    let id = registry
                        .publish(start, 0x100, vec![site(start + 16)])
                        .unwrap();
                    registry.unregister(id);
                }
            });
        }
    });
    assert_eq!(registry.lookup(0x2010), None);
    assert_eq!(registry.lookup(0x3010), None);
    registry.unregister(permanent_id);
}

fn context(state: *mut CpuState, window: usize, guest_base: u32) -> Context {
    Context {
        state,
        window,
        window_len: PAGE_BYTES,
        guest_base,
        permissions: std::ptr::null(),
        dispatching: Cell::new(false),
        pending: Cell::new(None),
    }
}

#[test]
#[allow(unused_assignments)] // The simulated signal observes writes through its raw state pointer.
fn checkpoint_validates_ea_full_width_and_store_page_proof() {
    let mut state = CpuState {
        eip: 20,
        fault_addr: 0x2004,
        ..CpuState::default()
    };
    let pointer = std::ptr::addr_of_mut!(state);
    let checkpoint = context(pointer, 0x10000, 0x2000);
    let read = site(0x1010);
    let write = Site {
        write: true,
        ..read
    };
    let expected = Fault {
        site: read,
        address: 0x2004,
        host_address: 0x10006,
    };
    assert_eq!(unsafe { checkpoint.fault(read, 0x10006) }, Some(expected));
    assert!(unsafe { checkpoint.fault(write, 0x10004) }.is_some());
    for host_address in [0x10003, 0x10008, usize::MAX] {
        assert_eq!(unsafe { checkpoint.fault(read, host_address) }, None);
    }
    state.eip = 24;
    assert_eq!(unsafe { checkpoint.fault(read, 0x10004) }, None);
    state.eip = 20;
    for address in [0x1ffc, 0x2ffe, u32::MAX] {
        state.fault_addr = address;
        assert_eq!(unsafe { checkpoint.fault(read, 0x10000) }, None);
    }
    state.fault_addr = 0x2003;
    assert!(unsafe { checkpoint.fault(read, 0x10003) }.is_some());
    assert_eq!(unsafe { checkpoint.fault(write, 0x10003) }, None);
    state.fault_addr = 0x2004;
    let shifted_host = context_for_shifted_host(pointer);
    assert_eq!(unsafe { shifted_host.fault(write, 0x10005) }, None);
    let shifted_guest = context(pointer, 0x10000, 0x2001);
    state.fault_addr = 0x2005;
    assert_eq!(unsafe { shifted_guest.fault(write, 0x10004) }, None);
    state.fault_addr = 0x2ffc;
    assert!(unsafe { checkpoint.fault(write, 0x10ffc) }.is_some());
    state.fault_addr = 0x2ffe;
    assert_eq!(unsafe { checkpoint.fault(read, 0x10ffe) }, None);
    let wider = Context {
        window_len: PAGE_BYTES * 2,
        ..checkpoint
    };
    assert!(unsafe { wider.fault(read, 0x10fff) }.is_some());
    assert_eq!(unsafe { wider.fault(write, 0x10fff) }, None);
    let null = context(std::ptr::null_mut(), 0x10000, 0x2000);
    assert_eq!(unsafe { null.fault(read, 0x10000) }, None);
    let top = context(pointer, 0x10000, 0xffff_f000);
    state.fault_addr = 0xffff_fffe;
    assert_eq!(unsafe { top.fault(read, 0x10ffe) }, None);
}

fn context_for_shifted_host(state: *mut CpuState) -> Context {
    context(state, 0x10001, 0x2000)
}

#[test]
fn immutable_permissions_reject_unexplained_accessible_faults() {
    use crate::memory::{EXEC, READ, WRITE};
    let mut state = CpuState {
        eip: 20,
        fault_addr: 0x2004,
        ..CpuState::default()
    };
    let pointer = std::ptr::addr_of_mut!(state);
    let read = site(0x1010);
    let write = Site {
        write: true,
        ..read
    };
    for permission in [0, READ, WRITE, READ | WRITE, READ | WRITE | EXEC] {
        let table = [permission];
        let checkpoint = Context {
            permissions: table.as_ptr(),
            ..context(pointer, 0x10000, 0x2000)
        };
        assert_eq!(
            unsafe { checkpoint.fault(read, 0x10004) }.is_some(),
            permission & READ == 0
        );
        assert_eq!(
            unsafe { checkpoint.fault(write, 0x10004) }.is_some(),
            permission != READ | WRITE
        );
    }
}

#[test]
fn nested_contexts_and_unwinding_restore_previous_scope() {
    let mut state = CpuState::default();
    let pointer = std::ptr::addr_of_mut!(state);
    assert!(unsafe { with_context(std::ptr::null_mut(), 0x10000, PAGE_BYTES, 0, || ()) }.is_err());
    assert!(unsafe { with_context(pointer, usize::MAX, PAGE_BYTES, 0, || ()) }.is_err());
    assert!(unsafe { with_context(pointer, 0x10000, PAGE_BYTES, u32::MAX, || ()) }.is_err());
    assert!(ACTIVE.with(|slot| slot.get().is_null()));
    let result = unsafe {
        with_context(pointer, 0x10000, PAGE_BYTES, 0, || {
            let outer = ACTIVE.with(Cell::get);
            assert!(!outer.is_null());
            let inner = with_context(pointer, 0x20000, PAGE_BYTES, 0, || {
                assert_ne!(ACTIVE.with(Cell::get), outer);
                7
            })
            .unwrap();
            assert_eq!(inner, (7, None));
            assert_eq!(ACTIVE.with(Cell::get), outer);
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = with_context(pointer, 0x20000, PAGE_BYTES, 0, || {
                    panic!("synthetic unwind")
                });
            }));
            assert!(unwind.is_err());
            assert_eq!(ACTIVE.with(Cell::get), outer);
            9
        })
    }
    .unwrap();
    assert_eq!(result, (9, None));
    assert!(ACTIVE.with(|slot| slot.get().is_null()));
}

unsafe extern "C" {
    fn adv_posix_probe_start(
        window: *mut u8,
        offset: u32,
        value: u32,
        prefix: *mut u32,
        write: u32,
        recover: unsafe extern "C" fn() -> u32,
    ) -> u32;
    static adv_posix_probe_load: u8;
    static adv_posix_probe_store: u8;
    static adv_posix_probe_recovery: u8;
    static adv_posix_probe_end: u8;
}
unsafe extern "C" fn fallback() -> u32 {
    // The fixture adds one before its ordinary same-frame epilogue returns.
    crate::Exit::Fallback as u32 - 1
}

#[test]
fn controlled_inline_fault_returns_only_the_canonical_checkpoint() {
    if std::env::var_os("ADV_INLINE_FAULT_CHILD").is_some() {
        run_controlled_fault();
        return;
    }
    let executable = std::env::current_exe().unwrap();
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    let mut child =
        if let Some(runner) = std::env::var_os("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER") {
            let mut command = Command::new(runner);
            command.arg(&executable);
            command
        } else {
            Command::new(&executable)
        };
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    let mut child = Command::new(&executable);
    let output = child.arg("--exact")
        .arg("fault_recovery::inline::tests::controlled_inline_fault_returns_only_the_canonical_checkpoint")
        .arg("--nocapture")
        .env("ADV_INLINE_FAULT_CHILD", "1")
        .output().unwrap();
    assert!(
        output.status.success(),
        "inline fault subprocess failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_controlled_fault() {
    let window_len = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(window_len >= PAGE_BYTES as libc::c_long);
    let window_len = window_len as usize;
    let window = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            window_len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(window, libc::MAP_FAILED);
    let start = adv_posix_probe_start as *const () as usize;
    let end = std::ptr::addr_of!(adv_posix_probe_end) as usize;
    let mut sites = vec![
        Site {
            host_pc: std::ptr::addr_of!(adv_posix_probe_load) as usize,
            recovery_pc: std::ptr::addr_of!(adv_posix_probe_recovery) as usize,
            ..site(start)
        },
        Site {
            host_pc: std::ptr::addr_of!(adv_posix_probe_store) as usize,
            recovery_pc: std::ptr::addr_of!(adv_posix_probe_recovery) as usize,
            write: true,
            ..site(start)
        },
    ];
    sites.sort_unstable_by_key(|site| site.host_pc);
    let registration = unsafe { Registration::publish(start, end - start, sites) }.unwrap();
    assert_eq!(
        unsafe {
            dispatch(
                libc::SIGSEGV,
                std::ptr::addr_of!(adv_posix_probe_load) as usize,
                window as usize + 4,
                0,
                std::ptr::dangling_mut::<c_void>(),
            )
        },
        0
    );
    let mut state = CpuState {
        regs: [17; 8],
        eip: 20,
        eflags: 0x896,
        fuel: 11,
        fault_addr: 0x2004,
        ..CpuState::default()
    };
    let pointer = std::ptr::addr_of_mut!(state);
    let mut prefix = 0;
    unsafe { window.cast::<u32>().add(1).write(0x1234) };
    let (value, fault) = unsafe {
        with_context(pointer, window as usize, window_len, 0x2000, || {
            rejects_unclaimable_dispatches(pointer, window as usize);
            adv_posix_probe_start(window.cast(), 4, 0x5678, &mut prefix, 0, fallback)
        })
    }
    .unwrap();
    assert_eq!(value, 0x1234);
    assert_eq!(fault, None);
    let before = state.clone();

    for write in [false, true] {
        let (value, fault) = unsafe {
            with_context(pointer, window as usize, window_len, 0x2000, || {
                // A checked helper may alter mappings inside this live context.
                assert_eq!(libc::mprotect(window, window_len, libc::PROT_NONE), 0);
                adv_posix_probe_start(
                    window.cast(),
                    4,
                    0x5678,
                    &mut prefix,
                    u32::from(write),
                    fallback,
                )
            })
        }
        .unwrap();
        assert_eq!(value, crate::Exit::Fallback as u32);
        let fault = fault.unwrap();
        assert_eq!(fault.address, 0x2004);
        assert_eq!(fault.host_address, window as usize + 4);
        assert_eq!(fault.site.write, write);
        assert_eq!(fault.site.guest_pc, state.eip);
        assert_eq!(state, before);
    }
    assert_eq!(prefix, 3);
    assert_eq!(
        unsafe { libc::mprotect(window, window_len, libc::PROT_READ) },
        0
    );
    assert_eq!(unsafe { window.cast::<u32>().add(1).read() }, 0x1234);
    drop(registration);
    assert_eq!(
        REGISTRY.lookup(std::ptr::addr_of!(adv_posix_probe_load) as usize),
        None
    );
    assert_eq!(unsafe { libc::munmap(window, window_len) }, 0);
}

fn rejects_unclaimable_dispatches(state: *mut CpuState, window: usize) {
    let pc = std::ptr::addr_of!(adv_posix_probe_load) as usize;
    let site = REGISTRY.lookup(pc).unwrap();
    // Every case must return before capture touches this deliberately unusable
    // context pointer. This also detects accidental widening of the claim gate.
    let fake_context = std::ptr::dangling_mut::<c_void>();
    let unsupported_signal = if cfg!(target_os = "macos") {
        libc::SIGILL
    } else {
        libc::SIGBUS
    };
    for (signal, pc, host_address, access, host_context) in [
        (unsupported_signal, pc, window + 4, 0, fake_context),
        (libc::SIGSEGV, pc + 1, window + 4, 0, fake_context),
        (libc::SIGSEGV, pc, window + 4, usize::MAX - 1, fake_context),
        (libc::SIGSEGV, pc, window + 4, 1, fake_context),
        (libc::SIGSEGV, pc, window + 4, 2, fake_context),
        (libc::SIGSEGV, pc, window + 4, 0, std::ptr::null_mut()),
        (libc::SIGSEGV, pc, window + 8, 0, fake_context),
    ] {
        assert_eq!(
            unsafe { dispatch(signal, pc, host_address, access, host_context) },
            0
        );
    }
    let pointer = ACTIVE.with(Cell::get);
    let context = unsafe { &*pointer };
    context.dispatching.set(true);
    assert_eq!(
        unsafe { dispatch(libc::SIGSEGV, pc, window + 4, 0, fake_context) },
        0
    );
    context.dispatching.set(false);
    context.pending.set(Some(Fault {
        site,
        address: 0x2004,
        host_address: window + 4,
    }));
    assert_eq!(
        unsafe { dispatch(libc::SIGSEGV, pc, window + 4, 0, fake_context) },
        0
    );
    context.pending.set(None);
    unsafe { (*state).eip = 24 };
    assert_eq!(
        unsafe { dispatch(libc::SIGSEGV, pc, window + 4, 0, fake_context) },
        0
    );
    unsafe { (*state).eip = site.guest_pc };
    assert!(!context.dispatching.get());
    assert_eq!(context.pending.get(), None);
}
