//! Guest IMediaObject activation, negotiation and streaming.
use super::*;

type Types = Box<dyn FnOnce(&mut Env<'_>, Vec<MediaType>) -> Outcome>;

const IID_IMEDIAOBJECT: Iid = guid(
    0xd8ad_0f58,
    0x5494,
    0x4102,
    [0x97, 0xc5, 0xec, 0x79, 0x8e, 0x59, 0xbc, 0xf4],
);
pub(crate) const IID_IMEDIABUFFER: Iid = guid(
    0x59ef_a8b9,
    0x938c,
    0x4a26,
    [0x82, 0xf2, 0x95, 0xcb, 0x84, 0xcd, 0xc8, 0x37],
);

pub(super) fn init(env: &mut Env<'_>, filter: u32, clsid: Iid) -> Outcome {
    let create = env
        .ext
        .try_get::<Host>()
        .and_then(|h| (h.export)(env, "combase.dll", "CoCreateInstance"));
    let Some(create) = create else {
        return Outcome::Return(REGDB_E_CLASSNOTREG);
    };
    let Some(scratch) = alloc(env, 36) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let _ = write_guid(env, scratch, &clsid);
    let _ = write_guid(env, scratch + 16, &adv_os::com::IUNKNOWN);
    Outcome::call(
        create,
        vec![scratch, filter, 1, scratch + 16, scratch + 32],
        move |env, result| {
            let object = env.cpu.read_u32(scratch + 32).unwrap_or(0);
            free(env, scratch);
            if result.value() != S_OK {
                return Outcome::Return(result.value());
            }
            let dmo = Dmo::new(env, object);
            state(env).filters.get_mut(&filter).unwrap().dmo = Some(dmo);
            with(
                env,
                filter,
                Box::new(move |env, media| {
                    let Some(counts) = alloc(env, 8) else {
                        return Outcome::Return(E_OUTOFMEMORY);
                    };
                    guest_call(env, media, 3, vec![counts, counts + 4], move |env, hr| {
                        let inputs = env.cpu.read_u32(counts).unwrap_or(0);
                        let outputs = env.cpu.read_u32(counts + 4).unwrap_or(0);
                        free(env, counts);
                        if hr & 0x8000_0000 != 0 {
                            return Outcome::Return(hr);
                        }
                        for (dir, count, prefix) in
                            [(INPUT, inputs, "in"), (OUTPUT, outputs, "out")]
                        {
                            for index in 0..count {
                                if let Err(hr) = filter::new_pin(
                                    env,
                                    filter,
                                    dir,
                                    &format!("{prefix}{index}"),
                                    GUID_NULL,
                                    None,
                                ) {
                                    return Outcome::Return(hr);
                                }
                            }
                        }
                        Outcome::Return(S_OK)
                    })
                }),
            )
        },
    )
}

pub(super) fn with(env: &mut Env<'_>, filter: u32, then: Done) -> Outcome {
    let object = dmo(env, filter).unwrap().object;
    let Some(scratch) = alloc(env, 20) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let _ = write_guid(env, scratch, &IID_IMEDIAOBJECT);
    guest_call(
        env,
        object,
        0,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let media = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            free(env, scratch);
            if hr != S_OK {
                return Outcome::Return(hr);
            }
            let outcome = then(env, media);
            after(
                env,
                outcome,
                Box::new(move |env, results| {
                    let hr = results[0];
                    guest_call(env, media, 2, vec![], move |_, _| Outcome::Return(hr))
                }),
            )
        },
    )
}

fn index(env: &Env<'_>, pin: u32) -> u32 {
    let s = env.ext.get::<State>();
    let p = &s.pins[&pin];
    s.filters[&p.filter]
        .pins
        .iter()
        .filter(|&&other| s.pins[&other].dir == p.dir)
        .position(|&other| other == pin)
        .unwrap() as u32
}

pub(super) fn set_type(env: &mut Env<'_>, pin: u32, mt: u32, flags: u32) -> Outcome {
    let (filter, dir) = {
        let p = &state(env).pins[&pin];
        (p.filter, p.dir)
    };
    let index = index(env, pin);
    with(
        env,
        filter,
        Box::new(move |env, media| {
            guest_call(
                env,
                media,
                if dir == INPUT { 8 } else { 9 },
                vec![index, mt, flags],
                |_, hr| Outcome::Return(hr),
            )
        }),
    )
}

