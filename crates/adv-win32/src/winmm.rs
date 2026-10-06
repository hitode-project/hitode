//! Multimedia timers and joystick queries on the cooperative guest owner.
//! No host thread, device polling or timer-resolution request is used here.
use adv_abi::{GuestPtr, Registry, win32};
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
// Callback ExitThread is unsupported native timer misuse. We do not restart
// that process service: synchronous kill observes its thread object, later sets
// fail, and a subsequent timer API reaps an abandoned active payload. Without
// another API, that bounded charge lasts until the process's Objects is dropped.
const WORKER: &str = "__pal_multimedia_timer_worker";
mod timer;
use timer::{Entry, Queue, Timer};

#[derive(Default)]
struct Timers {
    periods: BTreeMap<u32, u32>,
    queue: Queue,
    worker: Option<CtxId>,
    wake: u32,
    complete: u32,
}

pub fn register(registry: &mut Registry) {
    registry.register(joy_position_ABI);
    for (name, argc, handler) in [
        ("timeGetDevCaps", 2, caps as adv_machine::Handler),
        ("timeGetTime", 0, get_time),
        ("timeBeginPeriod", 1, begin_period),
        ("timeEndPeriod", 1, end_period),
        ("timeSetEvent", 5, set_event),
        ("timeKillEvent", 1, kill_event),
        ("waveOutGetNumDevs", 0, wave_out_get_num_devs),
        ("waveOutGetDevCapsA", 3, wave_out_get_dev_caps_a),
        ("mixerGetNumDevs", 0, mixer_get_num_devs),
        ("mixerOpen", 5, mixer_open),
        ("mixerClose", 1, mixer_close),
        ("midiOutGetNumDevs", 0, midi_out_get_num_devs),
        ("midiOutGetDevCapsA", 3, midi_out_get_dev_caps_a),
        (WORKER, 0, worker),
    ] {
        crate::stdcall(registry, "winmm.dll", name, argc, handler);
    }
}

