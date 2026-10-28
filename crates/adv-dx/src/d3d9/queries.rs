//! Queries observe recording submission without parking a guest thread.
use super::*;
use std::{sync::OnceLock, time::Instant};

const IID_QUERY: Iid = [
    0x60, 0x14, 0x77, 0xd9, 0x95, 0xa6, 0x26, 0x4f, 0xbb, 0xd3, 0x27, 0xb8, 0x40, 0xb5, 0x41, 0xcc,
];
#[derive(Default)]
pub(super) struct DeviceState {
    pub(super) active: u32,
    pub(super) pixels: u64,
    pub(super) scanned: usize,
    pub(super) submissions: u64,
}
pub(super) struct Query {
    pub(super) device: Id,
    kind: i32,
    begin: Option<u64>,
    submission: Option<u64>,
    value: u64,
}
fn query_object<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Query> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::Query(q) => Some(q),
        _ => None,
    }
}
fn size(kind: i32) -> u32 {
    if matches!(kind, 10 | 12) { 8 } else { 4 }
}
fn timestamp() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        create_ABI,
        query_query_ABI,
        query_add_ref_ABI,
        query_release_ABI,
        get_device_ABI,
        get_type_ABI,
        data_size_ABI,
        issue_ABI,
        get_data_ABI,
    ] {
        registry.register(api);
    }
}
unknown!(
    query_query,
    query_add_ref,
    query_release,
    "IDirect3DQuery9::QueryInterface",
    "IDirect3DQuery9::AddRef",
    "IDirect3DQuery9::Release"
);
#[win32(d3d9, name = "IDirect3DDevice9::CreateQuery", level = implement)]
fn create(env: &mut Env<'_>, this: GuestPtr<()>, kind: i32, output: GuestPtr<u32>) -> u32 {
    if !output.is_null() && output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    if device(env, this.addr()).is_none() {
        return INVALID;
    }
    if !matches!(kind, 8..=12) {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::CreateQuery",
            &format!("unsupported query type {kind}"),
        );
        return UNAVAILABLE;
    }
    if output.is_null() {
        return 0;
    }
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    if com(env)
        .add_ref_identity(&mut env.sys.objects, owner)
        .is_none()
    {
        return OUT_OF_MEMORY;
    }
    let Some((pointer, _)) = object(
        env,
        "IDirect3DQuery9",
        IID_QUERY,
        Payload::Query(Query {
            device: owner,
            kind,
            begin: None,
            submission: None,
            value: u64::from(kind == 8),
        }),
    ) else {
        let _ = com(env).release_identity(&mut env.sys.objects, owner);
        return OUT_OF_MEMORY;
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9, name = "IDirect3DQuery9::GetDevice", level = implement)]
fn get_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    let Some(q) = query_object(env, this.addr()) else {
        return INVALID;
    };
    child_device(env, q.device, output)
}
#[win32(d3d9, name = "IDirect3DQuery9::GetType", level = implement)]
fn get_type(env: &mut Env<'_>, this: GuestPtr<()>) -> i32 {
    query_object(env, this.addr()).map_or(0, |q| q.kind)
}
#[win32(d3d9, name = "IDirect3DQuery9::GetDataSize", level = implement)]
fn data_size(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    query_object(env, this.addr()).map_or(0, |q| size(q.kind))
}
#[win32(d3d9, name = "IDirect3DQuery9::Issue", level = implement)]
fn issue(env: &mut Env<'_>, this: GuestPtr<()>, flags: u32) -> u32 {
    let Some(q) = query_object(env, this.addr()) else {
        return INVALID;
    };
    let (owner, kind, begin) = (q.device, q.kind, q.begin);
    if flags != 1 && !(flags == 2 && matches!(kind, 9 | 11)) {
        return INVALID;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) else {
        return INVALID;
    };
    let pixels = if kind == 9 {
        recording::query_pixels(&d.frame, &mut d.queries)
    } else {
        0
    };
    if kind == 9 {
        if flags == 2 && begin.is_none() {
            d.queries.active += 1;
        }
        if flags == 1 && begin.is_some() {
            d.queries.active -= 1;
        }
    }
    let submission = d.queries.submissions + u64::from(recording::query_pending(&d.frame));
    let value = match kind {
        8 => 1,
        9 => pixels.wrapping_sub(begin.unwrap_or(pixels)),
        10 => timestamp(),
        11 => 0,
        12 => 1_000_000_000,
        _ => unreachable!(),
    };
    let Some(Payload::Query(q)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        unreachable!()
    };
    q.begin = (flags == 2).then_some(pixels);
    q.submission = (flags == 1).then_some(submission);
    q.value = value;
    0
}
pub(super) fn destroy(env: &mut Env<'_>, q: Query) {
    if q.kind == 9
        && q.begin.is_some()
        && let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, q.device)
    {
        d.queries.active -= 1;
    }
}
#[win32(d3d9, name = "IDirect3DQuery9::GetData", level = implement)]
fn get_data(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<u8>,
    bytes: u32,
    flags: u32,
) -> u32 {
    let Some(q) = query_object(env, this.addr()) else {
        return INVALID;
    };
    let (owner, kind, submission, value, building) =
        (q.device, q.kind, q.submission, q.value, q.begin.is_some());
    if flags & !1 != 0 || (bytes != 0 && (bytes != size(kind) || output.is_null())) {
        return INVALID;
    }
    if bytes != 0
        && adv_abi::guest::check_range(env.cpu, output.addr(), bytes, Prot::WRITE).is_err()
    {
        return INVALID;
    }
    if building {
        return 1;
    }
    if let Some(submission) = submission {
        let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
            return INVALID;
        };
        if d.gpu_lost || d.needs_reset {
            return DEVICE_LOST;
        }
        if d.queries.submissions < submission {
            if flags == 0 {
                return 1;
            }
            match recording::query_flush(env, owner) {
                Ok(()) => {}
                Err(Error::Busy) => return 1,
                Err(error) => {
                    return match gpu_error(error) {
                        Outcome::Return(code) => code,
                        _ => unreachable!(),
                    };
                }
            }
            let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
                unreachable!()
            };
            if d.queries.submissions < submission {
                return 1;
            }
            if d.gpu_lost {
                return DEVICE_LOST;
            }
        }
        // Submit the backend's pending batches without a completion wait.
        let Some(gpu) = env.ext.get::<State>().gpu.as_ref() else {
            return UNAVAILABLE;
        };
        match gpu.context().submission() {
            Ok(lease) => {
                lease.submit([]);
            }
            Err(Error::Busy) => return 1,
            Err(_) => return DEVICE_LOST,
        }
        if let Some(Payload::Query(q)) =
            com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
        {
            q.submission = None;
        }
    }
    if bytes == 0 {
        return 0;
    }
    env.cpu
        .write(output.addr(), &value.to_le_bytes()[..bytes as usize])
        .map_or(INVALID, |()| 0)
}
