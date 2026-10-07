//! FLS for the thread contexts of the no-fiber profile. TLS retains its TEB
//! owner in sync; FLS has independent indices and guest cleanup callbacks.
use std::collections::{BTreeMap, VecDeque};

use adv_abi::{GuestPtr, Registry, win32};
use adv_machine::{CtxId, Env, Outcome, outcome::DeferredContinuation};

use crate::util::{err, set_last_error};

// The installed SDK winnt.h FLS_MAXIMUM_AVAILABLE. Fiber creation/switching
// remain unavailable, so each existing guest thread has one FLS value bank.
const MAX_SLOTS: usize = 4080;

#[derive(Default)]
struct LocalStorage {
    slots: Vec<Option<Slot>>,
    next_generation: u64,
    values: BTreeMap<CtxId, BTreeMap<u32, u32>>,
}

struct Slot {
    generation: u64,
    callback: u32,
    freeing: bool,
}

#[win32(kernel32, name = "FlsAlloc", level = implement)]
fn alloc(env: &mut Env<'_>, callback: GuestPtr<()>) -> u32 {
    let storage = env.ext.get_or_default::<LocalStorage>();
    let index = storage
        .slots
        .iter()
        .position(Option::is_none)
        .unwrap_or(storage.slots.len());
    if index == MAX_SLOTS {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
        return u32::MAX;
    }
    storage.next_generation += 1;
    let slot = Some(Slot {
        generation: storage.next_generation,
        callback: callback.addr(),
        freeing: false,
    });
    if index == storage.slots.len() {
        storage.slots.push(slot);
    } else {
        storage.slots[index] = slot;
    }
    index as u32
}

#[win32(kernel32, name = "FlsGetValue", level = implement)]
fn get(env: &mut Env<'_>, index: u32) -> GuestPtr<()> {
    let storage = env.ext.get_or_default::<LocalStorage>();
    if storage
        .slots
        .get(index as usize)
        .is_none_or(Option::is_none)
    {
        set_last_error(env, err::INVALID_PARAMETER);
        return GuestPtr::new(0);
    }
    let value = storage
        .values
        .get(&env.context)
        .and_then(|values| values.get(&index))
        .copied()
        .unwrap_or(0);
    // A null value is a successful read; callers distinguish it from failure
    // with LastError, just as for TlsGetValue.
    set_last_error(env, 0);
    GuestPtr::new(value)
}

#[win32(kernel32, name = "FlsSetValue", level = implement)]
fn set(env: &mut Env<'_>, index: u32, value: GuestPtr<()>) -> i32 {
    let storage = env.ext.get_or_default::<LocalStorage>();
    if storage
        .slots
        .get(index as usize)
        .is_none_or(Option::is_none)
    {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    let values = storage.values.entry(env.context).or_default();
    if value.addr() == 0 {
        values.remove(&index);
    } else {
        values.insert(index, value.addr());
    }
    1
}

fn run_free(
    env: &mut Env<'_>,
    index: u32,
    generation: u64,
    callback: u32,
    mut values: VecDeque<u32>,
) -> Outcome {
    if callback != 0 {
        if values.is_empty() {
            // A callback may assign a new value. Windows processes that value
            // again during FlsFree; an endlessly rearming callback remains
            // guest execution subject to the normal watchdog and scheduling.
            values = env
                .ext
                .get_mut::<LocalStorage>()
                .values
                .values_mut()
                .filter_map(|values| values.remove(&index))
                .collect();
        }
        if let Some(value) = values.pop_front() {
            return Outcome::call(callback, vec![value], move |env, _| {
                run_free(env, index, generation, callback, values)
            });
        }
    }
    let storage = env.ext.get_mut::<LocalStorage>();
    if storage.slots[index as usize]
        .as_ref()
        .is_some_and(|slot| slot.generation == generation)
    {
        for values in storage.values.values_mut() {
            values.remove(&index);
        }
        storage.slots[index as usize] = None;
    }
    Outcome::Return(1)
}

#[win32(kernel32, name = "FlsFree", level = implement)]
fn free(env: &mut Env<'_>, index: u32) -> Outcome {
    let storage = env.ext.get_or_default::<LocalStorage>();
    let Some(slot) = storage
        .slots
        .get_mut(index as usize)
        .and_then(Option::as_mut)
    else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    if slot.freeing {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    slot.freeing = true;
    let (generation, callback) = (slot.generation, slot.callback);
    // Clear all banks before any guest call. FlsFree invokes these callbacks
    // on the freeing context, including values owned by another live thread.
    let values = storage
        .values
        .values_mut()
        .filter_map(|values| values.remove(&index))
        .collect();
    run_free(env, index, generation, callback, values)
}

fn run_detach(
    env: &mut Env<'_>,
    mut slots: VecDeque<(u32, u64)>,
    then: DeferredContinuation,
) -> Outcome {
    while let Some((index, generation)) = slots.pop_front() {
        let storage = env.ext.get_mut::<LocalStorage>();
        let Some(slot) = storage.slots[index as usize]
            .as_ref()
            .filter(|slot| slot.generation == generation && !slot.freeing)
        else {
            continue;
        };
        let value = storage
            .values
            .get(&env.context)
            .and_then(|values| values.get(&index))
            .copied()
            .unwrap_or(0);
        if value == 0 || slot.callback == 0 {
            continue;
        }
        // The exiting thread can still read its value inside the callback.
        // Consume it afterwards, even if that callback assigns a new value.
        return Outcome::call(slot.callback, vec![value], move |env, _| {
            let storage = env.ext.get_mut::<LocalStorage>();
            if storage.slots[index as usize]
                .as_ref()
                .is_some_and(|slot| slot.generation == generation)
                && let Some(values) = storage.values.get_mut(&env.context)
            {
                values.remove(&index);
            }
            run_detach(env, slots, then)
        });
    }
    env.ext
        .get_mut::<LocalStorage>()
        .values
        .remove(&env.context);
    then(env)
}

/// Compose orderly thread/process shutdown with guest FLS callbacks. Abrupt
/// TerminateThread/TerminateProcess paths deliberately do not call this.
pub(crate) fn detach_fls(env: &mut Env<'_>, then: DeferredContinuation) -> Outcome {
    let Some(storage) = env.ext.try_get::<LocalStorage>() else {
        return then(env);
    };
    let slots = storage
        .values
        .get(&env.context)
        .into_iter()
        .flat_map(|values| values.keys())
        .filter_map(|&index| {
            storage.slots[index as usize]
                .as_ref()
                .map(|slot| (index, slot.generation))
        })
        .collect();
    run_detach(env, slots, then)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(alloc_ABI);
    registry.register(get_ABI);
    registry.register(set_ABI);
    registry.register(free_ABI);
}