pub(super) fn types(env: &mut Env<'_>, pin: u32, then: Types) -> Outcome {
    let filter = filter_of(env, pin);
    with(
        env,
        filter,
        Box::new(move |env, media| {
            let Some(scratch) = alloc(env, MEDIA_TYPE_SIZE) else {
                return Outcome::Return(E_OUTOFMEMORY);
            };
            next_type(env, media, pin, scratch, 0, Vec::new(), then)
        }),
    )
}

fn next_type(
    env: &mut Env<'_>,
    media: u32,
    pin: u32,
    scratch: u32,
    at: u32,
    mut types: Vec<MediaType>,
    then: Types,
) -> Outcome {
    let slot = if state(env).pins[&pin].dir == INPUT {
        6
    } else {
        7
    };
    guest_call(
        env,
        media,
        slot,
        vec![index(env, pin), at, scratch],
        move |env, hr| {
            if hr != S_OK {
                free(env, scratch);
                return then(env, types);
            }
            if let Ok(mt) = MediaType::read(env, scratch) {
                types.push(mt);
            }
            filter::free_type_block(env, scratch);
            next_type(env, media, pin, scratch, at + 1, types, then)
        },
    )
}

pub(super) fn receive_connection(env: &mut Env<'_>, pin: u32, peer: u32, mt: u32) -> Outcome {
    let s = env.ext.get::<State>();
    let p = &s.pins[&pin];
    if s.filters[&p.filter].state != graph::STOPPED {
        return Outcome::Return(0x8004_0224);
    }
    if p.peer != 0 {
        return Outcome::Return(VFW_E_ALREADY_CONNECTED);
    }
    let outcome = set_type(env, pin, mt, 1);
    after(
        env,
        outcome,
        Box::new(move |env, results| {
            if results[0] != S_OK {
                return Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED);
            }
            let outcome = set_type(env, pin, mt, 0);
            after(
                env,
                outcome,
                Box::new(move |env, results| {
                    if results[0] != S_OK {
                        return Outcome::Return(results[0]);
                    }
                    let value = MediaType::read(env, mt).unwrap();
                    let p = state(env).pins.get_mut(&pin).unwrap();
                    p.peer = peer;
                    p.mt = Some(value);
                    guest_call(env, peer, 1, vec![], |_, _| Outcome::Return(S_OK))
                }),
            )
        }),
    )
}

pub(super) fn transition(env: &mut Env<'_>, filter: u32, value: u32, then: Done) -> Outcome {
    let previous = state(env).filters[&filter].state;
    if value == graph::STOPPED && dmo(env, filter).unwrap().receiving {
        let wake = dmo(env, filter).unwrap().wake;
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline: None,
            },
            move |env, _| transition(env, filter, value, then),
        );
    }
    with(
        env,
        filter,
        Box::new(move |env, media| {
            let mut calls = VecDeque::new();
            if value == graph::STOPPED {
                for pin in output_pins(env, filter) {
                    let allocator = state(env).pins[&pin].allocator;
                    if allocator != 0 {
                        calls.push_back(Call(allocator, 6, vec![]));
                    }
                }
                calls.push_back(Call(media, 16, vec![]));
                calls.push_back(Call(media, 19, vec![]));
            } else if previous == graph::STOPPED {
                return guest_call(env, media, 18, vec![], move |env, hr| {
                    if hr & 0x8000_0000 != 0 {
                        return then(env, hr);
                    }
                    let calls = output_pins(env, filter)
                        .iter()
                        .filter_map(|pin| {
                            let allocator = state(env).pins[pin].allocator;
                            (allocator != 0).then_some(Call(allocator, 5, vec![]))
                        })
                        .collect();
                    chain(
                        env,
                        calls,
                        Vec::new(),
                        Box::new(move |env, _| {
                            state(env).filters.get_mut(&filter).unwrap().state = value;
                            dmo(env, filter).unwrap().eos = false;
                            then(env, S_OK)
                        }),
                    )
                });
            }
            chain(
                env,
                calls,
                Vec::new(),
                Box::new(move |env, _| {
                    state(env).filters.get_mut(&filter).unwrap().state = value;
                    if value != graph::STOPPED {
                        dmo(env, filter).unwrap().eos = false;
                    }
                    then(env, S_OK)
                }),
            )
        }),
    )
}

