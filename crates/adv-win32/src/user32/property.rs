//! Window properties share global atom identities and the live USER owner.
use adv_abi::{AnsiStr, Env, GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::Prot;
use adv_machine::Outcome;

use super::{
    user,
    window::{temp_alloc, temp_free},
};
use crate::{
    Win32,
    kernel32::atom::{self, Name},
    util::set_last_error,
};

fn fail(env: &mut Env<'_>, error: u32, value: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(value)
}

fn key(env: &mut Env<'_>, pointer: u32, wide: bool, add: bool) -> Result<u16, u32> {
    if pointer < 0x10000 {
        return Ok(pointer as u16);
    }
    match atom::name(env, pointer, wide)? {
        Name::Integer(atom) => Ok(atom),
        Name::String(units) => {
            // Native SetPropA first finds an existing global atom, whereas W
            // adds a reference even when replacing the same named property.
            // RemoveProp does not delete either kind of atom reference.
            if add
                && !wide
                && let Ok(id) = atom::resolve(env, units.clone(), false)
            {
                return Ok(id);
            }
            atom::resolve(env, units, add)
        }
    }
}

fn property(env: &mut Env<'_>, wide: bool, operation: u8) -> Outcome {
    let (hwnd, pointer) = (env.arg(0), env.arg(1));
    // RemoveProp resolves the name first; native Get/Set validate HWND first.
    if operation != 2 && !user(env).windows.contains_key(&env.sys.objects, &hwnd) {
        return fail(env, 1400, 0);
    }
    let atom = match key(env, pointer, wide, operation == 1) {
        Ok(atom) => atom,
        Err(error) => return fail(env, error, 0),
    };
    if operation == 1 && atom == 0 {
        return fail(env, 87, 0);
    }
    let value = if operation == 1 { env.arg(2) } else { 0 };
    let Some(window) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    else {
        return fail(env, 1400, 0);
    };
    let properties = &mut window.control.properties;
    let index = properties.iter().position(|&(id, _, _)| id == atom);
    match operation {
        1 => {
            if let Some(index) = index {
                properties[index].1 = value;
            } else {
                if properties.try_reserve(1).is_err() {
                    return fail(env, 8, 0);
                }
                properties.push((atom, value, pointer >= 0x10000));
            }
            Outcome::Return(1)
        }
        2 => Outcome::Return(index.map_or(0, |index| properties.remove(index).1)),
        _ => Outcome::Return(index.map_or(0, |index| properties[index].1)),
    }
}

struct Enumeration {
    hwnd: u32,
    callback: u32,
    parameter: Option<u32>,
    wide: bool,
    properties: std::vec::IntoIter<(u16, u32, bool)>,
    last: u32,
}

fn next(env: &mut Env<'_>, mut enumeration: Enumeration) -> Outcome {
    let Some((id, data, _)) = enumeration.properties.next() else {
        return Outcome::Return(enumeration.last);
    };
    let mut allocation = 0;
    let name = if id < 0xc000 {
        u32::from(id)
    } else {
        let Some(units) = atom::spelling(env, id) else {
            return fail(env, 6, u32::MAX);
        };
        let mut bytes: Vec<u8> = if enumeration.wide {
            units.into_iter().flat_map(u16::to_le_bytes).collect()
        } else {
            env.ext.get::<Win32>().codec.from_utf16(0, &units).0
        };
        bytes.resize(bytes.len() + if enumeration.wide { 2 } else { 1 }, 0);
        allocation = temp_alloc(env, bytes.len() as u32);
        if allocation == 0 {
            return fail(env, 8, u32::MAX);
        }
        if let Err(error) = env.cpu.write(allocation, &bytes) {
            temp_free(env, allocation);
            return Outcome::Fail(error.to_string());
        }
        allocation
    };
    let mut args = vec![enumeration.hwnd, name, data];
    if let Some(parameter) = enumeration.parameter {
        args.push(parameter);
    }
    Outcome::call(enumeration.callback, args, move |env, result| {
        if allocation != 0 {
            temp_free(env, allocation);
        }
        enumeration.last = result.value();
        if enumeration.last == 0 {
            Outcome::Return(0)
        } else {
            next(env, enumeration)
        }
    })
}

pub(super) fn retire_window(env: &mut Env<'_>, hwnd: u32) {
    let properties = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
        .map(|w| std::mem::take(&mut w.control.properties))
        .unwrap_or_default();
    for (id, _, named) in properties {
        // Native window destruction releases one string-property atom reference;
        // explicitly supplied atoms remain the application's responsibility.
        if named {
            let _ = atom::release(env, id);
        }
    }
}

fn enumerate(env: &mut Env<'_>, wide: bool, extended: bool) -> Outcome {
    let (hwnd, callback) = (env.arg(0), env.arg(1));
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return fail(env, 1400, u32::MAX);
    };
    let properties = window.control.properties.clone();
    if properties.is_empty() {
        return Outcome::Return(u32::MAX);
    }
    if callback == 0 || !env.cpu.accessible(callback, 1, Prot::EXEC) {
        return fail(env, 87, u32::MAX);
    }
    next(
        env,
        Enumeration {
            hwnd,
            callback,
            parameter: extended.then(|| env.arg(2)),
            wide,
            properties: properties.into_iter(),
            last: u32::MAX,
        },
    )
}

