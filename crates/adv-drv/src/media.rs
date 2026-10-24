//! Wine 11 winegstreamer and winedmo Unix tables over host media workers.
//! PE modules and their COM objects stay Wine's. Guest reads service AVIO;
//! worker completions signal machine objects, never execution-thread waits.
use std::collections::{HashMap, VecDeque};

use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_device::media::wine::{Command, Event, Info, Output, Sample, Worker};
use adv_machine::{Machine, Object, sched::Wait};
use adv_sync::WakeEvent;

mod demuxer;
mod format;
mod parser;
mod transform;

const WG: &str = "winegstreamer.dll";
const DMO: &str = "winedmo.dll";
const FAIL: u32 = 0x8000_4005;
const WRONG_STATE: u32 = 0x8004_0227;
const UNSUCCESSFUL: u32 = 0xc000_0001;
const BUFFER_SMALL: u32 = 0xc000_0023;
const NOT_FOUND: u32 = 0xc000_0225;
const NAMES: [&str; 37] = [
    "wg_init_gstreamer",
    "wg_parser_create",
    "wg_parser_destroy",
    "wg_parser_connect",
    "wg_parser_disconnect",
    "wg_parser_get_next_read_offset",
    "wg_parser_push_data",
    "wg_parser_get_stream_count",
    "wg_parser_get_stream",
    "wg_parser_stream_get_current_format",
    "wg_parser_stream_get_codec_format",
    "wg_parser_stream_enable",
    "wg_parser_stream_disable",
    "wg_parser_stream_get_buffer",
    "wg_parser_stream_copy_buffer",
    "wg_parser_stream_release_buffer",
    "wg_parser_stream_notify_qos",
    "wg_parser_stream_get_duration",
    "wg_parser_stream_get_tag",
    "wg_parser_stream_seek",
    "wg_transform_create",
    "wg_transform_destroy",
    "wg_transform_get_output_type",
    "wg_transform_set_output_type",
    "wg_transform_push_data",
    "wg_transform_read_data",
    "wg_transform_get_status",
    "wg_transform_drain",
    "wg_transform_flush",
    "wg_transform_notify_qos",
    "wg_muxer_create",
    "wg_muxer_destroy",
    "wg_muxer_add_stream",
    "wg_muxer_start",
    "wg_muxer_push_sample",
    "wg_muxer_read_data",
    "wg_muxer_finalize",
];
const DMO_NAMES: [&str; 9] = [
    "process_attach",
    "demuxer_check",
    "demuxer_create",
    "demuxer_destroy",
    "demuxer_read",
    "demuxer_seek",
    "demuxer_stream_lang",
    "demuxer_stream_name",
    "demuxer_stream_type",
];

#[derive(Default)]
struct State {
    wake: Option<WakeEvent>,
    next: u32,
    sources: HashMap<u32, Source>,
    streams: HashMap<u32, (u32, usize)>,
}
struct Source {
    worker: Option<Worker>,
    info: Option<Info>,
    error: Option<String>,
    read: Option<(u64, u32)>,
    read_event: u32,
    work_event: u32,
    connected: bool,
    compressed: bool,
    tracks: Vec<Track>,
    busy: usize,
    done: bool,
    waiting: Option<u32>,
    output_type: Vec<u8>,
    output_major: Vec<u8>,
    context: u32,
    start: u64,
    stop: Option<u64>,
}
struct Track {
    handle: u32,
    format: [u8; 120],
    output: Output,
    enabled: bool,
    pending: bool,
    eof: bool,
    samples: VecDeque<Sample>,
    offset: usize,
    discontinuity: bool,
}
impl Source {
    fn new(env: &mut Env<'_>, compressed: bool) -> Self {
        let mut event = || {
            env.sys.objects.insert(Object::Event {
                manual_reset: true,
                signaled: false,
            })
        };
        Self {
            worker: None,
            info: None,
            error: None,
            read: None,
            read_event: event(),
            work_event: event(),
            connected: false,
            compressed,
            tracks: Vec::new(),
            busy: 0,
            done: false,
            waiting: None,
            output_type: Vec::new(),
            output_major: Vec::new(),
            context: 0,
            start: 0,
            stop: None,
        }
    }
    fn send(&self, command: Command) -> Result<(), String> {
        self.worker
            .as_ref()
            .ok_or("media worker absent")?
            .send(command)
            .map_err(|e| e.to_string())
    }
}
fn id(env: &mut Env<'_>) -> u32 {
    let s = env.ext.get_or_default::<State>();
    s.next += 1;
    s.next
}
fn read(env: &Env<'_>, at: u32) -> Result<u32, String> {
    env.cpu.read_u32(at).map_err(|e| e.to_string())
}
fn wide(env: &Env<'_>, at: u32) -> Result<u64, String> {
    Ok(u64::from(read(env, at)?) | u64::from(read(env, at + 4)?) << 32)
}
fn write(env: &mut Env<'_>, at: u32, value: u32) -> Result<(), String> {
    env.cpu.write_u32(at, value).map_err(|e| e.to_string())
}
fn put_wide(env: &mut Env<'_>, at: u32, value: u64) -> Result<(), String> {
    env.cpu
        .write(at, &value.to_le_bytes())
        .map_err(|e| e.to_string())
}
fn bytes(env: &Env<'_>, at: u32, size: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0; size];
    env.cpu.read(at, &mut b).map_err(|e| e.to_string())?;
    Ok(b)
}
fn finish(result: Result<u32, String>) -> Outcome {
    match result {
        Ok(status) => Outcome::Return(status),
        Err(error) => Outcome::Fail(error),
    }
}
fn wait(env: &mut Env<'_>, event: u32, code: u32, args: u32) -> Outcome {
    env.sys.reset_event(event);
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline: None,
        },
        move |env, _| dispatch(env, code, args),
    )
}