fn output_pins(env: &Env<'_>, filter: u32) -> Vec<u32> {
    let s = env.ext.get::<State>();
    s.filters[&filter]
        .pins
        .iter()
        .copied()
        .filter(|p| s.pins[p].dir == OUTPUT)
        .collect()
}

pub(super) fn connect(env: &mut Env<'_>, pin: u32, peer: u32, mt: u32) -> Outcome {
    let s = env.ext.get::<State>();
    let p = &s.pins[&pin];
    if s.filters[&p.filter].state != graph::STOPPED {
        return Outcome::Return(0x8004_0224);
    }
    if p.dir != OUTPUT {
        return Outcome::Return(0x8000_ffff);
    }
    if p.peer != 0 {
        return Outcome::Return(VFW_E_ALREADY_CONNECTED);
    }
    let wanted = (mt != 0).then(|| MediaType::read(env, mt).unwrap());
    if let Some(wanted) = &wanted
        && wanted.major != GUID_NULL
        && wanted.format_type != GUID_NULL
    {
        return try_connect(env, pin, peer, vec![wanted.clone()].into(), true);
    }
    let Some(scratch) = alloc(env, 8) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    guest_call(env, peer, 12, vec![scratch], move |env, hr| {
        let enumerator = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK {
            free(env, scratch);
            return own_candidates(env, pin, peer, wanted, Vec::new());
        }
        peer_types(env, pin, peer, wanted, enumerator, scratch, Vec::new())
    })
}

fn peer_types(
    env: &mut Env<'_>,
    pin: u32,
    peer: u32,
    wanted: Option<MediaType>,
    enumerator: u32,
    scratch: u32,
    mut types: Vec<MediaType>,
) -> Outcome {
    guest_call(
        env,
        enumerator,
        3,
        vec![1, scratch + 4, 0],
        move |env, hr| {
            if hr != S_OK {
                free(env, scratch);
                return guest_call(env, enumerator, 2, vec![], move |env, _| {
                    own_candidates(env, pin, peer, wanted, types)
                });
            }
            let pointer = env.cpu.read_u32(scratch + 4).unwrap();
            types.push(MediaType::read(env, pointer).unwrap());
            filter::free_type_block(env, pointer);
            adv_os::com::task_free(env, pointer);
            peer_types(env, pin, peer, wanted, enumerator, scratch, types)
        },
    )
}

fn own_candidates(
    env: &mut Env<'_>,
    pin: u32,
    peer: u32,
    wanted: Option<MediaType>,
    mut candidates: Vec<MediaType>,
) -> Outcome {
    types(
        env,
        pin,
        Box::new(move |env, own| {
            candidates.extend(own);
            if let Some(wanted) = wanted {
                candidates.retain(|mt| {
                    (wanted.major == GUID_NULL || wanted.major == mt.major)
                        && (wanted.sub == GUID_NULL || wanted.sub == mt.sub)
                        && (wanted.format_type == GUID_NULL || wanted.format_type == mt.format_type)
                });
            }
            try_connect(env, pin, peer, candidates.into(), false)
        }),
    )
}

