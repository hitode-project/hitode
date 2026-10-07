use super::*;
use adv_cpu::{JitCpu, backend::Backend};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

fn check<B: Backend>(make: impl Fn() -> B) {
    for pending in [false, true] {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(make(), registry, Clock::virtual_time()).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/nt-termination-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "fixture.exe", &root),
        )
        .unwrap();
        machine.spawn("wait caller", PARK_ADDR, &[], 65536).unwrap();
        if pending {
            assert!(
                machine
                    .sys
                    .process_identity
                    .completion
                    .begin_termination(43)
            );
        }
        let objects = machine.sys.objects.object_count();
        let mut env = Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        set_last_error(&mut env, 0x13572468);
        for handle in [u32::MAX, u32::MAX - 1] {
            assert!(matches!(
                wait_one(&mut env, handle, 0),
                Outcome::Return(WAIT_TIMEOUT)
            ));
            assert_eq!(crate::util::last_error(&env), 0x13572468);
            for ms in [3, INFINITE] {
                let now = env.sys.clock.now_us();
                let Outcome::Block {
                    wait,
                    then,
                    allow_time_skip,
                } = wait_one(&mut env, handle, ms)
                else {
                    panic!("a live caller must remain blocked on its own completion");
                };
                assert_eq!(
                    wait,
                    Wait::Objects {
                        handles: vec![handle],
                        all: false,
                        deadline: (ms != INFINITE).then_some(now + 3000),
                        notification: None,
                    }
                );
                assert!(!allow_time_skip);
                if ms != INFINITE {
                    assert!(matches!(
                        then(&mut env, Resume::Woke(WakeReason::Timeout)),
                        Outcome::Return(WAIT_TIMEOUT)
                    ));
                }
                assert_eq!(crate::util::last_error(&env), 0x13572468);
            }
        }
        assert_eq!(env.sys.objects.object_count(), objects);
        assert!(matches!(
            wait_one(&mut env, 0x12345678, 0),
            Outcome::Return(WAIT_FAILED)
        ));
        assert_eq!(crate::util::last_error(&env), err::INVALID_HANDLE);
        let denied = env.sys.objects.insert(Object::Process {
            identity: env.sys.process_identity.clone(),
            access: 1,
        });
        assert!(matches!(
            wait_one(&mut env, denied, 0),
            Outcome::Return(WAIT_FAILED)
        ));
        assert_eq!(crate::util::last_error(&env), err::ACCESS_DENIED);
    }
}

#[test]
fn current_pseudo_waits_preserve_live_owner_and_pending_status() {
    for jit in [false, true] {
        check(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    check(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
