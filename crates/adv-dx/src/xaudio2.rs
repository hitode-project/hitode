//! XAudio2's versioned ABI over the session's device-owned voice graph.
//! Wine engines retain the compatibility path for foreign XAPOs. They remain
//! stopped while native voices run, and share the same host output on fallback.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, atomic::Ordering};

use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Backend, GuestExt};
use adv_device::xaudio2::{self as device, Command};
use adv_machine::{Env, Machine, Object, Outcome, Wait};

pub const MODULES: [&str; 3] = ["xaudio2_7.dll", "xaudio2_8.dll", "xaudio2_9.dll"];
const INVALID: u32 = 0x8896_0001;
const IUNKNOWN: [u8; 16] = adv_abi::com::IUNKNOWN;
const ICLASSFACTORY: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const CLASSES: [[u8; 16]; 4] = [
    [
        0x85, 0x86, 0x50, 0x5a, 0x54, 0xa2, 0xba, 0x4f, 0x9b, 0x82, 0x9a, 0x24, 0xb0, 0x03, 0x06,
        0xaf,
    ],
    [
        0x35, 0xea, 0x05, 0xdb, 0x29, 0x03, 0x4b, 0x4d, 0xa5, 0x3a, 0x6d, 0xea, 0xd0, 0x3d, 0x38,
        0x52,
    ],
    [
        0x5f, 0x10, 0xc1, 0xca, 0x9b, 0x61, 0x04, 0x4d, 0x83, 0x1a, 0x44, 0xe1, 0xcb, 0xf1, 0x2d,
        0x57,
    ],
    [
        0x0e, 0x13, 0x93, 0x6a, 0x53, 0x1d, 0xd1, 0x41, 0xa9, 0xcf, 0xe7, 0x58, 0x80, 0x0b, 0xb1,
        0x79,
    ],
];
const IIDS: [[u8; 16]; 3] = [
    [
        0x58, 0x1f, 0xcf, 0x8b, 0xe7, 0x9f, 0x83, 0x45, 0x8a, 0xc6, 0xe2, 0xad, 0xc4, 0x65, 0xc8,
        0xbb,
    ],
    [
        0xc8, 0xda, 0xd8, 0x60, 0xa1, 0x5a, 0x8e, 0x4e, 0xb5, 0x97, 0x2f, 0x5e, 0x28, 0x83, 0xd4,
        0x84,
    ],
    [
        0xcf, 0xe3, 0x02, 0x2b, 0x0b, 0x2e, 0xc3, 0x4e, 0xbe, 0x45, 0x1b, 0x2a, 0x3f, 0xe7, 0x21,
        0x0d,
    ],
];
const ENGINE: [(&str, u8); 16] = [
    ("Engine::QueryInterface", 3),
    ("Engine::AddRef", 1),
    ("Engine::Release", 1),
    ("Engine::RegisterForCallbacks", 2),
    ("Engine::UnregisterForCallbacks", 2),
    ("Engine::CreateSourceVoice", 8),
    ("Engine::CreateSubmixVoice", 8),
    ("Engine::CreateMasteringVoice", 8),
    ("Engine::StartEngine", 1),
    ("Engine::StopEngine", 1),
    ("Engine::CommitChanges", 2),
    ("Engine::GetPerformanceData", 2),
    ("Engine::SetDebugConfiguration", 3),
    ("Engine::GetDeviceCount", 2),
    ("Engine::GetDeviceDetails", 3),
    ("Engine::Initialize", 3),
];
const VOICE: [(&str, u8); 29] = [
    ("Voice::GetVoiceDetails", 2),
    ("Voice::SetOutputVoices", 2),
    ("Voice::SetEffectChain", 2),
    ("Voice::EnableEffect", 3),
    ("Voice::DisableEffect", 3),
    ("Voice::GetEffectState", 3),
    ("Voice::SetEffectParameters", 5),
    ("Voice::GetEffectParameters", 4),
    ("Voice::SetFilterParameters", 3),
    ("Voice::GetFilterParameters", 2),
    ("Voice::SetOutputFilterParameters", 4),
    ("Voice::GetOutputFilterParameters", 3),
    ("Voice::SetVolume", 3),
    ("Voice::GetVolume", 2),
    ("Voice::SetChannelVolumes", 4),
    ("Voice::GetChannelVolumes", 3),
    ("Voice::SetOutputMatrix", 6),
    ("Voice::GetOutputMatrix", 5),
    ("Voice::DestroyVoice", 1),
    ("Voice::Start", 3),
    ("Voice::Stop", 3),
    ("Voice::SubmitSourceBuffer", 3),
    ("Voice::FlushSourceBuffers", 1),
    ("Voice::Discontinuity", 1),
    ("Voice::ExitLoop", 2),
    ("Voice::GetState", 3),
    ("Voice::SetFrequencyRatio", 3),
    ("Voice::GetFrequencyRatio", 2),
    ("Voice::SetSourceSampleRate", 2),
];
const FACTORY: [(&str, u8); 5] = [
    ("Factory::QueryInterface", 3),
    ("Factory::AddRef", 1),
    ("Factory::Release", 1),
    ("Factory::CreateInstance", 4),
    ("Factory::LockServer", 2),
];

struct Engine {
    generation: u64,
    version: usize,
    original: u32,
    references: u32,
    control: device::Control,
    fallback: bool,
    callbacks: Vec<u32>,
    running: bool,
    matrices: Vec<(u32, u32, u32, Vec<f32>)>,
    pending: Vec<(u32, Command)>,
}
struct Voice {
    generation: u64,
    engine: u32,
    original: u32,
    kind: u32,
    channels: usize,
    rate: u32,
    flags: u32,
    format: Vec<u8>,
    state: Arc<device::State>,
    max_ratio: f32,
    volume: f32,
    playing: bool,
    volumes: Vec<f32>,
    ratio: f32,
    sends: Vec<device::Send>,
    effects: Vec<(u32, bool, usize)>,
    parameters: BTreeMap<u32, Vec<u8>>,
    buffers: VecDeque<Submitted>,
    fallback_allocations: Vec<u32>,
}
struct Submitted {
    token: u32,
    descriptor: [u32; 9],
    data: Vec<u8>,
    wma: Vec<u8>,
}
#[derive(Clone, Copy)]
enum Effect {
    Meter,
    Reverb,
    FxReverb,
}
#[derive(Default)]
struct State {
    engines: BTreeMap<u32, Engine>,
    voices: BTreeMap<u32, Voice>,
    effects: BTreeMap<u32, Effect>,
    tables: [[u32; 4]; 3],
    factory: u32,
    factories: BTreeMap<u32, (usize, u32)>,
    worker: u32,
    worker_event: u32,
    callbacks: VecDeque<device::Callback>,
    next_buffer: u32,
    next_generation: u64,
    pending_callbacks: VecDeque<device::Callback>,
    worker_started: bool,
}

pub fn register(registry: &mut Registry) {
    for (version, module) in MODULES.into_iter().enumerate() {
        for (index, (name, mut argc)) in ENGINE.into_iter().enumerate() {
            if version == 0 && index == 7 {
                argc = 7;
            }
            registry.add_with_data(
                module,
                name,
                Conv::Stdcall { argc },
                engine_method,
                ((version as u64) << 32) | index as u64,
            );
        }
        for (index, (name, mut argc)) in VOICE.into_iter().enumerate() {
            if version == 0 && index == 25 {
                argc = 2;
            }
            registry.add_with_data(
                module,
                name,
                Conv::Stdcall { argc },
                voice_method,
                ((version as u64) << 32) | index as u64,
            );
        }
        registry.add_with_data(
            module,
            "XAudio2Create",
            Conv::Stdcall { argc: 3 },
            create_export,
            version as u64,
        );
        registry.add_with_data(
            module,
            "XAudio2CreateWithVersionInfo",
            Conv::Stdcall { argc: 4 },
            create_export,
            version as u64,
        );
        for (index, (name, argc)) in [
            ("DllCanUnloadNow", 0),
            ("DllGetClassObject", 3),
            ("DllRegisterServer", 0),
            ("DllUnregisterServer", 0),
            ("CreateAudioReverb", 1),
            ("CreateAudioVolumeMeter", 1),
            ("CreateFX", 4),
            ("X3DAudioCalculate", 5),
            ("X3DAudioInitialize", 3),
        ]
        .into_iter()
        .enumerate()
        {
            let conv = if index >= 6 {
                Conv::Cdecl { argc }
            } else {
                Conv::Stdcall { argc }
            };
            registry.add_with_data(
                module,
                name,
                conv,
                export,
                ((version as u64) << 32) | index as u64,
            );
        }
    }
    for (index, (name, argc)) in FACTORY.into_iter().enumerate() {
        registry.add_with_data(
            MODULES[0],
            name,
            Conv::Stdcall { argc },
            factory_method,
            index as u64,
        );
    }
    registry.add(
        MODULES[0],
        "CallbackWorker",
        Conv::Stdcall { argc: 1 },
        worker,
    );
    registry.add(
        MODULES[0],
        "Master::GetChannelMask",
        Conv::Stdcall { argc: 2 },
        |env| {
            let this = env.arg(0);
            let out = env.arg(1);
            let original = env.ext.get::<State>().voices[&this].original;
            forward(
                env,
                original,
                19,
                vec![original, out],
                Box::new(|_, hr| Outcome::Return(hr)),
            )
        },
    );
}

