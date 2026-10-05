//! Host IMemAllocator and IMediaSample. A sample is retained internally for its
//! allocator's lifetime; when its last guest reference goes it returns to the
//! free list instead of being destroyed.
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

use super::*;

pub(crate) struct Allocator {
    pub face: u32,
    samples: Vec<u32>,
    pub free: Vec<u32>,
    size: u32,
    committed: bool,
}

#[derive(Clone)]
pub(crate) struct Sample {
    allocator: u32,
    pub buffer: u32,
    size: u32,
    pub actual: u32,
    pub time: Option<(i64, i64)>,
    media_time: Option<(i64, i64)>,
    pub sync_point: bool,
    preroll: bool,
    pub discontinuity: bool,
}

/// An allocator with `count` committed buffers of `size` bytes. The caller owns
/// the returned (canonical) reference.
pub(crate) fn create_allocator(env: &mut Env<'_>, count: u32, size: u32) -> Result<u32, u32> {
    let faces = super::create(env, &[Face::MemAllocator], Obj::Allocator)?;
    let allocator = faces[0];
    state(env).allocators.insert(
        allocator,
        Allocator {
            face: allocator,
            samples: Vec::new(),
            free: Vec::new(),
            size,
            committed: true,
        },
    );
    for _ in 0..count {
        let Some(buffer) = alloc(env, size) else {
            return Err(E_OUTOFMEMORY);
        };
        let sample = super::create(env, &[Face::MediaSample], Obj::Sample)?[0];
        state(env).samples.insert(
            sample,
            Sample {
                allocator,
                buffer,
                size,
                actual: 0,
                time: None,
                media_time: None,
                sync_point: true,
                preroll: false,
                discontinuity: false,
            },
        );
        retain_internal(env, sample);
        state(env)
            .allocators
            .get_mut(&allocator)
            .unwrap()
            .samples
            .push(sample);
        // Dropping the creation reference puts the sample on the free list.
        release(env, sample);
    }
    Ok(allocator)
}

/// A free sample with one reference held by the caller.
pub(crate) fn take(env: &mut Env<'_>, allocator: u32) -> Option<u32> {
    let sample = state(env).allocators.get_mut(&allocator)?.free.pop()?;
    add_ref(env, sample);
    let s = state(env).samples.get_mut(&sample).unwrap();
    s.actual = 0;
    s.time = None;
    s.media_time = None;
    s.sync_point = true;
    s.preroll = false;
    s.discontinuity = false;
    Some(sample)
}

/// Called whenever a host object's external references reach zero.
pub(crate) fn returned(env: &mut Env<'_>, canonical: u32) {
    let state = state(env);
    let Some(sample) = state.samples.get(&canonical) else {
        return;
    };
    if let Some(allocator) = state.allocators.get_mut(&sample.allocator)
        && !allocator.free.contains(&canonical)
    {
        allocator.free.push(canonical);
    }
}

pub(crate) fn call(env: &mut Env<'_>, face: Face, method: &str, this: u32) -> Option<Outcome> {
    let this = canonical(env, this)?;
    match face {
        Face::MemAllocator => allocator_method(env, this, method),
        Face::MediaSample => sample_method(env, this, method),
        _ => None,
    }
}

fn allocator_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let a1 = env.arg(1);
    let properties = |env: &mut Env<'_>, output: u32| {
        let a = &state(env).allocators[&this];
        let raw: Vec<u8> = [a.samples.len() as u32, a.size, 1, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        env.cpu.write(output, &raw).is_ok()
    };
    match method {
        // Buffers are fixed by the connection; report what exists.
        "SetProperties" => {
            let output = env.arg(2);
            r(if properties(env, output) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetProperties" => r(if properties(env, a1) { S_OK } else { E_POINTER }),
        "Commit" => {
            state(env).allocators.get_mut(&this)?.committed = true;
            r(S_OK)
        }
        "Decommit" => {
            state(env).allocators.get_mut(&this)?.committed = false;
            r(S_OK)
        }
        "GetBuffer" => {
            let _ = env.cpu.write_u32(a1, 0);
            if !state(env).allocators[&this].committed {
                return r(VFW_E_NOT_COMMITTED);
            }
            match take(env, this) {
                Some(sample) => {
                    let _ = env.cpu.write_u32(a1, sample);
                    r(S_OK)
                }
                // Only the host stream draws from these allocators.
                None => r(VFW_E_WRONG_STATE),
            }
        }
        "ReleaseBuffer" => {
            release(env, a1);
            r(S_OK)
        }
        _ => None,
    }
}

fn sample_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let (a1, a2) = (env.arg(1), env.arg(2));
    let s = state(env).samples.get(&this)?.clone();
    let flag = |value: bool| if value { S_OK } else { S_FALSE };
    match method {
        "GetPointer" => {
            let buffer = s.buffer;
            r(if env.cpu.write_u32(a1, buffer).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetSize" => Some(Outcome::Return(s.size)),
        "GetTime" | "GetMediaTime" => {
            let value = if method == "GetTime" {
                s.time
            } else {
                s.media_time
            };
            let Some((start, stop)) = value else {
                return r(0x8004_0249); // VFW_E_SAMPLE_TIME_NOT_SET
            };
            write_i64(env, a1, start);
            write_i64(env, a2, stop);
            r(S_OK)
        }
        "SetTime" | "SetMediaTime" => {
            let value = (a1 != 0)
                .then(|| Some((read_i64(env, a1)?, read_i64(env, a2).unwrap_or(0))))
                .flatten();
            let s = state(env).samples.get_mut(&this)?;
            if method == "SetTime" {
                s.time = value;
            } else {
                s.media_time = value;
            }
            r(S_OK)
        }
        "IsSyncPoint" => r(flag(s.sync_point)),
        "SetSyncPoint" => {
            state(env).samples.get_mut(&this)?.sync_point = a1 != 0;
            r(S_OK)
        }
        "IsPreroll" => r(flag(s.preroll)),
        "SetPreroll" => {
            state(env).samples.get_mut(&this)?.preroll = a1 != 0;
            r(S_OK)
        }
        "IsDiscontinuity" => r(flag(s.discontinuity)),
        "SetDiscontinuity" => {
            state(env).samples.get_mut(&this)?.discontinuity = a1 != 0;
            r(S_OK)
        }
        "GetActualDataLength" => Some(Outcome::Return(s.actual)),
        "SetActualDataLength" => {
            if a1 > s.size {
                return r(E_INVALIDARG);
            }
            state(env).samples.get_mut(&this)?.actual = a1;
            r(S_OK)
        }
        // The connection type never changes mid-stream.
        "GetMediaType" => {
            let _ = env.cpu.write_u32(a1, 0);
            r(S_FALSE)
        }
        "SetMediaType" => r(S_OK),
        _ => None,
    }
}
