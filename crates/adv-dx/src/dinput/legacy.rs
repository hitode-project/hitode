//! Legacy DirectInput interfaces have their own SDK layouts over common COM owners.
use super::*;
use adv_machine::{Ext, Object, Objects};
use std::collections::VecDeque;

const IID_INPUT_A: Iid = [
    0x60, 0x13, 0x52, 0x89, 0x8a, 0xaa, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const IID_INPUT_W: Iid = [
    0x61, 0x13, 0x52, 0x89, 0x8a, 0xaa, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const IID_DEVICE_A: Iid = [
    0x80, 0xe6, 0x44, 0x59, 0x2e, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const IID_DEVICE_W: Iid = [
    0x81, 0xe6, 0x44, 0x59, 0x2e, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const IID_DEVICE2_A: Iid = [
    0x82, 0xe6, 0x44, 0x59, 0x2e, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const IID_DEVICE2_W: Iid = [
    0x83, 0xe6, 0x44, 0x59, 0x2e, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const KEYBOARD: Iid = [
    0x61, 0x2b, 0x1d, 0x6f, 0xa0, 0xd5, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const MOUSE: Iid = [
    0x60, 0x2b, 0x1d, 0x6f, 0xa0, 0xd5, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const KEY: Iid = [
    0x20, 0x82, 0x72, 0x55, 0x3c, 0xd3, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const BUTTON: Iid = [
    0xf0, 0x02, 0x6d, 0xa3, 0xf3, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const NOT_ACQUIRED: u32 = 0x8007000c;
const INPUT_LOST: u32 = 0x8007001e;
const ACQUIRED: u32 = 0x800700aa;

/// Read fresh process input; the device frontend does not own a second key state.
pub struct InputProvider(pub fn(&Ext, &Objects, u32) -> Option<InputSnapshot>);
pub struct InputSnapshot {
    pub keys: [u8; 256],
    pub cursor: (i32, i32),
    pub foreground: bool,
}
#[derive(Default)]
struct Events {
    wheel: i64,
    sequence: u32,
}
pub(super) struct Tables {
    input: [u32; 2],
    device: [u32; 2],
}
struct LegacyFactory;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyboard,
    Mouse,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Key(u8),
    Axis(u8),
    Button(u8),
    Missing(u32),
}
impl Source {
    fn size(self) -> u32 {
        match self {
            Self::Axis(_) => 4,
            Self::Missing(size) => size,
            _ => 1,
        }
    }
}
struct Format {
    size: u32,
    objects: Vec<(u32, Source)>,
}
struct Device {
    kind: Kind,
    acquired: bool,
    cooperative: Option<(u32, u32)>,
    format: Option<Format>,
    cursor: (i32, i32),
    event_cursor: (i32, i32),
    wheel: i64,
    buffer_size: usize,
    buffer: VecDeque<[u32; 4]>,
    overflow: bool,
    event: u32,
}
trait Cleanup {
    fn cleanup(self, env: &mut Env<'_>);
}
impl Cleanup for LegacyFactory {
    fn cleanup(self, _: &mut Env<'_>) {}
}
impl Cleanup for Device {
    fn cleanup(self, env: &mut Env<'_>) {
        if self.event != 0 {
            env.sys.objects.close(self.event);
        }
    }
}
fn unsupported(env: &mut Env<'_>, method: &str, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, &format!("dinput.dll!{method}"), reason);
    UNSUPPORTED
}
fn initialize_legacy(env: &mut Env<'_>) -> bool {
    if !initialize(env) {
        return false;
    }
    if env.ext.get::<State>().legacy.is_some() {
        return true;
    }
    let mut allocated = Vec::new();
    let mut build = |interface: &str, slots: u32| -> Option<u32> {
        let table = alloc(env, slots * 4)?;
        allocated.push(table);
        let signatures: Vec<_> = adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| s.module == interface)
            .collect();
        if signatures.len() != slots as usize {
            return None;
        }
        for signature in signatures {
            let slot = u32::from(signature.com_slot?);
            if slot >= slots {
                return None;
            }
            // Device2 inherits the base slots without changing their behavior.
            let module = if slot < 18 {
                interface.replace("IDirectInputDevice2", "IDirectInputDevice")
            } else {
                interface.to_owned()
            };
            let entry = env
                .sys
                .thunks
                .publish(
                    env.cpu,
                    "dinput.dll",
                    &format!("{module}::{}", signature.name),
                )
                .ok()?;
            env.cpu.write_u32(table + slot * 4, entry).ok()?;
        }
        Some(table)
    };
    let tables = (|| {
        Some(Tables {
            input: [build("IDirectInputA", 8)?, build("IDirectInputW", 8)?],
            device: [
                build("IDirectInputDevice2A", 27)?,
                build("IDirectInputDevice2W", 27)?,
            ],
        })
    })();
    match tables {
        Some(tables) => {
            env.ext.get_mut::<State>().legacy = Some(tables);
            true
        }
        None => {
            for pointer in allocated {
                free(env, pointer);
            }
            false
        }
    }
}
fn release_owned<T: 'static>(env: &mut Env<'_>, this: u32) -> (u32, Option<T>) {
    let Some(state) = env.ext.try_get::<State>() else {
        return (0, None);
    };
    let Some(released) = state.com.release::<T>(&mut env.sys.objects, this) else {
        return (0, None);
    };
    let value = released.destroyed.map(|destroyed| {
        for pointer in destroyed.interfaces {
            free(env, pointer);
        }
        destroyed.value
    });
    (released.references, value)
}
fn new_object<T: 'static>(env: &mut Env<'_>, wide: bool, device: bool, value: T) -> Option<u32> {
    if !initialize_legacy(env) {
        return None;
    }
    let first = alloc(env, 8)?;
    let Some(second) = alloc(env, 8) else {
        free(env, first);
        return None;
    };
    let state = env.ext.get::<State>();
    let com = state.com;
    let tables = state.legacy.as_ref().unwrap();
    let (tables, iids) = if device {
        (tables.device, [IID_DEVICE_A, IID_DEVICE_W])
    } else {
        (tables.input, [IID_INPUT_A, IID_INPUT_W])
    };
    let index = usize::from(wide);
    if env.cpu.write_u32(first, tables[index]).is_err()
        || env.cpu.write_u32(second, tables[1 - index]).is_err()
    {
        free(env, second);
        free(env, first);
        return None;
    }
    let Some(identity) = com.create(&mut env.sys.objects, first, iids[index], value) else {
        free(env, second);
        free(env, first);
        return None;
    };
    if !com.register_interface(&mut env.sys.objects, identity, iids[1 - index], second) {
        free(env, second);
        release_owned::<T>(env, first);
        return None;
    }
    if device {
        // Base and Device2 IIDs share the extended vtable and COM identity.
        let extended = [IID_DEVICE2_A, IID_DEVICE2_W];
        if !com.register_interface(&mut env.sys.objects, identity, extended[index], first)
            || !com.register_interface(&mut env.sys.objects, identity, extended[1 - index], second)
        {
            release_owned::<T>(env, first);
            return None;
        }
    }
    com.enable_guest(env, identity);
    Some(first)
}
fn create_legacy(
    env: &mut Env<'_>,
    instance: Handle,
    version: u32,
    output: GuestPtr<u32>,
    outer: GuestPtr<()>,
    wide: bool,
) -> u32 {
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let _ = output.write(env.cpu, 0);
    if instance.0 == 0 || version == 0 {
        return INVALID;
    }
    if version < 0x300 {
        return 0x8007047e;
    }
    if version > 0x700 {
        return 0x80070481;
    }
    if outer.addr() != 0 {
        return NO_AGGREGATION;
    }
    let Some(pointer) = new_object(env, wide, false, LegacyFactory) else {
        return OUT_OF_MEMORY;
    };
    let _ = output.write(env.cpu, pointer);
    0
}
#[win32(dinput, name="DirectInputCreateA", level=state)]
fn create_a(
    env: &mut Env<'_>,
    instance: Handle,
    version: u32,
    output: GuestPtr<u32>,
    outer: GuestPtr<()>,
) -> u32 {
    create_legacy(env, instance, version, output, outer, false)
}
#[win32(dinput, name="DirectInputCreateW", level=state)]
fn create_w(
    env: &mut Env<'_>,
    instance: Handle,
    version: u32,
    output: GuestPtr<u32>,
    outer: GuestPtr<()>,
) -> u32 {
    create_legacy(env, instance, version, output, outer, true)
}
fn enumerate_controllers(
    env: &mut Env<'_>,
    this: u32,
    kind: u32,
    callback: GuestPtr<()>,
    flags: u32,
    wide: bool,
) -> u32 {
    if !env.ext.try_get::<State>().is_some_and(|state| {
        state
            .com
            .get::<LegacyFactory>(&env.sys.objects, this)
            .is_some()
    }) || kind > 4
        || flags & !0x30101 != 0
        || adv_abi::guest::check_range(env.cpu, callback.addr(), 1, Prot::EXEC).is_err()
    {
        return INVALID;
    }
    // Legacy DIDEVTYPE_JOYSTICK is 4; the shared input profile has no controllers.
    // Existing keyboard/mouse devices must not be hidden by a false empty ALL set.
    if kind != 4 {
        return unsupported(
            env,
            if wide {
                "IDirectInputW::EnumDevices"
            } else {
                "IDirectInputA::EnumDevices"
            },
            "only the empty controller set is enumerable; keyboard/mouse enumeration is unavailable",
        );
    }
    0
}
#[win32(dinput, name="IDirectInputA::EnumDevices", level=state)]
fn enum_a(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: u32,
    callback: GuestPtr<()>,
    _context: GuestPtr<()>,
    flags: u32,
) -> u32 {
    enumerate_controllers(env, this.addr(), kind, callback, flags, false)
}
#[win32(dinput, name="IDirectInputW::EnumDevices", level=state)]
fn enum_w(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: u32,
    callback: GuestPtr<()>,
    _context: GuestPtr<()>,
    flags: u32,
) -> u32 {
    enumerate_controllers(env, this.addr(), kind, callback, flags, true)
}
fn query_owned<T: 'static>(
    env: &mut Env<'_>,
    this: u32,
    iid: GuestPtr<Iid>,
    output: GuestPtr<u32>,
) -> u32 {
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let _ = output.write(env.cpu, 0);
    if adv_abi::guest::check_range(env.cpu, iid.addr(), 16, Prot::READ).is_err() {
        return INVALID;
    }
    let Ok(iid) = iid.read(env.cpu) else {
        return INVALID;
    };
    let Some(state) = env.ext.try_get::<State>() else {
        return INVALID;
    };
    match state.com.query::<T>(&mut env.sys.objects, this, iid) {
        Ok(pointer) => {
            let _ = output.write(env.cpu, pointer);
            0
        }
        Err(QueryError::NoInterface) => NO_INTERFACE,
        Err(QueryError::ReferenceLimit) => OUT_OF_MEMORY,
        Err(QueryError::InvalidObject) => INVALID,
    }
}
fn device<'a>(env: &'a Env<'_>, this: u32) -> Option<&'a Device> {
    env.ext.try_get::<State>()?.com.get(&env.sys.objects, this)
}
fn device_mut<'a>(env: &'a mut Env<'_>, this: u32) -> Option<&'a mut Device> {
    env.ext
        .try_get::<State>()?
        .com
        .get_mut(&mut env.sys.objects, this)
}
fn snapshot(env: &Env<'_>, hwnd: u32) -> Option<InputSnapshot> {
    (env.ext.try_get::<InputProvider>()?.0)(env.ext, &env.sys.objects, hwnd)
}
fn create_device(
    env: &mut Env<'_>,
    this: u32,
    guid: GuestPtr<Iid>,
    output: GuestPtr<u32>,
    outer: GuestPtr<()>,
    wide: bool,
) -> u32 {
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let _ = output.write(env.cpu, 0);
    if !env
        .ext
        .try_get::<State>()
        .is_some_and(|s| s.com.get::<LegacyFactory>(&env.sys.objects, this).is_some())
    {
        return INVALID;
    }
    if outer.addr() != 0 {
        return NO_AGGREGATION;
    }
    if adv_abi::guest::check_range(env.cpu, guid.addr(), 16, Prot::READ).is_err() {
        return INVALID;
    }
    let Ok(guid) = guid.read(env.cpu) else {
        return INVALID;
    };
    let kind = match guid {
        KEYBOARD => Kind::Keyboard,
        MOUSE => Kind::Mouse,
        _ => return 0x80040154,
    };
    let value = Device {
        kind,
        acquired: false,
        cooperative: None,
        format: None,
        cursor: (0, 0),
        event_cursor: (0, 0),
        wheel: 0,
        buffer_size: 0,
        buffer: VecDeque::new(),
        overflow: false,
        event: 0,
    };
    let Some(pointer) = new_object(env, wide, true, value) else {
        return OUT_OF_MEMORY;
    };
    let _ = output.write(env.cpu, pointer);
    0
}
fn set_cooperative(env: &mut Env<'_>, this: u32, hwnd: u32, flags: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if value.acquired {
        return ACQUIRED;
    }
    if flags & !0x1f != 0 || !matches!(flags & 3, 1 | 2) || !matches!(flags & 12, 4 | 8) {
        return INVALID;
    }
    if flags & 3 != 2 || flags & 16 != 0 {
        return unsupported(
            env,
            "IDirectInputDevice::SetCooperativeLevel",
            "exclusive capture or disabling Windows keys is unavailable",
        );
    }
    if hwnd == 0 || snapshot(env, hwnd).is_none() {
        return INVALID;
    }
    device_mut(env, this).unwrap().cooperative = Some((hwnd, flags));
    0
}
fn set_format(env: &mut Env<'_>, this: u32, pointer: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if value.acquired {
        return ACQUIRED;
    }
    let kind = value.kind;
    if adv_abi::guest::check_range(env.cpu, pointer, 24, Prot::READ).is_err() {
        return INVALID;
    }
    let words: [u32; 6] =
        std::array::from_fn(|i| env.cpu.read_u32(pointer + i as u32 * 4).unwrap());
    let [size, object_size, flags, bytes, count, objects] = words;
    if size != 24
        || object_size != 16
        || bytes == 0
        || bytes > 4096
        || count > 256
        || adv_abi::guest::check_range(env.cpu, objects, count * 16, Prot::READ).is_err()
    {
        return INVALID;
    }
    if flags != 2 {
        return unsupported(
            env,
            "IDirectInputDevice::SetDataFormat",
            "only relative-axis keyboard/mouse data formats are available",
        );
    }
    let mut mapped = Vec::new();
    let mut used = Vec::new();
    for index in 0..count {
        let entry = objects + index * 16;
        let guid_p = env.cpu.read_u32(entry).unwrap();
        let offset = env.cpu.read_u32(entry + 4).unwrap();
        let object_type = env.cpu.read_u32(entry + 8).unwrap();
        if env.cpu.read_u32(entry + 12).unwrap() != 0 || object_type & !0x80ffff0f != 0 {
            return unsupported(
                env,
                "IDirectInputDevice::SetDataFormat",
                "object flags or types exceed the keyboard/mouse profile",
            );
        }
        if !matches!(object_type & 0xff, 1 | 3 | 4 | 12) {
            return unsupported(
                env,
                "IDirectInputDevice::SetDataFormat",
                "absolute axes, toggle buttons, or mixed object types are unavailable",
            );
        }
        let guid = if guid_p == 0 {
            None
        } else {
            if adv_abi::guest::check_range(env.cpu, guid_p, 16, Prot::READ).is_err() {
                return INVALID;
            }
            Some(GuestPtr::<Iid>::new(guid_p).read(env.cpu).unwrap())
        };
        let instance = (object_type >> 8) & 0xffff;
        let optional = object_type & 0x80000000 != 0;
        let mut source = None;
        let width = if object_type & 3 != 0 { 4 } else { 1 };
        match kind {
            Kind::Keyboard if object_type & 0xc != 0 && guid.is_none_or(|g| g == KEY) => {
                let key = if instance == 0xffff {
                    (1..=255).find(|&k| {
                        scan_vk(k as u8).is_some() && !used.contains(&Source::Key(k as u8))
                    })
                } else {
                    Some(instance)
                };
                source = key
                    .filter(|&k| k > 0 && k < 256 && scan_vk(k as u8).is_some())
                    .map(|k| Source::Key(k as u8));
            }
            Kind::Mouse if object_type & 3 != 0 => {
                for axis in 0..3 {
                    let mut axis_guid = [
                        0xe0, 0x02, 0x6d, 0xa3, 0xf3, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45,
                        0x53, 0x54, 0, 0,
                    ];
                    axis_guid[0] += axis;
                    if guid.is_none_or(|g| g == axis_guid)
                        && (instance == 0xffff || instance == u32::from(axis))
                        && !used.contains(&Source::Axis(axis))
                    {
                        source = Some(Source::Axis(axis));
                        break;
                    }
                }
            }
            Kind::Mouse if object_type & 0xc != 0 && guid.is_none_or(|g| g == BUTTON) => {
                source = (0..4)
                    .find(|&button| {
                        (instance == 0xffff || instance == u32::from(button))
                            && !used.contains(&Source::Button(button))
                    })
                    .map(Source::Button);
            }
            _ => {
                return unsupported(
                    env,
                    "IDirectInputDevice::SetDataFormat",
                    "requested object is outside the keyboard/mouse profile",
                );
            }
        }
        let source = match source {
            Some(s) => s,
            None if optional => Source::Missing(width),
            None => return 0x80070002,
        };
        if offset
            .checked_add(source.size())
            .is_none_or(|end| end > bytes)
            || mapped.iter().any(|&(other, s): &(u32, Source)| {
                offset < other + s.size() && other < offset + source.size()
            })
        {
            return INVALID;
        }
        used.push(source);
        mapped.push((offset, source));
    }
    let value = device_mut(env, this).unwrap();
    value.format = Some(Format {
        size: bytes,
        objects: mapped,
    });
    value.buffer.clear();
    value.overflow = false;
    0
}
fn acquire(env: &mut Env<'_>, this: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    let Some((hwnd, flags)) = value.cooperative else {
        return INVALID;
    };
    if value.format.is_none() {
        return INVALID;
    }
    let Some(input) = snapshot(env, hwnd) else {
        return INVALID;
    };
    if flags & 4 != 0 && !input.foreground {
        return 0x80070005;
    }
    let wheel = env.ext.get_or_default::<Events>().wheel;
    let value = device_mut(env, this).unwrap();
    if value.acquired {
        return 1;
    }
    value.acquired = true;
    value.cursor = input.cursor;
    value.event_cursor = input.cursor;
    value.wheel = wheel;
    value.buffer.clear();
    value.overflow = false;
    0
}
fn unacquire(env: &mut Env<'_>, this: u32) -> u32 {
    let Some(value) = device_mut(env, this) else {
        return INVALID;
    };
    let old = std::mem::replace(&mut value.acquired, false);
    value.buffer.clear();
    value.overflow = false;
    u32::from(!old)
}
fn poll(env: &mut Env<'_>, this: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if !value.acquired {
        return NOT_ACQUIRED;
    }
    let Some((hwnd, flags)) = value.cooperative else {
        return INVALID;
    };
    if snapshot(env, hwnd).is_none_or(|input| flags & 4 != 0 && !input.foreground) {
        device_mut(env, this).unwrap().acquired = false;
        return INPUT_LOST;
    }
    // Keyboard and mouse input is already updated by the host event path.
    1 // DI_NOEFFECT
}
#[win32(dinput, name="IDirectInputDevice2A::Poll", level=state)]
fn poll_a(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    poll(env, this.addr())
}
#[win32(dinput, name="IDirectInputDevice2W::Poll", level=state)]
fn poll_w(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    poll(env, this.addr())
}
fn get_state(env: &mut Env<'_>, this: u32, size: u32, out: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if !value.acquired {
        return NOT_ACQUIRED;
    }
    let Some(format) = value.format.as_ref() else {
        return INVALID;
    };
    if size != format.size || adv_abi::guest::check_range(env.cpu, out, size, Prot::WRITE).is_err()
    {
        return INVALID;
    }
    let Some((hwnd, flags)) = value.cooperative else {
        return INVALID;
    };
    let Some(input) = snapshot(env, hwnd) else {
        device_mut(env, this).unwrap().acquired = false;
        return INPUT_LOST;
    };
    if flags & 4 != 0 && !input.foreground {
        device_mut(env, this).unwrap().acquired = false;
        return INPUT_LOST;
    }
    let wheel = env.ext.try_get::<Events>().map_or(0, |e| e.wheel);
    let value = device(env, this).unwrap();
    let axes = [
        input.cursor.0.wrapping_sub(value.cursor.0),
        input.cursor.1.wrapping_sub(value.cursor.1),
        wheel
            .wrapping_sub(value.wheel)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32,
    ];
    let mut bytes = vec![0; size as usize];
    for &(offset, source) in &value.format.as_ref().unwrap().objects {
        match source {
            Source::Key(scan) => bytes[offset as usize] = key_state(&input.keys, scan),
            Source::Button(button) => {
                bytes[offset as usize] = input.keys[[1, 2, 4, 5][button as usize]] & 0x80
            }
            Source::Axis(axis) => bytes[offset as usize..offset as usize + 4]
                .copy_from_slice(&axes[axis as usize].to_le_bytes()),
            Source::Missing(_) => {}
        }
    }
    if env.cpu.write(out, &bytes).is_err() {
        return INVALID;
    }
    let value = device_mut(env, this).unwrap();
    value.cursor = input.cursor;
    value.wheel = wheel;
    0
}
fn set_property(env: &mut Env<'_>, this: u32, property: u32, pointer: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if value.acquired {
        return ACQUIRED;
    }
    if property != 1 {
        return unsupported(
            env,
            "IDirectInputDevice::SetProperty",
            "only DIPROP_BUFFERSIZE is available",
        );
    }
    if adv_abi::guest::check_range(env.cpu, pointer, 20, Prot::READ).is_err() {
        return INVALID;
    }
    let words: [u32; 5] =
        std::array::from_fn(|i| env.cpu.read_u32(pointer + i as u32 * 4).unwrap());
    if words[..4] != [20, 16, 0, 0] || words[4] > 4096 {
        return INVALID;
    }
    let value = device_mut(env, this).unwrap();
    value.buffer_size = words[4] as usize;
    value.buffer.clear();
    value.overflow = false;
    0
}
fn set_event(env: &mut Env<'_>, this: u32, event: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if value.acquired {
        return ACQUIRED;
    }
    let event = if event == 0 {
        0
    } else {
        if !matches!(env.sys.objects.get(event), Some(Object::Event { .. })) {
            return INVALID;
        }
        let Some(owned) = env.sys.objects.duplicate(event) else {
            return OUT_OF_MEMORY;
        };
        owned
    };
    let old = std::mem::replace(&mut device_mut(env, this).unwrap().event, event);
    if old != 0 {
        env.sys.objects.close(old);
    }
    0
}
fn get_data(env: &mut Env<'_>, this: u32, size: u32, out: u32, count: u32, flags: u32) -> u32 {
    let Some(value) = device(env, this) else {
        return INVALID;
    };
    if !value.acquired {
        return NOT_ACQUIRED;
    }
    if value.buffer_size == 0 {
        return 0x80040207;
    }
    if !matches!(size, 16 | 20)
        || flags & !1 != 0
        || adv_abi::guest::check_range(env.cpu, count, 4, Prot::RW).is_err()
    {
        return INVALID;
    }
    let Some((hwnd, cooperative)) = value.cooperative else {
        return INVALID;
    };
    if snapshot(env, hwnd).is_none_or(|input| cooperative & 4 != 0 && !input.foreground) {
        device_mut(env, this).unwrap().acquired = false;
        return INPUT_LOST;
    }
    let value = device(env, this).unwrap();
    let requested = env.cpu.read_u32(count).unwrap() as usize;
    let available = requested.min(value.buffer.len());
    if out != 0
        && adv_abi::guest::check_range(env.cpu, out, (available as u32) * size, Prot::WRITE)
            .is_err()
    {
        return INVALID;
    }
    let mut bytes = Vec::new();
    for event in value.buffer.iter().take(available) {
        for word in event {
            bytes.extend(word.to_le_bytes());
        }
        if size == 20 {
            bytes.extend(0u32.to_le_bytes());
        }
    }
    let overflow = value.overflow;
    if out != 0 && env.cpu.write(out, &bytes).is_err() {
        return INVALID;
    }
    if env.cpu.write_u32(count, available as u32).is_err() {
        return INVALID;
    }
    if flags & 1 == 0 {
        let value = device_mut(env, this).unwrap();
        value.buffer.drain(..available);
        value.overflow = false;
    }
    u32::from(overflow)
}
/// Translate accepted host input into buffered device records and wheel motion.
/// Immediate keys and cursor coordinates remain owned by the supplied provider.
pub fn post_input(env: &mut Env<'_>, message: u32, wparam: u32, lparam: u32) {
    let wheel = if message == 0x20a {
        (wparam >> 16) as u16 as i16 as i64
    } else {
        0
    };
    let events = env.ext.get_or_default::<Events>();
    events.wheel = events.wheel.wrapping_add(wheel);
    events.sequence = events.sequence.wrapping_add(1);
    let sequence = events.sequence;
    let Some(state) = env.ext.try_get::<State>() else {
        return;
    };
    let com = state.com;
    let identities: Vec<_> = env
        .sys
        .objects
        .all_hosts::<adv_os::com::ComObject<Device>>()
        .map(|(id, _)| id)
        .collect();
    let mut signal = Vec::new();
    for identity in identities {
        let Some(value) = com.get_identity(&env.sys.objects, identity) else {
            continue;
        };
        if !value.acquired {
            continue;
        }
        let Some((hwnd, flags)) = value.cooperative else {
            continue;
        };
        let Some(input) = snapshot(env, hwnd) else {
            continue;
        };
        if flags & 4 != 0 && !input.foreground {
            continue;
        }
        let mut changes = Vec::new();
        for &(offset, source) in &value.format.as_ref().unwrap().objects {
            let change = match source {
                Source::Key(scan)
                    if matches!(message, 0x100 | 0x101 | 0x104 | 0x105)
                        && key_event(scan, wparam, lparam) =>
                {
                    Some(if matches!(message, 0x100 | 0x104) {
                        0x80
                    } else {
                        0
                    })
                }
                Source::Button(button) if mouse_button(message, wparam) == Some(button) => {
                    Some(if matches!(message, 0x201 | 0x204 | 0x207 | 0x20b) {
                        0x80
                    } else {
                        0
                    })
                }
                Source::Axis(0) if message == 0x200 => {
                    Some(input.cursor.0.wrapping_sub(value.event_cursor.0) as u32)
                }
                Source::Axis(1) if message == 0x200 => {
                    Some(input.cursor.1.wrapping_sub(value.event_cursor.1) as u32)
                }
                Source::Axis(2) if message == 0x20a => Some(wheel as u32),
                _ => None,
            };
            if let Some(data) = change
                && (!matches!(source, Source::Axis(_)) || data != 0)
            {
                changes.push([
                    offset,
                    data,
                    (env.sys.clock.now_us() / 1000) as u32,
                    sequence,
                ]);
            }
        }
        let value = com
            .get_identity_mut(&mut env.sys.objects, identity)
            .unwrap();
        value.event_cursor = input.cursor;
        if !changes.is_empty() && value.event != 0 {
            signal.push(value.event);
        }
        if value.buffer_size != 0 {
            for event in changes {
                if value.buffer.len() == value.buffer_size {
                    value.buffer.pop_front();
                    value.overflow = true;
                }
                value.buffer.push_back(event);
            }
        }
    }
    for event in signal {
        env.sys.set_event(event);
    }
}
pub fn release_input(env: &mut Env<'_>) {
    let Some(state) = env.ext.try_get::<State>() else {
        return;
    };
    let com = state.com;
    let identities: Vec<_> = env
        .sys
        .objects
        .all_hosts::<adv_os::com::ComObject<Device>>()
        .map(|(id, _)| id)
        .collect();
    for identity in identities {
        if let Some(value) = com.get_identity_mut(&mut env.sys.objects, identity)
            && value.cooperative.is_some_and(|(_, flags)| flags & 4 != 0)
        {
            value.acquired = false;
            value.buffer.clear();
            value.overflow = false;
        }
    }
}
fn mouse_button(message: u32, wparam: u32) -> Option<u8> {
    match message {
        0x201 | 0x202 => Some(0),
        0x204 | 0x205 => Some(1),
        0x207 | 0x208 => Some(2),
        0x20b | 0x20c if wparam >> 16 == 1 => Some(3),
        _ => None,
    }
}
fn key_event(scan: u8, wparam: u32, lparam: u32) -> bool {
    let supplied = ((lparam >> 16) & 0x7f) as u8 | (((lparam >> 24) & 1) as u8 * 0x80);
    if supplied != 0 {
        return scan == supplied;
    }
    scan_vk(scan) == Some(wparam as usize)
        || matches!((scan, wparam), (29, 0x11) | (42, 0x10) | (56, 0x12))
}
// Enhanced-PC scan positions used by the configured virtual keyboard profile.
fn scan_vk(scan: u8) -> Option<usize> {
    adv_os::keyboard::directinput_virtual_key(scan)
}
fn key_state(keys: &[u8; 256], scan: u8) -> u8 {
    let Some(vk) = scan_vk(scan) else {
        return 0;
    };
    // Aggregate modifier VKs are a fallback only when neither side is known.
    let fallback = match scan {
        29 if (keys[0xa2] | keys[0xa3]) & 0x80 == 0 => keys[0x11],
        42 if (keys[0xa0] | keys[0xa1]) & 0x80 == 0 => keys[0x10],
        56 if (keys[0xa4] | keys[0xa5]) & 0x80 == 0 => keys[0x12],
        _ => 0,
    };
    (keys[vk] | fallback) & 0x80
}
macro_rules! interfaces {
    ($q:ident,$a:ident,$r:ident,$ty:ty,$qn:tt,$an:tt,$rn:tt) => {
        #[win32(dinput,name=$qn,level=implement)]
        fn $q(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            iid: GuestPtr<Iid>,
            out: GuestPtr<u32>,
        ) -> u32 {
            query_owned::<$ty>(env, this.addr(), iid, out)
        }
        #[win32(dinput,name=$an,level=implement)]
        fn $a(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            env.ext
                .try_get::<State>()
                .and_then(|s| s.com.add_ref::<$ty>(&mut env.sys.objects, this.addr()))
                .unwrap_or(0)
        }
        #[win32(dinput,name=$rn,level=implement)]
        fn $r(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            let (refs, value) = release_owned::<$ty>(env, this.addr());
            if let Some(value) = value {
                value.cleanup(env);
            }
            refs
        }
    };
}
interfaces!(
    input_query_a,
    input_add_a,
    input_release_a,
    LegacyFactory,
    "IDirectInputA::QueryInterface",
    "IDirectInputA::AddRef",
    "IDirectInputA::Release"
);
interfaces!(
    input_query_w,
    input_add_w,
    input_release_w,
    LegacyFactory,
    "IDirectInputW::QueryInterface",
    "IDirectInputW::AddRef",
    "IDirectInputW::Release"
);
interfaces!(
    device_query_a,
    device_add_a,
    device_release_a,
    Device,
    "IDirectInputDeviceA::QueryInterface",
    "IDirectInputDeviceA::AddRef",
    "IDirectInputDeviceA::Release"
);
interfaces!(
    device_query_w,
    device_add_w,
    device_release_w,
    Device,
    "IDirectInputDeviceW::QueryInterface",
    "IDirectInputDeviceW::AddRef",
    "IDirectInputDeviceW::Release"
);
macro_rules! device_methods {
    ($create:ident,$format:ident,$coop:ident,$acquire:ident,$unacquire:ident,$state:ident,$property:ident,$event:ident,$data:ident,$wide:expr,$c:tt,$f:tt,$o:tt,$a:tt,$u:tt,$s:tt,$p:tt,$e:tt,$d:tt)=>{
        #[win32(dinput,name=$c,level=state)]fn $create(env:&mut Env<'_>,this:GuestPtr<()>,guid:GuestPtr<Iid>,out:GuestPtr<u32>,outer:GuestPtr<()>)->u32{create_device(env,this.addr(),guid,out,outer,$wide)}
        #[win32(dinput,name=$f,level=state)]fn $format(env:&mut Env<'_>,this:GuestPtr<()>,format:GuestPtr<u8>)->u32{set_format(env,this.addr(),format.addr())}
        #[win32(dinput,name=$o,level=state)]fn $coop(env:&mut Env<'_>,this:GuestPtr<()>,hwnd:Handle,flags:u32)->u32{set_cooperative(env,this.addr(),hwnd.0,flags)}
        #[win32(dinput,name=$a,level=state)]fn $acquire(env:&mut Env<'_>,this:GuestPtr<()>)->u32{acquire(env,this.addr())}
        #[win32(dinput,name=$u,level=state)]fn $unacquire(env:&mut Env<'_>,this:GuestPtr<()>)->u32{unacquire(env,this.addr())}
        #[win32(dinput,name=$s,level=implement)]fn $state(env:&mut Env<'_>,this:GuestPtr<()>,size:u32,out:GuestPtr<u8>)->u32{get_state(env,this.addr(),size,out.addr())}
        #[win32(dinput,name=$p,level=state)]fn $property(env:&mut Env<'_>,this:GuestPtr<()>,property:GuestPtr<Iid>,value:GuestPtr<u8>)->u32{set_property(env,this.addr(),property.addr(),value.addr())}
        #[win32(dinput,name=$e,level=state)]fn $event(env:&mut Env<'_>,this:GuestPtr<()>,event:Handle)->u32{set_event(env,this.addr(),event.0)}
        #[win32(dinput,name=$d,level=implement)]fn $data(env:&mut Env<'_>,this:GuestPtr<()>,size:u32,out:GuestPtr<u8>,count:GuestPtr<u32>,flags:u32)->u32{get_data(env,this.addr(),size,out.addr(),count.addr(),flags)}
    };
}
device_methods!(
    create_device_a,
    format_a,
    cooperative_a,
    acquire_a,
    unacquire_a,
    state_a,
    property_a,
    event_a,
    data_a,
    false,
    "IDirectInputA::CreateDevice",
    "IDirectInputDeviceA::SetDataFormat",
    "IDirectInputDeviceA::SetCooperativeLevel",
    "IDirectInputDeviceA::Acquire",
    "IDirectInputDeviceA::Unacquire",
    "IDirectInputDeviceA::GetDeviceState",
    "IDirectInputDeviceA::SetProperty",
    "IDirectInputDeviceA::SetEventNotification",
    "IDirectInputDeviceA::GetDeviceData"
);
device_methods!(
    create_device_w,
    format_w,
    cooperative_w,
    acquire_w,
    unacquire_w,
    state_w,
    property_w,
    event_w,
    data_w,
    true,
    "IDirectInputW::CreateDevice",
    "IDirectInputDeviceW::SetDataFormat",
    "IDirectInputDeviceW::SetCooperativeLevel",
    "IDirectInputDeviceW::Acquire",
    "IDirectInputDeviceW::Unacquire",
    "IDirectInputDeviceW::GetDeviceState",
    "IDirectInputDeviceW::SetProperty",
    "IDirectInputDeviceW::SetEventNotification",
    "IDirectInputDeviceW::GetDeviceData"
);
pub(super) fn register(registry: &mut Registry) {
    for api in [
        create_a_ABI,
        create_w_ABI,
        poll_a_ABI,
        poll_w_ABI,
        input_query_a_ABI,
        input_add_a_ABI,
        input_release_a_ABI,
        enum_a_ABI,
        input_query_w_ABI,
        input_add_w_ABI,
        input_release_w_ABI,
        enum_w_ABI,
        device_query_a_ABI,
        device_add_a_ABI,
        device_release_a_ABI,
        device_query_w_ABI,
        device_add_w_ABI,
        device_release_w_ABI,
        create_device_a_ABI,
        format_a_ABI,
        cooperative_a_ABI,
        acquire_a_ABI,
        unacquire_a_ABI,
        state_a_ABI,
        property_a_ABI,
        event_a_ABI,
        data_a_ABI,
        create_device_w_ABI,
        format_w_ABI,
        cooperative_w_ABI,
        acquire_w_ABI,
        unacquire_w_ABI,
        state_w_ABI,
        property_w_ABI,
        event_w_ABI,
        data_w_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests;
