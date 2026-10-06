//! Accelerator tables are typed USER objects, shared by resource and dynamic APIs.
//! Translation uses the queue's keyboard state and the ordinary synchronous send path.
use std::collections::BTreeMap;

use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Namespace, Objects, Outcome};
use adv_pe::ResId;

use super::{message, user};
use crate::util::set_last_error;

const FVIRTKEY: u8 = 1;
const FSHIFT: u8 = 4;
const FCONTROL: u8 = 8;
const FALT: u8 = 16;
const INVALID_ACCEL: u32 = 1403;
const MAX_ENTRIES: usize = 32767;

#[derive(Clone, Copy)]
struct Entry {
    flags: u8,
    key: u16,
    command: u16,
}

struct Table {
    entries: Vec<Entry>,
    resource: Option<(u32, u32)>,
    references: u32,
}

#[derive(Default)]
struct Accelerators {
    namespace: Option<Namespace>,
    // This index contains aliases, never a second owner of the ACCEL payload.
    resources: BTreeMap<(u32, u32), u32>,
}

impl Accelerators {
    fn get<'a>(&self, objects: &'a Objects, handle: u32) -> Option<&'a Table> {
        objects.owned(objects.lookup_alias::<Table>(self.namespace?, handle)?)
    }

    fn insert(&mut self, objects: &mut Objects, table: Table) -> Option<u32> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x0800_0004, 0x08ff_fffc);
        }
        let namespace = self.namespace?;
        if objects.alias_count(namespace) >= 16384 {
            return None;
        }
        let resource = table.resource;
        let handle = objects.insert_host_in(namespace, table)?;
        if let Some(resource) = resource {
            self.resources.insert(resource, handle);
        }
        Some(handle)
    }
}

fn error(env: &mut Env<'_>, code: u32) -> u32 {
    set_last_error(env, code);
    0
}

fn ansi_key(env: &Env<'_>, key: u16) -> u16 {
    // CreateAcceleratorTableA converts one ANSI byte, even though ACCEL.key is
    // a WORD. A lead byte without its trail maps to zero, as native USER does.
    let (units, invalid) = env
        .ext
        .get::<crate::Win32>()
        .codec
        .to_utf16_checked(0, &[key as u8]);
    if invalid {
        0
    } else {
        units.first().copied().unwrap_or(0)
    }
}

fn output_key(env: &Env<'_>, key: u16) -> u16 {
    let bytes = env.ext.get::<crate::Win32>().codec.from_utf16(0, &[key]).0;
    match bytes.as_slice() {
        [first, second, ..] => u16::from_le_bytes([*first, *second]),
        [first] => u16::from(*first),
        [] => 0,
    }
}

fn ansi_message_key(env: &Env<'_>, key: u16, system_char: bool) -> u16 {
    // Native TranslateAcceleratorA marshals packed WM_CHAR bytes in display
    // order, while WM_SYSCHAR uses the little-endian WORD order also emitted by
    // CopyAcceleratorTableA. Neither path has CreateAcceleratorTableA's truncation.
    let [low, high] = key.to_le_bytes();
    let bytes = if high == 0 {
        vec![low]
    } else if system_char {
        vec![low, high]
    } else {
        vec![high, low]
    };
    let (units, invalid) = env
        .ext
        .get::<crate::Win32>()
        .codec
        .to_utf16_checked(0, &bytes);
    if invalid {
        0
    } else {
        units.first().copied().unwrap_or(0)
    }
}

