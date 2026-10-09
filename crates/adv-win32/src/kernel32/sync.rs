//! Synchronization. API object ownership stays on the controller; guest atomics
//! synchronize with native leases, and contended locks park until release.

use std::{cell::Cell, collections::HashMap, rc::Rc};

use adv_abi::{AnsiStr, Registry, WideStr, win32};
use adv_cpu::backend::{Guest, GuestExt, MemFault};
use adv_machine::{Env, Object, Outcome, Resume, Wait, WakeReason};

use super::K32;
use crate::util::{err, set_last_error};
use crate::{Win32, stdcall};

mod event;
mod local;
pub(crate) mod native;
mod owned;
pub(crate) use local::detach_fls;
#[cfg(test)]
mod event_tests;
#[cfg(test)]
mod wait_tests;

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const WAIT_FAILED: u32 = u32::MAX;
const INFINITE: u32 = u32::MAX;

// CRITICAL_SECTION (x86): DebugInfo, LockCount, RecursionCount, OwningThread, LockSemaphore, SpinCount.
// LockCount follows Windows: -1 when free, plus one for every hold (recursion
// included) and every waiter. One locked update decides each transition, so the
// guest code for Enter/Leave (`guest_code`) and these handlers interoperate.
const CS_LOCK_COUNT: u32 = 4;
const CS_RECURSION: u32 = 8;
const CS_OWNER: u32 = 12;
pub(crate) const CS_SIZE: u32 = 24;

/// Guest EnterCriticalSection has counted the caller and found another owner.
pub(crate) const CS_WAIT: &str = "<EnterCriticalSectionWait>";
/// Guest LeaveCriticalSection released the section while others were counted.
pub(crate) const CS_WAKE: &str = "<LeaveCriticalSectionWake>";

#[derive(Default)]
struct CriticalWaits(HashMap<u32, CriticalWait>);

struct CriticalWait {
    event: u32,
    pending: Rc<Cell<usize>>,
}

/// Continuation cancellation (including thread exit) also drops its wait count.
struct CriticalTicket(Rc<Cell<usize>>);
impl Drop for CriticalTicket {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

pub(crate) fn release_cs_wait(env: &mut Env<'_>, cs: u32) -> Result<(), String> {
    let waits = env.ext.get_or_default::<CriticalWaits>();
    if waits.0.get(&cs).is_some_and(|wait| wait.pending.get() != 0) {
        return Err(format!(
            "critical section {cs:#x} deleted or initialized with live waiters"
        ));
    }
    if let Some(wait) = waits.0.remove(&cs) {
        env.sys.objects.close(wait.event);
    }
    Ok(())
}

pub(crate) fn initialize_cs(cpu: &mut dyn Guest, cs: u32) -> Result<(), MemFault> {
    cpu.write(cs, &[0; CS_SIZE as usize])?;
    cpu.write_u32(cs + CS_LOCK_COUNT, u32::MAX)
}

fn init_cs(env: &mut Env<'_>) -> Outcome {
    let cs = env.arg(0);
    if let Err(error) = release_cs_wait(env, cs) {
        return Outcome::Fail(error);
    }
    let _ = initialize_cs(env.cpu, cs);
    Outcome::Return(1)
}

/// One lazily allocated auto-reset event per contended critical section. A
/// release signals it only while somebody is counted, and nobody can take the
/// section before that waiter does, so at most one signal is ever pending.
fn cs_event(env: &mut Env<'_>, cs: u32) -> Option<u32> {
    let waits = env.ext.get_or_default::<CriticalWaits>();
    if let Some(wait) = waits.0.get(&cs) {
        return Some(wait.event);
    }
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    if event == 0 {
        return None;
    }
    waits.0.insert(
        cs,
        CriticalWait {
            event,
            pending: Rc::new(Cell::new(0)),
        },
    );
    Some(event)
}

fn cs_taken(env: &mut Env<'_>, cs: u32, recursion: u32) {
    env.sys.schedule_trace.event(
        "cs_take",
        env.context,
        u64::from(cs),
        u64::from(recursion),
        0,
    );
}

/// The caller is already counted in LockCount; the release that wakes it hands
/// the section over.
fn wait_cs(env: &mut Env<'_>, cs: u32) -> Outcome {
    let Some(event) = cs_event(env, cs) else {
        return Outcome::Fail("critical section wait resource limit exhausted".into());
    };
    let wait = &env.ext.get_or_default::<CriticalWaits>().0[&cs];
    wait.pending.set(wait.pending.get() + 1);
    let ticket = CriticalTicket(wait.pending.clone());
    if env.sys.schedule_trace.enabled() {
        env.sys.schedule_trace.event(
            "cs_wait",
            env.context,
            u64::from(cs),
            u64::from(env.cpu.read_u32(cs + CS_OWNER).unwrap_or(0)),
            u64::from(event),
        );
    }
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline: None,
        },
        move |env, _| {
            drop(ticket);
            if env
                .ext
                .get_or_default::<CriticalWaits>()
                .0
                .get(&cs)
                .is_none_or(|wait| wait.event != event)
            {
                return Outcome::Fail(format!("critical section {cs:#x} changed while waiting"));
            }
            let tid = env.ctx().tid;
            let _ = env.cpu.write_u32(cs + CS_OWNER, tid);
            let _ = env.cpu.write_u32(cs + CS_RECURSION, 1);
            cs_taken(env, cs, 1);
            Outcome::Return(0)
        },
    )
}