fn alloc(env: &mut Env<'_>, size: u32) -> Result<u32, String> {
    (env.ext.get::<Host>().allocator.allocate)(env, size)
        .ok_or_else(|| "guest allocation failed".into())
}
fn free(env: &mut Env<'_>, at: u32) {
    (env.ext.get::<Host>().allocator.free)(env, at);
}
fn read(env: &Env<'_>, at: u32) -> Result<u32, String> {
    env.cpu.read_u32(at).map_err(|e| e.to_string())
}
fn write(env: &mut Env<'_>, at: u32, value: u32) -> Result<(), String> {
    env.cpu.write_u32(at, value).map_err(|e| e.to_string())
}
fn bytes(env: &Env<'_>, at: u32, size: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0; size];
    env.cpu.read(at, &mut b).map_err(|e| e.to_string())?;
    Ok(b)
}
fn words(env: &Env<'_>, at: u32, count: usize) -> Result<Vec<u32>, String> {
    (0..count).map(|i| read(env, at + 4 * i as u32)).collect()
}
fn put(env: &mut Env<'_>, at: u32, words: &[u32]) -> Result<(), String> {
    for (i, &word) in words.iter().enumerate() {
        write(env, at + 4 * i as u32, word)?;
    }
    Ok(())
}
fn guid(env: &Env<'_>, at: u32) -> Result<[u8; 16], String> {
    Ok(bytes(env, at, 16)?.try_into().unwrap())
}
fn finish(result: Result<u32, String>) -> Outcome {
    match result {
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(format!("XAudio2: {error}")),
    }
}
fn data(env: &Env<'_>) -> (usize, usize) {
    let data = env.sys.thunks.get(env.thunk).f.data;
    ((data >> 32) as usize, data as u32 as usize)
}
fn command(env: &mut Env<'_>, engine: u32, set: u32, command: Command) {
    env.ext
        .get_mut::<State>()
        .engines
        .get_mut(&engine)
        .unwrap()
        .control
        .send(set, command);
    env.ext
        .get::<adv_device::AudioControl>()
        .sync_clock(env.sys.clock.now_us());
}
fn method(env: &Env<'_>, object: u32, index: usize) -> Result<u32, String> {
    read(env, read(env, object)? + 4 * index as u32)
}
fn engine_slot(version: usize, index: usize) -> usize {
    if version == 0 {
        match index {
            0..=2 => index,
            13..=15 => index - 10,
            _ => index + 3,
        }
    } else {
        index
    }
}
pub type Then = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
#[derive(Clone, Copy)]
pub struct Host {
    pub allocator: crate::interop::GuestAllocator,
    pub original: fn(&mut Env<'_>, usize, &'static str, Vec<u32>, Then) -> Outcome,
    pub export: fn(&Env<'_>, &str, &str) -> Result<u32, String>,
}
fn kernel(env: &Env<'_>, name: &str) -> Result<u32, String> {
    (env.ext.get::<Host>().export)(env, "kernel32.dll", name)
}
fn original(
    env: &mut Env<'_>,
    version: usize,
    name: &'static str,
    args: Vec<u32>,
    then: Then,
) -> Outcome {
    (env.ext.get::<Host>().original)(env, version, name, args, then)
}

fn forward(
    env: &mut Env<'_>,
    object: u32,
    index: usize,
    mut args: Vec<u32>,
    then: Then,
) -> Outcome {
    args[0] = object;
    match method(env, object, index) {
        Ok(target) => Outcome::call(target, args, move |env, result| then(env, result.value())),
        Err(error) => finish(Err(error)),
    }
}

pub fn install<B: Backend>(machine: &mut Machine<B>, host: Host) -> Result<(), String> {
    let mut state = State::default();
    for (version, module) in MODULES.into_iter().enumerate() {
        let mut table = |methods: &[(&str, u8)]| -> Result<u32, String> {
            let page = machine
                .sys
                .vm
                .alloc(&mut machine.cpu, 4096, adv_cpu::backend::Prot::RW)
                .map_err(|e| e.to_string())?;
            for (i, (name, _)) in methods.iter().enumerate() {
                let entry = machine
                    .sys
                    .thunks
                    .publish(&mut machine.cpu, module, name)
                    .map_err(|e| e.to_string())?;
                machine
                    .cpu
                    .write_u32(page + 4 * i as u32, entry)
                    .map_err(|e| e.to_string())?;
            }
            Ok(page)
        };
        let engine: Vec<_> = if version == 0 {
            ENGINE[..3]
                .iter()
                .chain(ENGINE[13..].iter())
                .chain(ENGINE[3..13].iter())
                .copied()
                .collect()
        } else {
            ENGINE[..13].to_vec()
        };
        state.tables[version][0] = table(&engine)?;
        state.tables[version][1] = table(&VOICE)?;
        state.tables[version][2] = table(&VOICE[..19])?;
        let mut master = VOICE[..19].to_vec();
        if version > 0 {
            master.push(("Master::GetChannelMask", 2));
        }
        // The channel-mask thunk is shared by all versions.
        if version > 0 {
            let page = table(&VOICE[..19])?;
            let entry = machine
                .sys
                .thunks
                .publish(&mut machine.cpu, MODULES[0], "Master::GetChannelMask")
                .map_err(|e| e.to_string())?;
            machine
                .cpu
                .write_u32(page + 76, entry)
                .map_err(|e| e.to_string())?;
            state.tables[version][3] = page;
        } else {
            state.tables[version][3] = table(&master)?;
        }
    }
    let page = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, adv_cpu::backend::Prot::RW)
        .map_err(|e| e.to_string())?;
    for (i, (name, _)) in FACTORY.into_iter().enumerate() {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULES[0], name)
            .map_err(|e| e.to_string())?;
        machine
            .cpu
            .write_u32(page + 4 * i as u32, entry)
            .map_err(|e| e.to_string())?;
    }
    state.factory = page;
    state.worker = machine
        .sys
        .thunks
        .publish(&mut machine.cpu, MODULES[0], "CallbackWorker")
        .map_err(|e| e.to_string())?;
    state.worker_event = machine.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    machine.ext.insert(state);
    machine.ext.insert(host);
    Ok(())
}