fn try_connect(
    env: &mut Env<'_>,
    pin: u32,
    peer: u32,
    mut candidates: VecDeque<MediaType>,
    exact: bool,
) -> Outcome {
    let Some(mt) = candidates.pop_front() else {
        return Outcome::Return(if exact {
            VFW_E_TYPE_NOT_ACCEPTED
        } else {
            VFW_E_NO_ACCEPTABLE_TYPES
        });
    };
    let Some(scratch) = alloc(env, MEDIA_TYPE_SIZE + 48) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    if let Err(hr) = mt.write(env, scratch) {
        free(env, scratch);
        return Outcome::Return(hr);
    }
    let outcome = set_type(env, pin, scratch, 1);
    after(
        env,
        outcome,
        Box::new(move |env, result| {
            if result[0] != S_OK {
                filter::free_type_block(env, scratch);
                free(env, scratch);
                return try_connect(env, pin, peer, candidates, exact);
            }
            guest_call(env, peer, 4, vec![pin, scratch], move |env, hr| {
                if hr != S_OK {
                    filter::free_type_block(env, scratch);
                    free(env, scratch);
                    return try_connect(env, pin, peer, candidates, exact);
                }
                let outcome = set_type(env, pin, scratch, 0);
                after(
                    env,
                    outcome,
                    Box::new(move |env, result| {
                        filter::free_type_block(env, scratch);
                        if result[0] != S_OK {
                            free(env, scratch);
                            return Outcome::Return(result[0]);
                        }
                        let _ = write_guid(env, scratch, &IID_IMEMINPUTPIN);
                        guest_call(env, peer, 0, vec![scratch, scratch + 16], move |env, hr| {
                            if hr != S_OK {
                                free(env, scratch);
                                return Outcome::Return(hr);
                            }
                            let mem = env.cpu.read_u32(scratch + 16).unwrap();
                            let p = state(env).pins.get_mut(&pin).unwrap();
                            p.peer = peer;
                            p.mem = mem;
                            p.mt = Some(mt);
                            guest_call(env, peer, 1, vec![], move |env, _| {
                                guest_call(env, mem, 3, vec![scratch + 20], move |env, hr| {
                                    let allocator = if hr == S_OK {
                                        env.cpu.read_u32(scratch + 20).unwrap()
                                    } else {
                                        0
                                    };
                                    let allocator = if allocator != 0 {
                                        allocator
                                    } else {
                                        match sample::create_allocator(env, 0, 0) {
                                            Ok(a) => a,
                                            Err(hr) => {
                                                free(env, scratch);
                                                return Outcome::Return(hr);
                                            }
                                        }
                                    };
                                    state(env).pins.get_mut(&pin).unwrap().allocator = allocator;
                                    let _ = env.cpu.write(scratch + 24, &[0; 16]);
                                    guest_call(env, mem, 5, vec![scratch + 24], move |env, _| {
                                        let filter = filter_of(env, pin);
                                        with(
                                            env,
                                            filter,
                                            Box::new(move |env, media| {
                                                guest_call(
                                                    env,
                                                    media,
                                                    13,
                                                    vec![
                                                        index(env, pin),
                                                        scratch + 40,
                                                        scratch + 44,
                                                    ],
                                                    move |env, hr| {
                                                        if hr & 0x8000_0000 != 0 {
                                                            free(env, scratch);
                                                            return filter::abort_guest_connection(
                                                                env, pin, peer, hr,
                                                            );
                                                        }
                                                        for (at, floor) in [
                                                            (24, 1),
                                                            (
                                                                28,
                                                                16384u32.max(
                                                                    env.cpu
                                                                        .read_u32(scratch + 40)
                                                                        .unwrap(),
                                                                ),
                                                            ),
                                                            (
                                                                32,
                                                                env.cpu
                                                                    .read_u32(scratch + 44)
                                                                    .unwrap(),
                                                            ),
                                                        ] {
                                                            let value = env
                                                                .cpu
                                                                .read_u32(scratch + at)
                                                                .unwrap()
                                                                .max(floor);
                                                            let _ = env
                                                                .cpu
                                                                .write_u32(scratch + at, value);
                                                        }
                                                        guest_call(
                                                            env,
                                                            allocator,
                                                            3,
                                                            vec![scratch + 24, scratch],
                                                            move |env, hr| {
                                                                free(env, scratch);
                                                                if hr & 0x8000_0000 != 0 {
                                                                    return filter::abort_guest_connection(env, pin, peer, hr);
                                                                }
                                                                guest_call(
                                                                    env,
                                                                    allocator,
                                                                    6,
                                                                    vec![],
                                                                    move |env, _| {
                                                                        guest_call(
                                                                            env,
                                                                            mem,
                                                                            4,
                                                                            vec![allocator, 0],
                                                                            |_, hr| {
                                                                                Outcome::Return(hr)
                                                                            },
                                                                        )
                                                                    },
                                                                )
                                                            },
                                                        )
                                                    },
                                                )
                                            }),
                                        )
                                    })
                                })
                            })
                        })
                    }),
                )
            })
        }),
    )
}