pub(super) fn register(registry: &mut Registry) {
    for (index, name) in NAMES.into_iter().enumerate() {
        registry.add_with_data(
            WG,
            name,
            Conv::Cdecl { argc: 1 },
            |env| {
                let code = env.sys.thunks.get(env.thunk).f.data as u32;
                let args = env.arg(0);
                dispatch(env, code, args)
            },
            index as u64,
        );
    }
    for (index, name) in DMO_NAMES.into_iter().enumerate() {
        registry.add_with_data(
            DMO,
            name,
            Conv::Cdecl { argc: 1 },
            |env| {
                let code = env.sys.thunks.get(env.thunk).f.data as u32;
                let args = env.arg(0);
                dispatch(env, code, args)
            },
            index as u64 + 64,
        );
    }
}
pub fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    for (module, names) in [(WG, NAMES.as_slice()), (DMO, DMO_NAMES.as_slice())] {
        let table = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .map_err(|e| e.to_string())?;
        for (index, name) in names.iter().enumerate() {
            let entry = machine
                .sys
                .thunks
                .publish(&mut machine.cpu, module, name)
                .map_err(|e| e.to_string())?;
            machine
                .cpu
                .write_u32(table + index as u32 * 4, entry)
                .map_err(|e| e.to_string())?;
        }
        machine
            .ext
            .get_or_default::<adv_nt::unixlib::HostTables>()
            .0
            .insert(module.into(), table);
    }
    Ok(())
}
pub fn set_wake(ext: &mut adv_machine::Ext, wake: WakeEvent) {
    ext.get_or_default::<State>().wake = Some(wake);
}

/// Drain worker notifications before the session sleeps. No decoder work runs here.
pub fn pump<B: Backend>(machine: &mut Machine<B>) -> Result<bool, String> {
    let Some(state) = machine.ext.try_get_mut::<State>() else {
        return Ok(false);
    };
    let mut signals = Vec::new();
    for source in state.sources.values_mut() {
        while let Some(event) = source.worker.as_ref().and_then(Worker::poll) {
            match event {
                Event::Read { offset, size } => {
                    source.read = Some((offset, size));
                    signals.push(source.read_event);
                    if source.context != 0 {
                        signals.push(source.work_event);
                    }
                }
                Event::Ready(result) => {
                    match result {
                        Ok(info) => source.info = Some(info),
                        Err(error) => source.error = Some(error.to_string()),
                    }
                    signals.push(source.work_event);
                }
                Event::Sample(index, result) => {
                    let track = &mut source.tracks[index];
                    track.pending = false;
                    match result {
                        Ok(Some(sample))
                            if track.enabled && !matches!(source.waiting, Some(19 | 69)) =>
                        {
                            track.samples.push_back(sample)
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => track.eof = true,
                        Err(error) => source.error = Some(error.to_string()),
                    }
                    signals.push(source.work_event);
                }
                Event::Done(result) => {
                    source.busy = source.busy.saturating_sub(1);
                    source.done = true;
                    if let Err(error) = result {
                        source.error = Some(error.to_string());
                    }
                    signals.push(source.work_event);
                }
            }
        }
    }
    for &event in &signals {
        machine.sys.set_event(event);
    }
    Ok(!signals.is_empty())
}
fn dispatch(env: &mut Env<'_>, code: u32, args: u32) -> Outcome {
    match code {
        0 => Outcome::Return(0),
        1..=19 => parser::call(env, code, args),
        20..=29 => transform::call(env, code, args),
        // wg_muxer_create returns STATUS_UNSUCCESSFUL when no encoder exists.
        30..=36 => Outcome::Return(UNSUCCESSFUL),
        64..=72 => demuxer::call(env, code, args),
        _ => unreachable!(),
    }
}
