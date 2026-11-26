//! Shared resources alias one backing allocation within an execution.
use super::*;
use crate::d3d11::{desc::Misc, object::Shared};
use adv_abi::types::SECURITY_ATTRIBUTES;
use adv_machine::{Object as KernelObject, Resume, Wait, WakeReason};
use std::sync::{Arc, Weak};

const READ: u32 = 0x80000000;
const WRITE: u32 = 1;
const WAIT_TIMEOUT: u32 = 258;

struct NtHandle {
    shared: Arc<Shared>,
    access: u32,
    name: Option<Vec<u16>>,
}

pub(super) fn legacy_handle(env: &mut Env<'_>, pointer: u32) -> Result<u32, Error> {
    let object = ops::object(env, table(env), pointer)?;
    let misc = object.resource().unwrap().misc();
    if !misc.shared() || misc.contains(Misc::SHARED_NTHANDLE) {
        return Err(dxgi::Error::InvalidCall.into());
    }
    Ok(share(env, pointer)?.handle)
}

fn share(env: &mut Env<'_>, pointer: u32) -> Result<Arc<Shared>, Error> {
    let table = table(env);
    let object = ops::object(env, table, pointer)?;
    if let Some(shared) = object.shared() {
        return Ok(shared.clone());
    }
    let resource = object.resource().unwrap();
    let device = device_state(env, object.device().unwrap())?;
    if device.null_device || !resource.misc().shared() {
        return Err(dxgi::Error::InvalidCall.into());
    }
    let adapter = dxgi::adapter_kind(env, device.adapter)?;
    let identity = table.key(&env.sys.objects, pointer).unwrap();
    let backing = gpu::backing(env, pointer, resource)?;
    let state = env.ext.get_mut::<State>();
    state.last_shared = state
        .last_shared
        .checked_add(1)
        .ok_or(ObjectError::Exhausted)?;
    let handle = state.last_shared;
    let shared = Arc::new(Shared {
        handle,
        identity,
        resource,
        adapter,
        backing,
        keyed: Default::default(),
    });
    if !resource.misc().contains(Misc::SHARED_NTHANDLE) {
        state.shared.insert(handle, Arc::downgrade(&shared));
    }
    table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .share(shared.clone());
    Ok(shared)
}

fn legacy(env: &Env<'_>, handle: u32) -> Option<Arc<Shared>> {
    env.ext
        .get::<State>()
        .shared
        .get(&handle)
        .and_then(Weak::upgrade)
}

pub(super) fn adapter(env: &Env<'_>, handle: u32) -> Option<Adapter> {
    env.sys
        .objects
        .host::<NtHandle>(handle)
        .map(|entry| entry.shared.adapter)
        .or_else(|| legacy(env, handle).map(|shared| shared.adapter))
}

pub(in crate::d3d11::glue) fn open_legacy(
    env: &mut Env<'_>,
    device: u32,
    handle: u32,
    iid: Iid,
) -> Result<u32, Error> {
    let shared = legacy(env, handle).ok_or(Error::InvalidArgument)?;
    open(env, device, shared, iid)
}

fn open(env: &mut Env<'_>, device: u32, shared: Arc<Shared>, iid: Iid) -> Result<u32, Error> {
    let adapter = dxgi::adapter_kind(env, device_state(env, device)?.adapter)?;
    if adapter != shared.adapter {
        return Err(Error::InvalidArgument);
    }
    let resource = attach_resource(env, device, shared.resource)?;
    let table = table(env);
    table
        .get_mut(&mut env.sys.objects, resource)
        .unwrap()
        .share(shared);
    let result = table.query(&mut env.sys.objects, resource, iid);
    let released = table.release(&mut env.sys.objects, resource);
    retire(env, released);
    Ok(result?)
}

fn name(env: &Env<'_>, address: u32) -> Result<Vec<u16>, Error> {
    let mut name = Vec::new();
    for byte in (0..520).step_by(2) {
        let ch: u16 = read(
            env,
            address.checked_add(byte).ok_or(Error::InvalidArgument)?,
        )?;
        if ch == 0 {
            return Ok(name);
        }
        name.push(ch);
    }
    Err(Error::InvalidArgument)
}

fn valid_access(access: u32) -> bool {
    access != 0 && access & !(READ | WRITE) == 0
}

pub(super) fn create_shared_handle(env: &mut Env<'_>) -> Outcome {
    let [resource, attributes, access, name_ptr, out] = std::array::from_fn(|i| env.arg(i as u32));
    call((|| {
        write(env, out, 0u32)?;
        if !valid_access(access) {
            return Err(Error::InvalidArgument);
        }
        let description = ops::object(env, table(env), resource)?.resource().unwrap();
        if !description.misc().contains(Misc::SHARED_NTHANDLE) {
            return Err(dxgi::Error::InvalidCall.into());
        }
        let inherit = if attributes == 0 {
            0
        } else {
            let attributes: SECURITY_ATTRIBUTES = read(env, attributes)?;
            u32::from(attributes.bInheritHandle != 0)
        };
        let name = (name_ptr != 0).then(|| name(env, name_ptr)).transpose()?;
        let shared = share(env, resource)?;
        let mut keyed = shared.keyed.lock().unwrap();
        if keyed.nt_created {
            return Err(dxgi::Error::InvalidCall.into());
        }
        if let Some(name) = &name
            && env
                .sys
                .objects
                .hosts::<NtHandle>()
                .any(|(_, entry)| entry.name.as_ref() == Some(name))
        {
            return Err(dxgi::Error::NameAlreadyExists.into());
        }
        let handle = env.sys.objects.insert_host(NtHandle {
            shared: shared.clone(),
            access,
            name,
        });
        if handle == 0 {
            return Err(ObjectError::Exhausted.into());
        }
        env.sys.objects.set_handle_flags(handle, 1, inherit);
        keyed.nt_created = true;
        write(env, out, handle)
    })())
}

