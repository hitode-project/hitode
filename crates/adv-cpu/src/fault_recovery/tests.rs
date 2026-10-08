//! Controlled native faults run in fresh subprocesses so an unclaimed signal
//! never terminates the test harness or changes another test's handler chain.
use super::*;
use crate::virtual_memory::GuestPageProtection;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    static adv_posix_probe_chain_recovery: u8;
    static adv_posix_probe_end: u8;
    fn adv_posix_fault_pc(context: *const c_void) -> usize;
    fn adv_posix_fault_set_pc(context: *mut c_void, pc: usize);
    fn adv_posix_fault_synchronous(signal: i32, code: i32) -> i32;
}
static CHAIN_COUNT: AtomicUsize = AtomicUsize::new(0);
static CHAIN_SIGNAL: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn host_chain(signal: i32, _info: *mut libc::siginfo_t, context: *mut c_void) {
    CHAIN_COUNT.fetch_add(1, Ordering::Relaxed);
    CHAIN_SIGNAL.store(signal as usize, Ordering::Relaxed);
    // Darwin pthread_kill can report a memory-fault si_code for raise().
    // This fixture handler owns only its probe PCs, never the raise frame.
    let pc = unsafe { adv_posix_fault_pc(context) };
    if pc != std::ptr::addr_of!(adv_posix_probe_load) as usize
        && pc != std::ptr::addr_of!(adv_posix_probe_store) as usize
    {
        return;
    }
    unsafe {
        adv_posix_fault_set_pc(
            context,
            std::ptr::addr_of!(adv_posix_probe_chain_recovery) as usize,
        )
    };
}
unsafe extern "C" fn recover() -> u32 {
    let Some(outcome) = (unsafe { recover_current() }) else {
        return 0xffff;
    };
    match outcome.kind {
        RecoveryKind::Completed => outcome.value,
        RecoveryKind::AccessViolation => 0xf000,
        RecoveryKind::CodeWriteProtection => 0xf100,
    }
}

fn point(write: bool) -> NativeRecoveryPoint {
    #[cfg(target_arch = "aarch64")]
    let (registers, flags, address, value) = (
        crate::execution_abi::ARM64_GPRS.map(RecoveryValue::HostRegister),
        RecoveryValue::HostRegister(crate::execution_abi::ARM64_FLAGS),
        RecoveryValue::HostRegister(9),
        RecoveryValue::HostRegister(11),
    );
    #[cfg(target_arch = "x86_64")]
    let (registers, flags, address, value) = (
        [
            RecoveryValue::HostRegister(0),
            RecoveryValue::HostRegister(10),
            RecoveryValue::HostRegister(11),
            RecoveryValue::HostRegister(3),
            RecoveryValue::Constant(0x54),
            RecoveryValue::HostRegister(5),
            RecoveryValue::HostRegister(12),
            RecoveryValue::HostRegister(13),
        ],
        RecoveryValue::HostRegister(14),
        RecoveryValue::HostRegister(6),
        RecoveryValue::HostRegister(2),
    );
    NativeRecoveryPoint {
        host_pc: if write {
            std::ptr::addr_of!(adv_posix_probe_store) as usize
        } else {
            std::ptr::addr_of!(adv_posix_probe_load) as usize
        },
        recovery_pc: std::ptr::addr_of!(adv_posix_probe_recovery) as usize,
        guest_pc: 20,
        next_pc: 24,
        width: 4,
        access: if write {
            RecoveryAccess::Write
        } else {
            RecoveryAccess::Read
        },
        proof: if write {
            StoreCommitProof::AlignedPrivateScalar
        } else {
            StoreCommitProof::LoadBeforeCommit
        },
        registers,
        flags,
        address,
        value,
        destination: (!write).then_some(2),
        committed_units: 3,
    }
}

fn region<'a>(owner: &'a dyn Any, points: &'a [NativeRecoveryPoint]) -> CodeRegion<'a> {
    let start = adv_posix_probe_start as *const () as usize;
    let end = std::ptr::addr_of!(adv_posix_probe_end) as usize;
    unsafe { CodeRegion::new(owner, start, end - start, points) }.unwrap()
}