fn create_export(env: &mut Env<'_>) -> Outcome {
    let version = env.sys.thunks.get(env.thunk).f.data as usize;
    let args = (0..3).map(|i| env.arg(i)).collect();
    create(env, version, args)
}
fn create(env: &mut Env<'_>, version: usize, args: Vec<u32>) -> Outcome {
    let out = args[0];
    if version == 0 {
        let scratch = match alloc(env, 64) {
            Ok(at) => at,
            Err(error) => return finish(Err(error)),
        };
        let prepared = env
            .cpu
            .write(scratch, &CLASSES[0])
            .and_then(|()| env.cpu.write(scratch + 16, &ICLASSFACTORY))
            .map_err(|e| e.to_string());
        if let Err(error) = prepared {
            return finish(Err(error));
        }
        original(
            env,
            0,
            "DllGetClassObject",
            vec![scratch, scratch + 16, scratch + 32],
            Box::new(move |env, hr| {
                if hr != 0 {
                    free(env, scratch);
                    return Outcome::Return(hr);
                }
                let factory = read(env, scratch + 32).unwrap();
                if let Err(error) = env.cpu.write(scratch + 16, &IIDS[0]) {
                    return finish(Err(error.to_string()));
                }
                forward(
                    env,
                    factory,
                    3,
                    vec![factory, 0, scratch + 16, out],
                    Box::new(move |env, hr| {
                        forward(
                            env,
                            factory,
                            2,
                            vec![factory],
                            Box::new(move |env, _| {
                                free(env, scratch);
                                if hr != 0 {
                                    Outcome::Return(hr)
                                } else {
                                    wrap_engine(env, version, out)
                                }
                            }),
                        )
                    }),
                )
            }),
        )
    } else {
        original(
            env,
            version,
            "XAudio2Create",
            args,
            Box::new(move |env, hr| {
                if hr == 0 {
                    wrap_engine(env, version, out)
                } else {
                    Outcome::Return(hr)
                }
            }),
        )
    }
}
fn wrap_engine(env: &mut Env<'_>, version: usize, out: u32) -> Outcome {
    let prepared = (|| {
        let original = read(env, out)?;
        let object = alloc(env, 4)?;
        let table = env.ext.get::<State>().tables[version][0];
        write(env, object, table)?;
        write(env, out, object)?;
        Ok((object, original))
    })();
    let (object, original) = match prepared {
        Ok(v) => v,
        Err(error) => return finish(Err(error)),
    };
    env.ext.get_mut::<State>().next_generation += 1;
    let generation = env.ext.get::<State>().next_generation;
    let (control, mut graph) = device::Graph::new();
    graph.owner = object;
    graph.generation = generation;
    let source = source(env, object);
    env.ext
        .get::<adv_device::AudioControl>()
        .add_xaudio(source, graph);
    env.ext.get_mut::<State>().engines.insert(
        object,
        Engine {
            generation,
            version,
            original,
            references: 1,
            control,
            fallback: false,
            callbacks: Vec::new(),
            running: true,
            matrices: Vec::new(),
            pending: Vec::new(),
        },
    );
    forward(
        env,
        original,
        engine_slot(version, 9),
        vec![original],
        Box::new(move |env, _| {
            if env.ext.get::<State>().worker_started {
                return Outcome::Return(0);
            }
            env.ext.get_mut::<State>().worker_started = true;
            let entry = env.ext.get::<State>().worker;
            let target = match kernel(env, "CreateThread") {
                Ok(t) => t,
                Err(error) => return finish(Err(error)),
            };
            Outcome::call(target, vec![0, 0, entry, 0, 0, 0], |env, result| {
                if result.value() == 0 {
                    return Outcome::Return(0x8007_000e);
                }
                match kernel(env, "CloseHandle") {
                    Ok(target) => {
                        Outcome::call(target, vec![result.value()], |_, _| Outcome::Return(0))
                    }
                    Err(error) => finish(Err(error)),
                }
            })
        }),
    )
}
fn activate_effect(env: &mut Env<'_>, class: usize, iid: u32, out: u32) -> Outcome {
    let scratch = match alloc(env, 36) {
        Ok(at) => at,
        Err(error) => return finish(Err(error)),
    };
    if let Err(error) = env
        .cpu
        .write(scratch, &CLASSES[class])
        .and_then(|()| env.cpu.write(scratch + 16, &ICLASSFACTORY))
    {
        return finish(Err(error.to_string()));
    }
    original(
        env,
        0,
        "DllGetClassObject",
        vec![scratch, scratch + 16, scratch + 32],
        Box::new(move |env, hr| {
            if hr != 0 {
                free(env, scratch);
                return Outcome::Return(hr);
            }
            let factory = read(env, scratch + 32).unwrap();
            forward(
                env,
                factory,
                3,
                vec![factory, 0, iid, out],
                Box::new(move |env, hr| {
                    forward(
                        env,
                        factory,
                        2,
                        vec![factory],
                        Box::new(move |env, _| {
                            free(env, scratch);
                            if hr == 0
                                && let Ok(effect) = read(env, out)
                            {
                                env.ext.get_mut::<State>().effects.insert(
                                    effect,
                                    if class == 2 {
                                        Effect::Meter
                                    } else {
                                        Effect::Reverb
                                    },
                                );
                            }
                            Outcome::Return(hr)
                        }),
                    )
                }),
            )
        }),
    )
}
fn source(env: &Env<'_>, object: u32) -> u64 {
    (u64::from(env.sys.pid) << 32) | u64::from(object)
}

fn export(env: &mut Env<'_>) -> Outcome {
    let (version, index) = data(env);
    let names = [
        "DllCanUnloadNow",
        "DllGetClassObject",
        "DllRegisterServer",
        "DllUnregisterServer",
        "CreateAudioReverb",
        "CreateAudioVolumeMeter",
        "CreateFX",
        "X3DAudioCalculate",
        "X3DAudioInitialize",
    ];
    if index == 0 {
        return Outcome::Return(1);
    }
    if index == 1
        && let Ok(class) = guid(env, env.arg(0))
        && CLASSES.contains(&class)
    {
        return finish((|| {
            let iid = guid(env, env.arg(1))?;
            let out = env.arg(2);
            if iid != ICLASSFACTORY && iid != IUNKNOWN {
                write(env, out, 0)?;
                return Ok(0x8000_4002);
            }
            let object = alloc(env, 4)?;
            write(env, object, env.ext.get::<State>().factory)?;
            write(env, out, object)?;
            env.ext.get_mut::<State>().factories.insert(
                object,
                (CLASSES.iter().position(|c| *c == class).unwrap(), 1),
            );
            Ok(0)
        })());
    }
    let argc = match index {
        0 | 2 | 3 => 0,
        1 | 8 => 3,
        4 | 5 => 1,
        6 => 4,
        _ => 5,
    };
    let args: Vec<_> = (0..argc as u32).map(|i| env.arg(i)).collect();
    let effect = match index {
        4 => Some(Effect::Reverb),
        5 => Some(Effect::Meter),
        6 => guid(env, args[0])
            .ok()
            .filter(|class| {
                *class
                    == [
                        0x56, 0xca, 0x9a, 0x7d, 0x68, 0xcb, 0x07, 0x48, 0xb6, 0x32, 0xb1, 0x37,
                        0x35, 0x2e, 0x85, 0x96,
                    ]
            })
            .map(|_| Effect::FxReverb),
        _ => None,
    };
    let out = match index {
        4 | 5 => args[0],
        6 => args[1],
        _ => 0,
    };
    original(
        env,
        version,
        names[index],
        args,
        Box::new(move |env, hr| {
            if hr == 0
                && let Some(effect) = effect
                && let Ok(pointer) = read(env, out)
            {
                env.ext.get_mut::<State>().effects.insert(pointer, effect);
            }
            Outcome::Return(hr)
        }),
    )
}
fn factory_method(env: &mut Env<'_>) -> Outcome {
    let index = env.sys.thunks.get(env.thunk).f.data as usize;
    match index {
        0 => finish((|| {
            let iid = guid(env, env.arg(1))?;
            let out = env.arg(2);
            if iid == IUNKNOWN || iid == ICLASSFACTORY {
                write(env, out, env.arg(0))?;
                let this = env.arg(0);
                env.ext
                    .get_mut::<State>()
                    .factories
                    .get_mut(&this)
                    .unwrap()
                    .1 += 1;
                Ok(0)
            } else {
                write(env, out, 0)?;
                Ok(0x8000_4002)
            }
        })()),
        1 | 2 => {
            let this = env.arg(0);
            let factory = env.ext.get_mut::<State>().factories.get_mut(&this).unwrap();
            if index == 1 {
                factory.1 += 1;
            } else {
                factory.1 -= 1;
            }
            let references = factory.1;
            if references == 0 {
                env.ext.get_mut::<State>().factories.remove(&this);
                free(env, this);
            }
            Outcome::Return(references)
        }
        3 => {
            if let Err(error) = write(env, env.arg(3), 0) {
                return finish(Err(error));
            }
            if env.arg(1) != 0 {
                return Outcome::Return(0x8004_0110);
            }
            let class = env.ext.get::<State>().factories[&env.arg(0)].0;
            if class >= 2 {
                let out = env.arg(3);
                let iid = env.arg(2);
                return activate_effect(env, class, iid, out);
            }
            let iid = guid(env, env.arg(2));
            if !matches!(iid,Ok(iid) if iid==IUNKNOWN || iid==IIDS[0]) {
                return Outcome::Return(0x8000_4002);
            }
            create(env, 0, vec![env.arg(3), 0, 0xffff_ffff])
        }
        _ => Outcome::Return(0),
    }
}