fn create(env: &mut Env<'_>, pointer: u32, count: i32, wide: bool) -> u32 {
    if !(1..=MAX_ENTRIES as i32).contains(&count) {
        return error(env, 87);
    }
    let length = count as u32 * 6;
    // USER returns NULL without altering LastError for this documented-invalid
    // NULL array, unlike its explicit invalid-count rejection.
    if pointer == 0 {
        return 0;
    }
    if check_range(env.cpu, pointer, length, Prot::READ).is_err() {
        return error(env, 87);
    }
    let mut bytes = vec![0; length as usize];
    if env.cpu.read(pointer, &mut bytes).is_err() {
        return error(env, 87);
    }
    let entries = bytes
        .chunks_exact(6)
        .map(|bytes| {
            let flags = bytes[0] & 0x7f;
            let key = u16::from_le_bytes([bytes[2], bytes[3]]);
            Entry {
                flags,
                key: if !wide && flags & FVIRTKEY == 0 {
                    ansi_key(env, key)
                } else {
                    key
                },
                command: u16::from_le_bytes([bytes[4], bytes[5]]),
            }
        })
        .collect();
    let handle = env.ext.get_or_default::<Accelerators>().insert(
        &mut env.sys.objects,
        Table {
            entries,
            resource: None,
            references: 1,
        },
    );
    handle.unwrap_or_else(|| error(env, 8))
}

fn copy(env: &mut Env<'_>, handle: u32, pointer: u32, count: i32, wide: bool) -> i32 {
    let Some(table) = env
        .ext
        .try_get::<Accelerators>()
        .and_then(|state| state.get(&env.sys.objects, handle))
    else {
        return error(env, INVALID_ACCEL) as i32;
    };
    if pointer == 0 {
        return table.entries.len() as i32;
    }
    if count <= 0 {
        return 0;
    }
    let count = (count as usize).min(table.entries.len());
    if check_range(env.cpu, pointer, count as u32 * 6, Prot::WRITE).is_err() {
        return error(env, 87) as i32;
    }
    let mut bytes = Vec::with_capacity(count * 6);
    for entry in table.entries.iter().take(count) {
        let key = if !wide && entry.flags & FVIRTKEY == 0 {
            output_key(env, entry.key)
        } else {
            entry.key
        };
        bytes.extend_from_slice(&[entry.flags, 0]);
        bytes.extend_from_slice(&key.to_le_bytes());
        bytes.extend_from_slice(&entry.command.to_le_bytes());
    }
    if env.cpu.write(pointer, &bytes).is_err() {
        return error(env, 87) as i32;
    }
    count as i32
}

fn resource_error(env: &Env<'_>, instance: u32) -> u32 {
    let instance = if instance == 0 {
        env.cpu
            .read_u32(adv_machine::layout::PEB_ADDR + adv_machine::layout::peb::IMAGE_BASE)
            .unwrap_or(0)
    } else {
        instance
    };
    let modules = adv_os::modules(env.ext);
    modules.by_handle(instance).map_or(1812, |index| {
        let module = modules.get(index);
        match &module.resources {
            _ if module.data_file.is_some() => 1814,
            None => 1812,
            Some(resources) if resources.get(&ResId::Id(9)).is_none() => 1813,
            Some(_) => 1814,
        }
    })
}

fn load(env: &mut Env<'_>, instance: u32, pointer: u32, wide: bool) -> u32 {
    if pointer >= 0x10000 {
        let valid = if wide {
            WideStr(GuestPtr::new(pointer)).read(env.cpu, 32768).is_ok()
        } else {
            AnsiStr(GuestPtr::new(pointer)).read(env.cpu, 32768).is_ok()
        };
        if !valid {
            return error(env, 87);
        }
    }
    let name = crate::kernel32::res_id(crate::kernel32::int_or_str(env, pointer, wide));
    let Some((base, length)) =
        crate::kernel32::find_resource_data(env, instance, &ResId::Id(9), &name)
    else {
        let code = resource_error(env, instance);
        return error(env, code);
    };
    let resource = (base, length);
    let state = env.ext.get_or_default::<Accelerators>();
    if let Some(&handle) = state.resources.get(&resource) {
        let identity = env
            .sys
            .objects
            .lookup_alias::<Table>(state.namespace.unwrap(), handle);
        if let Some(table) = identity.and_then(|id| env.sys.objects.owned_mut(id)) {
            let Some(references) = table.references.checked_add(1) else {
                return error(env, 8);
            };
            table.references = references;
            return handle;
        }
    }
    if length < 8 || length % 8 != 0 || length as usize / 8 > MAX_ENTRIES {
        return error(env, 13);
    }
    if check_range(env.cpu, base, length, Prot::READ).is_err() {
        return error(env, 13);
    }
    let mut bytes = vec![0; length as usize];
    if env.cpu.read(base, &mut bytes).is_err() {
        return error(env, 13);
    }
    let entries = bytes
        .chunks_exact(8)
        .map(|bytes| Entry {
            flags: bytes[0] & 0x7f,
            key: u16::from_le_bytes([bytes[2], bytes[3]]),
            command: u16::from_le_bytes([bytes[4], bytes[5]]),
        })
        .collect();
    env.ext
        .get_mut::<Accelerators>()
        .insert(
            &mut env.sys.objects,
            Table {
                entries,
                resource: Some(resource),
                references: 1,
            },
        )
        .unwrap_or_else(|| error(env, 8))
}

