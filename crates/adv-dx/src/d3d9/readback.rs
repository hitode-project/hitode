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
    format: u32,
    ticket: Option<Readback>,
    job: Option<Task>,
    result: Option<u32>,
    lock_output: Option<GuestPtr<D3DLOCKED_RECT>>,
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
    if !matches!(pool, 2 | 3) || format::bytes(format).is_none() || !shared.is_null() {
        let reason = if format::bytes(format).is_none() {
            "pixel format is unsupported (supported: 21/22/26)"
        } else {
            "pool or shared-handle combination is unsupported (supported: SYSTEMMEM/SCRATCH)"
        };
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::CreateOffscreenPlainSurface",
            &format!("{reason}: format={format}, pool={pool}"),
        );
        return Outcome::Return(UNAVAILABLE);
    }
    let Some(length) = format::length(format, width, height) else {
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
            levels: Vec::new(),
            level: 0,
            lod: 0,
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
) -> Outcome {
    if let Some((container, level)) = resources::container_level(env, this.addr()) {
        return Outcome::Return(resources::texture_lock(
            env,
            GuestPtr::new(container),
            level,
            output,
            rect,
            flags,
        ));
    }
    let Some(r) = resources::resource(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let back_buffer = lockable_back_buffer(env, this.addr(), r);
    // DONOTWAIT needs a genuinely nonblocking readiness protocol. Until that
    // exists it is rejected, never silently turned into a Machine wait.
    let allowed_flags = if back_buffer { 0x810 | 0x8000 } else { 0x10 };
    if r.kind != Kind::Surface || r.lock.is_some() || flags & !allowed_flags != 0 {
        return Outcome::Return(INVALID);
    }
    if !back_buffer && (r.desc.Pool < 2 || r.storage == 0) {
        return Outcome::Return(INVALID);
    }
    let rectangle = match lock_rectangle(env, r.desc.Width, r.desc.Height, rect) {
        Ok(rectangle) => rectangle,
        Err(error) => return Outcome::Return(error),
    };
    if adv_abi::guest::check_range(env.cpu, output.addr(), 8, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let selected = Lock {
        rectangle,
        flags,
        ready: !back_buffer,
    };
    if !back_buffer {
        let value = locked_rect(r, rectangle);
        if output.write(env.cpu, value).is_err() {
            return Outcome::Return(INVALID);
        }
        set_lock(env, id, Some(selected));
        return Outcome::Return(0);
    }
    let owner = r.device;
    let format = r.desc.Format;
    let state = env.ext.get::<State>();
    if state.sender.is_none() || state.wake.is_none() {
        return Outcome::Return(UNAVAILABLE);
    }
    let Some(image) = r
        .surface
        .and_then(|s| (state.provider.target)(env.ext, s))
        .map(|t| t.image)
    else {
        return Outcome::Return(INVALID);
    };
    let Some(length) = format::length(format, r.desc.Width, r.desc.Height) else {
        return Outcome::Return(INVALID);
    };
    let storage = r.storage;
    // This is explicit guest staging owned by the same Resource, allocated on
    // the first CPU lock and reused. Creation/Present never read GPU pixels.
    let storage = if storage == 0 {
        let Some(storage) = alloc(env, length) else {
            return Outcome::Return(OUT_OF_MEMORY);
        };
        if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
            r.storage = storage;
        }
        storage
    } else {
        storage
    };
    if adv_abi::guest::check_range(env.cpu, storage, length, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let event = match retain_transfer(env, owner, id) {
        Ok(event) => event,
        Err(error) => return Outcome::Return(error),
    };
    set_lock(env, id, Some(selected));
    enqueue(
        env,
        PendingTransfer {
            context: env.context,
            event,
            owner,
            destination: id,
            image,
            storage,
            length,
            format,
            ticket: None,
            job: None,
            result: None,
            lock_output: Some(output),
        },
    )
}

pub(super) fn lockable_back_buffer(env: &Env<'_>, pointer: u32, r: &Resource) -> bool {
    if r.desc.Pool != 0 || r.desc.MultiSampleType != 0 || r.desc.MultiSampleQuality != 0 {
        return false;
    }
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer);
    matches!(com(env).get_identity(&env.sys.objects, r.device),
        Some(Payload::Device(d)) if d.parameters.Flags & 1 != 0 && d.back_buffer == id)
}
fn lock_rectangle(
    env: &Env<'_>,
    width: u32,
    height: u32,
    rect: GuestPtr<RECT>,
) -> Result<Rect, u32> {
    if rect.is_null() {
        return Ok(Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        });
    }
    let rect = rect.read(env.cpu).map_err(|_| INVALID)?;
    if rect.left < 0
        || rect.top < 0
        || rect.right <= rect.left
        || rect.bottom <= rect.top
        || rect.right as u32 > width
        || rect.bottom as u32 > height
    {
        return Err(INVALID);
    }
    Ok(Rect {
        x: rect.left as u32,
        y: rect.top as u32,
        w: (rect.right - rect.left) as u32,
        h: (rect.bottom - rect.top) as u32,
    })
}
fn locked_rect(r: &Resource, rectangle: Rect) -> D3DLOCKED_RECT {
    let pitch = format::pitch(r.desc.Format, r.desc.Width).unwrap();
    let bytes = format::bytes(r.desc.Format).unwrap();
    D3DLOCKED_RECT {
        Pitch: pitch as i32,
        pBits: r.storage + rectangle.y * pitch + rectangle.x * bytes,
    }
}
fn set_lock(env: &mut Env<'_>, resource: Id, lock: Option<Lock>) {
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, resource) {
        r.lock = lock;
    }
}
#[win32(d3d9,name="IDirect3DSurface9::UnlockRect",level=implement)]
fn unlock(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    if let Some((container, level)) = resources::container_level(env, this.addr()) {
        return resources::texture_unlock(env, GuestPtr::new(container), level);
    }
    let Some(r) = resources::resource(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let Some(lock) = r.lock.filter(|lock| lock.ready) else {
        return Outcome::Return(INVALID);
    };
    if r.kind != Kind::Surface {
        return Outcome::Return(INVALID);
    }
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    if r.desc.Pool >= 2 || lock.flags & 0x10 != 0 {
        set_lock(env, id, None);
        return Outcome::Return(0);
    }
    if !lockable_back_buffer(env, this.addr(), r) {
        return Outcome::Return(INVALID);
    }
    let provider = env.ext.get::<State>().provider;
    let Some(target) = r.surface.and_then(|s| (provider.target)(env.ext, s)) else {
        return Outcome::Return(INVALID);
    };
    let pitch = format::pitch(r.desc.Format, r.desc.Width).unwrap();
    let row = format::pitch(r.desc.Format, lock.rectangle.w).unwrap();
    let size = (lock.rectangle.h - 1) * pitch + row;
    let address = locked_rect(r, lock.rectangle).pBits;
    let format = r.desc.Format;
    let mut pixels = vec![0; size as usize];
    if adv_abi::guest::check_range(env.cpu, address, size, Prot::READ).is_err()
        || env.cpu.read(address, &mut pixels).is_err()
    {
        return Outcome::Return(INVALID);
    }
    let Some((pixels, pitch)) =
        format::upload(format, pixels, pitch, lock.rectangle.w, lock.rectangle.h)
    else {
        return Outcome::Return(INVALID);
    };
    set_lock(
        env,
        id,
        Some(Lock {
            ready: false,
            ..lock
        }),
    );
    dispatch(
        env,
        id,
        Operation::Upload {
            resource: id,
            level: None,
            target,
            pixels,
            pitch,
            rectangle: lock.rectangle,
        },
    )
}

/// Surface upload retries reserve the existing lock. Failure leaves it usable
/// for retry; cancellation of its guest continuation releases the reservation.
pub(super) fn upload_failed(env: &mut Env<'_>, operation: &Operation, cancelled: bool) {
    if let Operation::Upload {
        resource, level, ..
    } = operation
        && let Some(Payload::Resource(r)) =
            com(env).get_identity_mut(&mut env.sys.objects, *resource)
    {
        if let Some(level) = level {
            if let Some(selected) = r.levels.get_mut(*level as usize) {
                selected.dirty = true;
                if cancelled {
                    selected.lock = None;
                } else if let Some(lock) = &mut selected.lock {
                    lock.ready = true;
                }
            }
        } else if cancelled {
            r.lock = None;
        } else if let Some(lock) = &mut r.lock {
            lock.ready = true;
        }
    }
}
#[win32(d3d9,name="IDirect3DDevice9::GetRenderTargetData",level=implement)]
pub(super) fn transfer(
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
        || resources::locked(env, dst)
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
    let Some(storage) = resources::storage(env, dst) else {
        return Outcome::Return(INVALID);
    };
    let format = dst.desc.Format;
    let Some(length) = format::length(format, dst.desc.Width, dst.desc.Height) else {
        return Outcome::Return(INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, storage, length, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let destination = com(env)
        .identity::<Payload>(&env.sys.objects, destination.addr())
        .unwrap();
    let destination = resources::binding_owner(env, destination);
    let event = match retain_transfer(env, owner, destination) {
        Ok(event) => event,
        Err(error) => return Outcome::Return(error),
    };
    enqueue(
        env,
        PendingTransfer {
            context: env.context,
            event,
            owner,
            destination,
            image,
            storage,
            length,
            format,
            ticket: None,
            job: None,
            result: None,
            lock_output: None,
        },
    )
}
fn retain_transfer(env: &mut Env<'_>, owner: Id, destination: Id) -> Result<u32, u32> {
    if !com(env).retain_internal(&mut env.sys.objects, owner) {
        return Err(OUT_OF_MEMORY);
    }
    if !com(env).retain_internal(&mut env.sys.objects, destination) {
        let _ = com(env).release_internal(&mut env.sys.objects, owner);
        return Err(OUT_OF_MEMORY);
    }
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    if event == 0 {
        for id in [destination, owner] {
            let _ = com(env).release_internal(&mut env.sys.objects, id);
        }
        return Err(OUT_OF_MEMORY);
    }
    Ok(event)
}
fn enqueue(env: &mut Env<'_>, transfer: PendingTransfer) -> Outcome {
    let event = transfer.event;
    env.ext.get_mut::<State>().transfers.push(transfer);
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
            let mut transfer = env.ext.get_mut::<State>().transfers.swap_remove(index);
            let mut result = transfer
                .result
                .expect("readback signals only on typed completion");
            if let Some(output) = transfer.lock_output {
                result = publish_lock(env, transfer.destination, output, result);
                // Published locks belong to the Resource. Failed or cancelled
                // unpublished locks are still cleared by cleanup below.
                if result == 0 {
                    transfer.lock_output = None;
                }
            }
            cleanup(env, transfer);
            Outcome::Return(result)
        },
    )
}
fn publish_lock(
    env: &mut Env<'_>,
    resource: Id,
    output: GuestPtr<D3DLOCKED_RECT>,
    result: u32,
) -> u32 {
    if result != 0 {
        return result;
    }
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, resource) else {
        return INVALID;
    };
    let Some(lock) = r.lock else {
        return INVALID;
    };
    let value = locked_rect(r, lock.rectangle);
    if output.write(env.cpu, value).is_err() {
        return INVALID;
    }
    set_lock(
        env,
        resource,
        Some(Lock {
            ready: true,
            ..lock
        }),
    );
    0
}
fn cleanup(env: &mut Env<'_>, transfer: PendingTransfer) {
    if transfer.result.is_none()
        && let Some(ticket) = &transfer.ticket
    {
        ticket.cancel();
    }
    release_transfer(
        env,
        transfer.event,
        transfer.destination,
        transfer.owner,
        transfer.lock_output.is_some(),
    );
}
fn release_transfer(
    env: &mut Env<'_>,
    event: u32,
    destination: Id,
    owner: Id,
    unpublished_lock: bool,
) {
    if unpublished_lock {
        set_lock(env, destination, None);
    }
    env.sys.objects.close(event);
    for id in [destination, owner] {
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
                    Ok(pixels)
                        if pixels.len()
                            == transfer.image.width() as usize
                                * transfer.image.height() as usize
                                * 4 =>
                    {
                        match format::readback(transfer.format, pixels)
                            .filter(|pixels| pixels.len() == transfer.length as usize)
                        {
                            Some(pixels) => {
                                // A different guest thread can change protection while
                                // this thread is blocked. Revalidate the complete span
                                // before committing any bytes on every backend.
                                if adv_abi::guest::check_range(
                                    env.cpu,
                                    transfer.storage,
                                    transfer.length,
                                    Prot::WRITE,
                                )
                                .is_ok()
                                    && env.cpu.write(transfer.storage, &pixels).is_ok()
                                {
                                    0
                                } else {
                                    INVALID
                                }
                            }
                            None => DEVICE_LOST,
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

#[cfg(test)]
mod tests;
