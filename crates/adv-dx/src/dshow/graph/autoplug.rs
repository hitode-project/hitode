//! IGraphBuilder::Connect and Render, autoplugged as quartz does: a direct
//! connection, then members newest first, then the filter mapper's matches,
//! each tried through its input pins and recursing through its output pins.
//! Host decoding stands in for the mapper's parsers and decoders when a guest
//! pin is rendered.
use std::rc::Rc;

use super::*;
use crate::dshow::filter::{INPUT, OUTPUT};

const MAX_DEPTH: u32 = 5;
const VFW_E_CANNOT_CONNECT: u32 = 0x8004_0217;
const VFW_E_CANNOT_RENDER: u32 = 0x8004_0218;
const VFW_E_NO_AUDIO_HARDWARE: u32 = 0x8004_0256;
const VFW_S_PARTIAL_RENDER: u32 = 0x0004_0242;
const E_FAIL: u32 = 0x8000_4005;
const MERIT_UNLIKELY: u32 = 0x0040_0000;
const IID_IPROPERTYBAG: Iid = guid(
    0x5527_2a00,
    0x42cb,
    0x11ce,
    [0x81, 0x35, 0x00, 0xaa, 0x00, 0x4b, 0xb8, 0x51],
);

type Hr = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
type Pins = Box<dyn FnOnce(&mut Env<'_>, Vec<u32>) -> Outcome>;
type Flag = Box<dyn FnOnce(&mut Env<'_>, bool) -> Outcome>;
type Types = Box<dyn FnOnce(&mut Env<'_>, u32, Vec<(Iid, Iid)>) -> Outcome>;
type Finish = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;
/// A COM pointer, or 0.
type Pointer = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
/// An HRESULT and a pin direction.
type Direction = Box<dyn FnOnce(&mut Env<'_>, u32, u32) -> Outcome>;
/// The next candidate (`None`) or the final HRESULT.
type Step = Box<dyn FnOnce(&mut Env<'_>, Option<u32>) -> Outcome>;

/// One autoplug request: the target input (0 when rendering) and whether
/// only filters with outputs may be added, as Connect and RenderEx ask.
#[derive(Clone, Copy)]
struct Plug {
    graph: u32,
    sink: u32,
    existing: bool,
}

fn failed(hr: u32) -> bool {
    hr & 0x8000_0000 != 0
}

fn hr_of(env: &mut Env<'_>, outcome: Outcome, then: Hr) -> Outcome {
    after(
        env,
        outcome,
        Box::new(move |env, results| then(env, results[0])),
    )
}

fn export(env: &Env<'_>, module: &str, name: &str) -> Option<u32> {
    env.ext
        .try_get::<Host>()
        .and_then(|host| (host.export)(env, module, name))
}

/// IGraphBuilder::Connect; an input given first is swapped with the output.
pub(super) fn connect(env: &mut Env<'_>, graph: u32, source: u32, sink: u32) -> Outcome {
    if source == 0 || sink == 0 {
        return Outcome::Return(E_POINTER);
    }
    direction(
        env,
        source,
        Box::new(move |env, hr, dir| {
            if failed(hr) {
                return Outcome::Return(hr);
            }
            let (source, sink) = if dir == INPUT {
                (sink, source)
            } else {
                (source, sink)
            };
            let plug = Plug {
                graph,
                sink,
                existing: true,
            };
            closes_loop(
                env,
                source,
                sink,
                Box::new(move |env, looped| {
                    if looped {
                        return Outcome::Return(VFW_E_CANNOT_CONNECT);
                    }
                    autoplug(env, plug, source, 0, Box::new(|_, hr| Outcome::Return(hr)))
                }),
            )
        }),
    )
}

/// Pins visited at most while looking for a loop.
const LOOP_STEPS: u32 = 64;

/// Whether `source` feeding `sink` would close a loop: walking downstream
/// from `sink` through each input's internal connections (every output of its
/// filter when it names none) reaches the filter owning `source`.
fn closes_loop(env: &mut Env<'_>, source: u32, sink: u32, then: Flag) -> Outcome {
    owner(
        env,
        source,
        Box::new(move |env, target| {
            if target == 0 {
                return then(env, false);
            }
            walk(
                env,
                target,
                VecDeque::from([sink]),
                0,
                Box::new(move |env, found| {
                    guest_call(env, target, 2, vec![], move |env, _| then(env, found))
                }),
            )
        }),
    )
}

fn walk(
    env: &mut Env<'_>,
    target: u32,
    mut inputs: VecDeque<u32>,
    steps: u32,
    then: Flag,
) -> Outcome {
    let Some(input) = inputs.pop_front() else {
        return then(env, false);
    };
    if steps >= LOOP_STEPS {
        return then(env, false);
    }
    internal(
        env,
        input,
        Box::new(move |env, outputs| follow(env, target, outputs.into(), inputs, steps + 1, then)),
    )
}

/// Check each held output: its filter closing the loop, else its peer
/// continuing the walk. Every output reference is released.
fn follow(
    env: &mut Env<'_>,
    target: u32,
    mut outputs: VecDeque<u32>,
    mut inputs: VecDeque<u32>,
    steps: u32,
    then: Flag,
) -> Outcome {
    let Some(output) = outputs.pop_front() else {
        return walk(env, target, inputs, steps, then);
    };
    owner(
        env,
        output,
        Box::new(move |env, filter| {
            let next: Finish = Box::new(move |env| {
                if filter == target {
                    let rest: Vec<u32> = std::iter::once(output).chain(outputs).collect();
                    return release_all(env, rest, Box::new(move |env| then(env, true)));
                }
                peer(
                    env,
                    output,
                    Box::new(move |env, next| {
                        // A connected peer stays alive through its connection.
                        if next != 0 {
                            inputs.push_back(next);
                        }
                        guest_call(env, output, 2, vec![], move |env, _| {
                            follow(env, target, outputs, inputs, steps, then)
                        })
                    }),
                )
            });
            if filter == 0 {
                return next(env);
            }
            guest_call(env, filter, 2, vec![], move |env, _| next(env))
        }),
    )
}

/// The outputs `input` feeds: its internal connections, or every output pin
/// of its filter when it does not say. Each comes with a reference.
fn internal(env: &mut Env<'_>, input: u32, then: Pins) -> Outcome {
    const CAPACITY: u32 = 16;
    let Some(scratch) = alloc(env, CAPACITY * 4 + 4) else {
        return then(env, Vec::new());
    };
    let count = scratch + CAPACITY * 4;
    let _ = env.cpu.write_u32(count, CAPACITY);
    guest_call(env, input, 13, vec![scratch, count], move |env, hr| {
        if hr == S_OK {
            let n = env.cpu.read_u32(count).unwrap_or(0).min(CAPACITY);
            let pins = (0..n)
                .filter_map(|i| env.cpu.read_u32(scratch + i * 4).ok())
                .filter(|&pin| pin != 0)
                .collect();
            free(env, scratch);
            return then(env, pins);
        }
        free(env, scratch);
        owner(
            env,
            input,
            Box::new(move |env, filter| {
                if filter == 0 {
                    return then(env, Vec::new());
                }
                pins(
                    env,
                    filter,
                    Box::new(move |env, all| {
                        guest_call(env, filter, 2, vec![], move |env, _| {
                            keep_outputs(env, all.into(), Vec::new(), then)
                        })
                    }),
                )
            }),
        )
    })
}

fn keep_outputs(
    env: &mut Env<'_>,
    mut pins: VecDeque<u32>,
    mut kept: Vec<u32>,
    then: Pins,
) -> Outcome {
    let Some(pin) = pins.pop_front() else {
        return then(env, kept);
    };
    direction(
        env,
        pin,
        Box::new(move |env, hr, dir| {
            if hr == S_OK && dir == OUTPUT {
                kept.push(pin);
                return keep_outputs(env, pins, kept, then);
            }
            guest_call(env, pin, 2, vec![], move |env, _| {
                keep_outputs(env, pins, kept, then)
            })
        }),
    )
}

/// The filter owning `pin`, with a reference, or 0.
fn owner(env: &mut Env<'_>, pin: u32, then: Pointer) -> Outcome {
    let Some(scratch) = alloc(env, 264) else {
        return then(env, 0);
    };
    let _ = env.cpu.write(scratch, &[0; 264]);
    guest_call(env, pin, 8, vec![scratch], move |env, hr| {
        let filter = env.cpu.read_u32(scratch).unwrap_or(0);
        free(env, scratch);
        then(env, if hr == S_OK { filter } else { 0 })
    })
}

/// The pin connected to `pin`, or 0; no reference is kept.
fn peer(env: &mut Env<'_>, pin: u32, then: Pointer) -> Outcome {
    let Some(scratch) = alloc(env, 4) else {
        return then(env, 0);
    };
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, pin, 6, vec![scratch], move |env, hr| {
        let other = env.cpu.read_u32(scratch).unwrap_or(0);
        free(env, scratch);
        if hr != S_OK || other == 0 {
            return then(env, 0);
        }
        guest_call(env, other, 2, vec![], move |env, _| then(env, other))
    })
}

/// IGraphBuilder::Render of a guest output pin.
pub(super) fn render(env: &mut Env<'_>, graph: u32, source: u32, existing: bool) -> Outcome {
    let plug = Plug {
        graph,
        sink: 0,
        existing,
    };
    autoplug(
        env,
        plug,
        source,
        0,
        Box::new(|_, hr| {
            Outcome::Return(if hr == VFW_E_CANNOT_CONNECT {
                VFW_E_CANNOT_RENDER
            } else {
                hr
            })
        }),
    )
}

fn autoplug(env: &mut Env<'_>, plug: Plug, source: u32, depth: u32, then: Hr) -> Outcome {
    if depth >= MAX_DEPTH {
        return then(env, VFW_E_CANNOT_CONNECT);
    }
    if plug.sink == 0 {
        return members(env, plug, source, depth, then);
    }
    let outcome = filter::connect(env, source, plug.sink, 0);
    hr_of(
        env,
        outcome,
        Box::new(move |env, hr| {
            // These failures say intermediate filters cannot help either.
            if !failed(hr) || hr == VFW_E_NOT_CONNECTED || hr == VFW_E_NO_AUDIO_HARDWARE {
                return then(env, hr);
            }
            members(env, plug, source, depth, then)
        }),
    )
}

fn members(env: &mut Env<'_>, plug: Plug, source: u32, depth: u32, then: Hr) -> Outcome {
    let list: Vec<u32> = g(env, plug.graph)
        .members
        .iter()
        .rev()
        .map(|m| m.filter)
        .collect();
    try_members(env, plug, source, depth, list.into(), then)
}

fn try_members(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    depth: u32,
    mut list: VecDeque<u32>,
    then: Hr,
) -> Outcome {
    let Some(filter) = list.pop_front() else {
        return stand_in(env, plug, source, depth, then);
    };
    through_filter(
        env,
        plug,
        source,
        filter,
        depth,
        Box::new(move |env, hr| {
            if !failed(hr) {
                return then(env, hr);
            }
            try_members(env, plug, source, depth, list, then)
        }),
    )
}

/// Rendering a guest pin tries host decoding before the mapper's filters.
fn stand_in(env: &mut Env<'_>, plug: Plug, source: u32, depth: u32, then: Hr) -> Outcome {
    if plug.sink != 0 || filter::host_pin(env, source).is_some() {
        return mapped(env, plug, source, depth, then);
    }
    let outcome = super::render::guest_source(env, plug.graph, source, plug.existing);
    hr_of(
        env,
        outcome,
        Box::new(move |env, hr| {
            if !failed(hr) {
                return then(env, hr);
            }
            mapped(env, plug, source, depth, then)
        }),
    )
}

/// Try every pin of `filter` as the input `source` connects to.
fn through_filter(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    filter: u32,
    depth: u32,
    then: Hr,
) -> Outcome {
    pins(
        env,
        filter,
        Box::new(move |env, pins| {
            let pins = Rc::new(pins);
            try_sinks(env, plug, source, filter, depth, pins, 0, then)
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn try_sinks(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    filter: u32,
    depth: u32,
    pins: Rc<Vec<u32>>,
    at: usize,
    then: Hr,
) -> Outcome {
    let Some(&pin) = pins.get(at) else {
        return release_all(
            env,
            pins.to_vec(),
            Box::new(move |env| then(env, VFW_E_CANNOT_CONNECT)),
        );
    };
    let next = pins.clone();
    through_sink(
        env,
        plug,
        source,
        filter,
        pin,
        depth,
        Box::new(move |env, hr| {
            if failed(hr) {
                return try_sinks(env, plug, source, filter, depth, next, at + 1, then);
            }
            release_all(env, next.to_vec(), Box::new(move |env| then(env, hr)))
        }),
    )
}

/// Connect `source` to `middle`, then plug onward from the filter's free
/// outputs. With a sink the first success wins; other free outputs make it a
/// partial render. Without one every output is rendered.
fn through_sink(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    filter: u32,
    middle: u32,
    depth: u32,
    then: Hr,
) -> Outcome {
    direction(
        env,
        middle,
        Box::new(move |env, hr, dir| {
            if hr != S_OK || dir != INPUT {
                return then(env, E_FAIL);
            }
            connected(
                env,
                middle,
                Box::new(move |env, busy| {
                    if busy {
                        return then(env, E_FAIL);
                    }
                    let outcome = filter::connect(env, source, middle, 0);
                    hr_of(
                        env,
                        outcome,
                        Box::new(move |env, hr| {
                            if failed(hr) {
                                return then(env, E_FAIL);
                            }
                            onward(env, plug, source, filter, middle, depth, then)
                        }),
                    )
                }),
            )
        }),
    )
}

fn onward(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    filter: u32,
    middle: u32,
    depth: u32,
    then: Hr,
) -> Outcome {
    pins(
        env,
        filter,
        Box::new(move |env, pins| {
            let held = pins.clone();
            outputs(
                env,
                pins.into(),
                Vec::new(),
                Box::new(move |env, outputs| {
                    let count = outputs.len();
                    let finish: Hr = Box::new(move |env, hr| {
                        release_all(env, held, Box::new(move |env| then(env, hr)))
                    });
                    let state = Onward {
                        plug,
                        source,
                        middle,
                        depth,
                        count,
                        any: false,
                        all: true,
                    };
                    plug_outputs(env, state, outputs.into(), finish)
                }),
            )
        }),
    )
}

#[derive(Clone, Copy)]
struct Onward {
    plug: Plug,
    source: u32,
    middle: u32,
    depth: u32,
    count: usize,
    any: bool,
    all: bool,
}

fn plug_outputs(env: &mut Env<'_>, state: Onward, mut outputs: VecDeque<u32>, then: Hr) -> Outcome {
    let Some(output) = outputs.pop_front() else {
        if state.plug.sink == 0 && state.all {
            return then(env, S_OK);
        }
        if state.plug.sink == 0 && state.any {
            return then(env, VFW_S_PARTIAL_RENDER);
        }
        return undo(env, state.source, state.middle, then);
    };
    autoplug(
        env,
        state.plug,
        output,
        state.depth + 1,
        Box::new(move |env, hr| {
            if !failed(hr) && state.plug.sink != 0 {
                let partial = hr == S_OK && state.count > 1;
                return then(env, if partial { VFW_S_PARTIAL_RENDER } else { hr });
            }
            let state = Onward {
                any: state.any || !failed(hr),
                all: state.all && hr == S_OK,
                ..state
            };
            plug_outputs(env, state, outputs, then)
        }),
    )
}

fn undo(env: &mut Env<'_>, source: u32, middle: u32, then: Hr) -> Outcome {
    let outcome = filter::disconnect(env, source);
    hr_of(
        env,
        outcome,
        Box::new(move |env, _| {
            let outcome = filter::disconnect(env, middle);
            hr_of(env, outcome, Box::new(move |env, _| then(env, E_FAIL)))
        }),
    )
}

/// The free output pins quartz plugs onward from: unconnected, and not
/// named with a leading '~'.
fn outputs(env: &mut Env<'_>, mut pins: VecDeque<u32>, mut found: Vec<u32>, then: Pins) -> Outcome {
    let Some(pin) = pins.pop_front() else {
        return then(env, found);
    };
    let Some(scratch) = alloc(env, 264) else {
        return outputs(env, pins, found, then);
    };
    let _ = env.cpu.write(scratch, &[0; 264]);
    guest_call(env, pin, 8, vec![scratch], move |env, hr| {
        let owner = env.cpu.read_u32(scratch).unwrap_or(0);
        let dir = env.cpu.read_u32(scratch + 4).unwrap_or(INPUT);
        let first = env.cpu.read_u16(scratch + 8).unwrap_or(0);
        free(env, scratch);
        let candidate = hr == S_OK && dir == OUTPUT && first != u16::from(b'~');
        let check: Finish = Box::new(move |env| {
            if !candidate {
                return outputs(env, pins, found, then);
            }
            connected(
                env,
                pin,
                Box::new(move |env, busy| {
                    if !busy {
                        found.push(pin);
                    }
                    outputs(env, pins, found, then)
                }),
            )
        });
        if owner == 0 {
            return check(env);
        }
        guest_call(env, owner, 2, vec![], move |env, _| check(env))
    })
}

/// The mapper's registered filters for the source's media types, by merit.
fn mapped(env: &mut Env<'_>, plug: Plug, source: u32, depth: u32, then: Hr) -> Outcome {
    mapper(
        env,
        plug.graph,
        Box::new(move |env, mapper| {
            if mapper == 0 {
                return then(env, VFW_E_CANNOT_CONNECT);
            }
            let finish: Hr = Box::new(move |env, hr| {
                guest_call(env, mapper, 2, vec![], move |env, _| then(env, hr))
            });
            media_types(
                env,
                source,
                Box::new(move |env, hr, types| {
                    if failed(hr) {
                        return finish(env, hr);
                    }
                    enumerate(env, plug, source, depth, mapper, types, finish)
                }),
            )
        }),
    )
}

fn enumerate(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    depth: u32,
    mapper: u32,
    types: Vec<(Iid, Iid)>,
    then: Hr,
) -> Outcome {
    let size = types.len() as u32 * 32 + 4;
    let Some(block) = alloc(env, size) else {
        return then(env, E_OUTOFMEMORY);
    };
    for (index, (major, sub)) in types.iter().enumerate() {
        let at = block + index as u32 * 32;
        let _ = write_guid(env, at, major);
        let _ = write_guid(env, at + 16, sub);
    }
    let output = block + size - 4;
    let _ = env.cpu.write_u32(output, 0);
    let count = types.len() as u32;
    let existing = u32::from(plug.existing);
    let args = vec![
        output,
        0,
        0,
        MERIT_UNLIKELY,
        1,
        count,
        block,
        0,
        0,
        0,
        existing,
        0,
        0,
        0,
        0,
    ];
    guest_call(env, mapper, 6, args, move |env, hr| {
        let monikers = env.cpu.read_u32(output).unwrap_or(0);
        free(env, block);
        if failed(hr) || monikers == 0 {
            return then(env, if failed(hr) { hr } else { VFW_E_CANNOT_CONNECT });
        }
        candidates(
            env,
            plug,
            source,
            depth,
            monikers,
            Box::new(move |env, hr| {
                guest_call(env, monikers, 2, vec![], move |env, _| then(env, hr))
            }),
        )
    })
}

/// Scratch for one moniker: the moniker, its property bag, a VARIANT, the
/// created filter, then IID_IPropertyBag, IID_IBaseFilter and "FriendlyName".
const MONIKER: u32 = 0;
const BAG: u32 = 4;
const VARIANT: u32 = 8;
const CREATED: u32 = 24;
const BAG_IID: u32 = 28;
const FILTER_IID: u32 = 44;
const FRIENDLY_NAME: u32 = 60;
const MONIKER_SCRATCH: u32 = 88;

fn candidates(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    depth: u32,
    monikers: u32,
    then: Hr,
) -> Outcome {
    let Some(scratch) = alloc(env, MONIKER_SCRATCH) else {
        return then(env, E_OUTOFMEMORY);
    };
    let _ = env.cpu.write(scratch, &[0; MONIKER_SCRATCH as usize]);
    let _ = write_guid(env, scratch + BAG_IID, &IID_IPROPERTYBAG);
    let _ = write_guid(env, scratch + FILTER_IID, &IID_IBASEFILTER);
    let name: Vec<u8> = "FriendlyName\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let _ = env.cpu.write(scratch + FRIENDLY_NAME, &name);
    next_candidate(
        env,
        plug,
        source,
        depth,
        monikers,
        scratch,
        Box::new(move |env, hr| {
            free(env, scratch);
            then(env, hr)
        }),
    )
}

fn next_candidate(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    depth: u32,
    monikers: u32,
    scratch: u32,
    then: Hr,
) -> Outcome {
    let _ = env.cpu.write_u32(scratch + MONIKER, 0);
    guest_call(
        env,
        monikers,
        3,
        vec![1, scratch + MONIKER, 0],
        move |env, hr| {
            let moniker = env.cpu.read_u32(scratch + MONIKER).unwrap_or(0);
            if hr != S_OK || moniker == 0 {
                return then(env, VFW_E_CANNOT_CONNECT);
            }
            let step: Step = Box::new(move |env, result| match result {
                Some(hr) => then(env, hr),
                None => next_candidate(env, plug, source, depth, monikers, scratch, then),
            });
            let _ = env.cpu.write_u32(scratch + BAG, 0);
            let bind = vec![0, 0, scratch + BAG_IID, scratch + BAG];
            guest_call(env, moniker, 9, bind, move |env, hr| {
                let bag = env.cpu.read_u32(scratch + BAG).unwrap_or(0);
                if hr != S_OK || bag == 0 {
                    return guest_call(env, moniker, 2, vec![], move |env, _| step(env, None));
                }
                let _ = env.cpu.write(scratch + VARIANT, &[0; 16]);
                let read = vec![scratch + FRIENDLY_NAME, scratch + VARIANT, 0];
                guest_call(env, bag, 3, read, move |env, named| {
                    guest_call(env, bag, 2, vec![], move |env, _| {
                        if failed(named) {
                            return guest_call(env, moniker, 2, vec![], move |env, _| {
                                step(env, None)
                            });
                        }
                        create(env, plug, source, depth, moniker, scratch, step)
                    })
                })
            })
        },
    )
}

/// Create, name and add one candidate, then plug through it; a candidate
/// that does not lead on is removed again.
fn create(
    env: &mut Env<'_>,
    plug: Plug,
    source: u32,
    depth: u32,
    moniker: u32,
    scratch: u32,
    step: Step,
) -> Outcome {
    let _ = env.cpu.write_u32(scratch + CREATED, 0);
    let bind = vec![0, 0, scratch + FILTER_IID, scratch + CREATED];
    guest_call(env, moniker, 8, bind, move |env, hr| {
        guest_call(env, moniker, 2, vec![], move |env, _| {
            let filter = env.cpu.read_u32(scratch + CREATED).unwrap_or(0);
            if hr != S_OK || filter == 0 {
                return clear_variant(env, scratch + VARIANT, Box::new(move |env| step(env, None)));
            }
            let name = env.cpu.read_u32(scratch + VARIANT + 8).unwrap_or(0);
            super::add_filter_then(env, plug.graph, filter, name, move |env, added| {
                clear_variant(
                    env,
                    scratch + VARIANT,
                    Box::new(move |env| {
                        if failed(added) {
                            return guest_call(env, filter, 2, vec![], move |env, _| {
                                step(env, None)
                            });
                        }
                        through_filter(
                            env,
                            plug,
                            source,
                            filter,
                            depth,
                            Box::new(move |env, hr| {
                                if !failed(hr) {
                                    // The graph keeps the filter; drop the creation reference.
                                    return guest_call(env, filter, 2, vec![], move |env, _| {
                                        step(env, Some(hr))
                                    });
                                }
                                let outcome = super::remove_filter(env, plug.graph, filter);
                                hr_of(
                                    env,
                                    outcome,
                                    Box::new(move |env, _| {
                                        guest_call(env, filter, 2, vec![], move |env, _| {
                                            step(env, None)
                                        })
                                    }),
                                )
                            }),
                        )
                    }),
                )
            })
        })
    })
}

fn clear_variant(env: &mut Env<'_>, variant: u32, then: Finish) -> Outcome {
    match export(env, "oleaut32.dll", "VariantClear") {
        Some(clear) => Outcome::call(clear, vec![variant], move |env, _| then(env)),
        None => then(env),
    }
}

/// The graph's IFilterMapper2, created on first use.
fn mapper(env: &mut Env<'_>, graph: u32, then: Pointer) -> Outcome {
    let Some(scratch) = alloc(env, 20) else {
        return then(env, 0);
    };
    let _ = write_guid(env, scratch, &mapper::IID_IFILTERMAPPER2);
    let _ = env.cpu.write_u32(scratch + 16, 0);
    let outcome = mapper::query(env, graph, scratch, scratch + 16);
    hr_of(
        env,
        outcome,
        Box::new(move |env, hr| {
            let mapper = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            free(env, scratch);
            then(env, if hr == S_OK { mapper } else { 0 })
        }),
    )
}

/// The (major, sub) pairs a pin enumerates; failures return their HRESULT.
fn media_types(env: &mut Env<'_>, pin: u32, then: Types) -> Outcome {
    let Some(scratch) = alloc(env, 8) else {
        return then(env, E_OUTOFMEMORY, Vec::new());
    };
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, pin, 12, vec![scratch], move |env, hr| {
        let types = env.cpu.read_u32(scratch).unwrap_or(0);
        if failed(hr) || types == 0 {
            free(env, scratch);
            return then(env, if failed(hr) { hr } else { E_POINTER }, Vec::new());
        }
        next_type(
            env,
            types,
            scratch,
            Vec::new(),
            Box::new(move |env, hr, found| {
                free(env, scratch);
                guest_call(env, types, 2, vec![], move |env, _| then(env, hr, found))
            }),
        )
    })
}

fn next_type(
    env: &mut Env<'_>,
    types: u32,
    scratch: u32,
    mut found: Vec<(Iid, Iid)>,
    then: Types,
) -> Outcome {
    let _ = env.cpu.write_u32(scratch + 4, 0);
    guest_call(env, types, 3, vec![1, scratch + 4, 0], move |env, hr| {
        let mt = env.cpu.read_u32(scratch + 4).unwrap_or(0);
        if failed(hr) {
            return then(env, hr, found);
        }
        if hr != S_OK || mt == 0 {
            return then(env, S_OK, found);
        }
        let (Ok(major), Ok(sub)) = (read_guid(env, mt), read_guid(env, mt + 16)) else {
            return then(env, E_POINTER, found);
        };
        found.push((major, sub));
        free_media_type(
            env,
            mt,
            Box::new(move |env| next_type(env, types, scratch, found, then)),
        )
    })
}

/// DeleteMediaType: the format block, its IUnknown, then the structure.
fn free_media_type(env: &mut Env<'_>, mt: u32, then: Finish) -> Outcome {
    let unknown = env.cpu.read_u32(mt + 60).unwrap_or(0);
    let format = env.cpu.read_u32(mt + 68).unwrap_or(0);
    let Some(task_free) = export(env, "combase.dll", "CoTaskMemFree") else {
        return then(env);
    };
    let release: Finish = Box::new(move |env| {
        let finish: Finish =
            Box::new(move |_| Outcome::call(task_free, vec![mt], move |env, _| then(env)));
        if unknown == 0 {
            return finish(env);
        }
        guest_call(env, unknown, 2, vec![], move |env, _| finish(env))
    });
    if format == 0 {
        return release(env);
    }
    Outcome::call(task_free, vec![format], move |env, _| release(env))
}

/// Every pin of `filter`, each holding a reference.
pub(super) fn pins(env: &mut Env<'_>, filter: u32, then: Pins) -> Outcome {
    let Some(scratch) = alloc(env, 8) else {
        return then(env, Vec::new());
    };
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, filter, 10, vec![scratch], move |env, hr| {
        let pins = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || pins == 0 {
            free(env, scratch);
            return then(env, Vec::new());
        }
        next_pin(
            env,
            pins,
            scratch,
            Vec::new(),
            Box::new(move |env, found| {
                free(env, scratch);
                guest_call(env, pins, 2, vec![], move |env, _| then(env, found))
            }),
        )
    })
}

fn next_pin(
    env: &mut Env<'_>,
    pins: u32,
    scratch: u32,
    mut found: Vec<u32>,
    then: Pins,
) -> Outcome {
    let _ = env.cpu.write_u32(scratch + 4, 0);
    guest_call(env, pins, 3, vec![1, scratch + 4, 0], move |env, hr| {
        let pin = env.cpu.read_u32(scratch + 4).unwrap_or(0);
        if hr != S_OK || pin == 0 {
            return then(env, found);
        }
        found.push(pin);
        next_pin(env, pins, scratch, found, then)
    })
}

fn release_all(env: &mut Env<'_>, pins: Vec<u32>, then: Finish) -> Outcome {
    let calls = pins.into_iter().map(|pin| Call(pin, 2, vec![])).collect();
    chain(env, calls, Vec::new(), Box::new(move |env, _| then(env)))
}

fn direction(env: &mut Env<'_>, pin: u32, then: Direction) -> Outcome {
    let Some(scratch) = alloc(env, 4) else {
        return then(env, E_OUTOFMEMORY, INPUT);
    };
    let _ = env.cpu.write_u32(scratch, INPUT);
    guest_call(env, pin, 9, vec![scratch], move |env, hr| {
        let dir = env.cpu.read_u32(scratch).unwrap_or(INPUT);
        free(env, scratch);
        then(env, hr, dir)
    })
}

fn connected(env: &mut Env<'_>, pin: u32, then: Flag) -> Outcome {
    let Some(scratch) = alloc(env, 4) else {
        return then(env, false);
    };
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, pin, 6, vec![scratch], move |env, hr| {
        let peer = env.cpu.read_u32(scratch).unwrap_or(0);
        free(env, scratch);
        if hr != S_OK || peer == 0 {
            return then(env, false);
        }
        guest_call(env, peer, 2, vec![], move |env, _| then(env, true))
    })
}