fn mixer_get_num_devs(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

fn mixer_open(env: &mut Env<'_>) -> Outcome {
    let phmx = env.arg(0);
    if phmx == 0 {
        return Outcome::Return(11); // MMSYSERR_INVALPARAM
    }
    Outcome::Return(6) // MMSYSERR_NODRIVER
}

fn mixer_close(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(0) // MMSYSERR_NOERROR
}

fn wave_out_get_num_devs(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(1)
}

fn wave_out_get_dev_caps_a(env: &mut Env<'_>) -> Outcome {
    let (device_id, output, size) = (env.arg(0), env.arg(1), env.arg(2));
    if device_id != 0xffff_ffff && device_id != 0 {
        return Outcome::Return(2); // MMSYSERR_BADDEVICEID
    }
    if output == 0 || size == 0 {
        return Outcome::Return(11); // MMSYSERR_INVALPARAM
    }
    let mut caps = [0u8; 52];
    caps[0..2].copy_from_slice(&1u16.to_le_bytes()); // wMid = 1 (MM_MICROSOFT)
    caps[2..4].copy_from_slice(&1u16.to_le_bytes()); // wPid = 1 (MM_MSFT_WAVEOUT)
    caps[4..8].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // vDriverVersion = 1.0
    let name = b"Generic Audio Output\0";
    caps[8..8 + name.len()].copy_from_slice(name);
    // dwFormats = 0x000f_ffff (standard PCM formats)
    caps[40..44].copy_from_slice(&0x000f_ffffu32.to_le_bytes());
    // wChannels = 2
    caps[44..46].copy_from_slice(&2u16.to_le_bytes());
    // dwSupport = 0
    caps[48..52].copy_from_slice(&0u32.to_le_bytes());

    let write_len = (size as usize).min(caps.len());
    if env.cpu.write(output, &caps[..write_len]).is_err() {
        return Outcome::Return(11); // MMSYSERR_INVALPARAM
    }
    Outcome::Return(0) // MMSYSERR_NOERROR
}

fn midi_out_get_num_devs(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(1)
}

fn midi_out_get_dev_caps_a(env: &mut Env<'_>) -> Outcome {
    let (device_id, output, size) = (env.arg(0), env.arg(1), env.arg(2));
    if device_id != 0xffff_ffff && device_id != 0 {
        return Outcome::Return(2); // MMSYSERR_BADDEVICEID
    }
    if output == 0 || size == 0 {
        return Outcome::Return(11); // MMSYSERR_INVALPARAM
    }
    let mut caps = [0u8; 52];
    caps[0..2].copy_from_slice(&1u16.to_le_bytes()); // wMid = 1 (MM_MICROSOFT)
    caps[2..4].copy_from_slice(&1u16.to_le_bytes()); // wPid = 1 (MM_MSFT_GENERIC_MIDIOUT)
    caps[4..8].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // vDriverVersion = 1.0
    let name = b"Generic MIDI Output\0";
    caps[8..8 + name.len()].copy_from_slice(name);
    // wTechnology: MOD_SWSYNTH = 7
    caps[40..42].copy_from_slice(&7u16.to_le_bytes());
    // wVoices = 32
    caps[42..44].copy_from_slice(&32u16.to_le_bytes());
    // wNotes = 32
    caps[44..46].copy_from_slice(&32u16.to_le_bytes());
    // wChannelMask = 0xffff
    caps[46..48].copy_from_slice(&0xffffu16.to_le_bytes());
    // dwSupport = 0
    caps[48..52].copy_from_slice(&0u32.to_le_bytes());

    let write_len = (size as usize).min(caps.len());
    if env.cpu.write(output, &caps[..write_len]).is_err() {
        return Outcome::Return(11); // MMSYSERR_INVALPARAM
    }
    Outcome::Return(0) // MMSYSERR_NOERROR
}

/// The virtual input profile has no configured game controllers, matching
/// DirectInput's empty GAMECTRL enumeration. Native WinMM on that profile
/// returns JOYERR_PARMS even for IDs 0..15; JOYERR_UNPLUGGED describes a
/// configured joystick that has been disconnected. Failure leaves JOYINFO
/// untouched, so neither guest output permissions nor host devices are probed.
#[win32(winmm, name = "joyGetPos", level = safe_fail)]
fn joy_position(_env: &mut Env<'_>, _id: u32, output: GuestPtr<()>) -> u32 {
    if output.is_null() {
        11 // MMSYSERR_INVALPARAM
    } else {
        165 // JOYERR_PARMS
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
    if let Some(id) = state(env).worker {
        // A callback which exits its worker breaks this process service. Do not
        // silently restart it or fabricate another callback delivery clock.
        return !env.sys.contexts[id as usize].is_done();
    }
    if env.sys.contexts.len() >= adv_machine::layout::MAX_CONTEXTS as usize {
        return false;
    }
    let entry = match env.sys.thunks.resolve(env.cpu, "winmm.dll", WORKER) {
        Ok(entry) => entry,
        Err(_) => return false,
    };
    let wake = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    if wake == 0 {
        return false;
    }
    let complete = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: true,
    });
    if complete == 0 {
        env.sys.objects.close(wake);
        return false;
    }
    let id = match crate::kernel32::spawn_thread(env, "multimedia callbacks", entry, 0, 0x10_0000) {
        Ok(id) => id,
        Err(_) => {
            env.sys.objects.close(wake);
            env.sys.objects.close(complete);
            return false;
        }
    };
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
    {
        return Outcome::Return(0);
    }
    reap_dead_worker(env);
    if state(env)
        .worker
        .is_some_and(|id| env.sys.contexts[id as usize].is_done())
    {
        return Outcome::Return(0);
    }
    let now = env.sys.clock.guest_now_us();
    let entry = env.ext.get_mut::<Timers>().queue.insert(
        &mut env.sys.objects,
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
    let Some(entry) = entry else {
        return Outcome::Return(0);
    };
    // Charge the timer first. If events/thread cannot be allocated, remove the
    // unpublished alias; ensure_worker rolls back its own partial resources.
    if !ensure_worker(env) {
        env.ext
            .get_mut::<Timers>()
            .queue
            .remove(&mut env.sys.objects, entry);
        return Outcome::Return(0);
    }
    env.ext
        .get_mut::<Timers>()
        .queue
        .publish(&env.sys.objects, entry);
    let wake = state(env).wake;
    env.sys.set_event(wake);
    Outcome::Return(entry.id)
}
fn kill_event(env: &mut Env<'_>) -> Outcome {
    let id = env.arg(0);
    reap_dead_worker(env);
    let current = env.context;
    let Some(timer) = env
        .ext
        .get_mut::<Timers>()
        .queue
        .cancel(&mut env.sys.objects, id)
    else {
        return Outcome::Return(NOCANDO);
    };
    // Native permits synchronous cancellation from the callback itself. The
    // current invocation may return normally; no subsequent invocation starts.
    let timers = state(env);
    let worker = timers.worker;
    let wait = timer.running && timer.flags & SYNCHRONOUS != 0 && worker != Some(current);
    let (wake, complete) = (timers.wake, timers.complete);
    env.sys.set_event(wake);
    if wait {
        // ExitThread discards the callback continuation, so complete alone
        // could hang forever. The ordinary thread object also ends the wait.
        let thread = env.sys.contexts[worker.unwrap() as usize].thread_handle;
        Outcome::block(
            Wait::Objects {
                handles: vec![complete, thread],
                all: false,
                deadline: None,
                notification: None,
            },
            |env, _| {
                reap_dead_worker(env);
                Outcome::Return(0)
            },
        )
    } else {
        Outcome::Return(0)
    }
}
fn reap_dead_worker(env: &mut Env<'_>) {
    let timers = state(env);
    let worker = timers.worker;
    let active = timers.queue.active;
    if let (Some(worker), Some(active)) = (worker, active)
        && env.sys.contexts[worker as usize].is_done()
    {
        env.ext
            .get_mut::<Timers>()
            .queue
            .finished(&mut env.sys.objects, active);
        let complete = state(env).complete;
        env.sys.set_event(complete);
    }
}
fn finished(env: &mut Env<'_>, entry: Entry) -> Outcome {
    env.ext
        .get_mut::<Timers>()
        .queue
        .finished(&mut env.sys.objects, entry);
    let complete = state(env).complete;
    env.sys.set_event(complete);
    // A burst of due callbacks must not monopolize the execution owner.
    Outcome::block(Wait::Yield, |env, _| worker(env))
}
fn worker(env: &mut Env<'_>) -> Outcome {
    let current = env.context;
    if state(env).worker != Some(current) {
        return Outcome::Return(0);
    }
    let now = env.sys.clock.guest_now_us();
    let wake = state(env).wake;
    env.sys.reset_event(wake);
    let next = env
        .ext
        .get_mut::<Timers>()
        .queue
        .dispatch(&mut env.sys.objects, now);
    let Some((entry, timer)) = next else {
        let deadline = state(env).queue.next_due();
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline,
            },
            |env, _| worker(env),
        );
    };
    let complete = state(env).complete;
    env.sys.reset_event(complete);
    match timer.flags & (EVENT_SET | EVENT_PULSE) {
        EVENT_SET => {
            env.sys.set_event(timer.target);
            finished(env, entry)
        }
        EVENT_PULSE => {
            env.sys.set_event(timer.target);
            env.sys.reset_event(timer.target);
            finished(env, entry)
        }
        _ if timer.target == 0 => finished(env, entry),
        _ => Outcome::call(
            timer.target,
            vec![entry.id, 0, timer.user, 0, 0],
            move |env, _| finished(env, entry),
        ),
    }
}

pub mod mmio;
