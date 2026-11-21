//! The graph's IMediaSeeking and IMediaPosition. Like quartz, the graph asks
//! every member exposing IMediaSeeking in media time, newest first, and
//! combines the answers. Host playback answers as one more seeker: the host
//! timeline its renderers present.
use std::collections::VecDeque;

use adv_machine::{Env, Outcome};

use super::{
    Graph, RUNNING, TIME_FORMAT_MEDIA_TIME, TIME_FORMAT_NONE, Transition, current, duration,
    filter, g, resolve_position, seek, seek_guests,
};
use crate::dshow::*;

const GET_CAPABILITIES: u32 = 3;
const IS_FORMAT_SUPPORTED: u32 = 5;
const SET_TIME_FORMAT: u32 = 9;
const GET_DURATION: u32 = 10;
const GET_STOP_POSITION: u32 = 11;
const SET_POSITIONS: u32 = 14;
const SET_RATE: u32 = 17;
const GET_PREROLL: u32 = 19;

const ABSOLUTE: u32 = 1;
const RETURN_TIME: u32 = 8;
/// AM_SEEKING_CanSeekAbsolute through AM_SEEKING_CanGetDuration.
const HOST_CAPABILITIES: u32 = 0x3f;
const E_FAIL: u32 = 0x8000_4005;

#[derive(Clone, Copy)]
enum Seeker {
    Host,
    Guest(u32),
}

type Seekers = Box<dyn FnOnce(&mut Env<'_>, Vec<Seeker>) -> Outcome>;
type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

fn failed(hr: u32) -> bool {
    hr & 0x8000_0000 != 0
}

/// Whether host stages stream in the graph: one past the source is
/// connected, or a DMO stage learned its guest upstream. A host source only
/// read by a guest parser is just the async reader and seeks nothing.
fn host_timeline(env: &Env<'_>, graph: u32) -> bool {
    let state = env.ext.get::<State>();
    let connected = state.graphs[&graph]
        .members
        .iter()
        .filter_map(|m| state.filters.get(&m.host?))
        .filter(|f| f.role != filter::Role::FileSource)
        .any(|f| f.pins.iter().any(|p| state.pins[p].peer != 0));
    connected || !crate::dshow::dmo::upstream_seekers(env, graph).is_empty()
}

/// Populate cached `IMediaSeeking` pointers on guest members supporting media time.
pub(super) fn probe_seekers(
    env: &mut Env<'_>,
    graph: u32,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    seekers(env, graph, Box::new(move |env, _| then(env)))
}

/// Members' cached IMediaSeeking, newest first. A guest member is asked for
/// one until it gives one that supports media time, as quartz caches it.
fn seekers(env: &mut Env<'_>, graph: u32, then: Seekers) -> Outcome {
    let pending: VecDeque<u32> = g(env, graph)
        .members
        .iter()
        .rev()
        .filter(|m| m.host.is_none() && m.seeking == 0)
        .map(|m| m.filter)
        .collect();
    probe(env, graph, pending, then)
}

fn probe(env: &mut Env<'_>, graph: u32, mut pending: VecDeque<u32>, then: Seekers) -> Outcome {
    let Some(filter) = pending.pop_front() else {
        let host = host_timeline(env, graph);
        let mut list = Vec::new();
        for m in g(env, graph).members.iter().rev() {
            match m.host {
                Some(_) if host && !list.iter().any(|s| matches!(s, Seeker::Host)) => {
                    list.push(Seeker::Host)
                }
                None if m.seeking != 0 => list.push(Seeker::Guest(m.seeking)),
                _ => {}
            }
        }
        return then(env, list);
    };
    let Some(scratch) = alloc(env, 20) else {
        return probe(env, graph, pending, then);
    };
    let _ = write_guid(env, scratch, &IID_IMEDIASEEKING);
    let _ = env.cpu.write_u32(scratch + 16, 0);
    guest_call(
        env,
        filter,
        0,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let seeking = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            if hr != S_OK || seeking == 0 {
                free(env, scratch);
                return probe(env, graph, pending, then);
            }
            let _ = write_guid(env, scratch, &TIME_FORMAT_MEDIA_TIME);
            guest_call(
                env,
                seeking,
                IS_FORMAT_SUPPORTED,
                vec![scratch],
                move |env, hr| {
                    free(env, scratch);
                    let member = state(env)
                        .graphs
                        .get_mut(&graph)
                        .and_then(|g| g.members.iter_mut().find(|m| m.filter == filter));
                    match member {
                        Some(member) if hr == S_OK => {
                            member.seeking = seeking;
                            probe(env, graph, pending, then)
                        }
                        _ => guest_call(env, seeking, 2, vec![], move |env, _| {
                            probe(env, graph, pending, then)
                        }),
                    }
                },
            )
        },
    )
}

