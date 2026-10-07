//! Build pull-source graphs with the common splitter/decoders and
//! existing guest video renderers. Audio uses the ordinary DirectSound renderer.
use super::*;
use crate::dshow::filter::{INPUT, OUTPUT, Role};

const VFW_E_CANNOT_RENDER: u32 = 0x8004_0218;
const VFW_S_PARTIAL_RENDER: u32 = 0x0004_0242;

fn member(env: &mut Env<'_>, graph: u32, role: Role, clsid: Iid, name: &str) -> Result<u32, u32> {
    let filter = filter::new_filter(env, role, clsid)?;
    let name: Vec<u16> = name.encode_utf16().collect();
    let f = state(env).filters.get_mut(&filter).unwrap();
    f.graph = graph;
    f.name = name.clone();
    // Transfer the newly created reference to the graph.
    g(env, graph).members.push(Member {
        filter,
        name,
        host: Some(filter),
    });
    Ok(filter)
}

fn endpoint(env: &Env<'_>, filter: u32, dir: u32) -> u32 {
    let state = env.ext.get::<State>();
    *state.filters[&filter]
        .pins
        .iter()
        .find(|p| state.pins[p].dir == dir)
        .unwrap()
}
fn connect(env: &mut Env<'_>, out: u32, input: u32) -> Result<(), u32> {
    match filter::connect(env, out, input, 0) {
        Outcome::Return(S_OK) => Ok(()),
        Outcome::Return(error) => Err(error),
        _ => unreachable!("internal host connections are synchronous"),
    }
}

pub(super) fn pin(env: &mut Env<'_>, graph: u32, pointer: u32) -> Outcome {
    if pointer == 0 {
        return Outcome::Return(E_POINTER);
    }
    let Some(pin) = filter::host_pin(env, pointer) else {
        return guest_pin(env, graph, pointer);
    };
    let p = &state(env).pins[&pin];
    if p.dir != OUTPUT {
        return Outcome::Return(VFW_E_INVALID_DIRECTION);
    }
    if p.peer != 0 {
        return Outcome::Return(VFW_E_ALREADY_CONNECTED);
    }
    let filter = p.filter;
    if state(env).filters[&filter].graph != graph {
        return Outcome::Return(E_INVALIDARG);
    }
    if state(env).filters[&filter].role == Role::FileSource {
        return file(env, graph, filter, pin);
    }
    match state(env).pins[&pin].major {
        MEDIATYPE_VIDEO => video(
            env,
            graph,
            pin,
            Box::new(|_, results| Outcome::Return(results[0])),
        ),
        MEDIATYPE_AUDIO => {
            Outcome::Return(audio(env, graph, pin).map_or_else(|error| error, |_| S_OK))
        }
        _ => Outcome::Return(VFW_E_CANNOT_RENDER),
    }
}

/// Only filters added by this render attempt are removed on failure. A file
/// source recovers its decoder so the caller can retry rendering its output.
struct Build {
    graph: u32,
    splitter: u32,
    added: Vec<u32>,
    file: Option<(u32, u32)>,
    guest: Option<u32>,
}

impl Build {
    fn new(env: &mut Env<'_>, graph: u32) -> Result<Self, u32> {
        let splitter = member(
            env,
            graph,
            Role::Splitter,
            CLSID_MPEG1_SPLITTER,
            "Media splitter",
        )?;
        Ok(Self {
            graph,
            splitter,
            added: vec![splitter],
            file: None,
            guest: None,
        })
    }

    fn finish(self, env: &mut Env<'_>, hr: u32) -> Outcome {
        if hr & 0x8000_0000 == 0 {
            return Outcome::Return(hr);
        }
        if let Some((source, output)) = self.file {
            let media = state(env)
                .filters
                .get_mut(&self.splitter)
                .unwrap()
                .source
                .take();
            if media.is_some() {
                state(env).filters.get_mut(&source).unwrap().source = media;
            }
            let _ = filter::disconnect_owned(env, output);
        }
        let mut calls = VecDeque::new();
        let mut disconnected_source = false;
        for &filter in self.added.iter().rev() {
            let pins = state(env).filters[&filter].pins.clone();
            for pin in pins {
                let peer = state(env).pins[&pin].peer;
                if peer != 0 && filter::host_pin(env, peer).is_none() {
                    calls.push_back(Call(peer, 5, vec![]));
                    disconnected_source |= self.guest == Some(peer);
                }
                calls.extend(filter::disconnect_owned(env, pin));
            }
        }
        // A guest may have retained the proposed input before Connect failed.
        if let Some(output) = self.guest
            && !disconnected_source
        {
            calls.push_back(Call(output, 5, vec![]));
        }
        g(env, self.graph)
            .members
            .retain(|m| !m.host.is_some_and(|f| self.added.contains(&f)));
        for &filter in &self.added {
            state(env).filters.get_mut(&filter).unwrap().graph = 0;
        }
        chain(
            env,
            calls,
            Vec::new(),
            Box::new(move |env, _| {
                for filter in self.added.into_iter().rev() {
                    release(env, filter);
                }
                Outcome::Return(hr)
            }),
        )
    }
}