fn wake_cs(env: &mut Env<'_>, cs: u32) -> Outcome {
    match cs_event(env, cs) {
        Some(event) => {
            env.sys.set_event(event);
            Outcome::Return(0)
        }
        None => Outcome::Fail("critical section wait resource limit exhausted".into()),
    }
}

pub(crate) fn enter_cs(env: &mut Env<'_>, cs: u32) -> Outcome {
    // Guest code on other workers updates the same fields with locked instructions.
    env.cpu.quiesce_memory();
    let tid = env.ctx().tid;
    let lock = env.cpu.read_u32(cs + CS_LOCK_COUNT).unwrap_or(u32::MAX);
    let _ = env.cpu.write_u32(cs + CS_LOCK_COUNT, lock.wrapping_add(1));
    if lock == u32::MAX {
        let _ = env.cpu.write_u32(cs + CS_OWNER, tid);
        let _ = env.cpu.write_u32(cs + CS_RECURSION, 1);
        cs_taken(env, cs, 1);
        return Outcome::Return(0);
    }
    if env.cpu.read_u32(cs + CS_OWNER).unwrap_or(0) == tid {
        let rec = env.cpu.read_u32(cs + CS_RECURSION).unwrap_or(0) + 1;
        let _ = env.cpu.write_u32(cs + CS_RECURSION, rec);
        cs_taken(env, cs, rec);
        return Outcome::Return(0);
    }
    wait_cs(env, cs)
}

fn try_enter_cs(env: &mut Env<'_>, cs: u32) -> bool {
    env.cpu.quiesce_memory();
    let tid = env.ctx().tid;
    let lock = env.cpu.read_u32(cs + CS_LOCK_COUNT).unwrap_or(0);
    let rec = if lock == u32::MAX {
        let _ = env.cpu.write_u32(cs + CS_OWNER, tid);
        1
    } else if env.cpu.read_u32(cs + CS_OWNER).unwrap_or(0) == tid {
        env.cpu.read_u32(cs + CS_RECURSION).unwrap_or(0) + 1
    } else {
        return false;
    };
    let _ = env.cpu.write_u32(cs + CS_LOCK_COUNT, lock.wrapping_add(1));
    let _ = env.cpu.write_u32(cs + CS_RECURSION, rec);
    cs_taken(env, cs, rec);
    true
}

