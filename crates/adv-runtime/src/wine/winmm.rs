//! Native WinMM consumption and sequencing over the session mixer. Wine's
//! original module supplies its remaining exports through ordinary imports.
use std::{collections::VecDeque, path::Path};

use adv_abi::{
    Conv, Registry,
    dll::{Export, Import, ModuleImage},
};
use adv_cpu::backend::{Backend, GuestExt};
use adv_device::AudioControl;
use adv_machine::{Env, Machine, Object, Outcome, Wait};
use adv_pe::{ImportModule, ImportName};

mod exports;
mod sound;
mod stream;
mod wave;

const MODULE: &str = "winmm.dll";
const ORIGINAL: &str = "winewinmm.dll";
const LEAD: u64 = 30_000;

/// Native work completed by this front, available to the session's tests.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub wave_headers: u64,
    pub wave_completed: u64,
    pub wave_completed_while_blocked: u64,
    pub stream_events: u64,
    pub stream_headers: u64,
    pub stream_time_us: u64,
    pub sounds: u64,
}

#[derive(Clone, Copy)]
struct Callback {
    target: u32,
    instance: u32,
    flags: u32,
    handle: u32,
}
#[derive(Clone, Copy)]
struct Notice {
    callback: Callback,
    message: u32,
    param: u32,
}
#[derive(Default)]
struct Callbacks {
    pending: VecDeque<(Notice, Option<u32>)>,
    busy: Option<u32>,
    context: Option<u32>,
    drained: u32,
    event: u32,
    started: bool,
}

pub(super) fn register(registry: &mut Registry) {
    for (index, &(name, _, argc)) in exports::EXPORTS.iter().enumerate() {
        if name == "timeGetTime" {
            registry.add(MODULE, name, Conv::Stdcall { argc }, |env| {
                env.sys.poll_observation();
                adv_machine::clock::poll_clock(env, |_, us| Outcome::Return((us / 1000) as u32))
            });
            continue;
        }
        registry.add_with_data(
            MODULE,
            name,
            Conv::Stdcall { argc },
            |env| {
                let index = env.sys.thunks.get(env.thunk).f.data;
                let (name, _, argc) = exports::EXPORTS[index as usize];
                let mut args: Vec<_> = (0..u32::from(argc)).map(|i| env.arg(i)).collect();
                if name.starts_with("waveOut") && !name.contains("ErrorText") {
                    return wave::call(env, name, args);
                }
                if name.starts_with("midiStream") {
                    return stream::call(env, name, args);
                }
                if matches!(
                    name,
                    "PlaySound" | "PlaySoundA" | "PlaySoundW" | "sndPlaySoundA" | "sndPlaySoundW"
                ) {
                    return sound::call(env, name, args);
                }
                if name.starts_with("mixer") && !args.is_empty() && wave::is_native(env, args[0]) {
                    if name.contains("DevCaps") {
                        args[0] = 0;
                    } else if matches!(
                        name,
                        "mixerGetID"
                            | "mixerGetLineInfoA"
                            | "mixerGetLineInfoW"
                            | "mixerGetLineControlsA"
                            | "mixerGetLineControlsW"
                            | "mixerGetControlDetailsA"
                            | "mixerGetControlDetailsW"
                            | "mixerSetControlDetails"
                    ) {
                        let flags = args.last_mut().unwrap();
                        if *flags & 0xf0000000 == 0x90000000 {
                            *flags &= !0x80000000;
                            args[0] = 0;
                        }
                    }
                }
                if name.starts_with("midiOut")
                    && let Err(error) = stream::pump(env)
                {
                    return Outcome::Fail(error);
                }
                forward(env, name, args)
            },
            index as u64,
        );
    }
    registry.add(MODULE, "callback_worker", Conv::Stdcall { argc: 1 }, worker);
    for (name, ordinal) in [("ordinal3", 3), ("ordinal4", 4)] {
        registry.add_with_data(
            MODULE,
            name,
            Conv::Stdcall { argc: 0 },
            |env| {
                let ordinal = env.sys.thunks.get(env.thunk).f.data;
                forward(env, &format!("#{ordinal}"), vec![])
            },
            ordinal,
        );
    }
}