#[test]
fn asynchronous_and_extended_signal_causes_are_refused() {
    unsafe {
        assert_eq!(adv_posix_fault_synchronous(libc::SIGSEGV, 1), 1);
        assert_eq!(adv_posix_fault_synchronous(libc::SIGSEGV, 2), 1);
        assert_eq!(adv_posix_fault_synchronous(libc::SIGBUS, 2), 1);
        // ARM asynchronous MTE (8), synchronous but unsupported MTE (9),
        // and action-required/action-optional machine checks (4/5).
        for code in [0, -1, -6, 3, 4, 8, 9] {
            assert_eq!(adv_posix_fault_synchronous(libc::SIGSEGV, code), 0);
        }
        for code in [0, -1, -6, 4, 5] {
            assert_eq!(adv_posix_fault_synchronous(libc::SIGBUS, code), 0);
        }
    }
}

#[test]
fn unsupported_sites_are_rejected_before_native_execution() {
    let mut site = point(false);
    let owner = ();
    assert!(valid(&site));
    for width in [0, 8, 16, 64] {
        site.width = width;
        assert!(!valid(&site));
    }
    site = point(false);
    site.proof = StoreCommitProof::AlignedPrivateScalar;
    assert!(!valid(&site));
    site = point(true);
    site.proof = StoreCommitProof::LoadBeforeCommit;
    assert!(!valid(&site));
    site = point(false);
    site.destination = Some(8);
    assert!(!valid(&site));
    site = point(false);
    site.registers[0] = RecoveryValue::HostRegister(GPR_COUNT as u8);
    assert!(!valid(&site));
    site = point(false);
    let duplicate = [site, site];
    let start = adv_posix_probe_start as *const () as usize;
    let end = std::ptr::addr_of!(adv_posix_probe_end) as usize;
    assert!(unsafe { CodeRegion::new(&owner, start, end - start, &duplicate) }.is_err());
    site.recovery_pc = end;
    let outside = [site];
    assert!(unsafe { CodeRegion::new(&owner, start, end - start, &outside) }.is_err());
}