/// The host timeline's answer to IMediaSeeking method `slot`.
fn host(env: &mut Env<'_>, graph: u32, slot: u32, args: &[u32], then: Done) -> Outcome {
    match slot {
        GET_CAPABILITIES => {
            let _ = env.cpu.write_u32(args[0], HOST_CAPABILITIES);
            then(env, S_OK)
        }
        IS_FORMAT_SUPPORTED | SET_TIME_FORMAT => {
            let media_time = read_guid(env, args[0]).is_ok_and(|f| f == TIME_FORMAT_MEDIA_TIME);
            then(env, if media_time { S_OK } else { S_FALSE })
        }
        GET_DURATION => {
            let value = duration(env, graph);
            write_i64(env, args[0], value);
            then(env, S_OK)
        }
        GET_STOP_POSITION => {
            let value = stop_position(env, graph);
            write_i64(env, args[0], value);
            then(env, S_OK)
        }
        GET_PREROLL => {
            write_i64(env, args[0], 0);
            then(env, S_OK)
        }
        SET_RATE => {
            let rate = f64::from_bits(u64::from(args[0]) | (u64::from(args[1]) << 32));
            if rate.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                return then(env, E_INVALIDARG);
            }
            set_rate(env, graph, rate);
            then(env, S_OK)
        }
        SET_POSITIONS => {
            let [current_pointer, current_flags, stop_pointer, stop_flags] = args[..4] else {
                unreachable!()
            };
            let now = current(env, graph);
            let position = read_i64(env, current_pointer)
                .and_then(|value| resolve_position(now, value, current_flags));
            let stop_now = stop_position(env, graph);
            let stop = read_i64(env, stop_pointer)
                .and_then(|value| resolve_position(stop_now, value, stop_flags));
            seek(env, graph, position, stop);
            seek_guests(
                env,
                graph,
                position,
                stop,
                Box::new(move |env, _| {
                    let value = current(env, graph);
                    write_i64(env, current_pointer, value);
                    let value = stop_position(env, graph);
                    write_i64(env, stop_pointer, value);
                    then(env, S_OK)
                }),
            )
        }
        _ => then(env, E_NOTIMPL),
    }
}

/// Keep the stop position the members report as a run starts; completion
/// leaves the graph's position there.
pub(super) fn keep_stop(env: &mut Env<'_>, request: Transition, then: Finish) -> Outcome {
    longest(
        env,
        request.graph,
        GET_STOP_POSITION,
        Box::new(move |env, hr, stop| {
            if request.current(env) && !failed(hr) {
                g(env, request.graph).stream_stop = Some(stop);
            }
            then(env)
        }),
    )
}

fn stop_position(env: &Env<'_>, graph: u32) -> i64 {
    let stop = env.ext.get::<State>().graphs[&graph].stop;
    stop.unwrap_or_else(|| duration(env, graph))
}

fn set_rate(env: &mut Env<'_>, graph: u32, rate: f64) {
    let now = env.sys.clock.now_us();
    let graph_state: &mut Graph = g(env, graph);
    graph_state.position = graph_state.position(now);
    if graph_state.origin.is_some() {
        graph_state.origin = Some(now);
    }
    graph_state.rate = rate;
}