fn engine_method(env: &mut Env<'_>) -> Outcome {
    let (version, index) = data(env);
    let this = env.arg(0);
    let Some(engine) = env.ext.get::<State>().engines.get(&this) else {
        return Outcome::Return(INVALID);
    };
    let original = engine.original;
    if index == 0 {
        return finish((|| {
            let iid = guid(env, env.arg(1))?;
            let out = env.arg(2);
            if iid == IUNKNOWN || iid == IIDS[version] {
                write(env, out, this)?;
                env.ext
                    .get_mut::<State>()
                    .engines
                    .get_mut(&this)
                    .unwrap()
                    .references += 1;
                Ok(0)
            } else {
                write(env, out, 0)?;
                Ok(0x8000_4002)
            }
        })());
    }
    if index == 1 {
        let engine = env.ext.get_mut::<State>().engines.get_mut(&this).unwrap();
        engine.references += 1;
        return Outcome::Return(engine.references);
    }
    if index == 2 {
        let engine = env.ext.get_mut::<State>().engines.get_mut(&this).unwrap();
        engine.references -= 1;
        let references = engine.references;
        if references != 0 {
            return Outcome::Return(references);
        }
        env.ext
            .get::<adv_device::AudioControl>()
            .remove_pcm(source(env, this));
        env.ext.get_mut::<State>().engines.remove(&this);
        let voices: Vec<_> = env
            .ext
            .get::<State>()
            .voices
            .iter()
            .filter(|(_, v)| v.engine == this)
            .map(|(&id, _)| id)
            .collect();
        for voice in voices {
            env.ext.get_mut::<State>().voices.remove(&voice);
            free(env, voice);
        }
        free(env, this);
        return forward(
            env,
            original,
            2,
            vec![original],
            Box::new(|_, _| Outcome::Return(0)),
        );
    }
    if (5..=7).contains(&index) {
        return create_voice(env, this, index);
    }
    let argc = ENGINE[index].1;
    let args: Vec<_> = (0..argc as u32).map(|i| env.arg(i)).collect();
    let saved = args.clone();
    if index == 8 || index == 9 {
        let running = index == 8;
        env.ext
            .get_mut::<State>()
            .engines
            .get_mut(&this)
            .unwrap()
            .running = running;
        if !env.ext.get::<State>().engines[&this].fallback {
            command(env, this, 0, Command::Engine(running));
            return Outcome::Return(0);
        }
    }
    forward(
        env,
        original,
        engine_slot(version, index),
        args,
        Box::new(move |env, hr| {
            if hr != 0 && index != 11 {
                return Outcome::Return(hr);
            }
            match index {
                3 | 4 => {
                    let engine = env.ext.get_mut::<State>().engines.get_mut(&this).unwrap();
                    if index == 3 {
                        if !engine.callbacks.contains(&saved[1]) {
                            engine.callbacks.push(saved[1]);
                        }
                    } else {
                        engine.callbacks.retain(|&cb| cb != saved[1]);
                    }
                    let callbacks = engine.callbacks.clone();
                    command(env, this, 0, Command::Callbacks(callbacks));
                }
                10 => {
                    let mut committed = Vec::new();
                    env.ext
                        .get_mut::<State>()
                        .engines
                        .get_mut(&this)
                        .unwrap()
                        .matrices
                        .retain_mut(|(set, voice, destination, matrix)| {
                            if saved[1] == 0 || saved[1] == *set {
                                committed.push((*voice, *destination, std::mem::take(matrix)));
                                false
                            } else {
                                true
                            }
                        });
                    for (voice, destination, matrix) in committed {
                        if let Some(send) = env
                            .ext
                            .get_mut::<State>()
                            .voices
                            .get_mut(&voice)
                            .and_then(|v| v.sends.iter_mut().find(|s| s.voice == destination))
                        {
                            send.matrix = matrix;
                        }
                    }
                    let mut ready = Vec::new();
                    env.ext
                        .get_mut::<State>()
                        .engines
                        .get_mut(&this)
                        .unwrap()
                        .pending
                        .retain_mut(|(set, change)| {
                            if saved[1] == 0 || saved[1] == *set {
                                ready.push(std::mem::replace(change, Command::Commit(0)));
                                false
                            } else {
                                true
                            }
                        });
                    for change in ready {
                        apply_cache(env, change);
                    }
                    command(env, this, 0, Command::Commit(saved[1]));
                }
                11 => {
                    if !env.ext.get::<State>().engines[&this].fallback {
                        let active = env
                            .ext
                            .get::<State>()
                            .voices
                            .values()
                            .filter(|voice| {
                                voice.engine == this && voice.kind == 0 && voice.playing
                            })
                            .count() as u32;
                        return finish(write(env, saved[1] + 36, active).map(|()| 0));
                    }
                }
                15 => {
                    return forward(
                        env,
                        original,
                        engine_slot(version, 9),
                        vec![original],
                        Box::new(move |_, _| Outcome::Return(hr)),
                    );
                }
                _ => {}
            }
            Outcome::Return(hr)
        }),
    )
}

fn native_sends(
    env: &Env<'_>,
    engine: u32,
    at: u32,
    channels: usize,
) -> Result<Vec<device::Send>, String> {
    let destinations: Vec<(u32, u32)> = if at == 0 {
        env.ext
            .get::<State>()
            .voices
            .iter()
            .filter(|(_, v)| v.engine == engine && v.kind == 2)
            .map(|(&id, _)| (id, 0))
            .collect()
    } else {
        let count = read(env, at)?;
        let ptr = read(env, at + 4)?;
        (0..count)
            .map(|i| -> Result<_, String> {
                Ok((read(env, ptr + 8 * i + 4)?, read(env, ptr + 8 * i)?))
            })
            .collect::<Result<_, _>>()?
    };
    let mut sends = Vec::new();
    for (destination, flags) in destinations {
        let Some(voice) = env.ext.get::<State>().voices.get(&destination) else {
            return Err("unknown output voice".into());
        };
        let mut matrix = vec![0.0; channels * voice.channels];
        for d in 0..voice.channels {
            for s in 0..channels {
                matrix[d * channels + s] = if channels == 1 || s == d { 1.0 } else { 0.0 };
            }
        }
        sends.push(device::Send {
            voice: destination,
            channels: voice.channels,
            matrix,
            filter: (flags & 0x80 != 0).then_some([0.0, 1.0, 1.0]),
            filter_state: vec![[0.0; 2]; voice.channels],
        });
    }
    Ok(sends)
}
fn translate_sends(env: &mut Env<'_>, at: u32) -> Result<u32, String> {
    if at == 0 {
        return Ok(0);
    }
    let count = read(env, at)?;
    let ptr = read(env, at + 4)?;
    let copy = alloc(env, 8 + 8 * count)?;
    put(env, copy, &[count, copy + 8])?;
    for i in 0..count {
        let flags = read(env, ptr + 8 * i)?;
        let voice = read(env, ptr + 8 * i + 4)?;
        let original = env
            .ext
            .get::<State>()
            .voices
            .get(&voice)
            .map_or(voice, |v| v.original);
        put(env, copy + 8 + 8 * i, &[flags, original])?;
    }
    Ok(copy)
}
fn chain(env: &Env<'_>, at: u32) -> Result<Vec<(u32, bool, usize)>, String> {
    if at == 0 {
        return Ok(Vec::new());
    }
    let count = read(env, at)?;
    let ptr = read(env, at + 4)?;
    (0..count)
        .map(|i| {
            Ok((
                read(env, ptr + 12 * i)?,
                read(env, ptr + 12 * i + 4)? != 0,
                read(env, ptr + 12 * i + 8)? as usize,
            ))
        })
        .collect()
}
fn supported(env: &Env<'_>, voice: &Voice) -> bool {
    let mut channels = voice.channels;
    for (effect, _, output) in &voice.effects {
        let Some(kind) = env.ext.get::<State>().effects.get(effect) else {
            return false;
        };
        let valid = match kind {
            Effect::Reverb => matches!((channels, *output), (1, 1 | 6) | (2, 2 | 6) | (6, 6)),
            Effect::Meter | Effect::FxReverb => channels == *output,
        };
        if !valid {
            return false;
        }
        channels = *output;
    }
    true
}