pub(crate) fn leave_cs(env: &mut Env<'_>, cs: u32) -> Outcome {
    env.cpu.quiesce_memory();
    let rec = env.cpu.read_u32(cs + CS_RECURSION).unwrap_or(0);
    if rec == 0 {
        log::warn!(
            "LeaveCriticalSection({cs:#x}) not held, from {:#010x}",
            env.caller()
        );
        return Outcome::Return(0);
    }
    let lock = env
        .cpu
        .read_u32(cs + CS_LOCK_COUNT)
        .unwrap_or(0)
        .wrapping_sub(1);
    let _ = env.cpu.write_u32(cs + CS_RECURSION, rec - 1);
    if rec == 1 {
        let _ = env.cpu.write_u32(cs + CS_OWNER, 0);
    }
    let _ = env.cpu.write_u32(cs + CS_LOCK_COUNT, lock);
    env.sys.schedule_trace.event(
        "cs_release",
        env.context,
        u64::from(cs),
        u64::from(rec - 1),
        0,
    );
    if rec == 1 && (lock as i32) >= 0 {
        return wake_cs(env, cs);
    }
    Outcome::Return(0)
}

fn interlocked(env: &mut Env<'_>, f: impl Fn(u32, u32, u32) -> (u32, u32)) -> Outcome {
    env.cpu.quiesce_memory();
    let (p, a, b) = (env.arg(0), env.arg(1), env.arg(2));
    let old = env.cpu.read_u32(p).unwrap_or(0);
    let (new, ret) = f(old, a, b);
    let _ = env.cpu.write_u32(p, new);
    Outcome::Return(ret)
}

fn wait_result(r: Resume) -> Outcome {
    Outcome::Return(match r {
        Resume::Woke(WakeReason::Signaled) => WAIT_OBJECT_0,
        Resume::Woke(WakeReason::Objects { index, abandoned }) => {
            index + if abandoned { 0x80 } else { WAIT_OBJECT_0 }
        }
        _ => WAIT_TIMEOUT,
    })
}

fn wait_one(env: &mut Env<'_>, h: u32, ms: u32) -> Outcome {
    // A live caller cannot observe its own process/thread completion. Pseudo
    // handles need no table alias and retain the ordinary timeout/block path,
    // even when NULL process termination has published a pending exit status.
    let current = h == u32::MAX || h == u32::MAX - 1;
    if !current
        && let Some(Object::Process { access, .. } | Object::RemoteThread { access, .. }) =
            env.sys.objects.get(h)
        && access & 0x10_0000 == 0
    {
        set_last_error(env, err::ACCESS_DENIED);
        return Outcome::Return(WAIT_FAILED);
    }
    if !current
        && !matches!(
            env.sys.objects.get(h),
            Some(
                Object::Event { .. }
                    | Object::Timer { .. }
                    | Object::Mutex { .. }
                    | Object::Semaphore { .. }
                    | Object::Thread { .. }
                    | Object::Process { .. }
                    | Object::RemoteThread { .. }
            )
        )
    {
        log::warn!("wait on unknown handle {h:#x} from {:#010x}", env.caller());
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(WAIT_FAILED);
    }
    if !current && let Some(reason) = env.sys.try_acquire_objects(&[h], false) {
        return wait_result(Resume::Woke(reason));
    }
    if ms == 0 {
        return Outcome::Return(WAIT_TIMEOUT);
    }
    let deadline = (ms != INFINITE).then(|| env.sys.clock.now_us() + u64::from(ms) * 1000);
    Outcome::block(
        Wait::Objects {
            handles: vec![h],
            all: false,
            deadline,
            notification: None,
        },
        |_, r| wait_result(r),
    )
}

fn wait_multiple(env: &mut Env<'_>) -> Outcome {
    native::multiple_front(env, false)
}

fn create_event(env: &mut Env<'_>) -> Outcome {
    event::create_front(env)
}

#[win32(kernel32, name = "OpenEventA", level = implement)]
fn open_event_a(env: &mut Env<'_>, _access: u32, _inherit: i32, name: AnsiStr) -> Outcome {
    event::open_front(env, name.0.addr(), false)
}

#[win32(kernel32, name = "OpenEventW", level = implement)]
fn open_event_w(env: &mut Env<'_>, _access: u32, _inherit: i32, name: WideStr) -> Outcome {
    event::open_front(env, name.0.addr(), true)
}