fn destroy(env: &mut Env<'_>, handle: u32) -> i32 {
    let state = env.ext.get_or_default::<Accelerators>();
    let Some(namespace) = state.namespace else {
        return error(env, INVALID_ACCEL) as i32;
    };
    let Some(identity) = env.sys.objects.lookup_alias::<Table>(namespace, handle) else {
        return error(env, INVALID_ACCEL) as i32;
    };
    let table = env.sys.objects.owned_mut(identity).unwrap();
    if table.references > 1 {
        table.references -= 1;
        return 0;
    }
    let resource = table.resource;
    env.sys.objects.close_alias(namespace, handle);
    if let Some(resource) = resource {
        state.resources.remove(&resource);
    }
    1
}

fn translate(env: &mut Env<'_>, hwnd: u32, handle: u32, pointer: u32, wide: bool) -> Outcome {
    let Some(table) = env
        .ext
        .try_get::<Accelerators>()
        .and_then(|state| state.get(&env.sys.objects, handle))
    else {
        return Outcome::Return(error(env, INVALID_ACCEL));
    };
    if check_range(env.cpu, pointer, 28, Prot::READ).is_err() {
        return Outcome::Return(error(env, 87));
    }
    let msg = env.cpu.read_u32(pointer + 4).unwrap_or(0);
    let mut key = env.cpu.read_u32(pointer + 8).unwrap_or(0);
    if !matches!(msg, 0x100 | 0x104 | 0x102 | 0x106) {
        return Outcome::Return(0);
    }
    if !wide && matches!(msg, 0x102 | 0x106) {
        key = u32::from(ansi_message_key(env, key as u16, msg == 0x106));
    }
    let modifiers = [(0x10, FSHIFT), (0x11, FCONTROL), (0x12, FALT)]
        .into_iter()
        .fold(0, |flags, (vk, flag)| {
            flags
                | if super::queued_key_state(env, vk) & 0x80 != 0 {
                    flag
                } else {
                    0
                }
        });
    let command = table
        .entries
        .iter()
        .find(|entry| {
            u32::from(entry.key) == key
                && if entry.flags & FVIRTKEY != 0 {
                    matches!(msg, 0x100 | 0x104)
                        && entry.flags & (FSHIFT | FCONTROL | FALT) == modifiers
                } else {
                    matches!(msg, 0x102 | 0x106) && entry.flags & FALT == modifiers & FALT
                }
        })
        .map(|entry| entry.command);
    let Some(command) = command else {
        return Outcome::Return(0);
    };
    if !user(env).is_window(&env.sys.objects, hwnd) {
        return Outcome::Return(error(env, 1400));
    }
    translate_command(env, hwnd, command)
}

fn translate_command(env: &mut Env<'_>, hwnd: u32, command: u16) -> Outcome {
    let Some(menu) = super::menu::accelerator_lookup(env, hwnd, command) else {
        return dispatch_command(env, hwnd, command, false);
    };
    let window = user(env).windows.get(&env.sys.objects, &hwnd).unwrap();
    let capture = user(env)
        .queues
        .get(&env.context)
        .is_some_and(|queue| queue.capture != 0);
    if (!menu.system && window.style & 0x20000000 != 0) || capture {
        return Outcome::Return(1);
    }
    message::send(env, hwnd, 0x116, menu.root, 0, move |env, _| {
        let Some(menu) = super::menu::accelerator_lookup(env, hwnd, command) else {
            return Outcome::Return(1);
        };
        if let Some((popup, position)) = menu.popup {
            message::send(
                env,
                hwnd,
                0x117,
                popup,
                u32::from(position),
                move |env, _| dispatch_menu_command(env, hwnd, command),
            )
        } else {
            dispatch_menu_command(env, hwnd, command)
        }
    })
}