fn effects(env: &Env<'_>, voice: &Voice) -> Vec<device::Effect> {
    voice
        .effects
        .iter()
        .enumerate()
        .filter_map(|(i, (effect, enabled, channels))| {
            match env.ext.get::<State>().effects.get(effect)? {
                Effect::Meter => Some(device::Effect::Meter(*enabled)),
                Effect::Reverb => Some(device::Effect::Reverb {
                    enabled: *enabled,
                    channels: *channels,
                    parameters: voice
                        .parameters
                        .get(&(i as u32))
                        .cloned()
                        .unwrap_or_default(),
                }),
                Effect::FxReverb => Some(device::Effect::FxReverb),
            }
        })
        .collect()
}
fn create_voice(env: &mut Env<'_>, engine: u32, index: usize) -> Outcome {
    let version = env.ext.get::<State>().engines[&engine].version;
    let original = env.ext.get::<State>().engines[&engine].original;
    let kind = (index - 5) as u32;
    let argc = if version == 0 && index == 7 { 7 } else { 8 };
    let mut args: Vec<_> = (0..argc as u32).map(|i| env.arg(i)).collect();
    let saved = args.clone();
    let send_at = if index != 7 { args[6] } else { 0 };
    let chain_at = if index != 7 { args[7] } else { args[6] };
    let prepared = (|| {
        let (channels, rate, format) = if kind == 0 {
            let fmt = bytes(env, args[2], 18)?;
            let size = 18 + u16::from_le_bytes([fmt[16], fmt[17]]) as usize;
            (
                u16::from_le_bytes([fmt[2], fmt[3]]) as usize,
                u32::from_le_bytes(fmt[4..8].try_into().unwrap()),
                bytes(env, args[2], size)?,
            )
        } else {
            (
                if args[2] == 0 && kind == 2 {
                    2
                } else {
                    args[2] as usize
                },
                if args[3] == 0 { 44100 } else { args[3] },
                Vec::new(),
            )
        };
        let effects = chain(env, chain_at)?;
        let output_channels = effects.last().map_or(channels, |e| e.2);
        let sends = native_sends(env, engine, send_at, output_channels)?;
        let translated = translate_sends(env, send_at)?;
        Ok((channels, rate, format, sends, effects, translated))
    })();
    let (channels, rate, format, sends, chain, translated) = match prepared {
        Ok(v) => v,
        Err(error) => return finish(Err(error)),
    };
    if index != 7 {
        args[6] = translated;
    }
    let out = args[1];
    forward(
        env,
        original,
        engine_slot(version, index),
        args,
        Box::new(move |env, hr| {
            if translated != 0 {
                free(env, translated);
            }
            if hr != 0 {
                return Outcome::Return(hr);
            }
            let prepared = (|| {
                let original = read(env, out)?;
                let pointer = alloc(env, 4)?;
                let table = env.ext.get::<State>().tables[version][kind as usize + 1];
                write(env, pointer, table)?;
                write(env, out, pointer)?;
                Ok((pointer, original))
            })();
            let (pointer, original) = match prepared {
                Ok(v) => v,
                Err(error) => return finish(Err(error)),
            };
            env.ext.get_mut::<State>().next_generation += 1;
            let generation = env.ext.get::<State>().next_generation;
            let state = Arc::new(device::State::default());
            let voice = Voice {
                generation,
                engine,
                original,
                kind,
                channels,
                rate,
                flags: if kind == 0 { saved[3] } else { saved[4] },
                format,
                state: state.clone(),
                volume: 1.0,
                playing: false,
                volumes: vec![1.0; chain.last().map_or(channels, |e| e.2)],
                ratio: 1.0,
                max_ratio: if kind == 0 {
                    f32::from_bits(saved[4])
                } else {
                    1.0
                },
                sends: sends.clone(),
                effects: chain,
                parameters: BTreeMap::new(),
                buffers: VecDeque::new(),
                fallback_allocations: Vec::new(),
            };
            let unknown: Vec<_> = voice.effects.iter().map(|e| e.0).collect();
            let mut device_voice = device::Voice::new(
                pointer,
                channels,
                rate,
                match kind {
                    0 => 0,
                    1 => u64::from(saved[5]) + 1,
                    _ => u64::MAX,
                },
                if kind == 0 { saved[5] } else { 0 },
                state,
            );
            device_voice.generation = generation;
            device_voice.filter_enabled = voice.flags & 8 != 0 && kind != 2;
            device_voice.sends = sends;
            device_voice.effects = effects(env, &voice);
            env.ext.get_mut::<State>().voices.insert(pointer, voice);
            identify(
                env,
                unknown.into(),
                Box::new(move |env, _| {
                    let voice = &env.ext.get::<State>().voices[&pointer];
                    let native = supported(env, voice);
                    device_voice.effects = effects(env, voice);
                    if !env.ext.get::<State>().engines[&engine].fallback && native {
                        matrices(
                            env,
                            pointer,
                            0,
                            Box::new(move |env, _| {
                                device_voice.sends =
                                    env.ext.get::<State>().voices[&pointer].sends.clone();
                                command(env, engine, 0, Command::Create(Box::new(device_voice)));
                                Outcome::Return(0)
                            }),
                        )
                    } else if !native {
                        fallback(env, engine, Box::new(|_, _| Outcome::Return(0)))
                    } else {
                        Outcome::Return(0)
                    }
                }),
            )
        }),
    )
}