#[test]
fn controlled_faults_preserve_prefix_and_continue() {
    if let Ok(case) = std::env::var("ADV_CONTROLLED_FAULT_CASE") {
        run_case(&case);
        return;
    }
    for case in [
        "mixed_load",
        "denied_load",
        "mixed_store",
        "denied_store",
        "code_store",
        "cross_page_read",
        "unknown_pc",
        "accessible_fault",
        "outside_guest",
        "outside_guest_bus",
        "unaligned_store",
        "async_signal",
        "async_ignore",
        "async_default",
    ] {
        let executable = std::env::current_exe().unwrap();
        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        let mut child = if let Some(runner) =
            std::env::var_os("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER")
        {
            let mut command = Command::new(runner);
            command.arg(&executable);
            command
        } else {
            Command::new(&executable)
        };
        #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
        let mut child = Command::new(&executable);
        let result = child
            .arg("--exact")
            .arg("fault_recovery::tests::controlled_faults_preserve_prefix_and_continue")
            .arg("--nocapture")
            .env("ADV_CONTROLLED_FAULT_CASE", case)
            .output()
            .unwrap();
        if case == "async_default" {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(result.status.signal(), Some(libc::SIGSEGV));
            continue;
        }
        assert!(
            result.status.success(),
            "controlled fault case {case} failed: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn fp_environment() -> (u32, u32) {
    #[cfg(target_arch = "aarch64")]
    {
        let control: u64;
        let status: u64;
        unsafe {
            core::arch::asm!("mrs {control}, fpcr", "mrs {status}, fpsr", control = out(reg) control, status = out(reg) status, options(nomem, nostack, preserves_flags));
        }
        (control as u32, status as u32)
    }
    #[cfg(target_arch = "x86_64")]
    {
        let mut control = 0u32;
        unsafe {
            core::arch::asm!("stmxcsr [{pointer}]", pointer = in(reg) &mut control, options(nostack, preserves_flags));
        }
        (control, 0)
    }
}

fn run_case(case: &str) {
    // Installed before the library's OnceLock so refusal demonstrably reaches
    // an existing SA_SIGINFO host handler and its own same-frame continuation.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = host_chain as *const () as usize;
    // Linux SA_EXPOSE_TAGBITS (0x800) may be absent from libc bindings.
    action.sa_flags = libc::SA_SIGINFO;
    #[cfg(not(target_os = "macos"))]
    {
        action.sa_flags |= 0x800;
    }
    if case == "async_default" || case == "async_ignore" {
        action.sa_sigaction = if case == "async_default" {
            libc::SIG_DFL
        } else {
            libc::SIG_IGN
        };
        action.sa_flags = 0;
    }
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGSEGV, &action, std::ptr::null_mut()),
            0
        );
        assert_eq!(
            libc::sigaction(libc::SIGBUS, &action, std::ptr::null_mut()),
            0
        );
    }
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut previous) },
        0
    );
    let expected_tag_flag = previous.sa_flags & 0x800;
    let write = case.ends_with("store");
    let mixed = case.starts_with("mixed_");
    let direct = if mixed {
        Reservation::new_direct_with_host_page_size_for_test(16384, 16384).unwrap()
    } else {
        Reservation::new_direct(16384).unwrap()
    };
    let page = GuestPageProtection::READ_WRITE;
    direct.commit_direct(0, 4096, page).unwrap();
    let offset = if case == "cross_page_read" {
        4094
    } else if case == "unaligned_store" {
        4097
    } else if mixed || case == "accessible_fault" || case == "outside_guest" {
        64
    } else {
        4096
    };
    let payload = 0x1122_3344u32;
    if mixed {
        let initial = if write { 0x6655_4433u32 } else { payload };
        direct
            .checked_write(offset, &initial.to_le_bytes())
            .unwrap();
    }
    if case == "denied_store" {
        direct
            .commit_direct(4096, 4096, GuestPageProtection::READ_WRITE)
            .unwrap();
        direct
            .checked_write(4096, &0x6655_4433u32.to_le_bytes())
            .unwrap();
        direct
            .protect_direct(4096, 4096, GuestPageProtection::READ)
            .unwrap();
    }
    if case == "cross_page_read" {
        direct.checked_write(4094, &[0x55, 0x66]).unwrap();
    }
    if case == "code_store" {
        direct
            .commit_direct(
                4096,
                4096,
                GuestPageProtection::new(crate::READ | crate::WRITE, true).unwrap(),
            )
            .unwrap();
    }
    if case == "accessible_fault" {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        if page > 4096 {
            direct
                .commit_direct(4096, page - 4096, GuestPageProtection::READ_WRITE)
                .unwrap();
        }
        // Deliberately create a host fault not explained by frozen guest
        // metadata. It must remain a host fault, even at a registered PC.
        assert_eq!(
            unsafe { libc::mprotect(direct.ptr.as_ptr().cast(), 4096, libc::PROT_NONE) },
            0
        );
    }
    let outside = (case == "outside_guest").then(|| Reservation::new(16384).unwrap());
    let bus = (case == "outside_guest_bus").then(|| {
        // Match the direct/PCM backends: Android's libc symbol needs API 30,
        // while the syscall also works with this fixture's API 29 link target.
        #[cfg(target_os = "android")]
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                c"adv-fault-probe".as_ptr(),
                libc::MFD_CLOEXEC,
            ) as libc::c_int
        };
        #[cfg(target_os = "linux")]
        let fd = unsafe { libc::memfd_create(c"adv-fault-probe".as_ptr(), libc::MFD_CLOEXEC) };
        #[cfg(target_os = "macos")]
        let fd = unsafe {
            let mut name = std::ffi::CString::new(
                std::env::temp_dir()
                    .join("adv-fault-XXXXXX")
                    .as_os_str()
                    .as_encoded_bytes(),
            )
            .unwrap()
            .into_bytes_with_nul();
            let fd = libc::mkstemp(name.as_mut_ptr().cast());
            libc::unlink(name.as_ptr().cast());
            fd
        };
        assert!(fd >= 0);
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        assert_eq!(unsafe { libc::ftruncate(fd, 16384) }, 0);
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                16384,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        assert_eq!(unsafe { libc::ftruncate(fd, 0) }, 0);
        (mapping, owned)
    });
    let native_window = bus.as_ref().map_or_else(
        || outside.as_ref().unwrap_or(&direct).ptr.as_ptr(),
        |(mapping, _)| mapping.cast(),
    );
    let points = [point(write)];
    let owner = ();
    let region = region(&owner, if case == "unknown_pc" { &[] } else { &points });
    let mut state = CpuState {
        regs: [0xcccc_cccc; 8],
        eip: 1,
        eflags: 2,
        fuel: 77,
        ..CpuState::default()
    };
    let original = state.clone();
    let mut prefix = 0;
    let host_environment = fp_environment();
    if case.starts_with("async_") {
        let result = unsafe {
            with_session(&mut state, &[region], &direct, 0, |_state| {
                libc::raise(libc::SIGSEGV)
            })
        }
        .unwrap();
        assert_eq!(result.result, 0);
        assert_eq!(result.recoveries, 0);
        assert_eq!(state, original);
        assert_ne!(
            case, "async_default",
            "default signal disposition was swallowed"
        );
        assert_eq!(
            CHAIN_COUNT.load(Ordering::Relaxed),
            usize::from(case == "async_signal")
        );
        if case == "async_signal" {
            assert_eq!(CHAIN_SIGNAL.load(Ordering::Relaxed), libc::SIGSEGV as usize);
        }
        return;
    }
    let result = unsafe {
        with_session(&mut state, &[region], &direct, 0, |_state| {
            adv_posix_probe_start(
                native_window,
                offset as u32,
                payload,
                &mut prefix,
                u32::from(write),
                recover,
            )
        })
    }
    .unwrap();
    let mut installed: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut installed) },
        0
    );
    assert_eq!(installed.sa_flags & 0x800, expected_tag_flag);
    if let Some((mapping, _)) = &bus {
        assert_eq!(unsafe { libc::munmap(*mapping, 16384) }, 0);
    }
    assert_eq!(fp_environment(), host_environment);
    assert_eq!(prefix, 1, "the already committed prefix must never replay");
    let unclaimed = matches!(
        case,
        "unknown_pc"
            | "accessible_fault"
            | "outside_guest"
            | "outside_guest_bus"
            | "unaligned_store"
    );
    if unclaimed {
        assert_eq!(CHAIN_COUNT.load(Ordering::Relaxed), 1);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            CHAIN_SIGNAL.load(Ordering::Relaxed),
            if case == "outside_guest_bus" {
                libc::SIGBUS
            } else {
                libc::SIGSEGV
            } as usize
        );
        #[cfg(target_os = "macos")]
        assert!(matches!(
            CHAIN_SIGNAL.load(Ordering::Relaxed) as i32,
            libc::SIGBUS | libc::SIGSEGV
        ));
        assert_eq!(result.result, 0xface);
        assert_eq!(result.recoveries, 0);
        assert_eq!(state, original);
        return;
    }
    assert_eq!(CHAIN_COUNT.load(Ordering::Relaxed), 0);
    assert_eq!(result.recoveries, 1);
    let outcome = result.last.unwrap();
    assert_eq!(outcome.committed_units, 3);
    assert_eq!(outcome.address, offset as u32);
    let mut expected = [0x8000_0000, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87];
    if case == "mixed_load" {
        expected[2] = payload;
    }
    assert_eq!(state.regs, expected);
    assert_eq!(state.eflags & 0x8d5, 0x894);
    assert_eq!(
        state.fuel, 77,
        "the materialized budget must not be charged twice"
    );
    assert_eq!(state.x87, original.x87);
    assert_eq!(state.xmm, original.xmm);
    assert_eq!(state.mxcsr, original.mxcsr);
    let snapshot = result.snapshot.unwrap();
    assert_eq!(snapshot.gpr_count, GPR_COUNT);
    assert_eq!(snapshot.simd_count, SIMD_COUNT);
    assert_eq!(snapshot.pc as usize, points[0].host_pc);
    assert_ne!(snapshot.sp, 0);
    #[cfg(target_arch = "aarch64")]
    {
        assert_eq!(snapshot.architecture, 2);
        assert_eq!(snapshot.flags & 0xf000_0000, 0x9000_0000);
        assert_eq!(snapshot.simd[16], [0x5a; 16]);
        assert_eq!(
            u64::from_le_bytes(snapshot.simd[8][..8].try_into().unwrap()),
            0x1234
        );
        assert_eq!(snapshot.fpcr, 0x400000);
        assert_eq!(snapshot.fpsr, 0x10);
    }
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(snapshot.architecture, 1);
        assert_eq!(snapshot.flags & 0x8d5, 0x894);
        assert_eq!(snapshot.simd[8], [0x5a; 16]);
        assert_eq!(snapshot.fpcr, 0x5f80);
    }
    if mixed {
        assert_eq!(outcome.kind, RecoveryKind::Completed);
        assert_eq!(state.eip, 24);
        assert_eq!(state.fault_addr, 0);
        assert_eq!(
            result.result,
            payload + 1,
            "native continuation after the recovery callback ran"
        );
        let mut observed = [0; 4];
        direct.checked_read(offset, &mut observed).unwrap();
        assert_eq!(u32::from_le_bytes(observed), payload);
        let view = direct.direct_fault_view().unwrap();
        assert_eq!(
            view.classify(
                direct.ptr.as_ptr() as usize + offset,
                if write {
                    GuestAccess::Write
                } else {
                    GuestAccess::Read
                }
            ),
            FaultClassification::MixedPage
        );
    } else {
        assert_eq!(
            outcome.kind,
            if case == "code_store" {
                RecoveryKind::CodeWriteProtection
            } else {
                RecoveryKind::AccessViolation
            }
        );
        assert_eq!(state.eip, 20);
        assert_eq!(state.fault_addr, offset as u32);
        assert_eq!(
            result.result,
            if case == "code_store" { 0xf101 } else { 0xf001 }
        );
        if case == "denied_store" {
            let mut observed = [0; 4];
            direct.checked_read(offset, &mut observed).unwrap();
            assert_eq!(u32::from_le_bytes(observed), 0x6655_4433);
        }
        if case == "cross_page_read" {
            let mut observed = [0; 2];
            direct.checked_read(4094, &mut observed).unwrap();
            assert_eq!(observed, [0x55, 0x66]);
        }
        if case == "code_store" {
            let mut observed = [0; 4];
            direct.checked_read(offset, &mut observed).unwrap();
            assert_eq!(
                observed, [0; 4],
                "the code-write barrier must not be bypassed"
            );
        }
    }
}