fn dispatch_menu_command(env: &mut Env<'_>, hwnd: u32, command: u16) -> Outcome {
    let Some(menu) = super::menu::accelerator_lookup(env, hwnd, command) else {
        return Outcome::Return(1);
    };
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(1);
    };
    if !menu.enabled || !window.enabled() || (!menu.system && window.style & 0x20000000 != 0) {
        return Outcome::Return(1);
    }
    dispatch_command(env, hwnd, command, menu.system)
}

fn dispatch_command(env: &mut Env<'_>, hwnd: u32, command: u16, system: bool) -> Outcome {
    message::send(
        env,
        hwnd,
        if system { 0x112 } else { 0x111 },
        if system {
            u32::from(command)
        } else {
            0x10000 | u32::from(command)
        },
        if system { 0x10000 } else { 0 },
        |_, _| Outcome::Return(1),
    )
}

#[win32(user32, name = "CreateAcceleratorTableA", level = implement)]
fn create_a(env: &mut Env<'_>, entries: GuestPtr<u8>, count: i32) -> Handle {
    Handle(create(env, entries.addr(), count, false))
}
#[win32(user32, name = "CreateAcceleratorTableW", level = implement)]
fn create_w(env: &mut Env<'_>, entries: GuestPtr<u8>, count: i32) -> Handle {
    Handle(create(env, entries.addr(), count, true))
}
#[win32(user32, name = "CopyAcceleratorTableA", level = implement)]
fn copy_a(env: &mut Env<'_>, table: Handle, entries: GuestPtr<u8>, count: i32) -> i32 {
    copy(env, table.0, entries.addr(), count, false)
}
#[win32(user32, name = "CopyAcceleratorTableW", level = implement)]
fn copy_w(env: &mut Env<'_>, table: Handle, entries: GuestPtr<u8>, count: i32) -> i32 {
    copy(env, table.0, entries.addr(), count, true)
}
#[win32(user32, name = "LoadAcceleratorsA", level = implement)]
fn load_a(env: &mut Env<'_>, instance: Handle, name: AnsiStr) -> Handle {
    Handle(load(env, instance.0, name.0.addr(), false))
}
#[win32(user32, name = "LoadAcceleratorsW", level = implement)]
fn load_w(env: &mut Env<'_>, instance: Handle, name: WideStr) -> Handle {
    Handle(load(env, instance.0, name.0.addr(), true))
}
#[win32(user32, name = "DestroyAcceleratorTable", level = implement)]
fn destroy_table(env: &mut Env<'_>, table: Handle) -> i32 {
    destroy(env, table.0)
}
#[win32(user32, name = "TranslateAcceleratorA", level = implement)]
fn translate_a(env: &mut Env<'_>, hwnd: Handle, table: Handle, msg: GuestPtr<u8>) -> Outcome {
    translate(env, hwnd.0, table.0, msg.addr(), false)
}
#[win32(user32, name = "TranslateAcceleratorW", level = implement)]
fn translate_w(env: &mut Env<'_>, hwnd: Handle, table: Handle, msg: GuestPtr<u8>) -> Outcome {
    translate(env, hwnd.0, table.0, msg.addr(), true)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_a_ABI);
    registry.register(create_w_ABI);
    registry.register(copy_a_ABI);
    registry.register(copy_w_ABI);
    registry.register(load_a_ABI);
    registry.register(load_w_ABI);
    registry.register(destroy_table_ABI);
    registry.register(translate_a_ABI);
    registry.register(translate_w_ABI);
}
