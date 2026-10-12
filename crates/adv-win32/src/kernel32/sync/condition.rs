//! Condition-variable frontends atomically publish a wait and release its lock.
//! Reacquisition uses the same CS/SRW continuation, including after a timeout.
use super::*;
use adv_cpu::backend::Prot;
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Signaled,
    Finished,
}
struct Entry {
    event: u32,
    state: Rc<Cell<State>>,
}
#[derive(Default)]
struct Queues(HashMap<u32, VecDeque<Entry>>);
struct Ticket(Rc<Cell<State>>);
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.set(State::Finished);
    }
}

fn sweep(env: &mut Env<'_>, address: u32) {
    let queues = env.ext.get_or_default::<Queues>();
    if let Some(queue) = queues.0.get_mut(&address) {
        queue.retain(|entry| {
            if entry.state.get() == State::Finished {
                env.sys.objects.close(entry.event);
                false
            } else {
                true
            }
        });
        if queue.is_empty() {
            queues.0.remove(&address);
        }
    }
}

fn initialize(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    sweep(env, address);
    if env.ext.get_or_default::<Queues>().0.contains_key(&address) {
        return Outcome::Fail("condition variable initialized with live waiters".into());
    }
    match env.cpu.write_u32(address, 0) {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

fn wake(env: &mut Env<'_>, all: bool) -> Outcome {
    let address = env.arg(0);
    if let Err(error) = adv_abi::guest::check_range(env.cpu, address, 4, Prot::RW) {
        return Outcome::Fail(error.to_string());
    }
    sweep(env, address);
    let events: Vec<_> = env
        .ext
        .get_or_default::<Queues>()
        .0
        .get(&address)
        .into_iter()
        .flatten()
        .filter(|entry| entry.state.get() == State::Waiting)
        .take(if all { usize::MAX } else { 1 })
        .map(|entry| {
            entry.state.set(State::Signaled);
            entry.event
        })
        .collect();
    for event in events {
        env.sys.set_event(event);
    }
    Outcome::Return(0)
}

fn result(env: &mut Env<'_>, status: u32, native: bool) -> Outcome {
    if native {
        return Outcome::Return(status);
    }
    if status == 0 {
        Outcome::Return(1)
    } else {
        set_last_error(
            env,
            match status {
                0x102 => 1460,
                0xc0000005 => 998,
                0xc0000017 => 8,
                _ => 87,
            },
        );
        Outcome::Return(0)
    }
}

fn relocked(env: &mut Env<'_>, acquired: Outcome, status: u32, native: bool) -> Outcome {
    match acquired {
        Outcome::Return(_) => result(env, status, native),
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, resume| {
                let acquired = then(env, resume);
                relocked(env, acquired, status, native)
            }),
        },
        other => other,
    }
}

fn sleep(env: &mut Env<'_>, srw_lock: bool, native: bool) -> Outcome {
    let (address, lock, timeout) = (env.arg(0), env.arg(1), env.arg(2));
    let flags = if srw_lock { env.arg(3) } else { 0 };
    if flags & !1 != 0 {
        return result(env, 0xc000000d, native);
    }
    let shared = flags == 1;
    if adv_abi::guest::check_range(env.cpu, address, 4, Prot::RW).is_err()
        || adv_abi::guest::check_range(env.cpu, lock, if srw_lock { 4 } else { CS_SIZE }, Prot::RW)
            .is_err()
    {
        return result(env, 0xc0000005, native);
    }
    env.cpu.quiesce_memory();
    let held = if srw_lock {
        let word = env.cpu.read_u32(lock).unwrap();
        if shared {
            word >= 0x11 && word & 15 == 1
        } else {
            word == 1
        }
    } else {
        env.cpu.read_u32(lock + CS_RECURSION).unwrap() == 1
            && env.cpu.read_u32(lock + CS_OWNER).unwrap() == env.ctx().tid
    };
    if !held {
        return result(env, 0xc000000d, native);
    }
    let deadline = if native {
        match crate::kernel32::apc::timeout(env, timeout) {
            Ok(deadline) => deadline,
            Err(status) => return result(env, status, native),
        }
    } else {
        (timeout != INFINITE).then(|| {
            env.sys
                .clock
                .now_us()
                .saturating_add(u64::from(timeout) * 1000)
        })
    };
    sweep(env, address);
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    if event == 0 {
        return result(env, 0xc0000017, native);
    }
    let state = Rc::new(Cell::new(State::Waiting));
    env.ext
        .get_or_default::<Queues>()
        .0
        .entry(address)
        .or_default()
        .push_back(Entry {
            event,
            state: state.clone(),
        });
    let ticket = Ticket(state);
    let released = if srw_lock {
        srw::release_at(env, lock, shared)
    } else {
        leave_cs(env, lock)
    };
    if !matches!(released, Outcome::Return(_)) {
        drop(ticket);
        sweep(env, address);
        return released;
    }
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline,
        },
        move |env, reason| {
            let status = if matches!(reason, Resume::Woke(WakeReason::Timeout))
                && ticket.0.get() != State::Signaled
            {
                0x102
            } else {
                0
            };
            drop(ticket);
            sweep(env, address);
            let acquired = if srw_lock {
                srw::acquire(env, lock, shared, false)
            } else {
                enter_cs(env, lock)
            };
            relocked(env, acquired, status, native)
        },
    )
}

pub(super) fn register(registry: &mut Registry) {
    for module in [K32, "kernelbase.dll", "ntdll.dll"] {
        let rtl = module == "ntdll.dll";
        stdcall(
            registry,
            module,
            if rtl {
                "RtlInitializeConditionVariable"
            } else {
                "InitializeConditionVariable"
            },
            1,
            initialize,
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlWakeConditionVariable"
            } else {
                "WakeConditionVariable"
            },
            1,
            |env| wake(env, false),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlWakeAllConditionVariable"
            } else {
                "WakeAllConditionVariable"
            },
            1,
            |env| wake(env, true),
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlSleepConditionVariableCS"
            } else {
                "SleepConditionVariableCS"
            },
            3,
            if rtl {
                |env| sleep(env, false, true)
            } else {
                |env| sleep(env, false, false)
            },
        );
        stdcall(
            registry,
            module,
            if rtl {
                "RtlSleepConditionVariableSRW"
            } else {
                "SleepConditionVariableSRW"
            },
            4,
            if rtl {
                |env| sleep(env, true, true)
            } else {
                |env| sleep(env, true, false)
            },
        );
    }
}