fn guest_pin(env: &mut Env<'_>, graph: u32, output: u32) -> Outcome {
    // PIN_INFO is a filter pointer, direction and WCHAR[128]. The same cleared
    // block is reused for QueryDirection and ConnectedTo.
    let Some(scratch) = alloc(env, 264) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    guest_call(env, output, 9, vec![scratch], move |env, hr| {
        let direction = env.cpu.read_u32(scratch).unwrap_or(INPUT);
        if hr != S_OK || direction != OUTPUT {
            free(env, scratch);
            return Outcome::Return(if hr != S_OK {
                hr
            } else {
                VFW_E_INVALID_DIRECTION
            });
        }
        let _ = env.cpu.write_u32(scratch, 0);
        guest_call(env, output, 6, vec![scratch], move |env, hr| {
            let peer = env.cpu.read_u32(scratch).unwrap_or(0);
            if hr == S_OK && peer != 0 {
                free(env, scratch);
                return guest_call(env, peer, 2, vec![], |_, _| {
                    Outcome::Return(VFW_E_ALREADY_CONNECTED)
                });
            }
            if hr != VFW_E_NOT_CONNECTED {
                free(env, scratch);
                return Outcome::Return(if hr == S_OK { E_UNEXPECTED } else { hr });
            }
            let _ = env.cpu.write(scratch, &[0; 264]);
            guest_call(env, output, 8, vec![scratch], move |env, hr| {
                let owner = env.cpu.read_u32(scratch).unwrap_or(0);
                let direction = env.cpu.read_u32(scratch + 4).unwrap_or(INPUT);
                free(env, scratch);
                if hr != S_OK {
                    return Outcome::Return(hr);
                }
                let member = g(env, graph).members.iter().any(|m| m.filter == owner);
                let result = if direction != OUTPUT {
                    VFW_E_INVALID_DIRECTION
                } else if owner == 0 || !member {
                    E_INVALIDARG
                } else {
                    S_OK
                };
                guest_call(env, owner, 2, vec![], move |env, _| {
                    if result != S_OK {
                        return Outcome::Return(result);
                    }
                    // Hold the caller's pin across nested source/renderer calls.
                    guest_call(env, output, 1, vec![], move |env, _| {
                        let outcome = guest_source(env, graph, output);
                        after(
                            env,
                            outcome,
                            Box::new(move |env, results| {
                                let hr = results[0];
                                guest_call(env, output, 2, vec![], move |_, _| Outcome::Return(hr))
                            }),
                        )
                    })
                })
            })
        })
    })
}

/// Attach a continuation to the shared connection handler without replacing
/// the guest IPin::Connect/host ReceiveConnection negotiation it implements.
fn after(env: &mut Env<'_>, outcome: Outcome, finish: Then) -> Outcome {
    match outcome {
        Outcome::Return(hr) => finish(env, vec![hr]),
        Outcome::Call {
            target,
            args,
            then,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args,
            then: Box::new(move |env, resume| {
                let outcome = then(env, resume);
                after(env, outcome, finish)
            }),
            stack,
            unwindable,
        },
        _ => unreachable!("source connections only return or call guest COM methods"),
    }
}

fn guest_source(env: &mut Env<'_>, graph: u32, output: u32) -> Outcome {
    let mut build = match Build::new(env, graph) {
        Ok(build) => build,
        Err(error) => return Outcome::Return(error),
    };
    build.guest = Some(output);
    let input = endpoint(env, build.splitter, INPUT);
    let outcome = filter::connect(env, output, input, 0);
    after(
        env,
        outcome,
        Box::new(move |env, results| {
            if results[0] != S_OK {
                let hr = if results[0] & 0x8000_0000 != 0 {
                    results[0]
                } else {
                    VFW_E_CANNOT_RENDER
                };
                return build.finish(env, hr);
            }
            streams(env, build)
        }),
    )
}

