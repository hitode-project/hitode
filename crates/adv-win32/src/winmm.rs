//! Multimedia timers on one cooperative guest worker. No host thread, polling,
//! or system timer-resolution request is used by this persona.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{CtxId, Env, Object, Outcome, Wait};
use std::collections::BTreeMap;

const MIN_MS: u32 = 1;
const MAX_MS: u32 = 1_000_000;
const NOCANDO: u32 = 97;
const MAX_TIMERS: usize = 256;
const PERIODIC: u32 = 1;
const EVENT_SET: u32 = 0x10;
const EVENT_PULSE: u32 = 0x20;
const SYNCHRONOUS: u32 = 0x100;
const WORKER: &str = "__pal_multimedia_timer_worker";
#[derive(Clone, Copy)]
struct Timer {
    due: Option<u64>,
    interval: u64,
    target: u32,
    user: u32,
    flags: u32,
    running: bool,
    canceled: bool,
}
#[derive(Default)]
struct Timers {
    periods: BTreeMap<u32, u32>,
    timers: BTreeMap<u32, Timer>,
    worker: Option<CtxId>,
    wake: u32,
    complete: u32,
    next_id: u32,
}
impl Timers {
    fn next_due(&self) -> Option<u64> {
        self.timers
            .values()
            .filter(|timer| !timer.canceled)
            .filter_map(|timer| timer.due)
            .min()
    }
    fn due_id(&self, now: u64) -> Option<u32> {
        self.timers
            .iter()
            .filter(|(_, timer)| !timer.canceled && timer.due.is_some_and(|due| due <= now))
            .min_by_key(|(id, timer)| (timer.due, *id))
            .map(|(&id, _)| id)
    }
    fn dispatch(&mut self, id: u32, now: u64) -> Timer {
        let timer = self.timers.get_mut(&id).unwrap();
        timer.running = true;
        timer.due = if timer.flags & PERIODIC != 0 {
            let due = timer.due.unwrap();
            Some(due.saturating_add(
                (now.saturating_sub(due) / timer.interval + 1).saturating_mul(timer.interval),
            ))
        } else {
            None
        };
        *timer
    }
    fn finished(&mut self, id: u32) {
        if let Some(timer) = self.timers.get_mut(&id) {
            timer.running = false;
            if timer.canceled || timer.due.is_none() {
                self.timers.remove(&id);
            }
        }
    }
}