pub(super) fn install<B: Backend>(machine: &mut Machine<B>, prefix: &Path) -> Result<(), String> {
    let system = prefix.join("drive_c/windows/system32");
    let path = system.join(MODULE);
    let saved = system.join(ORIGINAL);
    if std::fs::symlink_metadata(&path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        let original = std::fs::read(&path).map_err(|e| e.to_string())?;
        if saved.exists() {
            std::fs::remove_file(&saved).map_err(|e| e.to_string())?;
        }
        std::fs::write(&saved, original).map_err(|e| e.to_string())?;
    }
    machine.ext.insert(Stats::default());
    machine.ext.insert(wave::Waves::default());
    machine.ext.insert(stream::Streams::default());
    machine.ext.insert(sound::Sound::default());
    machine.ext.insert(Callbacks::default());
    let mut exports: Vec<_> = exports::EXPORTS
        .iter()
        .map(|&(name, ordinal, _)| Export { name, ordinal })
        .collect();
    exports.extend([
        Export {
            name: "PlaySoundA",
            ordinal: 2,
        },
        Export {
            name: "ordinal3",
            ordinal: 3,
        },
        Export {
            name: "ordinal4",
            ordinal: 4,
        },
    ]);
    let imports = [
        ImportModule {
            dll: ORIGINAL.into(),
            imports: exports::EXPORTS
                .iter()
                .map(|&(name, _, _)| pe_import(name))
                .chain([3, 4].map(|ordinal| adv_pe::Import {
                    name: ImportName::Ordinal(ordinal),
                    iat_rva: 0,
                }))
                .collect(),
        },
        ImportModule {
            dll: "kernel32.dll".into(),
            imports: ["CreateThread", "CloseHandle"]
                .into_iter()
                .map(pe_import)
                .collect(),
        },
    ];
    let image = ModuleImage::build(machine, MODULE, &exports, &imports)?;
    // ModuleImage names host bindings. Wine's first three exports have no names.
    let mut bytes = image.bytes().to_vec();
    let pe = adv_pe::Pe::parse(&bytes).map_err(|e| e.to_string())?;
    let directory = pe.dirs[adv_pe::dir::EXPORT].rva as usize;
    let word = |at| u32::from_le_bytes(pe.image[at..at + 4].try_into().unwrap()) as usize;
    let raw = |rva: usize| {
        let section = pe
            .sections
            .iter()
            .find(|s| {
                rva >= s.virtual_address as usize
                    && rva < (s.virtual_address + s.virtual_size) as usize
            })
            .unwrap();
        rva - section.virtual_address as usize + section.raw_offset as usize
    };
    let base = word(directory + 16);
    let count = word(directory + 24);
    let names = word(directory + 32);
    let ordinals = word(directory + 36);
    let mut retained = 0;
    for index in 0..count {
        let at = ordinals + index * 2;
        let ordinal = u16::from_le_bytes(pe.image[at..at + 2].try_into().unwrap()) as usize;
        if ordinal + base < 5 {
            continue;
        }
        let out = raw(names) + retained * 4;
        bytes[out..out + 4].copy_from_slice(&pe.image[names + index * 4..names + index * 4 + 4]);
        let out = raw(ordinals) + retained * 2;
        bytes[out..out + 2].copy_from_slice(&pe.image[at..at + 2]);
        retained += 1;
    }
    let out = raw(directory) + 24;
    bytes[out..out + 4].copy_from_slice(&(retained as u32).to_le_bytes());
    adv_unix::builtin::install_bytes(prefix, MODULE, bytes).map_err(|e| e.to_string())?;
    Ok(())
}
fn pe_import(name: &str) -> adv_pe::Import {
    adv_pe::Import {
        name: ImportName::Name {
            hint: 0,
            name: name.into(),
        },
        iat_rva: 0,
    }
}
fn imported(env: &Env<'_>, dll: &str, name: &str) -> Result<Import, String> {
    let base = adv_nt::process::module_base(env.cpu, env.sys.peb, MODULE)
        .ok_or("WinMM image is not loaded")?;
    Import::bind_in(env, MODULE, base, dll, name)
        .ok_or_else(|| format!("WinMM import missing: {dll}!{name}"))
}
fn forward(env: &mut Env<'_>, name: &str, args: Vec<u32>) -> Outcome {
    match imported(env, ORIGINAL, name) {
        Ok(import) => import.call(env, args, |_, result| Outcome::Return(result.value())),
        Err(error) => Outcome::Fail(error),
    }
}
fn read(env: &Env<'_>, at: u32) -> Result<u32, String> {
    env.cpu.read_u32(at).map_err(|e| e.to_string())
}
fn write(env: &mut Env<'_>, at: u32, value: u32) -> Result<(), String> {
    env.cpu.write_u32(at, value).map_err(|e| e.to_string())
}
fn now(env: &Env<'_>) -> u64 {
    env.ext
        .get::<AudioControl>()
        .playback_clock()
        .now_us(env.sys.clock.now_us())
}
fn source(env: &Env<'_>, handle: u32) -> u64 {
    (u64::from(env.sys.pid) << 32) | u64::from(handle)
}