pub(super) fn input_method(env: &mut Env<'_>, filter: u32, method: &str) -> Option<Outcome> {
    let pointer = env.arg(1);
    match method {
        "NotifyAllocator" => {
            if pointer == 0 {
                return Some(Outcome::Return(E_POINTER));
            }
            Some(guest_call(env, pointer, 1, vec![], move |env, _| {
                let old =
                    std::mem::replace(&mut dmo(env, filter).unwrap().input_allocator, pointer);
                if old == 0 {
                    return Outcome::Return(S_OK);
                }
                guest_call(env, old, 2, vec![], |_, _| Outcome::Return(S_OK))
            }))
        }
        "GetAllocator" => {
            if pointer == 0 {
                return Some(Outcome::Return(E_POINTER));
            }
            let existing = dmo(env, filter).unwrap().input_allocator;
            let allocator = if existing != 0 {
                existing
            } else {
                match sample::create_allocator(env, 0, 0) {
                    Ok(allocator) => {
                        dmo(env, filter).unwrap().input_allocator = allocator;
                        allocator
                    }
                    Err(hr) => return Some(Outcome::Return(hr)),
                }
            };
            Some(guest_call(env, allocator, 1, vec![], move |env, _| {
                let _ = env.cpu.write_u32(pointer, allocator);
                Outcome::Return(S_OK)
            }))
        }
        "GetAllocatorRequirements" => Some(with(
            env,
            filter,
            Box::new(move |env, media| {
                let Some(scratch) = alloc(env, 12) else {
                    return Outcome::Return(E_OUTOFMEMORY);
                };
                guest_call(
                    env,
                    media,
                    12,
                    vec![0, scratch, scratch + 4, scratch + 8],
                    move |env, hr| {
                        if hr == S_OK {
                            let _ = env.cpu.write_u32(pointer, 1);
                            let _ = env
                                .cpu
                                .write_u32(pointer + 4, env.cpu.read_u32(scratch).unwrap());
                            let _ = env
                                .cpu
                                .write_u32(pointer + 8, env.cpu.read_u32(scratch + 8).unwrap());
                        }
                        free(env, scratch);
                        Outcome::Return(hr)
                    },
                )
            }),
        )),
        _ => None,
    }
}

pub(super) fn receive(env: &mut Env<'_>, filter: u32, sample: u32, done: Done) -> Outcome {
    if state(env).filters[&filter].state == graph::STOPPED {
        return done(env, VFW_E_WRONG_STATE);
    }
    let d = dmo(env, filter).unwrap();
    if d.flushing {
        return done(env, S_FALSE);
    }
    if d.eos {
        return done(env, 0x8004_022c);
    }
    d.receiving = true;
    let wake = d.wake;
    env.sys.reset_event(wake);
    let outcome = with(
        env,
        filter,
        Box::new(move |env, media| {
            guest_call(env, sample, 15, vec![], move |env, hr| {
                let input = move |env: &mut Env<'_>, hr| {
                    if hr != S_OK {
                        return Outcome::Return(hr);
                    }
                    process_input(env, filter, media, sample)
                };
                if hr == S_OK {
                    guest_call(env, media, 17, vec![0], move |env, hr| {
                        if hr & 0x8000_0000 != 0 {
                            return Outcome::Return(hr);
                        }
                        process_output(env, filter, media, Box::new(input))
                    })
                } else {
                    input(env, S_OK)
                }
            })
        }),
    );
    after(
        env,
        outcome,
        Box::new(move |env, results| {
            dmo(env, filter).unwrap().receiving = false;
            env.sys.set_event(wake);
            done(env, results[0])
        }),
    )
}