fn create_semaphore(env: &mut Env<'_>) -> Outcome {
    owned::create_front(env, owned::Kind::Semaphore)
}

fn release_semaphore(env: &mut Env<'_>) -> Outcome {
    owned::release_front(env, owned::Kind::Semaphore)
}

fn create_mutex(env: &mut Env<'_>) -> Outcome {
    owned::create_front(env, owned::Kind::Mutant)
}

#[win32(kernel32, name = "OpenMutexA", level = implement)]
fn open_mutex_a(env: &mut Env<'_>, _access: u32, _inherit: i32, _name: AnsiStr) -> Outcome {
    owned::open_front(env, owned::Kind::Mutant)
}

#[win32(kernel32, name = "OpenMutexW", level = implement)]
fn open_mutex_w(env: &mut Env<'_>, _access: u32, _inherit: i32, _name: WideStr) -> Outcome {
    owned::open_front(env, owned::Kind::Mutant)
}

fn release_mutex(env: &mut Env<'_>) -> Outcome {
    owned::release_front(env, owned::Kind::Mutant)
}

/// SLIST_HEADER (x86): `Next`, then `Depth` and `Sequence` u16s. The interlocked
/// entry points quiesce native leases before updating the list.
fn slist(
    env: &mut Env<'_>,
    head: u32,
    f: impl FnOnce(&mut Env<'_>, u32, u16) -> (u32, u16, u32),
) -> Outcome {
    let next = env.cpu.read_u32(head).unwrap_or(0);
    let ds = env.cpu.read_u32(head + 4).unwrap_or(0);
    let (next2, depth2, ret) = f(env, next, ds as u16);
    let seq2 = (ds >> 16).wrapping_add(1) & 0xffff;
    let _ = env.cpu.write_u32(head, next2);
    let _ = env
        .cpu
        .write_u32(head + 4, u32::from(depth2) | (seq2 << 16));
    Outcome::Return(ret)
}

#[cfg(all(test, windows, target_arch = "x86_64", target_env = "msvc"))]
mod critical_tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Prot, asm::Asm},
    };
    use adv_machine::{Clock, Conv, Machine, RunResult, SliceResult, layout::PARK_ADDR};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Observed {
        event: u32,
        acquired: Vec<u32>,
    }

    #[test]
    fn contended_critical_sections_park_wake_recurse_and_release_their_event() {
        const CODE: u32 = 0x400000;
        const CS: u32 = 0x410000;
        fn hold_until_waiters(env: &mut Env<'_>) -> Outcome {
            let ready = env.ext.get_or_default::<CriticalWaits>().0.get(&CS).is_some_and(|wait| {
                wait.pending.get() == 2
                    && env.sys.contexts.iter().filter(|context| matches!(context.waiting_on(), Some(Wait::Object { handle, .. }) if *handle == wait.event)).count() == 2
            });
            if ready {
                Outcome::Return(0)
            } else {
                Outcome::block(Wait::Yield, |env, _| hold_until_waiters(env))
            }
        }
        let mut registry = Registry::default();
        register(&mut registry);
        registry.add(
            "test",
            "hold",
            Conv::Stdcall { argc: 0 },
            hold_until_waiters,
        );
        registry.add("test", "probe", Conv::Stdcall { argc: 0 }, |e| {
                let wait = &e.ext.get_or_default::<CriticalWaits>().0[&CS];
                assert_eq!(wait.pending.get(), 2);
                let event = wait.event;
                assert_eq!(e.cpu.read_u32(CS + CS_RECURSION).unwrap(), 1);
                assert_eq!(e.sys.contexts.iter().filter(|c| matches!(c.waiting_on(), Some(Wait::Object {handle, ..}) if *handle == event)).count(), 2);
                e.ext.get_mut::<Observed>().event = event;
                Outcome::Return(0)
            });
        registry.add("test", "acquired", Conv::Stdcall { argc: 0 }, |e| {
            assert_eq!(e.cpu.read_u32(CS + CS_OWNER).unwrap(), e.ctx().tid);
            e.ext.get_mut::<Observed>().acquired.push(e.context);
            Outcome::Return(0)
        });
        registry.add("test", "cleanup", Conv::Stdcall { argc: 0 }, |e| {
            if e.ext.get::<Observed>().acquired.len() == 2 {
                release_cs_wait(e, CS).unwrap();
            }
            Outcome::ExitThread(0)
        });
        registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitThread(0)
        });
        let cpu = JitCpu::new(PARK_ADDR, true).unwrap();
        let mut m = Machine::new(cpu, registry, Clock::real()).unwrap();
        m.ext.insert(Observed::default());
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(CS, 4096, Prot::RW).unwrap();
        m.set_frame_handoff(false);
        let init = m.resolve_import(K32, "InitializeCriticalSection").unwrap();
        let enter = m.resolve_import(K32, "EnterCriticalSection").unwrap();
        let leave = m.resolve_import(K32, "LeaveCriticalSection").unwrap();
        let hold = m.resolve_import("test", "hold").unwrap();
        let probe = m.resolve_import("test", "probe").unwrap();
        let acquired = m.resolve_import("test", "acquired").unwrap();
        let cleanup = m.resolve_import("test", "cleanup").unwrap();
        let done = m.resolve_import("test", "done").unwrap();
        let mut owner = Asm::new(CODE);
        owner
            .push_imm(CS)
            .call(init)
            .push_imm(CS)
            .call(enter)
            .push_imm(CS)
            .call(enter)
            .call(hold)
            .push_imm(CS)
            .call(leave)
            .call(probe)
            .push_imm(CS)
            .call(leave)
            .call(done)
            .hlt();
        m.cpu.write(CODE, &owner.bytes).unwrap();
        m.spawn("owner", CODE, &[], 65536).unwrap();
        for index in 1..=2 {
            let address = CODE + index * 256;
            let mut waiter = Asm::new(address);
            waiter
                .push_imm(CS)
                .call(enter)
                .call(acquired)
                .push_imm(CS)
                .call(leave)
                .call(cleanup)
                .hlt();
            m.cpu.write(address, &waiter.bytes).unwrap();
            m.spawn("waiter", address, &[], 65536).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline);
            match m.run_slice(Duration::from_millis(8)) {
                SliceResult::Complete(RunResult::Exited(_)) => break,
                SliceResult::Complete(RunResult::Failed(error)) => panic!("{error}"),
                SliceResult::Complete(RunResult::Idle { .. }) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                _ => {}
            }
        }
        let observed = m.ext.get::<Observed>();
        assert_eq!(observed.acquired.len(), 2);
        assert_ne!(observed.acquired[0], observed.acquired[1]);
        assert!(m.sys.objects.get(observed.event).is_none());
        assert!(m.ext.get::<CriticalWaits>().0.is_empty());
    }
}