pub fn register(registry: &mut Registry) {
    for (name, argc, handler) in [
        ("timeGetDevCaps", 2, caps as adv_machine::Handler),
        ("timeGetTime", 0, get_time),
        ("timeBeginPeriod", 1, begin_period),
        ("timeEndPeriod", 1, end_period),
        ("timeSetEvent", 5, set_event),
        ("timeKillEvent", 1, kill_event),
        (WORKER, 0, worker),
    ] {
        crate::stdcall(registry, "winmm.dll", name, argc, handler);
    }
}
fn state<'a>(env: &'a mut Env<'_>) -> &'a mut Timers {
    if env.ext.try_get::<Timers>().is_none() {
        env.ext.insert(Timers::default());
    }
    env.ext.get_mut::<Timers>()
}
fn caps(env: &mut Env<'_>) -> Outcome {
    let (output, size) = (env.arg(0), env.arg(1));
    if output == 0 || size < 8 || !env.cpu.accessible(output, 8, Prot::WRITE) {
        return Outcome::Return(NOCANDO);
    }
    if env.cpu.write_u32(output, MIN_MS).is_err() || env.cpu.write_u32(output + 4, MAX_MS).is_err()
    {
        return Outcome::Return(NOCANDO);
    }
    Outcome::Return(0)
}
fn get_time(env: &mut Env<'_>) -> Outcome {
    Outcome::Return((env.sys.clock.guest_now_us() / 1000) as u32)
}
fn begin_period(env: &mut Env<'_>) -> Outcome {
    let period = env.arg(0);
    if period == 0 {
        return Outcome::Return(NOCANDO);
    }
    // Coarser than the default Windows 15.625-ms period needs no request.
    // Native accepts these values (even beyond the event-delay maximum).
    if period >= 16 {
        return Outcome::Return(0);
    }
    let count = state(env).periods.entry(period).or_default();
    *count = count.saturating_add(1);
    Outcome::Return(0)
}
fn end_period(env: &mut Env<'_>) -> Outcome {
    let period = env.arg(0);
    if period == 0 {
        return Outcome::Return(NOCANDO);
    }
    if period >= 16 {
        return Outcome::Return(0);
    }
    let periods = &mut state(env).periods;
    let Some(count) = periods.get_mut(&period) else {
        return Outcome::Return(NOCANDO);
    };
    *count -= 1;
    if *count == 0 {
        periods.remove(&period);
    }
    Outcome::Return(0)
}
fn ensure_worker(env: &mut Env<'_>) -> bool {
    if state(env).worker.is_some() {
        return true;
    }
    if env.sys.contexts.len() >= adv_machine::layout::MAX_CONTEXTS as usize {
        return false;
    }
    let entry = match env.sys.thunks.resolve(env.cpu, "winmm.dll", WORKER) {
        Ok(entry) => entry,
        Err(_) => return false,
    };
    let id = match env.sys.spawn(env.cpu, "MM", entry, &[], 0x10_0000) {
        Ok(id) => id,
        Err(_) => return false,
    };
    let wake = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let complete = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: true,
    });
    let timers = state(env);
    timers.worker = Some(id);
    timers.wake = wake;
    timers.complete = complete;
    true
}
fn set_event(env: &mut Env<'_>) -> Outcome {
    let (delay, _resolution, target, user, flags) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if !(MIN_MS..=MAX_MS).contains(&delay)
        || flags & !(PERIODIC | EVENT_SET | EVENT_PULSE | SYNCHRONOUS) != 0
        || state(env).timers.len() >= MAX_TIMERS
        || !ensure_worker(env)
    {
        return Outcome::Return(0);
    }
    let now = env.sys.clock.guest_now_us();
    let timers = state(env);
    loop {
        timers.next_id = timers.next_id.wrapping_add(1).max(1);
        if !timers.timers.contains_key(&timers.next_id) {
            break;
        }
    }
    let id = timers.next_id;
    timers.timers.insert(
        id,
        Timer {
            due: Some(now.saturating_add(u64::from(delay) * 1000)),
            interval: u64::from(delay) * 1000,
            target,
            user,
            flags,
            running: false,
            canceled: false,
        },
    );
    let wake = timers.wake;
    env.sys.set_event(wake);
    Outcome::Return(id)
}
fn kill_event(env: &mut Env<'_>) -> Outcome {
    let id = env.arg(0);
    let current = env.sys.current;
    let timers = state(env);
    let Some(timer) = timers.timers.get_mut(&id).filter(|timer| !timer.canceled) else {
        return Outcome::Return(NOCANDO);
    };
    timer.canceled = true;
    // Native permits synchronous cancellation from the callback itself. The
    // current invocation may return normally; no subsequent invocation starts.
    let wait = timer.running && timer.flags & SYNCHRONOUS != 0 && timers.worker != Some(current);
    if !timer.running {
        timers.timers.remove(&id);
    }
    let (wake, complete) = (timers.wake, timers.complete);
    env.sys.set_event(wake);
    if wait {
        Outcome::block_then_return(
            Wait::Object {
                handle: complete,
                deadline: None,
            },
            0,
        )
    } else {
        Outcome::Return(0)
    }
}
fn finished(env: &mut Env<'_>, id: u32) -> Outcome {
    let timers = state(env);
    timers.finished(id);
    let complete = timers.complete;
    env.sys.set_event(complete);
    // A burst of due callbacks must not monopolize the execution owner.
    Outcome::block(Wait::Yield, |env, _| worker(env))
}
fn worker(env: &mut Env<'_>) -> Outcome {
    let now = env.sys.clock.guest_now_us();
    let wake = state(env).wake;
    env.sys.reset_event(wake);
    let Some(id) = state(env).due_id(now) else {
        let deadline = state(env).next_due();
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline,
            },
            |env, _| worker(env),
        );
    };
    let timer = state(env).dispatch(id, now);
    let complete = state(env).complete;
    env.sys.reset_event(complete);
    match timer.flags & (EVENT_SET | EVENT_PULSE) {
        EVENT_SET => {
            env.sys.set_event(timer.target);
            finished(env, id)
        }
        EVENT_PULSE => {
            env.sys.set_event(timer.target);
            env.sys.reset_event(timer.target);
            finished(env, id)
        }
        _ if timer.target == 0 => finished(env, id),
        _ => Outcome::call(
            timer.target,
            vec![id, 0, timer.user, 0, 0],
            move |env, _| finished(env, id),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_periodic_timer_coalesces_missed_ticks_and_keeps_original_phase() {
        let mut timers = Timers::default();
        timers.timers.insert(
            1,
            Timer {
                due: Some(10_000),
                interval: 10_000,
                target: 1,
                user: 0,
                flags: PERIODIC,
                running: false,
                canceled: false,
            },
        );
        // Hundreds of thousands of missed periods produce one callback, then
        // wait for the first original-phase deadline strictly after now.
        let now = 3_600_005_000;
        assert_eq!(timers.due_id(now), Some(1));
        timers.dispatch(1, now);
        timers.finished(1);
        assert_eq!(timers.due_id(now), None);
        assert_eq!(timers.next_due(), Some(3_600_010_000));
        assert_eq!(timers.due_id(3_600_010_000), Some(1));
    }
}