fn process_input(env: &mut Env<'_>, filter: u32, media: u32, sample: u32) -> Outcome {
    let Some(scratch) = alloc(env, 16) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let calls = VecDeque::from([
        Call(sample, 11, vec![]),
        Call(sample, 7, vec![]),
        Call(sample, 5, vec![scratch, scratch + 8]),
    ]);
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            let (start, duration, time_flags) = if results[2] & 0x8000_0000 == 0 {
                let start = read_i64(env, scratch).unwrap();
                let duration = if results[2] == 0x0004_0270 {
                    1
                } else {
                    read_i64(env, scratch + 8).unwrap() - start
                };
                (start, duration, 6)
            } else {
                (0, 0, 0)
            };
            free(env, scratch);
            let flags = time_flags | u32::from(results[1] == S_OK);
            guest_call(env, sample, 1, vec![], move |env, _| {
                let buffer = match super::super::create(
                    env,
                    &[Face::MediaBuffer],
                    Obj::DmoBuffer {
                        sample,
                        length: results[0],
                    },
                ) {
                    Ok(faces) => faces[0],
                    Err(hr) => return Outcome::Return(hr),
                };
                guest_call(
                    env,
                    media,
                    21,
                    vec![
                        0,
                        buffer,
                        flags,
                        start as u32,
                        (start >> 32) as u32,
                        duration as u32,
                        (duration >> 32) as u32,
                    ],
                    move |env, hr| {
                        guest_call(env, buffer, 2, vec![], move |env, _| {
                            if hr & 0x8000_0000 != 0 {
                                return Outcome::Return(hr);
                            }
                            process_output(
                                env,
                                filter,
                                media,
                                Box::new(|_, hr| Outcome::Return(hr)),
                            )
                        })
                    },
                )
            })
        }),
    )
}

struct Output {
    filter: u32,
    media: u32,
    pins: Vec<u32>,
    samples: Vec<u32>,
    scratch: u32,
}

fn process_output(env: &mut Env<'_>, filter: u32, media: u32, done: Done) -> Outcome {
    let pins = output_pins(env, filter);
    let Some(scratch) = alloc(env, pins.len() as u32 * 24 + 12) else {
        return done(env, E_OUTOFMEMORY);
    };
    let _ = env.cpu.write(scratch, &vec![0; pins.len() * 24 + 12]);
    get_samples(
        env,
        Output {
            filter,
            media,
            pins,
            scratch,
            samples: Vec::new(),
        },
        done,
    )
}

fn get_samples(env: &mut Env<'_>, mut output: Output, done: Done) -> Outcome {
    let index = output.samples.len();
    let (scratch, media) = (output.scratch, output.media);
    if index == output.pins.len() {
        return guest_call(
            env,
            media,
            22,
            vec![
                1,
                output.pins.len() as u32,
                scratch,
                scratch + output.pins.len() as u32 * 24,
            ],
            move |env, hr| {
                if hr & 0x8000_0000 != 0 {
                    return release_samples(env, output, Box::new(move |env, _| done(env, S_OK)));
                }
                deliver_samples(env, output, 0, false, done)
            },
        );
    }
    let allocator = state(env).pins[&output.pins[index]].allocator;
    if allocator == 0 {
        output.samples.push(0);
        return get_samples(env, output, done);
    }
    let out = scratch + output.pins.len() as u32 * 24 + 4;
    guest_call(env, allocator, 7, vec![out, 0, 0, 0], move |env, hr| {
        if hr != S_OK {
            return release_samples(env, output, Box::new(move |env, _| done(env, hr)));
        }
        let sample = env.cpu.read_u32(out).unwrap();
        let buffer = match super::super::create(
            env,
            &[Face::MediaBuffer],
            Obj::DmoBuffer { sample, length: 0 },
        ) {
            Ok(faces) => faces[0],
            Err(hr) => return done(env, hr),
        };
        let _ = env.cpu.write_u32(scratch + index as u32 * 24, buffer);
        output.samples.push(buffer);
        guest_call(env, sample, 13, vec![out], move |env, hr| {
            if hr == S_OK {
                let mt = env.cpu.read_u32(out).unwrap();
                return guest_call(env, media, 9, vec![index as u32, mt, 0], move |env, hr| {
                    filter::free_type_block(env, mt);
                    adv_os::com::task_free(env, mt);
                    if hr != S_OK {
                        return release_samples(env, output, Box::new(move |env, _| done(env, hr)));
                    }
                    get_samples(env, output, done)
                });
            }
            get_samples(env, output, done)
        })
    })
}