pub(in crate::d3d11::glue) fn open_shared_nt(env: &mut Env<'_>) -> Outcome {
    let [device, handle, iid, out] = std::array::from_fn(|i| env.arg(i as u32));
    call((|| {
        write(env, out, 0u32)?;
        let shared = env
            .sys
            .objects
            .host::<NtHandle>(handle)
            .ok_or(Error::InvalidArgument)?
            .shared
            .clone();
        let iid = read(env, iid)?;
        let resource = open(env, device, shared, iid)?;
        write(env, out, resource)
    })())
}

pub(in crate::d3d11::glue) fn open_shared_by_name(env: &mut Env<'_>) -> Outcome {
    let [device, name_ptr, access, iid, out] = std::array::from_fn(|i| env.arg(i as u32));
    call((|| {
        write(env, out, 0u32)?;
        if !valid_access(access) {
            return Err(Error::InvalidArgument);
        }
        let name = name(env, name_ptr)?;
        let entry = env
            .sys
            .objects
            .hosts::<NtHandle>()
            .find_map(|(_, entry)| (entry.name.as_ref() == Some(&name)).then_some(entry))
            .ok_or(dxgi::Error::NotFound)?;
        if access & !entry.access != 0 {
            return Err(dxgi::Error::AccessDenied.into());
        }
        let shared = entry.shared.clone();
        let iid = read(env, iid)?;
        let resource = open(env, device, shared, iid)?;
        write(env, out, resource)
    })())
}

fn key(env: &Env<'_>) -> u64 {
    u64::from(env.arg(1)) | (u64::from(env.arg(2)) << 32)
}

pub(super) fn acquire_sync(env: &mut Env<'_>) -> Outcome {
    let (pointer, key, ms) = (env.arg(0), key(env), env.arg(3));
    let pointer = table(env).canonical(&env.sys.objects, pointer).unwrap();
    let deadline = (ms != u32::MAX).then(|| env.sys.clock.now_us() + u64::from(ms) * 1000);
    match share(env, pointer) {
        Ok(shared) => acquire(env, shared, pointer, key, deadline),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn acquire(
    env: &mut Env<'_>,
    shared: Arc<Shared>,
    pointer: u32,
    key: u64,
    deadline: Option<u64>,
) -> Outcome {
    let mut keyed = shared.keyed.lock().unwrap();
    if keyed.abandoned {
        return Outcome::Return(128);
    }
    if keyed.owner.is_none() && keyed.key == key {
        keyed.owner = Some(pointer);
        return Outcome::Return(0);
    }
    if deadline.is_some_and(|deadline| deadline <= env.sys.clock.now_us()) {
        return Outcome::Return(WAIT_TIMEOUT);
    }
    if keyed.event == 0 {
        keyed.event = env.sys.objects.insert(KernelObject::Event {
            manual_reset: true,
            signaled: false,
        });
        if keyed.event == 0 {
            return Outcome::Return(Error::Object(ObjectError::Exhausted).hresult());
        }
    }
    let handle = keyed.event;
    keyed.waiters += 1;
    drop(keyed);
    Outcome::block(Wait::Object { handle, deadline }, move |env, reason| {
        let mut keyed = shared.keyed.lock().unwrap();
        keyed.waiters -= 1;
        if keyed.waiters == 0 {
            env.sys.objects.close(std::mem::take(&mut keyed.event));
        }
        drop(keyed);
        if reason == Resume::Woke(WakeReason::Timeout) {
            Outcome::Return(WAIT_TIMEOUT)
        } else {
            acquire(env, shared, pointer, key, deadline)
        }
    })
}

pub(super) fn release_sync(env: &mut Env<'_>) -> Outcome {
    let (pointer, key) = (env.arg(0), key(env));
    let table = table(env);
    let pointer = table.canonical(&env.sys.objects, pointer).unwrap();
    let shared = table
        .get(&env.sys.objects, pointer)
        .unwrap()
        .shared()
        .cloned();
    let Some(shared) = shared else {
        return Outcome::Return(dxgi::Error::InvalidCall.hresult());
    };
    let mut keyed = shared.keyed.lock().unwrap();
    if keyed.owner != Some(pointer) {
        return Outcome::Return(dxgi::Error::InvalidCall.hresult());
    }
    if let Err(error) = gpu::flush_shared(env) {
        return Outcome::Return(error.hresult());
    }
    keyed.owner = None;
    keyed.key = key;
    let event = keyed.event;
    drop(keyed);
    if event != 0 {
        env.sys.set_event(event);
        env.sys.reset_event(event);
    }
    Outcome::Return(0)
}