fn file(env: &mut Env<'_>, graph: u32, source: u32, output: u32) -> Outcome {
    let Some(info) = state(env).filters[&source]
        .source
        .as_ref()
        .and_then(|s| s.info.clone())
    else {
        return Outcome::Return(VFW_E_UNSUPPORTED_STREAM);
    };
    let mut build = match Build::new(env, graph) {
        Ok(build) => build,
        Err(error) => return Outcome::Return(error),
    };
    build.file = Some((source, output));
    let input = endpoint(env, build.splitter, INPUT);
    if let Err(error) = connect(env, output, input) {
        return build.finish(env, error);
    }
    let media = state(env).filters.get_mut(&source).unwrap().source.take();
    state(env).filters.get_mut(&build.splitter).unwrap().source = media;
    for (present, major, sub, name) in [
        (
            info.video.is_some(),
            MEDIATYPE_VIDEO,
            MEDIASUBTYPE_MPEG1PAYLOAD,
            "Video",
        ),
        (
            info.audio.is_some(),
            MEDIATYPE_AUDIO,
            MEDIASUBTYPE_MPEG1AUDIOPAYLOAD,
            "Audio",
        ),
    ] {
        if !present {
            continue;
        }
        let mt = MediaType {
            major,
            sub,
            fixed_size: false,
            temporal: true,
            sample_size: 0,
            format_type: GUID_NULL,
            format: Vec::new(),
        };
        if let Err(error) = filter::new_pin(env, build.splitter, OUTPUT, name, major, Some(mt)) {
            return build.finish(env, error);
        }
    }
    streams(env, build)
}

/// Both file sources and guest IAsyncReader sources arrive here with the same
/// probed splitter outputs and use identical decoder/renderer negotiation.
fn streams(env: &mut Env<'_>, mut build: Build) -> Outcome {
    let graph = build.graph;
    let mut video_output = None;
    let mut audio_output = None;
    let state = env.ext.get::<State>();
    let outputs: Vec<(u32, Iid)> = state.filters[&build.splitter]
        .pins
        .iter()
        .filter_map(|&pin| {
            let p = &state.pins[&pin];
            (p.dir == OUTPUT).then_some((pin, p.major))
        })
        .collect();
    for (pin, major) in outputs {
        let (role, clsid, name) = if major == MEDIATYPE_VIDEO {
            (Role::VideoDecoder, CLSID_MPEG_VIDEO_CODEC, "Video decoder")
        } else if major == MEDIATYPE_AUDIO {
            (Role::AudioDecoder, CLSID_MPEG_AUDIO_CODEC, "Audio decoder")
        } else {
            continue;
        };
        let decoder = match member(env, graph, role, clsid, name) {
            Ok(filter) => filter,
            Err(error) => return build.finish(env, error),
        };
        build.added.push(decoder);
        let input = endpoint(env, decoder, INPUT);
        if let Err(error) = connect(env, pin, input) {
            return build.finish(env, error);
        }
        let out = endpoint(env, decoder, OUTPUT);
        if major == MEDIATYPE_VIDEO {
            video_output = Some(out);
        } else {
            audio_output = Some(out);
        }
    }
    let finish: Then = Box::new(move |env, results| {
        let video_result = results.first().copied();
        let audio_result = audio_output.map(|pin| audio(env, graph, pin));
        let video_ok = video_result == Some(S_OK);
        let audio_ok = audio_result.as_ref().is_some_and(Result::is_ok);
        let failed = video_result.is_some_and(|hr| hr != S_OK)
            || audio_result.as_ref().is_some_and(Result::is_err);
        let hr = if failed && (video_ok || audio_ok) {
            adv_abi::warn_unavailable(
                env,
                "IGraphBuilder::Render",
                "only part of the source's streams could be rendered",
            );
            VFW_S_PARTIAL_RENDER
        } else if failed {
            video_result
                .filter(|hr| *hr != S_OK)
                .or_else(|| audio_result.and_then(Result::err))
                .unwrap_or(VFW_E_CANNOT_RENDER)
        } else if video_ok || audio_ok {
            S_OK
        } else {
            VFW_E_CANNOT_RENDER
        };
        build.finish(env, hr)
    });
    match video_output {
        Some(out) => video(env, graph, out, finish),
        None => finish(env, Vec::new()),
    }
}

fn audio(env: &mut Env<'_>, graph: u32, out: u32) -> Result<(), u32> {
    let renderer = member(
        env,
        graph,
        Role::AudioRenderer,
        CLSID_DSOUND_RENDER,
        "Audio renderer",
    )?;
    let input = endpoint(env, renderer, INPUT);
    if let Err(error) = connect(env, out, input) {
        let _ = remove_filter(env, graph, renderer);
        return Err(error);
    }
    Ok(())
}