fn release_samples(env: &mut Env<'_>, output: Output, done: Done) -> Outcome {
    free(env, output.scratch);
    let calls = output
        .samples
        .into_iter()
        .filter(|&b| b != 0)
        .map(|b| Call(b, 2, vec![]))
        .collect();
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, _| done(env, S_OK)),
    )
}

fn deliver_samples(
    env: &mut Env<'_>,
    output: Output,
    index: usize,
    more: bool,
    done: Done,
) -> Outcome {
    if index == output.samples.len() {
        let (filter, media) = (output.filter, output.media);
        return release_samples(
            env,
            output,
            Box::new(move |env, _| {
                if more {
                    process_output(env, filter, media, done)
                } else {
                    done(env, S_OK)
                }
            }),
        );
    }
    let buffer = output.samples[index];
    if buffer == 0 {
        return deliver_samples(env, output, index + 1, more, done);
    }
    let (sample, length) = match payload(env, buffer).unwrap() {
        Obj::DmoBuffer { sample, length } => (*sample, *length),
        _ => unreachable!(),
    };
    let block = output.scratch + index as u32 * 24;
    let flags = env.cpu.read_u32(block + 4).unwrap();
    let more = more || flags & 0x0100_0000 != 0;
    let mut calls = VecDeque::from([
        Call(sample, 12, vec![length]),
        Call(sample, 8, vec![u32::from(flags & 1 != 0)]),
        Call(sample, 10, vec![0]),
        Call(sample, 16, vec![0]),
        Call(sample, 18, vec![0, 0]),
    ]);
    if flags & 2 != 0 {
        let start = read_i64(env, block + 8).unwrap();
        let stop = start + read_i64(env, block + 16).unwrap();
        write_i64(env, block + 16, stop);
        calls.push_back(Call(
            sample,
            6,
            vec![block + 8, if flags & 4 != 0 { block + 16 } else { 0 }],
        ));
    } else {
        calls.push_back(Call(sample, 6, vec![0, 0]));
    }
    if length != 0 {
        calls.push_back(Call(
            state(env).pins[&output.pins[index]].mem,
            6,
            vec![sample],
        ));
    }
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            let hr = *results.last().unwrap();
            if hr != S_OK {
                return release_samples(env, output, Box::new(move |env, _| done(env, hr)));
            }
            deliver_samples(env, output, index + 1, more, done)
        }),
    )
}

pub(super) fn pin_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let filter = filter_of(env, pin);
    let args = match method {
        "NewSegment" => (1..=6).map(|i| env.arg(i)).collect(),
        "EndOfStream" | "BeginFlush" | "EndFlush" => Vec::new(),
        _ => return None,
    };
    if state(env).pins[&pin].dir == OUTPUT {
        return Some(Outcome::Return(E_UNEXPECTED));
    }
    let d = dmo(env, filter).unwrap();
    if method == "EndOfStream" {
        if d.flushing {
            return Some(Outcome::Return(S_FALSE));
        }
        if d.eos {
            return Some(Outcome::Return(S_OK));
        }
        d.eos = true;
    }
    if method == "BeginFlush" {
        d.flushing = true;
    }
    if method == "EndFlush" {
        d.flushing = false;
        d.eos = false;
    }
    let slot = match method {
        "EndOfStream" => 14,
        "BeginFlush" => 15,
        "EndFlush" => 16,
        _ => 17,
    };
    Some(with(
        env,
        filter,
        Box::new(move |env, media| {
            let forward = move |env: &mut Env<'_>, _: u32| {
                let mut calls = VecDeque::new();
                if slot == 14 || slot == 16 {
                    calls.push_back(Call(media, 16, vec![]));
                }
                for pin in output_pins(env, filter) {
                    let peer = state(env).pins[&pin].peer;
                    if peer != 0 {
                        calls.push_back(Call(peer, slot, args.clone()));
                    }
                }
                chain(
                    env,
                    calls,
                    Vec::new(),
                    Box::new(|_, _| Outcome::Return(S_OK)),
                )
            };
            if slot == 14 {
                guest_call(env, media, 17, vec![0], move |env, _| {
                    process_output(env, filter, media, Box::new(forward))
                })
            } else {
                forward(env, S_OK)
            }
        }),
    ))
}