pub(super) fn register(reg: &mut Registry) {
    native::register(reg);
    event::register(reg);
    owned::register(reg);
    local::register(reg);
    stdcall(reg, K32, "InitializeSListHead", 1, |e| {
        let _ = e.cpu.write(e.arg(0), &[0; 8]);
        Outcome::Return(0)
    });
    stdcall(reg, K32, "InterlockedPushEntrySList", 2, |e| {
        e.cpu.quiesce_memory();
        let (head, entry) = (e.arg(0), e.arg(1));
        slist(e, head, |e, next, depth| {
            let _ = e.cpu.write_u32(entry, next);
            (entry, depth.wrapping_add(1), next)
        })
    });
    stdcall(reg, K32, "InterlockedPopEntrySList", 1, |e| {
        e.cpu.quiesce_memory();
        let head = e.arg(0);
        slist(e, head, |e, next, depth| match next {
            0 => (0, 0, 0),
            _ => (
                e.cpu.read_u32(next).unwrap_or(0),
                depth.saturating_sub(1),
                next,
            ),
        })
    });
    stdcall(reg, K32, "InterlockedFlushSList", 1, |e| {
        e.cpu.quiesce_memory();
        let head = e.arg(0);
        slist(e, head, |_, next, _| (0, 0, next))
    });
    stdcall(reg, K32, "QueryDepthSList", 1, |e| {
        Outcome::Return(u32::from(e.cpu.read_u16(e.arg(0) + 4).unwrap_or(0)))
    });
    stdcall(reg, K32, "InitializeCriticalSection", 1, init_cs);
    stdcall(
        reg,
        K32,
        "InitializeCriticalSectionAndSpinCount",
        2,
        init_cs,
    );
    stdcall(reg, K32, "InitializeCriticalSectionEx", 3, init_cs);
    stdcall(
        reg,
        K32,
        "DeleteCriticalSection",
        1,
        |e| match release_cs_wait(e, e.arg(0)) {
            Ok(()) => Outcome::Return(0),
            Err(error) => Outcome::Fail(error),
        },
    );
    stdcall(reg, K32, "EnterCriticalSection", 1, |e| {
        let cs = e.arg(0);
        enter_cs(e, cs)
    });
    stdcall(reg, K32, "TryEnterCriticalSection", 1, |e| {
        let cs = e.arg(0);
        Outcome::Return(u32::from(try_enter_cs(e, cs)))
    });
    stdcall(reg, K32, "LeaveCriticalSection", 1, |e| {
        let cs = e.arg(0);
        leave_cs(e, cs)
    });
    // Native Kernel32 exports forward to these RTL entries. Keep the shared
    // lock state but respect RTL's NTSTATUS/BOOLEAN result contracts.
    stdcall(reg, "ntdll.dll", "RtlInitializeCriticalSection", 1, |e| {
        let cs = e.arg(0);
        if let Err(error) = release_cs_wait(e, cs) {
            return Outcome::Fail(error);
        }
        match initialize_cs(e.cpu, cs) {
            Ok(()) => Outcome::Return(0),
            Err(fault) => adv_abi::guest::memory_failure(fault),
        }
    });
    stdcall(
        reg,
        "ntdll.dll",
        "RtlDeleteCriticalSection",
        1,
        |e| match release_cs_wait(e, e.arg(0)) {
            Ok(()) => Outcome::Return(0),
            Err(error) => Outcome::Fail(error),
        },
    );
    stdcall(reg, "ntdll.dll", "RtlEnterCriticalSection", 1, |e| {
        let cs = e.arg(0);
        enter_cs(e, cs)
    });
    stdcall(reg, "ntdll.dll", "RtlTryEnterCriticalSection", 1, |e| {
        let cs = e.arg(0);
        Outcome::Return(u32::from(try_enter_cs(e, cs)))
    });
    stdcall(reg, "ntdll.dll", "RtlLeaveCriticalSection", 1, |e| {
        let cs = e.arg(0);
        leave_cs(e, cs)
    });
    stdcall(reg, K32, CS_WAIT, 1, |e| {
        let cs = e.arg(0);
        wait_cs(e, cs)
    });
    stdcall(reg, K32, CS_WAKE, 1, |e| {
        let cs = e.arg(0);
        wake_cs(e, cs)
    });
    stdcall(reg, K32, "InterlockedIncrement", 1, |e| {
        interlocked(e, |o, _, _| (o.wrapping_add(1), o.wrapping_add(1)))
    });
    stdcall(reg, K32, "InterlockedDecrement", 1, |e| {
        interlocked(e, |o, _, _| (o.wrapping_sub(1), o.wrapping_sub(1)))
    });
    stdcall(reg, K32, "InterlockedExchange", 2, |e| {
        interlocked(e, |o, v, _| (v, o))
    });
    stdcall(reg, K32, "InterlockedExchangeAdd", 2, |e| {
        interlocked(e, |o, v, _| (o.wrapping_add(v), o))
    });
    stdcall(reg, K32, "InterlockedCompareExchange", 3, |e| {
        interlocked(e, |o, x, c| (if o == c { x } else { o }, o))
    });
    stdcall(reg, K32, "CreateEventA", 4, create_event);
    stdcall(reg, K32, "CreateEventW", 4, create_event);
    reg.register(open_event_a_ABI);
    reg.register(open_event_w_ABI);
    stdcall(reg, K32, "SetEvent", 1, |e| event::state_front(e, true));
    stdcall(reg, K32, "ResetEvent", 1, |e| event::state_front(e, false));
    stdcall(reg, K32, "CreateSemaphoreA", 4, create_semaphore);
    stdcall(reg, K32, "CreateSemaphoreW", 4, create_semaphore);
    stdcall(reg, K32, "OpenSemaphoreA", 3, |env| {
        owned::open_front(env, owned::Kind::Semaphore)
    });
    stdcall(reg, K32, "OpenSemaphoreW", 3, |env| {
        owned::open_front(env, owned::Kind::Semaphore)
    });
    stdcall(reg, K32, "ReleaseSemaphore", 3, release_semaphore);
    stdcall(reg, K32, "CreateMutexA", 3, create_mutex);
    stdcall(reg, K32, "CreateMutexW", 3, create_mutex);
    reg.register(open_mutex_a_ABI);
    reg.register(open_mutex_w_ABI);
    stdcall(reg, K32, "ReleaseMutex", 1, release_mutex);
    stdcall(reg, K32, "WaitForSingleObject", 2, |e| {
        let (h, ms) = (e.arg(0), e.arg(1));
        native::wait_front(e, h, ms, false)
    });
    stdcall(reg, K32, "WaitForSingleObjectEx", 3, |e| {
        let (h, ms, alertable) = (e.arg(0), e.arg(1), e.arg(2));
        native::wait_front(e, h, ms, alertable != 0)
    });
    stdcall(reg, K32, "WaitForMultipleObjects", 4, wait_multiple);
    stdcall(reg, K32, "WaitForMultipleObjectsEx", 5, |env| {
        let alertable = env.arg(4) != 0;
        native::multiple_front(env, alertable)
    });
    stdcall(reg, K32, "TlsAlloc", 0, |e| {
        let w = e.ext.get_mut::<Win32>();
        match w.tls_used.iter().position(|u| !u) {
            Some(i) => {
                w.tls_used[i] = true;
                Outcome::Return(i as u32)
            }
            None => Outcome::Return(u32::MAX),
        }
    });
    stdcall(reg, K32, "TlsFree", 1, |e| {
        let i = e.arg(0) as usize;
        let w = e.ext.get_mut::<Win32>();
        Outcome::Return(u32::from(
            i < 64 && std::mem::replace(&mut w.tls_used[i], false),
        ))
    });
    stdcall(reg, K32, "TlsGetValue", 1, |e| {
        let i = e.arg(0);
        let teb = e.ctx().teb;
        let v = if i < 64 {
            e.cpu
                .read_u32(teb + adv_machine::layout::teb::TLS_SLOTS + 4 * i)
                .unwrap_or(0)
        } else {
            0
        };
        set_last_error(e, 0);
        Outcome::Return(v)
    });
    stdcall(reg, K32, "TlsSetValue", 2, |e| {
        let (i, v) = (e.arg(0), e.arg(1));
        let teb = e.ctx().teb;
        if i >= 64 {
            set_last_error(e, err::INVALID_PARAMETER);
            return Outcome::Return(0);
        }
        let _ = e
            .cpu
            .write_u32(teb + adv_machine::layout::teb::TLS_SLOTS + 4 * i, v);
        Outcome::Return(1)
    });
    // Only these known leaf implementations can be reused as RTL providers.
    // Copying arbitrary Win32 frontends could route back into their own alias.
    for (front, native) in [
        ("InitializeSListHead", "RtlInitializeSListHead"),
        ("InterlockedPushEntrySList", "RtlInterlockedPushEntrySList"),
        ("InterlockedPopEntrySList", "RtlInterlockedPopEntrySList"),
        ("InterlockedFlushSList", "RtlInterlockedFlushSList"),
        ("QueryDepthSList", "RtlQueryDepthSList"),
    ] {
        let leaf = reg.get(K32, front).expect("registered SList owner");
        reg.add_with_data("ntdll.dll", native, leaf.conv, leaf.handler, leaf.data);
    }
}
