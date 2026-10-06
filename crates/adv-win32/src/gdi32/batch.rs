//! GDI batching policy. Drawing is synchronous in this personality,
//! so the policy never defers a command or creates a second drawing queue.
use adv_abi::{Registry, win32};
use adv_machine::Env;

const DEFAULT_LIMIT: u32 = 20; // The current virtual desktop's batching profile.

struct Batches(u32);

#[win32(gdi32, name = "GdiGetBatchLimit", level = state)]
fn get(env: &mut Env<'_>) -> u32 {
    env.ext.try_get::<Batches>().map_or(DEFAULT_LIMIT, |b| b.0)
}

#[win32(gdi32, name = "GdiSetBatchLimit", level = state)]
fn set(env: &mut Env<'_>, requested: u32) -> u32 {
    if requested > DEFAULT_LIMIT {
        return 0;
    }
    let previous = get(env);
    // The native desktop fixture observes this limit shared by threads in the
    // process, despite the API documentation describing per-thread batches.
    env.ext.insert(Batches(if requested == 0 {
        DEFAULT_LIMIT
    } else {
        requested
    }));
    previous
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_ABI);
    registry.register(set_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    fn env(m: &mut Machine<JitCpu>, context: u32) -> Env<'_> {
        Env {
            context,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        }
    }

    #[test]
    fn batch_limit_state_is_shared_and_invalid_limits_do_not_change_it() {
        let mut registry = Registry::default();
        register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        assert_eq!(get(&mut env(&mut m, 0)), DEFAULT_LIMIT);
        assert_eq!(set(&mut env(&mut m, 0), 1), DEFAULT_LIMIT);
        assert_eq!(get(&mut env(&mut m, 0)), 1);
        assert_eq!(get(&mut env(&mut m, 1)), 1);
        assert_eq!(set(&mut env(&mut m, 1), 2), 1);
        assert_eq!(get(&mut env(&mut m, 0)), 2);
        assert_eq!(get(&mut env(&mut m, 1)), 2);
        for invalid in [DEFAULT_LIMIT + 1, 310, u32::MAX] {
            assert_eq!(set(&mut env(&mut m, 0), invalid), 0);
            assert_eq!(get(&mut env(&mut m, 0)), 2);
        }
        assert_eq!(set(&mut env(&mut m, 0), 0), 2);
        assert_eq!(get(&mut env(&mut m, 0)), DEFAULT_LIMIT);
        assert_eq!(get(&mut env(&mut m, 1)), DEFAULT_LIMIT);
    }

    #[cfg(windows)]
    #[test]
    fn native_batch_limits_restore_default_and_are_shared_across_threads() {
        #[link(name = "gdi32")]
        unsafe extern "system" {
            fn GdiGetBatchLimit() -> u32;
            fn GdiSetBatchLimit(limit: u32) -> u32;
        }
        struct Restore(u32);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    GdiSetBatchLimit(self.0);
                }
            }
        }
        unsafe {
            let _restore = Restore(GdiGetBatchLimit());
            GdiSetBatchLimit(0);
            assert_eq!(GdiGetBatchLimit(), DEFAULT_LIMIT);
            assert_eq!(GdiSetBatchLimit(1), DEFAULT_LIMIT);
            assert_eq!(GdiGetBatchLimit(), 1);
            std::thread::spawn(|| {
                assert_eq!(GdiGetBatchLimit(), 1);
                assert_eq!(GdiSetBatchLimit(2), 1);
                assert_eq!(GdiGetBatchLimit(), 2);
            })
            .join()
            .unwrap();
            for invalid in [DEFAULT_LIMIT + 1, 310, u32::MAX] {
                assert_eq!(GdiSetBatchLimit(invalid), 0);
                assert_eq!(GdiGetBatchLimit(), 2);
            }
            assert_eq!(GdiSetBatchLimit(0), 2);
            assert_eq!(GdiGetBatchLimit(), DEFAULT_LIMIT);
        }
    }
}