fn voice_method(env: &mut Env<'_>) -> Outcome {
    let (version, index) = data(env);
    let this = env.arg(0);
    let Some(voice) = env.ext.get::<State>().voices.get(&this) else {
        return Outcome::Return(INVALID);
    };
    let (engine, original) = (voice.engine, voice.original);
    let is_fallback = env.ext.get::<State>().engines[&engine].fallback;
    let argc = if version == 0 && index == 25 {
        2
    } else {
        VOICE[index].1
    };
    let mut args: Vec<_> = (0..argc as u32).map(|i| env.arg(i)).collect();
    let saved = args.clone();
    if !is_fallback {
        match index {
            0 => {
                let out = args[1];
                let details = if version == 0 {
                    vec![voice.flags, voice.channels as u32, voice.rate]
                } else {
                    vec![voice.flags, voice.flags, voice.channels as u32, voice.rate]
                };
                return finish(put(env, out, &details).map(|()| 0));
            }
            7 => {
                let effect = args[1] as usize;
                if voice
                    .effects
                    .get(effect)
                    .and_then(|(pointer, _, _)| env.ext.get::<State>().effects.get(pointer))
                    .is_some_and(|e| matches!(e, Effect::Meter))
                {
                    let out = args[2];
                    if args[3] != 12 {
                        return Outcome::Return(0x8007_0057);
                    }
                    let state = voice.state.clone();
                    let channels = voice.effects[effect].2;
                    let prepared = (|| {
                        let peaks = read(env, out)?;
                        let rms = read(env, out + 4)?;
                        let count = read(env, out + 8)? as usize;
                        for i in 0..count.min(channels).min(64) {
                            env.cpu
                                .write_u32(
                                    peaks + 4 * i as u32,
                                    state.peak[i].load(Ordering::Acquire),
                                )
                                .map_err(|e| e.to_string())?;
                            env.cpu
                                .write_u32(rms + 4 * i as u32, state.rms[i].load(Ordering::Acquire))
                                .map_err(|e| e.to_string())?;
                        }
                        Ok(0)
                    })();
                    return finish(prepared);
                }
            }
            5 => {
                let enabled = voice.effects[args[1] as usize].1;
                return finish(write(env, args[2], u32::from(enabled)).map(|()| 0));
            }
            13 => {
                let volume = voice.volume;
                return finish(write(env, args[1], volume.to_bits()).map(|()| 0));
            }
            15 => {
                if args[1] as usize != voice.effects.last().map_or(voice.channels, |e| e.2) {
                    return Outcome::Return(0x8007_0057);
                }
                let volumes: Vec<_> = voice.volumes.iter().map(|v| v.to_bits()).collect();
                return finish(put(env, args[2], &volumes).map(|()| 0));
            }
            17 => {
                let dest = if args[1] == 0 {
                    voice.sends.first().map_or(0, |s| s.voice)
                } else {
                    args[1]
                };
                let Some(send) = voice.sends.iter().find(|s| s.voice == dest) else {
                    return Outcome::Return(INVALID);
                };
                let matrix: Vec<_> = send.matrix.iter().map(|v| v.to_bits()).collect();
                return finish(put(env, args[4], &matrix).map(|()| 0));
            }
            21 => return submit(env, this, &args),
            25 => {
                let state = voice.state.clone();
                let out = args[1];
                let count = state.queued.load(Ordering::Acquire);
                let context = state.context.load(Ordering::Acquire);
                if version > 0 && args[2] & 0x100 != 0 {
                    return finish(put(env, out, &[context, count]).map(|()| 0));
                }
                let samples = state.samples.load(Ordering::Acquire);
                return finish(
                    put(
                        env,
                        out,
                        &[context, count, samples as u32, (samples >> 32) as u32],
                    )
                    .map(|()| 0),
                );
            }
            27 => {
                let ratio = voice.ratio;
                return finish(write(env, args[1], ratio.to_bits()).map(|()| 0));
            }
            28 if version > 0 && voice.state.queued.load(Ordering::Acquire) != 0 => {
                return Outcome::Return(INVALID);
            }
            _ => {}
        }
    }
    let mut scratch = 0;
    if index == 1 {
        match translate_sends(env, args[1]) {
            Ok(at) => {
                scratch = at;
                args[1] = at;
            }
            Err(error) => return finish(Err(error)),
        }
    }
    if matches!(index, 10 | 11 | 16 | 17) && args[1] != 0 {
        args[1] = env
            .ext
            .get::<State>()
            .voices
            .get(&args[1])
            .map_or(args[1], |v| v.original);
    }
    forward(
        env,
        original,
        index,
        args,
        Box::new(move |env, hr| {
            if scratch != 0 {
                free(env, scratch);
            }
            if hr != 0 && !matches!(index, 0 | 5 | 9 | 11 | 13 | 15 | 17 | 18 | 25 | 27) {
                return Outcome::Return(hr);
            }
            if index == 18 {
                command(env, engine, 0, Command::Destroy(this));
                let state = env.ext.get_mut::<State>();
                let owner = state.engines.get_mut(&engine).unwrap();
                owner.matrices.retain(|(_, voice, _, _)| *voice != this);
                owner.pending.retain(|(_, change)| match change {
                    Command::Volume(voice, _)
                    | Command::Channels(voice, _)
                    | Command::Ratio(voice, _)
                    | Command::Playing(voice, _)
                    | Command::EffectEnabled(voice, ..) => *voice != this,
                    _ => true,
                });
                let voice = state.voices.remove(&this).unwrap();
                for pointer in voice.fallback_allocations {
                    free(env, pointer);
                }
                free(env, this);
                return Outcome::Return(0);
            }
            if is_fallback {
                return Outcome::Return(hr);
            }
            let result = native_change(env, this, index, &saved);
            match result {
                Ok(Some((set, change))) => command(env, engine, set, change),
                Ok(None) => {}
                Err(error) => return finish(Err(error)),
            }
            if index == 1 {
                return matrices(
                    env,
                    this,
                    0,
                    Box::new(move |env, _| {
                        let sends = env.ext.get::<State>().voices[&this].sends.clone();
                        command(env, engine, 0, Command::Sends(this, sends));
                        Outcome::Return(hr)
                    }),
                );
            }
            if index == 2 {
                let unknown = env.ext.get::<State>().voices[&this]
                    .effects
                    .iter()
                    .map(|e| e.0)
                    .collect();
                return identify(
                    env,
                    unknown,
                    Box::new(move |env, _| {
                        let voice = &env.ext.get::<State>().voices[&this];
                        if !supported(env, voice) {
                            fallback(env, engine, Box::new(move |_, _| Outcome::Return(hr)))
                        } else {
                            let effects = effects(env, voice);
                            command(env, engine, 0, Command::Effects(this, effects));
                            Outcome::Return(hr)
                        }
                    }),
                );
            }
            Outcome::Return(hr)
        }),
    )
}
fn native_change(
    env: &mut Env<'_>,
    this: u32,
    index: usize,
    args: &[u32],
) -> Result<Option<(u32, Command)>, String> {
    let engine = env.ext.get::<State>().voices[&this].engine;
    let channels = env.ext.get::<State>().voices[&this]
        .effects
        .last()
        .map_or(env.ext.get::<State>().voices[&this].channels, |e| e.2);
    let change = match index {
        1 => {
            let sends = native_sends(env, engine, args[1], channels)?;
            env.ext
                .get_mut::<State>()
                .voices
                .get_mut(&this)
                .unwrap()
                .sends = sends.clone();
            (0, Command::Sends(this, sends))
        }
        2 => {
            let chain = chain(env, args[1])?;
            env.ext
                .get_mut::<State>()
                .voices
                .get_mut(&this)
                .unwrap()
                .effects = chain;
            return Ok(None);
        }
        3 | 4 => {
            cache(
                env,
                engine,
                args[2],
                Command::EffectEnabled(this, args[1] as usize, index == 3),
            );
            (
                args[2],
                Command::EffectEnabled(this, args[1] as usize, index == 3),
            )
        }
        6 => {
            let parameters = bytes(env, args[2], args[3] as usize)?;
            env.ext
                .get_mut::<State>()
                .voices
                .get_mut(&this)
                .unwrap()
                .parameters
                .insert(args[1], parameters);
            let effects = effects(env, &env.ext.get::<State>().voices[&this]);
            (
                args[4],
                Command::Effect(this, args[1] as usize, effects[args[1] as usize].clone()),
            )
        }
        8 => {
            let voice = &env.ext.get::<State>().voices[&this];
            if voice.kind == 2 || voice.flags & 8 == 0 {
                return Ok(None);
            }
            let values = words(env, args[1], 3)?;
            (
                args[2],
                Command::Filter(
                    this,
                    [
                        values[0] as f32,
                        f32::from_bits(values[1]),
                        f32::from_bits(values[2]),
                    ],
                ),
            )
        }
        10 => {
            let voice = &env.ext.get::<State>().voices[&this];
            if voice.kind == 2 {
                return Ok(None);
            }
            let destination = if args[1] == 0 {
                voice.sends.first().map_or(0, |s| s.voice)
            } else {
                args[1]
            };
            let values = words(env, args[2], 3)?;
            (
                args[3],
                Command::SendFilter(
                    this,
                    destination,
                    [
                        values[0] as f32,
                        f32::from_bits(values[1]),
                        f32::from_bits(values[2]),
                    ],
                ),
            )
        }
        12 => {
            let volume = f32::from_bits(args[1]);
            cache(env, engine, args[2], Command::Volume(this, volume));
            (args[2], Command::Volume(this, volume))
        }
        14 => {
            let volumes: Vec<_> = words(env, args[2], args[1] as usize)?
                .into_iter()
                .map(f32::from_bits)
                .collect();
            cache(
                env,
                engine,
                args[3],
                Command::Channels(this, volumes.clone()),
            );
            (args[3], Command::Channels(this, volumes))
        }
        16 => {
            let matrix: Vec<_> = words(env, args[4], (args[2] * args[3]) as usize)?
                .into_iter()
                .map(f32::from_bits)
                .collect();
            let voice = &env.ext.get::<State>().voices[&this];
            let destination = if args[1] == 0 {
                voice.sends.first().map_or(0, |s| s.voice)
            } else {
                args[1]
            };
            if args[5] == 0 || !env.ext.get::<State>().engines[&engine].running {
                if let Some(send) = env
                    .ext
                    .get_mut::<State>()
                    .voices
                    .get_mut(&this)
                    .unwrap()
                    .sends
                    .iter_mut()
                    .find(|s| s.voice == destination)
                {
                    send.matrix = matrix.clone();
                }
            } else {
                env.ext
                    .get_mut::<State>()
                    .engines
                    .get_mut(&engine)
                    .unwrap()
                    .matrices
                    .push((args[5], this, destination, matrix.clone()));
            }
            (args[5], Command::Matrix(this, destination, matrix))
        }
        20 if args[1] & 0x20 != 0 => {
            cache(env, engine, args[2], Command::Playing(this, true));
            (args[2], Command::Tails(this))
        }
        19 | 20 => {
            cache(env, engine, args[2], Command::Playing(this, index == 19));
            (args[2], Command::Playing(this, index == 19))
        }
        22 => (0, Command::Flush(this)),
        23 => (0, Command::Discontinuity(this)),
        24 => (args[1], Command::ExitLoop(this)),
        26 => {
            let voice = env.ext.get_mut::<State>().voices.get_mut(&this).unwrap();
            if voice.flags & 2 != 0 {
                return Ok(None);
            }
            let ratio = f32::from_bits(args[1]).clamp(1.0 / 1024.0, voice.max_ratio);
            cache(env, engine, args[2], Command::Ratio(this, ratio));
            (args[2], Command::Ratio(this, ratio))
        }
        28 => {
            env.ext
                .get_mut::<State>()
                .voices
                .get_mut(&this)
                .unwrap()
                .rate = args[1];
            (0, Command::Rate(this, args[1]))
        }
        _ => return Ok(None),
    };
    Ok(Some(change))
}