#[win32(user32, name = "GetPropA", level = state)]
fn get_a(env: &mut Env<'_>, _window: Handle, _name: AnsiStr) -> Outcome {
    property(env, false, 0)
}
#[win32(user32, name = "GetPropW", level = state)]
fn get_w(env: &mut Env<'_>, _window: Handle, _name: WideStr) -> Outcome {
    property(env, true, 0)
}
#[win32(user32, name = "SetPropA", level = state)]
fn set_a(env: &mut Env<'_>, _window: Handle, _name: AnsiStr, _value: Handle) -> Outcome {
    property(env, false, 1)
}
#[win32(user32, name = "SetPropW", level = state)]
fn set_w(env: &mut Env<'_>, _window: Handle, _name: WideStr, _value: Handle) -> Outcome {
    property(env, true, 1)
}
#[win32(user32, name = "RemovePropA", level = state)]
fn remove_a(env: &mut Env<'_>, _window: Handle, _name: AnsiStr) -> Outcome {
    property(env, false, 2)
}
#[win32(user32, name = "RemovePropW", level = state)]
fn remove_w(env: &mut Env<'_>, _window: Handle, _name: WideStr) -> Outcome {
    property(env, true, 2)
}
#[win32(user32, name = "EnumPropsA", level = state)]
fn enum_a(env: &mut Env<'_>, _window: Handle, _callback: GuestPtr<()>) -> Outcome {
    enumerate(env, false, false)
}
#[win32(user32, name = "EnumPropsW", level = state)]
fn enum_w(env: &mut Env<'_>, _window: Handle, _callback: GuestPtr<()>) -> Outcome {
    enumerate(env, true, false)
}
#[win32(user32, name = "EnumPropsExA", level = state)]
fn enum_ex_a(
    env: &mut Env<'_>,
    _window: Handle,
    _callback: GuestPtr<()>,
    _parameter: u32,
) -> Outcome {
    enumerate(env, false, true)
}
#[win32(user32, name = "EnumPropsExW", level = state)]
fn enum_ex_w(
    env: &mut Env<'_>,
    _window: Handle,
    _callback: GuestPtr<()>,
    _parameter: u32,
) -> Outcome {
    enumerate(env, true, true)
}

pub(super) fn register(registry: &mut Registry) {
    for descriptor in [
        get_a_ABI,
        get_w_ABI,
        set_a_ABI,
        set_w_ABI,
        remove_a_ABI,
        remove_w_ABI,
        enum_a_ABI,
        enum_w_ABI,
        enum_ex_a_ABI,
        enum_ex_w_ABI,
    ] {
        registry.register(descriptor);
    }
}
