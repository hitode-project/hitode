//! Explicit guest readback uses the one background worker and Machine waits.
use super::*;
use adv_abi::types::{D3DLOCKED_RECT, D3DSURFACE_DESC};
use adv_device::{
    gpu::{Image, Readback},
    surface::Rect,
};
use adv_sync::background::{Priority, Task};
use resources::{Kind, Lock, Resource};
use std::sync::mpsc::TrySendError;

pub(super) struct PendingTransfer {
    context: u32,
    event: u32,
    owner: Id,
    destination: Id,
    image: Image,
    storage: u32,
    length: u32,
    ticket: Option<Readback>,
    job: Option<Task>,
    result: Option<u32>,
}
pub(super) fn register(registry: &mut Registry) {
    for api in [create_offscreen_ABI, transfer_ABI, lock_ABI, unlock_ABI] {
        registry.register(api);
    }
}
#[win32(d3d9,name="IDirect3DDevice9::CreateOffscreenPlainSurface",level=implement)]
fn create_offscreen(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    width: u32,
    height: u32,
    format: u32,
    pool: i32,
    output: GuestPtr<u32>,
    shared: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    if device(env, this.addr()).is_none() {
        return Outcome::Return(INVALID);
    }
    if !matches!(pool, 2 | 3) || !matches!(format, 21 | 22) || !shared.is_null() {
        return Outcome::Return(UNAVAILABLE);
    }
    let Some(length) = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| n > 0 && n <= 64 * 1024 * 1024)
    else {
        return Outcome::Return(INVALID);
    };
    let Some(storage) = alloc(env, length) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some((pointer, _)) = resources::make(
        env,
        Resource {
            pointer: 0,
            device: owner,
            kind: Kind::Surface,
            desc: D3DSURFACE_DESC {
                Format: format,
                Type: 1,
                Usage: 0,
                Pool: pool,
                MultiSampleType: 0,
                MultiSampleQuality: 0,
                Width: width,
                Height: height,
            },
            depth: None,
            surface: None,
            storage,
            owns_surface: false,
            lock: None,
            container: None,
            level_surface: None,
        },
    ) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return Outcome::Return(INVALID);
    }
    Outcome::Return(0)
}
#[win32(d3d9,name="IDirect3DSurface9::LockRect",level=implement)]
fn lock(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<D3DLOCKED_RECT>,
    rect: GuestPtr<RECT>,
    flags: u32,
) -> u32 {
    if let Some(container) = resources::container_pointer(env, this.addr()) {
        return resources::texture_lock(env, GuestPtr::new(container), 0, output, rect, flags);
    }
    let Some(r) = resources::resource(env, this.addr()) else {
        return INVALID;
    };
    if r.kind != Kind::Surface
        || r.desc.Pool < 2
        || r.storage == 0
        || r.lock.is_some()
        || flags & !0x10 != 0
    {
        return INVALID;
    }
    let rectangle = if rect.is_null() {
        Rect {
            x: 0,
            y: 0,
            w: r.desc.Width,
            h: r.desc.Height,
        }
    } else {
        let Ok(rect) = rect.read(env.cpu) else {
            return INVALID;
        };
        if rect.left < 0
            || rect.top < 0
            || rect.right <= rect.left
            || rect.bottom <= rect.top
            || rect.right as u32 > r.desc.Width
            || rect.bottom as u32 > r.desc.Height
        {
            return INVALID;
        }
        Rect {
            x: rect.left as u32,
            y: rect.top as u32,
            w: (rect.right - rect.left) as u32,
            h: (rect.bottom - rect.top) as u32,
        }
    };
    let pitch = r.desc.Width * 4;
    let bits = r.storage + rectangle.y * pitch + rectangle.x * 4;
    if output
        .write(
            env.cpu,
            D3DLOCKED_RECT {
                Pitch: pitch as i32,
                pBits: bits,
            },
        )
        .is_err()
    {
        return INVALID;
    }
    if let Some(Payload::Resource(r)) =
        com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    {
        r.lock = Some(Lock { rectangle, flags });
    }
    0
}
#[win32(d3d9,name="IDirect3DSurface9::UnlockRect",level=implement)]
fn unlock(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    if let Some(container) = resources::container_pointer(env, this.addr()) {
        return resources::texture_unlock(env, GuestPtr::new(container), 0);
    }
    let Some(Payload::Resource(r)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return Outcome::Return(INVALID);
    };
    if r.kind != Kind::Surface || r.desc.Pool < 2 || r.lock.is_none() {
        return Outcome::Return(INVALID);
    }
    r.lock = None;
    Outcome::Return(0)
}
#[win32(d3d9,name="IDirect3DDevice9::GetRenderTargetData",level=implement)]
fn transfer(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    source: GuestPtr<()>,
    destination: GuestPtr<()>,
) -> Outcome {
    let Some(owner) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| device(env, this.addr()).is_some())
    else {
        return Outcome::Return(INVALID);
    };
    let Some(src) = resources::resource(env, source.addr()) else {
        return Outcome::Return(INVALID);
    };
    let Some(dst) = resources::resource(env, destination.addr()) else {
        return Outcome::Return(INVALID);
    };
    if src.device != owner
        || dst.device != owner
        || src.kind != Kind::Surface
        || dst.kind != Kind::Surface
        || src.desc.Pool != 0
        || src.desc.Usage & 1 == 0
        || dst.desc.Pool != 2
        || dst.lock.is_some()
        || src.desc.Width != dst.desc.Width
        || src.desc.Height != dst.desc.Height
        || src.desc.Format != dst.desc.Format
    {
        return Outcome::Return(INVALID);
    }
    let state = env.ext.get::<State>();
    if state.sender.is_none() || state.wake.is_none() {
        return Outcome::Return(UNAVAILABLE);
    }
    let Some(image) = src
        .surface
        .and_then(|id| (state.provider.target)(env.ext, id))
        .map(|t| t.image)
    else {
        return Outcome::Return(INVALID);
    };
    let storage = dst.storage;
    let length = dst.desc.Width * dst.desc.Height * 4;
    if adv_abi::guest::check_range(env.cpu, storage, length, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let destination = com(env)
        .identity::<Payload>(&env.sys.objects, destination.addr())
        .unwrap();
    if !com(env).retain_internal(&mut env.sys.objects, owner) {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    if !com(env).retain_internal(&mut env.sys.objects, destination) {
        let _ = com(env).release_internal(&mut env.sys.objects, owner);
        return Outcome::Return(OUT_OF_MEMORY);
    }
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    if event == 0 {
        for id in [destination, owner] {
            let _ = com(env).release_internal(&mut env.sys.objects, id);
        }
        return Outcome::Return(OUT_OF_MEMORY);
    }
    env.ext.get_mut::<State>().transfers.push(PendingTransfer {
        context: env.sys.current,
        event,
        owner,
        destination,
        image,
        storage,
        length,
        ticket: None,
        job: None,
        result: None,
    });
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline: None,
        },
        move |env, _| {
            let index = env
                .ext
                .get::<State>()
                .transfers
                .iter()
                .position(|p| p.event == event)
                .expect("owned map wait");
            let transfer = env.ext.get_mut::<State>().transfers.swap_remove(index);
            let result = transfer
                .result
                .expect("readback signals only on typed completion");
            cleanup(env, transfer);
            Outcome::Return(result)
        },
    )
}
fn cleanup(env: &mut Env<'_>, transfer: PendingTransfer) {
    if transfer.result.is_none()
        && let Some(ticket) = &transfer.ticket
    {
        ticket.cancel();
    }
    env.sys.objects.close(transfer.event);
    for id in [transfer.destination, transfer.owner] {
        if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id) {
            destroy(env, dead);
        }
    }
}
pub(super) fn cancel_pending(env: &mut Env<'_>) {
    for transfer in std::mem::take(&mut env.ext.get_mut::<State>().transfers) {
        cleanup(env, transfer);
    }
}
pub(super) fn pump(env: &mut Env<'_>) -> bool {
    let transfers = std::mem::take(&mut env.ext.get_mut::<State>().transfers);
    let mut progress = false;
    for mut transfer in transfers {
        if env
            .sys
            .contexts
            .get(transfer.context as usize)
            .is_none_or(|ctx| ctx.is_done())
        {
            cleanup(env, transfer);
            continue;
        }
        if transfer.result.is_none() {
            if transfer.ticket.is_none() {
                let wake = env.ext.get::<State>().wake.clone();
                let mut gpu = env
                    .ext
                    .get_mut::<State>()
                    .gpu
                    .take()
                    .expect("installed readback GPU");
                let result = gpu.readback(&transfer.image, wake);
                env.ext.get_mut::<State>().gpu = Some(gpu);
                match result {
                    Ok(mut ticket) => {
                        transfer.job = Some(
                            Task::new(ticket.take_completion_job().unwrap())
                                .finish_on_shutdown()
                                .without_wake(),
                        );
                        transfer.ticket = Some(ticket);
                    }
                    Err(Error::Busy) => {}
                    Err(_) => transfer.result = Some(DEVICE_LOST),
                }
            }
            if let Some(job) = transfer.job.take() {
                let sender = env.ext.get::<State>().sender.clone().unwrap();
                match sender.borrow_mut().try_submit(Priority::Urgent, job) {
                    Ok(_) => {}
                    Err(TrySendError::Full(job)) => transfer.job = Some(job),
                    Err(TrySendError::Disconnected(_)) => {
                        transfer.ticket.as_ref().unwrap().cancel();
                        transfer.result = Some(DEVICE_LOST);
                    }
                }
            }
            if transfer.result.is_none()
                && let Some(ticket) = &mut transfer.ticket
                && let Some(result) = ticket.poll()
            {
                transfer.result = Some(match result {
                    Ok(pixels) if pixels.len() == transfer.length as usize => {
                        if env.cpu.write(transfer.storage, &pixels).is_ok() {
                            0
                        } else {
                            INVALID
                        }
                    }
                    _ => DEVICE_LOST,
                });
            }
            if transfer.result.is_some() {
                env.sys.set_event(transfer.event);
                progress = true;
            }
        }
        env.ext.get_mut::<State>().transfers.push(transfer);
    }
    progress
}
