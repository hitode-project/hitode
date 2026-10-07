//! Native loader locks use the same recursive mutex as module notifications.
use std::collections::BTreeMap;

use adv_abi::{GuestPtr, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{CtxId, Env, Object, Outcome, Wait};

#[derive(Default)]
struct Cookies {
    next: u32,
    owners: BTreeMap<u32, CtxId>,
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(lock_ABI);
    registry.register(unlock_ABI);
}

fn release(env: &mut Env<'_>, handle: u32) {
    if let Some(Object::Mutex { owner, recursion }) = env.sys.objects.get_mut(handle) {
        debug_assert_eq!(*owner, Some(env.context));
        *recursion -= 1;
        if *recursion == 0 {
            *owner = None;
        }
    }
}

fn acquired(env: &mut Env<'_>, handle: u32, disposition: u32, output: u32) -> Outcome {
    // A waiting caller can have lost its output mappings before waking.
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err()
        || (disposition != 0 && check_range(env.cpu, disposition, 4, Prot::WRITE).is_err())
    {
        release(env, handle);
        return Outcome::Return(0xc0000005);
    }
    let state = env.ext.get_or_default::<Cookies>();
    let Some(cookie) = state
        .next
        .checked_add(1)
        .filter(|_| state.owners.len() < 65536)
    else {
        release(env, handle);
        return Outcome::Return(0xc0000017);
    };
    if env.cpu.write_u32(output, cookie).is_err()
        || (disposition != 0 && env.cpu.write_u32(disposition, 1).is_err())
    {
        release(env, handle);
        return Outcome::Return(0xc0000005);
    }
    let state = env.ext.get_mut::<Cookies>();
    state.next = cookie;
    state.owners.insert(cookie, env.context);
    Outcome::Return(0)
}

#[win32(ntdll, name = "LdrLockLoaderLock", level = implement)]
fn lock(
    env: &mut Env<'_>,
    flags: u32,
    disposition: GuestPtr<u32>,
    cookie: GuestPtr<u32>,
) -> Outcome {
    if (!disposition.is_null() && check_range(env.cpu, disposition.addr(), 4, Prot::WRITE).is_err())
        || (!cookie.is_null() && check_range(env.cpu, cookie.addr(), 4, Prot::WRITE).is_err())
    {
        return Outcome::Return(0xc0000005);
    }
    if !disposition.is_null() {
        let _ = disposition.write(env.cpu, 0);
    }
    if !cookie.is_null() {
        let _ = cookie.write(env.cpu, 0);
    }
    if flags & !3 != 0 {
        return Outcome::Return(0xc00000ef);
    }
    if flags & 1 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!LdrLockLoaderLock",
            "RAISE_ON_ERRORS loader-lock profile is unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    if flags & 2 != 0 && disposition.is_null() {
        return Outcome::Return(0xc00000f0);
    }
    if cookie.is_null() {
        return Outcome::Return(0xc00000f1);
    }
    let Some(handle) = crate::kernel32::native_loader_mutex(env) else {
        return Outcome::Return(0xc0000017);
    };
    if env.sys.try_acquire_objects(&[handle], false).is_some() {
        return acquired(env, handle, disposition.addr(), cookie.addr());
    }
    if flags & 2 != 0 {
        let _ = disposition.write(env.cpu, 2);
        return Outcome::Return(0);
    }
    Outcome::block(
        Wait::Objects {
            handles: vec![handle],
            all: false,
            deadline: None,
            notification: None,
        },
        move |env, _| acquired(env, handle, disposition.addr(), cookie.addr()),
    )
}