fn submit(env: &mut Env<'_>, this: u32, args: &[u32]) -> Outcome {
    let prepared = (|| {
        let descriptor: [u32; 9] = words(env, args[1], 9)?.try_into().unwrap();
        let voice = &env.ext.get::<State>().voices[&this];
        if voice.state.queued.load(Ordering::Acquire) >= 64 {
            return Ok(None);
        }
        let data = bytes(env, descriptor[2], descriptor[1] as usize)?;
        let mut samples = device::decode(&voice.format, &data)?;
        let wma = if args[2] == 0 {
            Vec::new()
        } else {
            let count = read(env, args[2] + 4)?;
            bytes(env, read(env, args[2])?, count as usize * 4)?
        };
        if let Some(cumulative) = wma.as_chunks::<4>().0.last() {
            let bits = u16::from_le_bytes(voice.format[14..16].try_into().unwrap());
            samples.truncate(u32::from_le_bytes(*cumulative) as usize / (usize::from(bits) / 8));
        }
        let frames = (samples.len() / voice.channels) as u32;
        let mut begin = descriptor[3];
        let mut length = if descriptor[4] == 0 {
            frames.saturating_sub(begin)
        } else {
            descriptor[4]
        };
        let Some(mut end) = begin.checked_add(length) else {
            return Ok(None);
        };
        if begin > frames
            || end > frames
            || (descriptor[7] == 0 && (descriptor[5] != 0 || descriptor[6] != 0))
        {
            return Ok(None);
        }
        let mut loop_begin = descriptor[5];
        let mut loop_end = if descriptor[6] == 0 {
            end
        } else {
            let Some(end) = loop_begin.checked_add(descriptor[6]) else {
                return Ok(None);
            };
            end
        };
        let version = env.ext.get::<State>().engines[&voice.engine].version;
        if descriptor[7] != 0
            && (loop_begin >= end || (version > 0 && (loop_end <= begin || loop_end > end)))
        {
            return Ok(None);
        }
        let loops = if version == 0 && loop_end <= begin {
            0
        } else {
            descriptor[7]
        };
        let tag = u16::from_le_bytes(voice.format[..2].try_into().unwrap());
        if tag == 2 {
            let block = u32::from(u16::from_le_bytes(voice.format[18..20].try_into().unwrap()));
            let mut loop_length = loop_end - loop_begin;
            begin -= begin % block;
            length -= length % block;
            loop_begin -= loop_begin % block;
            loop_length -= loop_length % block;
            end = begin + length;
            loop_end = loop_begin + loop_length;
        } else if !wma.is_empty() {
            loop_begin = 0;
            loop_end = end;
        }
        let buffer = device::Buffer::new(
            samples,
            descriptor[8],
            [begin, end],
            [loop_begin, loop_end, loops],
            descriptor[0] & 0x40 != 0,
        );
        Ok(Some((
            voice.engine,
            buffer,
            Submitted {
                token: 0,
                descriptor,
                data,
                wma,
            },
        )))
    })();
    match prepared {
        Ok(Some((engine, mut buffer, mut submitted))) => {
            let state = env.ext.get_mut::<State>();
            state.next_buffer += 1;
            let token = state.next_buffer;
            buffer.token = token;
            submitted.token = token;
            let voice = state.voices.get_mut(&this).unwrap();
            voice.buffers.push_back(submitted);
            voice.state.queued.fetch_add(1, Ordering::Release);
            command(env, engine, 0, Command::Submit(this, buffer));
            Outcome::Return(0)
        }
        Ok(None) => Outcome::Return(INVALID),
        Err(error) => finish(Err(error)),
    }
}

/// The compatibility engine already holds the graph and API settings. Only
/// submitted storage remains to transfer before starting Wine's engine.
fn fallback(env: &mut Env<'_>, engine: u32, then: Then) -> Outcome {
    if env.ext.get::<State>().engines[&engine].fallback {
        return then(env, 0);
    }
    env.ext
        .get::<adv_device::AudioControl>()
        .remove_pcm(source(env, engine));
    env.ext
        .get_mut::<State>()
        .engines
        .get_mut(&engine)
        .unwrap()
        .fallback = true;
    let voices: Vec<u32> = env
        .ext
        .get::<State>()
        .voices
        .iter()
        .filter(|(_, v)| v.engine == engine)
        .map(|(&id, _)| id)
        .collect();
    replay(env, engine, voices.into(), then)
}
fn replay(env: &mut Env<'_>, engine: u32, mut voices: VecDeque<u32>, then: Then) -> Outcome {
    while let Some(&voice) = voices.front() {
        let record = env
            .ext
            .get_mut::<State>()
            .voices
            .get_mut(&voice)
            .unwrap()
            .buffers
            .pop_front();
        let Some(record) = record else {
            voices.pop_front();
            continue;
        };
        let prepared = (|| {
            let data = alloc(env, record.data.len() as u32)?;
            env.cpu
                .write(data, &record.data)
                .map_err(|e| e.to_string())?;
            let desc = alloc(env, 44 + record.wma.len() as u32)?;
            let mut descriptor = record.descriptor;
            descriptor[2] = data;
            put(env, desc, &descriptor)?;
            let wma = if record.wma.is_empty() {
                0
            } else {
                put(env, desc + 36, &[desc + 44, record.wma.len() as u32 / 4])?;
                env.cpu
                    .write(desc + 44, &record.wma)
                    .map_err(|e| e.to_string())?;
                desc + 36
            };
            env.ext
                .get_mut::<State>()
                .voices
                .get_mut(&voice)
                .unwrap()
                .fallback_allocations
                .extend([data, desc]);
            Ok((desc, wma))
        })();
        let (desc, wma) = match prepared {
            Ok(v) => v,
            Err(error) => return finish(Err(error)),
        };
        let original = env.ext.get::<State>().voices[&voice].original;
        return forward(
            env,
            original,
            21,
            vec![original, desc, wma],
            Box::new(move |env, hr| {
                if hr != 0 {
                    return then(env, hr);
                }
                replay(env, engine, voices, then)
            }),
        );
    }
    let engine = &env.ext.get::<State>().engines[&engine];
    if !engine.running {
        return then(env, 0);
    }
    forward(
        env,
        engine.original,
        engine_slot(engine.version, 8),
        vec![engine.original],
        then,
    )
}

