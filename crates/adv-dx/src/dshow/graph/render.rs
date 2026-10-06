//! Build the supported file-source graph with the common splitter/decoders and
//! existing guest video renderers. Audio uses the ordinary DirectSound renderer.
use super::*;
use crate::dshow::filter::{Role, INPUT, OUTPUT};

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
        adv_abi::warn_unavailable(
            env,
            "IGraphBuilder::Render",
            "automatic rendering of guest output pins is unavailable",
        );
        return Outcome::Return(VFW_E_CANNOT_RENDER);
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

fn file(env: &mut Env<'_>, graph: u32, source: u32, output: u32) -> Outcome {
    let Some(info) = state(env).filters[&source]
        .source
        .as_ref()
        .and_then(|s| s.info.clone())
    else {
        return Outcome::Return(VFW_E_UNSUPPORTED_STREAM);
    };
    let splitter = match member(
        env,
        graph,
        Role::Splitter,
        CLSID_MPEG1_SPLITTER,
        "Media splitter",
    ) {
        Ok(filter) => filter,
        Err(error) => return Outcome::Return(error),
    };
    let input = endpoint(env, splitter, INPUT);
    if let Err(error) = connect(env, output, input) {
        return Outcome::Return(error);
    }
    let media = state(env).filters.get_mut(&source).unwrap().source.take();
    state(env).filters.get_mut(&splitter).unwrap().source = media;
    let mut video_output = None;
    let mut audio_output = None;
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
        let pin = match filter::new_pin(env, splitter, OUTPUT, name, major, Some(mt)) {
            Ok(pin) => pin,
            Err(error) => return Outcome::Return(error),
        };
        let (role, clsid, name) = if major == MEDIATYPE_VIDEO {
            (Role::VideoDecoder, CLSID_MPEG_VIDEO_CODEC, "Video decoder")
        } else {
            (Role::AudioDecoder, CLSID_MPEG_AUDIO_CODEC, "Audio decoder")
        };
        let decoder = match member(env, graph, role, clsid, name) {
            Ok(filter) => filter,
            Err(error) => return Outcome::Return(error),
        };
        let input = endpoint(env, decoder, INPUT);
        if let Err(error) = connect(env, pin, input) {
            return Outcome::Return(error);
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
        if failed && (video_ok || audio_ok) {
            adv_abi::warn_unavailable(
                env,
                "IGraphBuilder::Render",
                "only part of the file's streams could be rendered",
            );
            Outcome::Return(VFW_S_PARTIAL_RENDER)
        } else if failed {
            Outcome::Return(
                video_result
                    .filter(|hr| *hr != S_OK)
                    .or_else(|| audio_result.and_then(Result::err))
                    .unwrap_or(VFW_E_CANNOT_RENDER),
            )
        } else if video_ok || audio_ok {
            Outcome::Return(S_OK)
        } else {
            Outcome::Return(VFW_E_CANNOT_RENDER)
        }
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
    connect(env, out, input)
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
        adv_abi::warn_unavailable(env, "IGraphBuilder::Render", "no existing video renderer accepted the decoded stream; built-in video windows are unavailable");
        return then(env, vec![VFW_E_CANNOT_RENDER]);
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
mod tests;