#[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
#[test]
fn inactive_sme_records_preserve_fpsimd_capture() {
    unsafe extern "C" {
        fn adv_posix_fault_test_context_layout(layout: *mut usize);
    }
    let mut layout = [0; 3];
    unsafe { adv_posix_fault_test_context_layout(layout.as_mut_ptr()) };
    let [size, offset, record_bytes] = layout;
    assert_eq!(record_bytes, 4096);
    assert!(offset + record_bytes <= size);
    // AArch64 u128 storage supplies the C context's 16-byte alignment. Query
    // Bionic/glibc for both offsets instead of trusting Rust libc's opaque tail
    // or its Android signal-mask layout to match the C signal boundary.
    let mut context = vec![0u128; size.div_ceil(16)];
    let records = unsafe {
        std::slice::from_raw_parts_mut(context.as_mut_ptr().cast::<u8>().add(offset), record_bytes)
    };
    records[..4].copy_from_slice(&0x4650_8001u32.to_ne_bytes());
    records[4..8].copy_from_slice(&528u32.to_ne_bytes());
    records[8..12].copy_from_slice(&0x10u32.to_ne_bytes());
    records[12..16].copy_from_slice(&0x0040_0000u32.to_ne_bytes());
    records[16..528].fill(0x5a);
    let mut control = 0;
    let mut status = 0;
    let mut vectors = [0u8; 512];
    // Cover hosts with no SME records as well as each record independently.
    for magics in [
        &[][..],
        &[0x5450_4902u32][..],
        &[0x5436_6345][..],
        &[0x5450_4902, 0x5436_6345][..],
    ] {
        records[528..].fill(0);
        for (index, magic) in magics.iter().enumerate() {
            let start = 528 + index * 16;
            records[start..start + 4].copy_from_slice(&magic.to_ne_bytes());
            records[start + 4..start + 8].copy_from_slice(&16u32.to_ne_bytes());
            // Nonzero TPIDR2 is preserved too; it is not vector register data.
            records[start + 8..start + 16].copy_from_slice(&64u64.to_ne_bytes());
        }
        assert_eq!(
            unsafe {
                adv_posix_fault_fp(
                    context.as_ptr().cast(),
                    &mut control,
                    &mut status,
                    vectors.as_mut_ptr(),
                )
            },
            1
        );
        assert_eq!((control, status), (0x0040_0000, 0x10));
        assert_eq!(vectors, [0x5a; 512]);
    }
    // A ZA payload means live matrix state, which this snapshot does not admit.
    records[552..556].copy_from_slice(&16u32.to_ne_bytes());
    records[548..552].copy_from_slice(&272u32.to_ne_bytes());
    assert_eq!(
        unsafe {
            adv_posix_fault_fp(
                context.as_ptr().cast(),
                &mut control,
                &mut status,
                vectors.as_mut_ptr(),
            )
        },
        0
    );
}
