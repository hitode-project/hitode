use super::*;
use crate::misc::test_support::{DATA, Fixture, env};
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::Outcome;

const CURRENT_THREAD: u32 = u32::MAX - 1;

#[test]
// spec: WIN32-API-6
fn ideal_processor_variants_share_the_one_cpu_affinity_profile() {
    let mut f = Fixture::new("ideal-processor-profile");
    let thread = f.m.sys.contexts[0].thread_handle;
    for handle in [CURRENT_THREAD, thread] {
        crate::util::set_last_error(&mut env(&mut f.m), 4660);
        assert_eq!(
            f.call("kernel32.dll", "SetThreadIdealProcessor", &[handle, 0]),
            0
        );
        assert_eq!(
            f.call("kernel32.dll", "SetThreadIdealProcessor", &[handle, 32]),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut f.m)), 4660);
        f.m.cpu.write(DATA, &[0xa5; 8]).unwrap();
        assert_eq!(
            f.call("kernel32.dll", "GetThreadIdealProcessorEx", &[handle, DATA]),
            1
        );
        assert_eq!(f.m.cpu.read_u32(DATA).unwrap(), 0);
        assert_eq!(f.m.cpu.read_u32(DATA + 4).unwrap(), 0xa5a5a5a5);
        assert_eq!(
            f.call(
                "kernel32.dll",
                "SetThreadIdealProcessorEx",
                &[handle, DATA, DATA]
            ),
            1
        );
        assert_eq!(
            f.call(
                "kernel32.dll",
                "SetThreadIdealProcessorEx",
                &[handle, DATA, 0]
            ),
            1
        );
        assert_eq!(f.m.cpu.read_u32(DATA).unwrap(), 0);
        assert_eq!(f.call("kernel32.dll", "GetCurrentProcessorNumber", &[]), 0);
        assert_eq!(
            f.call("kernel32.dll", "SetThreadAffinityMask", &[handle, 1]),
            1
        );
    }
    for number in [1, 31, 33, u32::MAX] {
        assert_eq!(
            f.call(
                "kernel32.dll",
                "SetThreadIdealProcessor",
                &[CURRENT_THREAD, number]
            ),
            u32::MAX
        );
        assert_eq!(crate::util::last_error(&env(&mut f.m)), 87);
    }
    for packed in [1, 0x10000, 0xff0000, 0x01000000, u32::MAX] {
        f.m.cpu.write_u32(DATA, packed).unwrap();
        f.m.cpu.write_u32(DATA + 8, 0xa5a5a5a5).unwrap();
        assert_eq!(
            f.call(
                "kernel32.dll",
                "SetThreadIdealProcessorEx",
                &[CURRENT_THREAD, DATA, DATA + 8]
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut f.m)), 87);
        assert_eq!(f.m.cpu.read_u32(DATA + 8).unwrap(), 0xa5a5a5a5);
    }
    f.cleanup();
}

#[test]
fn thread_identity_permissions_duplicates_and_exit_keep_the_same_query() {
    let mut f = Fixture::new("ideal-processor-owners");
    let identity = f.m.sys.thread_identity(0).unwrap();
    let query = f.m.sys.objects.insert(Object::RemoteThread {
        identity: identity.clone(),
        access: 0x800,
    });
    let set = f.m.sys.objects.insert(Object::RemoteThread {
        identity,
        access: 0x20,
    });
    let duplicate = f.m.sys.objects.duplicate(query).unwrap();
    f.m.sys.objects.close(query);
    f.m.cpu.write_u32(DATA, 0).unwrap();
    assert_eq!(
        f.call(
            "kernel32.dll",
            "GetThreadIdealProcessorEx",
            &[duplicate, DATA]
        ),
        1
    );
    assert_eq!(
        f.call("kernel32.dll", "GetThreadIdealProcessorEx", &[set, DATA]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut f.m)), 5);
    assert_eq!(
        f.call("kernel32.dll", "SetThreadIdealProcessor", &[duplicate, 0]),
        u32::MAX
    );
    assert_eq!(crate::util::last_error(&env(&mut f.m)), 5);
    assert_eq!(
        f.call(
            "kernel32.dll",
            "SetThreadIdealProcessorEx",
            &[duplicate, DATA, 0]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut f.m)), 5);
    assert_eq!(
        f.call("kernel32.dll", "SetThreadIdealProcessorEx", &[set, DATA, 0]),
        1
    );
    f.m.sys.terminate_context_stopped(0, 0);
    assert_eq!(
        f.call(
            "kernel32.dll",
            "GetThreadIdealProcessorEx",
            &[duplicate, DATA]
        ),
        1
    );
    assert_eq!(
        f.call("kernel32.dll", "SetThreadIdealProcessor", &[set, 32]),
        0
    );
    f.m.sys.objects.close(duplicate);
    assert_eq!(
        f.call(
            "kernel32.dll",
            "GetThreadIdealProcessorEx",
            &[duplicate, DATA]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut f.m)), 6);
    assert_eq!(
        f.call("kernel32.dll", "SetThreadIdealProcessor", &[0, 0]),
        u32::MAX
    );
    assert_eq!(crate::util::last_error(&env(&mut f.m)), 6);
    f.cleanup();
}

#[test]
fn getter_reports_noaccess_but_setter_pointer_faults_cannot_return_success() {
    let mut f = Fixture::new("ideal-processor-memory");
    for pointer in [0, u32::MAX - 1] {
        assert_eq!(
            f.call(
                "kernel32.dll",
                "GetThreadIdealProcessorEx",
                &[CURRENT_THREAD, pointer]
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut f.m)), 998);
        crate::util::set_last_error(&mut env(&mut f.m), 4660);
        assert!(matches!(
            f.invoke(
                "kernel32.dll",
                "SetThreadIdealProcessorEx",
                &[CURRENT_THREAD, pointer, 0]
            ),
            Outcome::Fail(_)
        ));
        assert_eq!(crate::util::last_error(&env(&mut f.m)), 4660);
    }
    f.m.cpu.write_u32(DATA, 0).unwrap();
    f.m.cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    f.m.cpu.write_u32(DATA + 4092, 0xa5a5a5a5).unwrap();
    assert_eq!(
        f.call(
            "kernel32.dll",
            "GetThreadIdealProcessorEx",
            &[CURRENT_THREAD, DATA + 4094]
        ),
        0
    );
    assert_eq!(f.m.cpu.read_u32(DATA + 4092).unwrap(), 0xa5a5a5a5);
    assert!(matches!(
        f.invoke(
            "kernel32.dll",
            "SetThreadIdealProcessorEx",
            &[CURRENT_THREAD, DATA, DATA + 4094]
        ),
        Outcome::Fail(_)
    ));
    assert_eq!(f.m.cpu.read_u32(DATA + 4092).unwrap(), 0xa5a5a5a5);
    f.cleanup();
}