fn worker(env: &mut Env<'_>) -> Outcome {
    while let Some(callback) = env.ext.get_mut::<State>().callbacks.pop_front() {
        if callback.voice != 0 {
            let state = env.ext.get::<State>();
            let live = state.voices.get(&callback.voice).is_some_and(|voice| {
                voice.generation == callback.generation && !state.engines[&voice.engine].fallback
            }) || state.engines.get(&callback.voice).is_some_and(|engine| {
                engine.generation == callback.generation
                    && !engine.fallback
                    && engine.callbacks.contains(&callback.object)
            });
            if !live {
                continue;
            }
        }
        let target = match method(env, callback.object, callback.method as usize) {
            Ok(target) => target,
            Err(error) => return finish(Err(error)),
        };
        let mut args = vec![callback.object];
        args.extend(callback.args);
        return Outcome::call(target, args, |env, _| worker(env));
    }
    let event = env.ext.get::<State>().worker_event;
    env.sys.reset_event(event);
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline: None,
        },
        |env, _| worker(env),
    )
}

pub fn pump<B: Backend>(machine: &mut Machine<B>) -> Result<Option<u64>, String> {
    let now = machine.sys.clock.now_us();
    let device_now = machine
        .ext
        .get::<adv_device::AudioControl>()
        .playback_clock()
        .now_us(now);
    let state = machine.ext.get_mut::<State>();
    let mut deadline = None;
    state.callbacks.append(&mut state.pending_callbacks);
    for engine in state.engines.values_mut().filter(|engine| !engine.fallback) {
        for callback in engine.control.events() {
            if callback.object == 0 {
                if let Some(voice) = state.voices.get_mut(&callback.method) {
                    voice
                        .buffers
                        .retain(|buffer| buffer.token != callback.args[0]);
                }
            } else {
                state.callbacks.push_back(callback);
            }
        }
        deadline = Some(now + 10_000);
    }
    let mut ready = VecDeque::new();
    let mut pending = VecDeque::new();
    while let Some(callback) = state.callbacks.pop_front() {
        if callback.due_us <= device_now {
            ready.push_back(callback);
        } else {
            let due = now + callback.due_us - device_now;
            deadline = Some(deadline.map_or(due, |d: u64| d.min(due)));
            pending.push_back(callback);
        }
    }
    // The worker only sees due callbacks; pending records stay in a separate
    // queue until the session's next device deadline.
    state.callbacks = ready;
    state.pending_callbacks.extend(pending);
    if !state.callbacks.is_empty() {
        machine.sys.set_event(state.worker_event);
    }
    Ok(deadline)
}

/// Identify built-in XAPOs even when Wine's unchanged xapofx DLL created them.
fn identify(env: &mut Env<'_>, mut objects: VecDeque<u32>, then: Then) -> Outcome {
    let Some(object) = objects.pop_front() else {
        return then(env, 0);
    };
    env.ext.get_mut::<State>().effects.remove(&object);
    let scratch = match alloc(env, 24) {
        Ok(at) => at,
        Err(error) => return finish(Err(error)),
    };
    let iid = [
        0x84, 0xb9, 0x10, 0xa4, 0x39, 0x98, 0x19, 0x48, 0xa0, 0xbe, 0x28, 0x56, 0xae, 0x6b, 0x3a,
        0xdb,
    ];
    if let Err(error) = env.cpu.write(scratch, &iid) {
        return finish(Err(error.to_string()));
    }
    forward(
        env,
        object,
        0,
        vec![object, scratch, scratch + 16],
        Box::new(move |env, hr| {
            if hr != 0 {
                free(env, scratch);
                return identify(env, objects, then);
            }
            let xapo = read(env, scratch + 16).unwrap();
            forward(
                env,
                xapo,
                3,
                vec![xapo, scratch + 20],
                Box::new(move |env, hr| {
                    let props = if hr == 0 {
                        read(env, scratch + 20).unwrap()
                    } else {
                        0
                    };
                    if props != 0
                        && let Ok(class) = guid(env, props)
                    {
                        let fx = [
                            0x56, 0xca, 0x9a, 0x7d, 0x68, 0xcb, 0x07, 0x48, 0xb6, 0x32, 0xb1, 0x37,
                            0x35, 0x2e, 0x85, 0x96,
                        ];
                        let legacy = [
                            0x01, 0xc0, 0x0b, 0xa9, 0x97, 0xe8, 0x97, 0xe8, 0x74, 0x39, 0x43, 0x55,
                            0, 0, 0, 2,
                        ];
                        let reverb = [
                            0x0e, 0x13, 0x93, 0x6a, 0x4e, 0xcb, 0xe1, 0x4c, 0xa9, 0xcf, 0xe7, 0x58,
                            0x80, 0x0b, 0xb1, 0x79,
                        ];
                        let effect = if class == fx || class == legacy {
                            Some(Effect::FxReverb)
                        } else if class == CLASSES[2] {
                            Some(Effect::Meter)
                        } else if class == reverb || class == CLASSES[3] {
                            Some(Effect::Reverb)
                        } else {
                            None
                        };
                        if let Some(effect) = effect {
                            env.ext.get_mut::<State>().effects.insert(object, effect);
                        }
                    }
                    forward(
                        env,
                        xapo,
                        2,
                        vec![xapo],
                        Box::new(move |env, _| {
                            free(env, scratch);
                            if props == 0 {
                                return identify(env, objects, then);
                            }
                            match (env.ext.get::<Host>().export)(env, "ole32.dll", "CoTaskMemFree")
                            {
                                Ok(target) => Outcome::call(target, vec![props], move |env, _| {
                                    identify(env, objects, then)
                                }),
                                Err(error) => finish(Err(error)),
                            }
                        }),
                    )
                }),
            )
        }),
    )
}

/// Read Wine's default speaker matrix after graph topology changes.
fn matrices(env: &mut Env<'_>, this: u32, index: usize, then: Then) -> Outcome {
    let voice = &env.ext.get::<State>().voices[&this];
    let Some(send) = voice.sends.get(index) else {
        return then(env, 0);
    };
    let source_channels = voice.effects.last().map_or(voice.channels, |e| e.2);
    let channels = send.channels;
    let destination = env.ext.get::<State>().voices[&send.voice].original;
    let original = voice.original;
    let scratch = match alloc(env, (source_channels * channels * 4) as u32) {
        Ok(at) => at,
        Err(error) => return finish(Err(error)),
    };
    forward(
        env,
        original,
        17,
        vec![
            original,
            destination,
            source_channels as u32,
            channels as u32,
            scratch,
        ],
        Box::new(move |env, _| {
            let matrix = match words(env, scratch, source_channels * channels) {
                Ok(matrix) => matrix.into_iter().map(f32::from_bits).collect(),
                Err(error) => return finish(Err(error)),
            };
            free(env, scratch);
            env.ext
                .get_mut::<State>()
                .voices
                .get_mut(&this)
                .unwrap()
                .sends[index]
                .matrix = matrix;
            matrices(env, this, index + 1, then)
        }),
    )
}

fn cache(env: &mut Env<'_>, engine: u32, set: u32, change: Command) {
    if set == 0 || !env.ext.get::<State>().engines[&engine].running {
        apply_cache(env, change);
    } else {
        env.ext
            .get_mut::<State>()
            .engines
            .get_mut(&engine)
            .unwrap()
            .pending
            .push((set, change));
    }
}
fn apply_cache(env: &mut Env<'_>, change: Command) {
    match change {
        Command::EffectEnabled(id, index, value) => {
            if let Some(voice) = env.ext.get_mut::<State>().voices.get_mut(&id) {
                voice.effects[index].1 = value;
            }
        }
        Command::Playing(id, value) => {
            if let Some(voice) = env.ext.get_mut::<State>().voices.get_mut(&id) {
                voice.playing = value;
            }
        }
        Command::Volume(id, value) => {
            if let Some(voice) = env.ext.get_mut::<State>().voices.get_mut(&id) {
                voice.volume = value;
            }
        }
        Command::Channels(id, values) => {
            if let Some(voice) = env.ext.get_mut::<State>().voices.get_mut(&id) {
                voice.volumes = values;
            }
        }
        Command::Ratio(id, value) => {
            if let Some(voice) = env.ext.get_mut::<State>().voices.get_mut(&id) {
                voice.ratio = value;
            }
        }
        _ => {}
    }
}