fn notify(env: &mut Env<'_>, notice: Notice) -> Result<(), String> {
    let callback = notice.callback;
    if callback.target == 0 || callback.flags & 0x70000 == 0 {
        return Ok(());
    }
    if callback.flags & 0x70000 == 0x50000 {
        return match adv_nt::sync::set_audio_event(env, callback.target) {
            Ok(()) | Err(adv_nt::status::Error::Status(_)) => Ok(()),
            Err(error) => Err(format!("{error:?}")),
        };
    }
    let offset = if notice.message == 0x3ca {
        Some(read(env, notice.param + 28)?)
    } else {
        None
    };
    let state = env.ext.get_mut::<Callbacks>();
    state.pending.push_back((notice, offset));
    let event = state.event;
    let drained = state.drained;
    env.sys.reset_event(drained);
    env.sys
        .set_event_with_priority(event, adv_machine::WakePriority::Audio);
    Ok(())
}
fn deliver(env: &mut Env<'_>, notice: Notice, result: u32) -> Outcome {
    let c = notice.callback;
    let callbacks = env.ext.get::<Callbacks>();
    if matches!(notice.message, 0x3bc | 0x3c8)
        && callbacks.context != Some(env.context)
        && (callbacks.busy == Some(c.handle)
            || callbacks
                .pending
                .iter()
                .any(|(n, _)| n.callback.handle == c.handle))
    {
        return Outcome::block(
            Wait::Object {
                handle: callbacks.drained,
                deadline: None,
            },
            move |env, _| deliver(env, notice, result),
        );
    }
    match imported(env, ORIGINAL, "DriverCallback") {
        Ok(import) => import.call(
            env,
            [
                c.target,
                c.flags >> 16,
                c.handle,
                notice.message,
                c.instance,
                notice.param,
                0,
            ],
            move |_, _| Outcome::Return(result),
        ),
        Err(error) => Outcome::Fail(error),
    }
}
fn worker(env: &mut Env<'_>) -> Outcome {
    env.ext.get_mut::<Callbacks>().context = Some(env.context);
    if let Some((notice, offset)) = env.ext.get_mut::<Callbacks>().pending.pop_front() {
        if let Some(offset) = offset
            && let Err(error) = write(env, notice.param + 28, offset)
        {
            return Outcome::Fail(error);
        }
        let c = notice.callback;
        env.ext.get_mut::<Callbacks>().busy = Some(c.handle);
        return match imported(env, ORIGINAL, "DriverCallback") {
            Ok(import) => import.call(
                env,
                [
                    c.target,
                    c.flags >> 16,
                    c.handle,
                    notice.message,
                    c.instance,
                    notice.param,
                    0,
                ],
                |env, _| {
                    env.ext.get_mut::<Callbacks>().busy = None;
                    worker(env)
                },
            ),
            Err(error) => Outcome::Fail(error),
        };
    }
    let event = env.ext.get::<Callbacks>().event;
    let drained = env.ext.get::<Callbacks>().drained;
    env.sys.set_event(drained);
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline: None,
        },
        |env, _| worker(env),
    )
}
fn start_worker(env: &mut Env<'_>, notice: Notice) -> Outcome {
    if notice.callback.target == 0 || matches!(notice.callback.flags & 0x70000, 0 | 0x50000) {
        return deliver(env, notice, 0);
    }
    if env.ext.get::<Callbacks>().started {
        return deliver(env, notice, 0);
    }
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    env.ext.get_mut::<Callbacks>().event = event;
    env.ext.get_mut::<Callbacks>().drained = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: true,
    });
    env.ext.get_mut::<Callbacks>().started = true;
    let prepared = env
        .sys
        .thunks
        .publish(env.cpu, MODULE, "callback_worker")
        .map_err(|e| e.to_string())
        .and_then(|entry| Ok((entry, imported(env, "kernel32.dll", "CreateThread")?)));
    match prepared {
        Ok((entry, create)) => create.call(env, [0, 0, entry, 0, 0, 0], move |env, result| {
            if result.value() == 0 {
                return Outcome::Return(7);
            }
            match imported(env, "kernel32.dll", "CloseHandle") {
                Ok(close) => {
                    close.call(env, [result.value()], move |env, _| deliver(env, notice, 0))
                }
                Err(error) => Outcome::Fail(error),
            }
        }),
        Err(error) => Outcome::Fail(error),
    }
}

pub(super) fn pump<B: Backend>(machine: &mut Machine<B>) -> Result<Option<u64>, String> {
    machine.visit_context(None, |env| {
        let wave = wave::pump(env)?;
        sound::pump(env);
        let stream = stream::pump(env)?;
        let midi = adv_drv::audio::midi::native_pump(env)?;
        Ok([wave, stream, midi].into_iter().flatten().min())
    })?
}

fn check_callback(
    env: &mut Env<'_>,
    target: u32,
    flags: u32,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    if flags & 0x70000 != 0x10000 || target == 0 {
        return then(env);
    }
    let is_window = adv_nt::process::module_base(env.cpu, env.sys.peb, "user32.dll")
        .and_then(|b| adv_nt::process::module_export(env.cpu, b, "IsWindow"));
    match is_window {
        Some(is_window) => Outcome::call(is_window, [target], move |env, result| {
            if result.value() == 0 {
                Outcome::Return(11)
            } else {
                then(env)
            }
        }),
        None => Outcome::Fail("WinMM cannot resolve IsWindow".into()),
    }
}