#[win32(ntdll, name = "LdrUnlockLoaderLock", level = implement)]
fn unlock(env: &mut Env<'_>, flags: u32, cookie: GuestPtr<()>) -> i32 {
    if flags & !1 != 0 {
        return 0xc00000efu32 as i32;
    }
    if flags != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!LdrUnlockLoaderLock",
            "RAISE_ON_ERRORS loader-unlock profile is unavailable",
        );
        return 0xc00000bbu32 as i32;
    }
    if cookie.is_null() {
        return 0;
    }
    let owner = env
        .ext
        .get_or_default::<Cookies>()
        .owners
        .get(&cookie.addr())
        .copied();
    if owner != Some(env.context) {
        return 0xc00000f0u32 as i32;
    }
    let Some(handle) = env.ext.get::<crate::Win32>().loader_lock else {
        return 0xc00000f0u32 as i32;
    };
    if !matches!(env.sys.objects.get(handle), Some(Object::Mutex { owner: Some(owner), recursion }) if *owner == env.context && *recursion != 0)
    {
        return 0xc00000f0u32 as i32;
    }
    release(env, handle);
    env.ext.get_mut::<Cookies>().owners.remove(&cookie.addr());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR, outcome::Resume};

    const CODE: u32 = 0x400000;
    const DATA: u32 = 0x410000;

    fn fixture() -> Machine<JitCpu> {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, true).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        crate::Win32::install(
            &mut m,
            crate::Win32Config::new(
                "/nonexistent/ldr-lock",
                "fixture.exe",
                "/nonexistent/ldr-lock-data",
            ),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        m.spawn("owner", CODE, &[], 65536).unwrap();
        m.spawn("waiter", CODE, &[], 65536).unwrap();
        m
    }

    fn in_thread(
        m: &mut Machine<JitCpu>,
        context: CtxId,
        call: impl FnOnce(&mut Env<'_>) -> Outcome,
    ) -> Outcome {
        m.sys.current = context;
        call(&mut Env {
            context,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        })
    }

    fn acquire(m: &mut Machine<JitCpu>, context: CtxId, flags: u32) -> Outcome {
        in_thread(m, context, |env| {
            lock(env, flags, GuestPtr::new(DATA), GuestPtr::new(DATA + 4))
        })
    }

    fn drop_cookie(m: &mut Machine<JitCpu>, context: CtxId, cookie: u32) -> u32 {
        let result = in_thread(m, context, |env| {
            Outcome::Return(unlock(env, 0, GuestPtr::new(cookie)) as u32)
        });
        let Outcome::Return(result) = result else {
            panic!("unlock did not return")
        };
        result
    }

    #[test]
    fn loader_cookie_ownership_recursion_and_wait_share_the_module_mutex() {
        let mut m = fixture();
        assert!(matches!(acquire(&mut m, 0, 0), Outcome::Return(0)));
        let first = m.cpu.read_u32(DATA + 4).unwrap();
        assert!(matches!(acquire(&mut m, 0, 2), Outcome::Return(0)));
        let second = m.cpu.read_u32(DATA + 4).unwrap();
        let handle = m.ext.get::<crate::Win32>().loader_lock.unwrap();
        assert_ne!(first, second);
        assert!(
            matches!(in_thread(&mut m, 0, |env| Outcome::Return(crate::kernel32::native_loader_mutex(env).unwrap())), Outcome::Return(value) if value == handle)
        );
        assert_eq!(drop_cookie(&mut m, 1, first), 0xc00000f0);
        assert!(matches!(acquire(&mut m, 1, 2), Outcome::Return(0)));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 2);
        assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
        let Outcome::Block { wait, then, .. } = acquire(&mut m, 1, 0) else {
            panic!("contended lock did not wait")
        };
        assert!(matches!(wait, Wait::Objects { handles, .. } if handles == [handle]));
        assert_eq!(drop_cookie(&mut m, 0, second), 0);
        m.sys.current = 1;
        assert!(m.sys.try_acquire_objects(&[handle], false).is_none());
        assert_eq!(drop_cookie(&mut m, 0, first), 0);
        m.sys.current = 1;
        let wake = m.sys.try_acquire_objects(&[handle], false).unwrap();
        assert!(matches!(
            in_thread(&mut m, 1, |env| then(env, Resume::Woke(wake))),
            Outcome::Return(0)
        ));
        let third = m.cpu.read_u32(DATA + 4).unwrap();
        assert_eq!(drop_cookie(&mut m, 1, third), 0);
        assert_eq!(drop_cookie(&mut m, 1, third), 0xc00000f0);
        assert!(matches!(
            m.sys.objects.get(handle),
            Some(Object::Mutex {
                owner: None,
                recursion: 0
            })
        ));
    }

    #[test]
    fn waiting_loader_lock_rechecks_output_and_releases_failed_acquisition() {
        let mut m = fixture();
        assert!(matches!(acquire(&mut m, 0, 0), Outcome::Return(0)));
        let cookie = m.cpu.read_u32(DATA + 4).unwrap();
        let Outcome::Block { then, .. } = acquire(&mut m, 1, 0) else {
            panic!("lock did not wait")
        };
        assert_eq!(drop_cookie(&mut m, 0, cookie), 0);
        let handle = m.ext.get::<crate::Win32>().loader_lock.unwrap();
        m.cpu.protect(DATA, 4096, Prot::READ).unwrap();
        m.sys.current = 1;
        let wake = m.sys.try_acquire_objects(&[handle], false).unwrap();
        assert!(matches!(
            in_thread(&mut m, 1, |env| then(env, Resume::Woke(wake))),
            Outcome::Return(0xc0000005)
        ));
        assert!(matches!(
            m.sys.objects.get(handle),
            Some(Object::Mutex {
                owner: None,
                recursion: 0
            })
        ));
        assert!(m.ext.get::<Cookies>().owners.is_empty());
    }
}