fn video(env: &mut Env<'_>, graph: u32, out: u32, then: Then) -> Outcome {
    let filters = g(env, graph)
        .members
        .iter()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .collect();
    let Some(scratch) = alloc(env, 16) else {
        return then(env, vec![E_OUTOFMEMORY]);
    };
    next_filter(env, out, filters, scratch, then)
}

fn next_filter(
    env: &mut Env<'_>,
    out: u32,
    mut filters: VecDeque<u32>,
    scratch: u32,
    then: Then,
) -> Outcome {
    let Some(filter) = filters.pop_front() else {
        free(env, scratch);
        let owner = state(env).pins[&out].filter;
        let graph = state(env).filters[&owner].graph;
        let hr = system_video(env, graph, out);
        return then(env, vec![hr]);
    };
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, filter, 10, vec![scratch], move |env, hr| {
        let enumerator = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || enumerator == 0 {
            return next_filter(env, out, filters, scratch, then);
        }
        next_pin(env, out, filters, enumerator, scratch, then)
    })
}

fn next_pin(
    env: &mut Env<'_>,
    out: u32,
    filters: VecDeque<u32>,
    enumerator: u32,
    scratch: u32,
    then: Then,
) -> Outcome {
    let _ = env.cpu.write_u32(scratch + 4, 0);
    guest_call(
        env,
        enumerator,
        3,
        vec![1, scratch + 4, scratch + 8],
        move |env, hr| {
            let pin = env.cpu.read_u32(scratch + 4).unwrap_or(0);
            if hr != S_OK || pin == 0 {
                return guest_call(env, enumerator, 2, vec![], move |env, _| {
                    next_filter(env, out, filters, scratch, then)
                });
            }
            guest_call(env, pin, 9, vec![scratch + 8], move |env, hr| {
                if hr != S_OK || env.cpu.read_u32(scratch + 8).unwrap_or(OUTPUT) != INPUT {
                    return guest_call(env, pin, 2, vec![], move |env, _| {
                        next_pin(env, out, filters, enumerator, scratch, then)
                    });
                }
                let _ = env.cpu.write_u32(scratch + 12, 0);
                guest_call(env, pin, 6, vec![scratch + 12], move |env, hr| {
                    let peer = env.cpu.read_u32(scratch + 12).unwrap_or(0);
                    if hr == S_OK && peer != 0 {
                        return guest_call(env, peer, 2, vec![], move |env, _| {
                            guest_call(env, pin, 2, vec![], move |env, _| {
                                next_pin(env, out, filters, enumerator, scratch, then)
                            })
                        });
                    }
                    if hr != VFW_E_NOT_CONNECTED {
                        return guest_call(env, pin, 2, vec![], move |env, _| {
                            next_pin(env, out, filters, enumerator, scratch, then)
                        });
                    }
                    guest_call(env, out, 3, vec![pin, 0], move |env, hr| {
                        guest_call(env, pin, 2, vec![], move |env, _| {
                            if hr != S_OK {
                                return next_pin(env, out, filters, enumerator, scratch, then);
                            }
                            guest_call(env, enumerator, 2, vec![], move |env, _| {
                                free(env, scratch);
                                then(env, vec![S_OK])
                            })
                        })
                    })
                })
            })
        },
    )
}

#[cfg(test)]
mod guest_tests;
fn system_video(env: &mut Env<'_>, graph: u32, out: u32) -> u32 {
    if env.ext.try_get::<VideoProvider>().is_none() {
        adv_abi::warn_unavailable(
            env,
            "IGraphBuilder::Render",
            "video presentation provider is unavailable",
        );
        return VFW_E_CANNOT_RENDER;
    }
    let existing = g(env, graph)
        .members
        .iter()
        .filter_map(|m| m.host)
        .collect::<Vec<_>>();
    for id in existing {
        if state(env).filters[&id].role == Role::VideoRenderer {
            let input = endpoint(env, id, INPUT);
            return connect(env, out, input).map_or_else(|hr| hr, |_| S_OK);
        }
    }
    let renderer = match member(
        env,
        graph,
        Role::VideoRenderer,
        CLSID_VIDEO_RENDER,
        "Video Renderer",
    ) {
        Ok(renderer) => renderer,
        Err(hr) => return hr,
    };
    let input = endpoint(env, renderer, INPUT);
    if let Err(hr) = connect(env, out, input) {
        g(env, graph).members.retain(|m| m.host != Some(renderer));
        state(env).filters.get_mut(&renderer).unwrap().graph = 0;
        release(env, renderer);
        return hr;
    }
    S_OK
}

#[cfg(test)]
mod tests;
