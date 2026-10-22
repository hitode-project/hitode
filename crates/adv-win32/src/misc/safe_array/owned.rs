//! The allocation index owns heap blocks; bounds and elements stay in guest memory.
use super::*;
use adv_abi::types::{SAFEARRAY, SAFEARRAYBOUND};
use adv_machine::Outcome;
use adv_os::Heaps;
use std::collections::BTreeMap;
mod lifecycle;
pub(in crate::misc) use lifecycle::{clone_then, destroy_then};
const BOUNDS: u32 = core::mem::offset_of!(SAFEARRAY, rgsabound) as u32;
const INVALID: u32 = E_INVALIDARG as u32;
const MEMORY: u32 = 0x8007000e;
const BAD_INDEX: u32 = 0x8002000b;
type Done<T> = Box<dyn FnOnce(&mut Env<'_>, Result<T, u32>) -> Outcome>;
#[derive(Clone, Copy)]
struct Allocation {
    base: u32,
    data: u32,
    bytes: u32,
    kind: u16,
}
#[derive(Default)]
struct Arrays(BTreeMap<u32, Allocation>);
struct View {
    bounds: Vec<SAFEARRAYBOUND>,
    data: u32,
    element: u32,
    bytes: u32,
    count: u32,
}
fn layout(kind: u16) -> Option<(u32, u16)> {
    Some(match kind {
        16 | 17 => (1, 0),
        2 | 11 | 18 => (2, 0),
        3 | 4 | 10 | 19 | 22 | 23 => (4, 0),
        5 | 6 | 7 | 20 | 21 => (8, 0),
        14 => (16, 0),
        8 => (4, 0x100),
        9 => (4, 0x400),
        13 => (4, 0x200),
        12 => (16, 0x800),
        _ => return None,
    })
}
fn capability(env: &mut Env<'_>, reason: &str) {
    let binding = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", binding.module, binding.name);
    adv_abi::warn_unavailable(env, &api, reason);
}
fn allocation(env: &Env<'_>, array: u32) -> Result<Allocation, u32> {
    env.ext
        .try_get::<Arrays>()
        .and_then(|a| a.0.get(&array))
        .copied()
        .ok_or(INVALID)
}
fn view(env: &Env<'_>, array: u32) -> Result<View, u32> {
    check_range(env.cpu, array, BOUNDS, Prot::READ).map_err(|_| INVALID)?;
    let dimensions = env.cpu.read_u16(array).map_err(|_| INVALID)? as u32;
    if dimensions == 0 {
        return Err(INVALID);
    }
    check_range(env.cpu, array, BOUNDS + dimensions * 8, Prot::READ).map_err(|_| INVALID)?;
    let mut bounds = Vec::with_capacity(dimensions as usize);
    for i in 0..dimensions {
        bounds.push(
            GuestPtr::<SAFEARRAYBOUND>::new(array + BOUNDS + i * 8)
                .read(env.cpu)
                .map_err(|_| INVALID)?,
        );
    }
    let count = if bounds.iter().any(|b| b.cElements == 0) {
        0
    } else {
        bounds
            .iter()
            .try_fold(1u32, |n, b| n.checked_mul(b.cElements))
            .ok_or(INVALID)?
    };
    let element = env.cpu.read_u32(array + 4).map_err(|_| INVALID)?;
    let bytes = count.checked_mul(element).ok_or(INVALID)?;
    let data = env.cpu.read_u32(array + DATA).map_err(|_| INVALID)?;
    Ok(View {
        bounds,
        data,
        element,
        bytes,
        count,
    })
}
fn checked_owned(env: &mut Env<'_>, array: u32) -> Result<(View, Allocation), u32> {
    let value = view(env, array)?;
    let owned = allocation(env, array).inspect_err(|_| {
        capability(env, "foreign array allocation ownership is unavailable");
    })?;
    if value.data != owned.data
        || value.bytes > owned.bytes
        || layout(owned.kind).is_none_or(|(size, _)| size != value.element)
    {
        return Err(INVALID);
    }
    if value.bytes != 0 {
        check_range(env.cpu, value.data, value.bytes, Prot::RW).map_err(|_| INVALID)?;
    }
    Ok((value, owned))
}
fn alloc(env: &mut Env<'_>, kind: u16, bounds: &[SAFEARRAYBOUND]) -> u32 {
    let Some((element, features)) = layout(kind) else {
        return 0;
    };
    if bounds.is_empty() || bounds.len() > u16::MAX as usize {
        return 0;
    }
    let count = if bounds.iter().any(|b| b.cElements == 0) {
        Some(0)
    } else {
        bounds
            .iter()
            .try_fold(1u32, |n, b| n.checked_mul(b.cElements))
    };
    let Some(bytes) = count.and_then(|n| n.checked_mul(element)) else {
        return 0;
    };
    let descriptor_bytes = 16 + BOUNDS + bounds.len() as u32 * 8;
    let Some(base) =
        env.ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, descriptor_bytes, true)
    else {
        return 0;
    };
    let data = if bytes == 0 {
        0
    } else {
        let Some(data) = env
            .ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, bytes, true)
        else {
            env.ext
                .get_mut::<Heaps>()
                .free(env.cpu, &mut env.sys.vm, 0, base);
            return 0;
        };
        data
    };
    let array = base + 16;
    let descriptor = SAFEARRAY {
        cDims: bounds.len() as u16,
        fFeatures: 0x80 | features,
        cbElements: element,
        cLocks: 0,
        pvData: data,
        rgsabound: [bounds[bounds.len() - 1]],
    };
    let result = (|| {
        GuestPtr::new(array).write(env.cpu, descriptor).ok()?;
        env.cpu.write_u32(array - 4, u32::from(kind)).ok()?;
        // Automation's descriptor stores the last logical dimension first.
        for (i, bound) in bounds.iter().rev().enumerate() {
            GuestPtr::new(array + BOUNDS + i as u32 * 8)
                .write(env.cpu, *bound)
                .ok()?;
        }
        Some(())
    })();
    if result.is_none() {
        if data != 0 {
            env.ext
                .get_mut::<Heaps>()
                .free(env.cpu, &mut env.sys.vm, 0, data);
        }
        env.ext
            .get_mut::<Heaps>()
            .free(env.cpu, &mut env.sys.vm, 0, base);
        return 0;
    }
    env.ext.get_or_default::<Arrays>().0.insert(
        array,
        Allocation {
            base,
            data,
            bytes,
            kind,
        },
    );
    array
}
fn retire(env: &mut Env<'_>, array: u32) {
    if let Some(allocation) = env.ext.get_or_default::<Arrays>().0.remove(&array) {
        let heaps = env.ext.get_mut::<Heaps>();
        if allocation.data != 0 {
            heaps.free(env.cpu, &mut env.sys.vm, 0, allocation.data);
        }
        heaps.free(env.cpu, &mut env.sys.vm, 0, allocation.base);
    }
}
#[win32(oleaut32,name="SafeArrayCreate",level=state)]
fn create(
    env: &mut Env<'_>,
    kind: u32,
    dimensions: u32,
    bounds: GuestPtr<SAFEARRAYBOUND>,
) -> GuestPtr<SAFEARRAY> {
    if dimensions == 0
        || dimensions > u16::MAX as u32
        || check_range(env.cpu, bounds.addr(), dimensions * 8, Prot::READ).is_err()
    {
        return GuestPtr::new(0);
    }
    let mut items = Vec::with_capacity(dimensions as usize);
    for i in 0..dimensions {
        match GuestPtr::new(bounds.addr() + i * 8).read(env.cpu) {
            Ok(bound) => items.push(bound),
            Err(_) => return GuestPtr::new(0),
        }
    }
    GuestPtr::new(alloc(env, kind as u16, &items))
}
fn bound(env: &mut Env<'_>, upper: bool) -> Outcome {
    let value = match view(env, env.arg(0)) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    let dimension = env.arg(1);
    if dimension == 0 || dimension > value.bounds.len() as u32 {
        return Outcome::Return(BAD_INDEX);
    }
    if check_range(env.cpu, env.arg(2), 4, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let bound = value.bounds[value.bounds.len() - dimension as usize];
    let result = if upper {
        bound
            .lLbound
            .wrapping_add(bound.cElements as i32)
            .wrapping_sub(1)
    } else {
        bound.lLbound
    };
    let _ = env.cpu.write_u32(env.arg(2), result as u32);
    Outcome::Return(0)
}
fn pointer(env: &mut Env<'_>) -> Outcome {
    let value = match view(env, env.arg(0)) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    if check_range(
        env.cpu,
        env.arg(1),
        value.bounds.len() as u32 * 4,
        Prot::READ,
    )
    .is_err()
        || check_range(env.cpu, env.arg(2), 4, Prot::WRITE).is_err()
    {
        return Outcome::Return(INVALID);
    }
    let mut offset = 0u32;
    let mut stride = value.element;
    for (i, bound) in value.bounds.iter().rev().enumerate() {
        let index = env.cpu.read_u32(env.arg(1) + i as u32 * 4).unwrap() as i32;
        let distance = i64::from(index) - i64::from(bound.lLbound);
        if distance < 0 || distance >= i64::from(bound.cElements) {
            return Outcome::Return(BAD_INDEX);
        }
        let Some(next) = (distance as u32)
            .checked_mul(stride)
            .and_then(|n| offset.checked_add(n))
        else {
            return Outcome::Return(INVALID);
        };
        offset = next;
        let Some(next) = stride.checked_mul(bound.cElements) else {
            return Outcome::Return(INVALID);
        };
        stride = next;
    }
    let Some(pointer) = value.data.checked_add(offset) else {
        return Outcome::Return(INVALID);
    };
    let _ = env.cpu.write_u32(env.arg(2), pointer);
    Outcome::Return(0)
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
    crate::stdcall(registry, "oleaut32.dll", "SafeArrayGetLBound", 3, |env| {
        bound(env, false)
    });
    crate::stdcall(registry, "oleaut32.dll", "SafeArrayGetUBound", 3, |env| {
        bound(env, true)
    });
    crate::stdcall(registry, "oleaut32.dll", "SafeArrayPtrOfIndex", 3, pointer);
}

#[cfg(test)]
mod tests;