type Finish = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;
type Fold = Box<dyn FnMut(&mut Env<'_>, u32) -> bool>;
/// An HRESULT and the combined answer.
type Answered<T> = Box<dyn FnOnce(&mut Env<'_>, u32, T) -> Outcome>;
type Found = Box<dyn FnOnce(&mut Env<'_>, Option<Seeker>) -> Outcome>;

/// Ask each seeker method `slot` with `args`; `fold` sees each HRESULT and
/// returns false to stop. `prime` rewrites the arguments' memory before each.
struct Ask {
    graph: u32,
    slot: u32,
    args: Vec<u32>,
    prime: Box<dyn FnMut(&mut Env<'_>)>,
    fold: Fold,
}

fn ask(env: &mut Env<'_>, mut a: Ask, mut seekers: VecDeque<Seeker>, done: Finish) -> Outcome {
    let Some(seeker) = seekers.pop_front() else {
        return done(env);
    };
    (a.prime)(env);
    let (graph, slot, args) = (a.graph, a.slot, a.args.clone());
    let next: Done = Box::new(move |env, hr| {
        if !(a.fold)(env, hr) {
            return done(env);
        }
        ask(env, a, seekers, done)
    });
    match seeker {
        Seeker::Host => host(env, graph, slot, &args, next),
        Seeker::Guest(pointer) => guest_call(env, pointer, slot, args, next),
    }
}

/// The HRESULTs of a broadcast: E_NOTIMPL answers abstain, the first other
/// non-S_OK answer is kept, and failures override success codes.
#[derive(Clone, Copy)]
struct Combined {
    hr: u32,
    answered: bool,
}

impl Combined {
    const fn new() -> Self {
        Self {
            hr: S_OK,
            answered: false,
        }
    }

    fn add(&mut self, hr: u32) {
        if hr == E_NOTIMPL {
            return;
        }
        self.answered = true;
        if self.hr == S_OK || (failed(hr) && !failed(self.hr)) {
            self.hr = hr;
        }
    }

    fn result(self) -> u32 {
        if self.answered { self.hr } else { E_NOTIMPL }
    }
}

fn shared<T>(value: T) -> std::rc::Rc<std::cell::Cell<T>> {
    std::rc::Rc::new(std::cell::Cell::new(value))
}

fn no_prime() -> Box<dyn FnMut(&mut Env<'_>)> {
    Box::new(|_| {})
}

/// Broadcast `slot` with one output word at `scratch`, ANDing the answers.
fn capabilities(
    env: &mut Env<'_>,
    graph: u32,
    seekers: Vec<Seeker>,
    then: Answered<u32>,
) -> Outcome {
    let Some(scratch) = alloc(env, 4) else {
        return then(env, E_OUTOFMEMORY, 0);
    };
    let caps = shared(u32::MAX);
    let combined = shared(Combined::new());
    let (fold_caps, fold_combined) = (caps.clone(), combined.clone());
    let a = Ask {
        graph,
        slot: GET_CAPABILITIES,
        args: vec![scratch],
        prime: no_prime(),
        fold: Box::new(move |env, hr| {
            let mut c = fold_combined.get();
            c.add(hr);
            fold_combined.set(c);
            if !failed(hr) {
                let value = env.cpu.read_u32(scratch).unwrap_or(0);
                fold_caps.set(fold_caps.get() & value);
            }
            true
        }),
    };
    ask(
        env,
        a,
        seekers.into(),
        Box::new(move |env| {
            free(env, scratch);
            then(env, combined.get().result(), caps.get())
        }),
    )
}

/// The largest answer to a time query (`slot`) among the seekers; quartz
/// returns the first real failure, and E_NOTIMPL when nobody answers.
fn longest(env: &mut Env<'_>, graph: u32, slot: u32, then: Answered<i64>) -> Outcome {
    seekers(
        env,
        graph,
        Box::new(move |env, seekers| {
            let Some(scratch) = alloc(env, 8) else {
                return then(env, E_OUTOFMEMORY, 0);
            };
            let best = shared((E_NOTIMPL, 0i64));
            let fold_best = best.clone();
            let a = Ask {
                graph,
                slot,
                args: vec![scratch],
                prime: no_prime(),
                fold: Box::new(move |env, hr| {
                    let (_, value) = fold_best.get();
                    if !failed(hr) {
                        let answer = read_i64(env, scratch).unwrap_or(0);
                        fold_best.set((S_OK, value.max(answer)));
                        return true;
                    }
                    if hr == E_NOTIMPL {
                        return true;
                    }
                    fold_best.set((hr, value));
                    false
                }),
            };
            ask(
                env,
                a,
                seekers.into(),
                Box::new(move |env| {
                    free(env, scratch);
                    let (hr, value) = best.get();
                    then(env, hr, value)
                }),
            )
        }),
    )
}

type Positioned = Box<dyn FnOnce(&mut Env<'_>, u32, i64, i64) -> Outcome>;

/// IMediaSeeking::SetPositions. Every seeker gets the caller's values, which
/// a ReturnTime caller sees replaced by each answer; the graph keeps the last
/// current position returned. A running graph pauses around the seek.
fn set_positions(
    env: &mut Env<'_>,
    graph: u32,
    (current_value, current_flags): (i64, u32),
    (stop_value, stop_flags): (i64, u32),
    then: Positioned,
) -> Outcome {
    seekers(
        env,
        graph,
        Box::new(move |env, seekers| {
            if seekers.is_empty() {
                return then(env, E_NOTIMPL, current_value, stop_value);
            }
            // A run still waiting for its members to pause is not paused again.
            if g(env, graph).state != RUNNING || g(env, graph).pending_run.is_some() {
                return position_all(
                    env,
                    graph,
                    seekers,
                    (current_value, current_flags),
                    (stop_value, stop_flags),
                    then,
                );
            }
            super::pause(
                env,
                graph,
                Box::new(move |env, _| {
                    position_all(
                        env,
                        graph,
                        seekers,
                        (current_value, current_flags),
                        (stop_value, stop_flags),
                        Box::new(move |env, hr, current, stop| {
                            super::run(
                                env,
                                graph,
                                Box::new(move |env, _| then(env, hr, current, stop)),
                            )
                        }),
                    )
                }),
            )
        }),
    )
}

fn position_all(
    env: &mut Env<'_>,
    graph: u32,
    seekers: Vec<Seeker>,
    (current_value, current_flags): (i64, u32),
    (stop_value, stop_flags): (i64, u32),
    then: Positioned,
) -> Outcome {
    let Some(scratch) = alloc(env, 16) else {
        return then(env, E_OUTOFMEMORY, current_value, stop_value);
    };
    // (result, caller's current, caller's stop, graph position)
    let outcome = shared((E_NOTIMPL, current_value, stop_value, None::<i64>));
    let (prime_outcome, fold_outcome) = (outcome.clone(), outcome.clone());
    let a = Ask {
        graph,
        slot: SET_POSITIONS,
        args: vec![
            scratch,
            current_flags | RETURN_TIME,
            scratch + 8,
            stop_flags,
        ],
        prime: Box::new(move |env| {
            let (_, current, stop, _) = prime_outcome.get();
            write_i64(env, scratch, current);
            write_i64(env, scratch + 8, stop);
        }),
        fold: Box::new(move |env, hr| {
            let (_, mut current, mut stop, mut position) = fold_outcome.get();
            if failed(hr) {
                if hr == E_NOTIMPL {
                    return true;
                }
                fold_outcome.set((hr, current, stop, position));
                return false;
            }
            let answer = read_i64(env, scratch).unwrap_or(0);
            if current_flags & RETURN_TIME != 0 {
                current = answer;
            }
            if stop_flags & RETURN_TIME != 0 {
                stop = read_i64(env, scratch + 8).unwrap_or(0);
            }
            position = Some(answer);
            fold_outcome.set((S_OK, current, stop, position));
            true
        }),
    };
    ask(
        env,
        a,
        seekers.into(),
        Box::new(move |env| {
            free(env, scratch);
            let (hr, current, stop, position) = outcome.get();
            if let Some(position) = position {
                let now = env.sys.clock.now_us();
                g(env, graph).set_position(position, now);
            }
            then(env, hr, current, stop)
        }),
    )
}

/// Re-seek every seeker to the graph's position, as quartz does on Stop.
pub(super) fn rewind(env: &mut Env<'_>, graph: u32, then: Finish) -> Outcome {
    let position = g(env, graph).position;
    set_positions(
        env,
        graph,
        (position, ABSOLUTE),
        (0, 0),
        Box::new(move |env, _, _, _| then(env)),
    )
}

fn write_time(env: &mut Env<'_>, output: u32, hr: u32, value: i64) -> Outcome {
    if !failed(hr) {
        write_i64(env, output, value);
    }
    Outcome::Return(hr)
}

pub(super) fn call(env: &mut Env<'_>, graph: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let a1 = env.arg(1);
    let pointer_methods = [
        "GetCapabilities",
        "CheckCapabilities",
        "QueryPreferredFormat",
        "GetTimeFormat",
        "IsUsingTimeFormat",
        "SetTimeFormat",
        "GetDuration",
        "GetStopPosition",
        "GetCurrentPosition",
        "GetRate",
    ];
    if a1 == 0 && pointer_methods.contains(&method) {
        return r(E_POINTER);
    }
    match method {
        "GetCapabilities" | "CheckCapabilities" => {
            let check = method == "CheckCapabilities";
            let wanted = env.cpu.read_u32(a1).unwrap_or(0);
            Some(seekers(
                env,
                graph,
                Box::new(move |env, seekers| {
                    if seekers.is_empty() {
                        let _ = env.cpu.write_u32(a1, 0);
                        return Outcome::Return(if check { E_FAIL } else { S_OK });
                    }
                    capabilities(
                        env,
                        graph,
                        seekers,
                        Box::new(move |env, hr, caps| {
                            if failed(hr) {
                                return Outcome::Return(hr);
                            }
                            let caps = if check { caps & wanted } else { caps };
                            let _ = env.cpu.write_u32(a1, caps);
                            Outcome::Return(match () {
                                _ if !check => hr,
                                _ if caps == 0 => E_FAIL,
                                _ if caps != wanted => S_FALSE,
                                _ => S_OK,
                            })
                        }),
                    )
                }),
            ))
        }
        "IsFormatSupported" => Some(seekers(
            env,
            graph,
            Box::new(move |env, seekers| {
                if seekers.is_empty() {
                    return Outcome::Return(E_NOTIMPL);
                }
                let Ok(format) = read_guid(env, a1) else {
                    return Outcome::Return(E_POINTER);
                };
                if format == TIME_FORMAT_MEDIA_TIME {
                    return Outcome::Return(S_OK);
                }
                supporter(
                    env,
                    graph,
                    seekers,
                    format,
                    Box::new(|_, found| {
                        Outcome::Return(if found.is_some() { S_OK } else { S_FALSE })
                    }),
                )
            }),
        )),
        "QueryPreferredFormat" => {
            let _ = write_guid(env, a1, &TIME_FORMAT_MEDIA_TIME);
            r(S_OK)
        }
        "GetTimeFormat" => {
            let format = g(env, graph).time_format;
            let _ = write_guid(env, a1, &format);
            r(S_OK)
        }
        "IsUsingTimeFormat" => {
            let format = read_guid(env, a1).unwrap_or(GUID_NULL);
            r(if format == g(env, graph).time_format {
                S_OK
            } else {
                S_FALSE
            })
        }
        "SetTimeFormat" => {
            let Ok(format) = read_guid(env, a1) else {
                return r(E_POINTER);
            };
            Some(set_time_format(env, graph, format))
        }
        "ConvertTimeFormat" => {
            let own = g(env, graph).time_format;
            let format = |env: &Env<'_>, pointer: u32| {
                if pointer == 0 {
                    own
                } else {
                    read_guid(env, pointer).unwrap_or(GUID_NULL)
                }
            };
            let (target, source) = (format(env, env.arg(2)), format(env, env.arg(5)));
            if target != source {
                return r(E_NOTIMPL);
            }
            let value = arg_i64(env, 3);
            r(if write_i64(env, a1, value) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetDuration" | "GetStopPosition" | "GetPreroll" => {
            let slot = match method {
                "GetDuration" => GET_DURATION,
                "GetStopPosition" => GET_STOP_POSITION,
                _ => GET_PREROLL,
            };
            if method != "GetPreroll" {
                write_i64(env, a1, 0);
            }
            Some(longest(
                env,
                graph,
                slot,
                Box::new(move |env, hr, value| write_time(env, a1, hr, value)),
            ))
        }
        "GetCurrentPosition" => {
            let value = current(env, graph);
            write_i64(env, a1, value);
            r(S_OK)
        }
        "GetPositions" => {
            let stop = env.arg(2);
            if a1 != 0 {
                let value = current(env, graph);
                write_i64(env, a1, value);
            }
            if stop == 0 {
                return r(S_OK);
            }
            Some(longest(
                env,
                graph,
                GET_STOP_POSITION,
                Box::new(move |env, hr, value| write_time(env, stop, hr, value)),
            ))
        }
        "GetAvailable" => {
            let latest = env.arg(2);
            Some(seekers(
                env,
                graph,
                Box::new(move |env, seekers| {
                    if !seekers.iter().any(|s| matches!(s, Seeker::Host)) {
                        return Outcome::Return(E_NOTIMPL);
                    }
                    write_i64(env, a1, 0);
                    let value = duration(env, graph);
                    write_i64(env, latest, value);
                    Outcome::Return(S_OK)
                }),
            ))
        }
        "SetPositions" => {
            let (current_flags, stop_pointer, stop_flags) = (env.arg(2), env.arg(3), env.arg(4));
            let current_value = read_i64(env, a1).unwrap_or(0);
            let stop_value = read_i64(env, stop_pointer).unwrap_or(0);
            Some(set_positions(
                env,
                graph,
                (current_value, current_flags),
                (stop_value, stop_flags),
                Box::new(move |env, hr, current, stop| {
                    if current_flags & RETURN_TIME != 0 {
                        write_i64(env, a1, current);
                    }
                    if stop_flags & RETURN_TIME != 0 {
                        write_i64(env, stop_pointer, stop);
                    }
                    Outcome::Return(hr)
                }),
            ))
        }
        "SetRate" => Some(rate(env, graph, arg_f64(env, 1))),
        "GetRate" => {
            let rate = g(env, graph).rate;
            let _ = env.cpu.write(a1, &rate.to_le_bytes());
            r(S_OK)
        }
        _ => None,
    }
}

/// The first seeker, newest first, that supports `format`.
fn supporter(
    env: &mut Env<'_>,
    graph: u32,
    seekers: Vec<Seeker>,
    format: Iid,
    then: Found,
) -> Outcome {
    let Some(scratch) = alloc(env, 16) else {
        return then(env, None);
    };
    let _ = write_guid(env, scratch, &format);
    let index = shared((0usize, None::<usize>));
    let fold_index = index.clone();
    let list = seekers.clone();
    let a = Ask {
        graph,
        slot: IS_FORMAT_SUPPORTED,
        args: vec![scratch],
        prime: no_prime(),
        fold: Box::new(move |_, hr| {
            let (at, _) = fold_index.get();
            if hr == S_OK {
                fold_index.set((at, Some(at)));
                return false;
            }
            fold_index.set((at + 1, None));
            true
        }),
    };
    ask(
        env,
        a,
        seekers.into(),
        Box::new(move |env| {
            free(env, scratch);
            then(env, index.get().1.map(|at| list[at]))
        }),
    )
}

/// SetTimeFormat: one supporting seeker carries a format other than media
/// time, every other seeker returns to media time. TIME_FORMAT_NONE
/// restores media time.
fn set_time_format(env: &mut Env<'_>, graph: u32, format: Iid) -> Outcome {
    seekers(
        env,
        graph,
        Box::new(move |env, seekers| {
            let media_time = format == TIME_FORMAT_MEDIA_TIME || format == TIME_FORMAT_NONE;
            if media_time && g(env, graph).time_format == TIME_FORMAT_MEDIA_TIME {
                return Outcome::Return(S_OK);
            }
            if seekers.is_empty() {
                return Outcome::Return(if media_time { S_OK } else { E_NOTIMPL });
            }
            let all = seekers.clone();
            let carry = move |env: &mut Env<'_>, carrier: Option<Seeker>| {
                let Some(scratch) = alloc(env, 32) else {
                    return Outcome::Return(E_OUTOFMEMORY);
                };
                let _ = write_guid(env, scratch, &format);
                let _ = write_guid(env, scratch + 16, &TIME_FORMAT_MEDIA_TIME);
                let carrier = carrier.and_then(|s| match s {
                    Seeker::Guest(pointer) => Some(pointer),
                    Seeker::Host => None,
                });
                let mut calls: VecDeque<Call> = all
                    .iter()
                    .filter_map(|s| match *s {
                        Seeker::Guest(pointer) if Some(pointer) != carrier => {
                            Some(Call(pointer, SET_TIME_FORMAT, vec![scratch + 16]))
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(pointer) = carrier {
                    calls.push_back(Call(pointer, SET_TIME_FORMAT, vec![scratch]));
                }
                chain(
                    env,
                    calls,
                    Vec::new(),
                    Box::new(move |env, results| {
                        free(env, scratch);
                        let hr = if carrier.is_some() {
                            *results.last().unwrap()
                        } else {
                            S_OK
                        };
                        if hr == S_OK {
                            g(env, graph).time_format = if media_time {
                                TIME_FORMAT_MEDIA_TIME
                            } else {
                                format
                            };
                        }
                        Outcome::Return(hr)
                    }),
                )
            };
            if media_time {
                return carry(env, None);
            }
            supporter(
                env,
                graph,
                seekers.clone(),
                format,
                Box::new(move |env, found| match found {
                    Some(Seeker::Guest(_)) => carry(env, found),
                    _ => Outcome::Return(E_FAIL),
                }),
            )
        }),
    )
}

/// SetRate reaches every seeker; with none, only the current rate stands.
fn rate(env: &mut Env<'_>, graph: u32, value: f64) -> Outcome {
    seekers(
        env,
        graph,
        Box::new(move |env, seekers| {
            if seekers.is_empty() {
                let same = g(env, graph).rate == value;
                return Outcome::Return(if same { S_OK } else { E_NOTIMPL });
            }
            let bits = value.to_bits();
            let combined = shared(Combined::new());
            let fold_combined = combined.clone();
            let a = Ask {
                graph,
                slot: SET_RATE,
                args: vec![bits as u32, (bits >> 32) as u32],
                prime: no_prime(),
                fold: Box::new(move |_, hr| {
                    let mut c = fold_combined.get();
                    c.add(hr);
                    fold_combined.set(c);
                    true
                }),
            };
            ask(
                env,
                a,
                seekers.into(),
                Box::new(move |env| {
                    let hr = combined.get().result();
                    if !failed(hr) {
                        set_rate(env, graph, value);
                    }
                    Outcome::Return(hr)
                }),
            )
        }),
    )
}

/// IMediaPosition: the graph's IMediaSeeking in REFTIME seconds.
pub(super) fn position(env: &mut Env<'_>, graph: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let a1 = env.arg(1);
    let seconds = move |env: &mut Env<'_>, hr: u32, value: i64| {
        if !failed(hr) {
            let _ = env
                .cpu
                .write(a1, &(value as f64 / 10_000_000.0).to_le_bytes());
        }
        Outcome::Return(hr)
    };
    let time = |env: &Env<'_>| (arg_f64(env, 1) * 10_000_000.0) as i64;
    match method {
        "get_Duration" => Some(longest(
            env,
            graph,
            GET_DURATION,
            Box::new(move |env, hr, value| seconds(env, hr, value)),
        )),
        "get_StopTime" => Some(longest(
            env,
            graph,
            GET_STOP_POSITION,
            Box::new(move |env, hr, value| seconds(env, hr, value)),
        )),
        "get_CurrentPosition" => {
            let value = current(env, graph);
            Some(seconds(env, S_OK, value))
        }
        "put_CurrentPosition" => {
            let value = time(env);
            Some(set_positions(
                env,
                graph,
                (value, ABSOLUTE),
                (0, 0),
                Box::new(|_, hr, _, _| Outcome::Return(hr)),
            ))
        }
        "put_StopTime" => {
            let value = time(env);
            Some(set_positions(
                env,
                graph,
                (0, 0),
                (value, ABSOLUTE),
                Box::new(|_, hr, _, _| Outcome::Return(hr)),
            ))
        }
        "get_PrerollTime" => {
            let _ = env.cpu.write(a1, &0f64.to_le_bytes());
            r(S_OK)
        }
        "put_PrerollTime" => r(S_OK),
        "put_Rate" => Some(rate(env, graph, arg_f64(env, 1))),
        "get_Rate" => {
            let value = g(env, graph).rate;
            let _ = env.cpu.write(a1, &value.to_le_bytes());
            r(S_OK)
        }
        "CanSeekForward" | "CanSeekBackward" => {
            let _ = env.cpu.write_u32(a1, u32::MAX);
            r(S_OK)
        }
        _ => None,
    }
}
