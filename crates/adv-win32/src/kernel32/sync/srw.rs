//! SRW API frontends share the guest lock word and existing event waits.
//! The lock is opaque; uncontended words use Windows' locked/shared-count layout.
use super::*;

#[derive(Default)]
struct Waits(HashMap<u32, Contention>);
struct Contention {
    event: u32,
    pending: Rc<Cell<usize>>,
}
struct Ticket(Rc<Cell<usize>>);
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

fn cleanup(env: &mut Env<'_>, address: u32) {
    let waits = env.ext.get_or_default::<Waits>();
    if waits
        .0
        .get(&address)
        .is_some_and(|wait| wait.pending.get() == 0)
    {
        let wait = waits.0.remove(&address).unwrap();
        env.sys.objects.close(wait.event);
    }
}

fn initialize(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    cleanup(env, address);
    if env.ext.get_or_default::<Waits>().0.contains_key(&address) {
        return Outcome::Fail("SRW lock initialized with live waiters".into());
    }
    env.cpu.quiesce_memory();
    match env.cpu.write_u32(address, 0) {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

fn take(env: &mut Env<'_>, address: u32, shared: bool) -> Result<bool, MemFault> {
    env.cpu.quiesce_memory();
    let word = env.cpu.read_u32(address)?;
    let next = if word == 0 {
        if shared { 0x11 } else { 1 }
    } else if shared && word >= 0x11 && word & 15 == 1 {
        let Some(next) = word.checked_add(16) else {
            return Ok(false);
        };
        next
    } else {
        return Ok(false);
    };
    env.cpu.write_u32(address, next)?;
    Ok(true)
}

pub(super) fn acquire(env: &mut Env<'_>, address: u32, shared: bool, attempt: bool) -> Outcome {
    cleanup(env, address);
    match take(env, address, shared) {
        Ok(true) => return Outcome::Return(u32::from(attempt)),
        Ok(false) if attempt => return Outcome::Return(0),
        Err(error) => return Outcome::Fail(error.to_string()),
        Ok(false) => {}
    }
    if !env.ext.get_or_default::<Waits>().0.contains_key(&address) {
        let event = env.sys.objects.insert(Object::Event {
            manual_reset: true,
            signaled: false,
        });
        if event == 0 {
            return Outcome::Fail("SRW lock wait resource limit exhausted".into());
        }
        env.ext.get_or_default::<Waits>().0.insert(
            address,
            Contention {
                event,
                pending: Rc::new(Cell::new(0)),
            },
        );
    }
    let wait = &env.ext.get_or_default::<Waits>().0[&address];
    let event = wait.event;
    wait.pending.set(wait.pending.get() + 1);
    let ticket = Ticket(wait.pending.clone());
    env.sys.reset_event(event);
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline: None,
        },
        move |env, _| {
            drop(ticket);
            acquire(env, address, shared, false)
        },
    )
}

fn release(env: &mut Env<'_>, shared: bool) -> Outcome {
    let address = env.arg(0);
    release_at(env, address, shared)
}

pub(super) fn release_at(env: &mut Env<'_>, address: u32, shared: bool) -> Outcome {
    env.cpu.quiesce_memory();
    let word = match env.cpu.read_u32(address) {
        Ok(word) => word,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    let next = if shared && word >= 0x11 && word & 15 == 1 {
        if word == 0x11 { 0 } else { word - 16 }
    } else if !shared && word == 1 {
        0
    } else {
        return Outcome::Fail("SRW lock released in an unheld mode".into());
    };
    if let Err(error) = env.cpu.write_u32(address, next) {
        return Outcome::Fail(error.to_string());
    }
    if next == 0
        && let Some(wait) = env.ext.get_or_default::<Waits>().0.get(&address)
    {
        let event = wait.event;
        env.sys.set_event(event);
    }
    cleanup(env, address);
    Outcome::Return(0)
}

pub(super) fn register(registry: &mut Registry) {
    for module in [K32, "kernelbase.dll", "ntdll.dll"] {
        let rtl = module == "ntdll.dll";
        stdcall(
            registry,
            module,
            if rtl {
                "RtlInitializeSRWLock"
            } else {
                "InitializeSRWLock"
            },
            1,
            initialize,
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlAcquireSRWLockExclusive"
            } else {
                "AcquireSRWLockExclusive"
            },
            1,
            |env| acquire(env, env.arg(0), false, false),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlAcquireSRWLockShared"
            } else {
                "AcquireSRWLockShared"
            },
            1,
            |env| acquire(env, env.arg(0), true, false),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlTryAcquireSRWLockExclusive"
            } else {
                "TryAcquireSRWLockExclusive"
            },
            1,
            |env| acquire(env, env.arg(0), false, true),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlTryAcquireSRWLockShared"
            } else {
                "TryAcquireSRWLockShared"
            },
            1,
            |env| acquire(env, env.arg(0), true, true),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlReleaseSRWLockExclusive"
            } else {
                "ReleaseSRWLockExclusive"
            },
            1,
            |env| release(env, false),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlReleaseSRWLockShared"
            } else {
                "ReleaseSRWLockShared"
            },
            1,
            |env| release(env, true),
        );
    }
}
