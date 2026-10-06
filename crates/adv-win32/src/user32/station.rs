//! The process has one interactive window station and one input desktop.
//! Their borrowed USER handles live in the process object table, outside the
//! kernel handle namespace, and stay stable for the life of the process.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Namespace, Objects};

use crate::util::set_last_error;

#[derive(Clone, Copy)]
enum UserObject {
    WindowStation,
    Desktop,
}

#[derive(Default)]
struct Stations {
    namespace: Option<Namespace>,
    station: Option<u32>,
    desktop: Option<u32>,
}

impl Stations {
    fn handle(&mut self, objects: &mut Objects, object: UserObject) -> Option<u32> {
        let slot = match object {
            UserObject::WindowStation => &mut self.station,
            UserObject::Desktop => &mut self.desktop,
        };
        if let Some(handle) = *slot {
            return Some(handle);
        }
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x0900_0004, 0x09ff_fffc);
        }
        let handle = objects.insert_host_in(self.namespace?, object)?;
        *slot = Some(handle);
        Some(handle)
    }

    fn object(&self, objects: &Objects, handle: u32) -> Option<UserObject> {
        objects
            .owned(objects.lookup_alias::<UserObject>(self.namespace?, handle)?)
            .copied()
    }
}

fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}

fn handle(env: &mut Env<'_>, object: UserObject) -> Handle {
    match env
        .ext
        .get_or_default::<Stations>()
        .handle(&mut env.sys.objects, object)
    {
        Some(handle) => Handle(handle),
        None => Handle(fail(env, 8) as u32), // ERROR_NOT_ENOUGH_MEMORY
    }
}

#[win32(user32, name = "GetProcessWindowStation", level = state)]
fn process_station(env: &mut Env<'_>) -> Handle {
    handle(env, UserObject::WindowStation)
}

#[win32(user32, name = "GetThreadDesktop", level = state)]
fn thread_desktop(env: &mut Env<'_>, thread: u32) -> Handle {
    if !env
        .sys
        .contexts
        .iter()
        .any(|context| context.tid == thread && !context.is_done())
    {
        // Native USER leaves LastError unchanged for an unknown thread ID.
        return Handle(0);
    }
    handle(env, UserObject::Desktop)
}

fn information(
    env: &mut Env<'_>,
    object: Handle,
    index: i32,
    output: GuestPtr<()>,
    length: u32,
    needed: GuestPtr<u32>,
    wide: bool,
) -> i32 {
    let Some(object) = env
        .ext
        .try_get::<Stations>()
        .and_then(|state| state.object(&env.sys.objects, object.0))
    else {
        // Invalid USER handles do not alter either output or LastError natively.
        return 0;
    };
    if !needed.is_null() && check_range(env.cpu, needed.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998); // ERROR_NOACCESS
    }
    let (bytes, short_needed) = match index {
        1 => {
            let mut flags = vec![0; 12]; // USEROBJECTFLAGS: inherit, reserved, flags
            if matches!(object, UserObject::WindowStation) {
                flags[8] = 1; // WSF_VISIBLE
            }
            (flags, 12)
        }
        2 | 3 => {
            let name = match (object, index) {
                (UserObject::WindowStation, 2) => "WinSta0",
                (UserObject::WindowStation, _) => "WindowStation",
                (UserObject::Desktop, 2) => "Default",
                (UserObject::Desktop, _) => "Desktop",
            };
            let bytes = if wide {
                name.encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect()
            } else {
                name.bytes().chain([0]).collect()
            };
            // The native ANSI wrapper reports the underlying UTF-16 size on
            // an insufficient buffer, then the ANSI size on a successful copy.
            (bytes, (name.len() as u32 + 1) * 2)
        }
        6 if matches!(object, UserObject::Desktop) => (1u32.to_le_bytes().to_vec(), 4),
        _ => {
            if matches!(index, 4 | 5) {
                adv_abi::warn_unavailable(
                    env,
                    if wide {
                        "user32.dll!GetUserObjectInformationW"
                    } else {
                        "user32.dll!GetUserObjectInformationA"
                    },
                    if index == 4 {
                        "user security identifiers are not modeled"
                    } else {
                        "desktop heap information is not modeled"
                    },
                );
            }
            if !needed.is_null() && needed.write(env.cpu, 0).is_err() {
                return fail(env, 998);
            }
            return fail(
                env,
                match index {
                    6 => 1,       // ERROR_INVALID_FUNCTION: station is not a desktop
                    4 | 5 => 120, // SID and desktop heap information are not modeled.
                    _ => 87,      // ERROR_INVALID_PARAMETER
                },
            );
        }
    };
    if length < bytes.len() as u32 {
        if !needed.is_null() && needed.write(env.cpu, short_needed).is_err() {
            return fail(env, 998);
        }
        return fail(env, 122); // ERROR_INSUFFICIENT_BUFFER; output stays untouched.
    }
    if check_range(env.cpu, output.addr(), bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(output.addr(), &bytes).is_err()
    {
        return fail(env, 998);
    }
    if !needed.is_null() && needed.write(env.cpu, bytes.len() as u32).is_err() {
        return fail(env, 998);
    }
    1
}

#[win32(user32, name = "GetUserObjectInformationA", level = state)]
fn information_a(
    env: &mut Env<'_>,
    object: Handle,
    index: i32,
    output: GuestPtr<()>,
    length: u32,
    needed: GuestPtr<u32>,
) -> i32 {
    information(env, object, index, output, length, needed, false)
}

#[win32(user32, name = "GetUserObjectInformationW", level = state)]
fn information_w(
    env: &mut Env<'_>,
    object: Handle,
    index: i32,
    output: GuestPtr<()>,
    length: u32,
    needed: GuestPtr<u32>,
) -> i32 {
    information(env, object, index, output, length, needed, true)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        process_station_ABI,
        thread_desktop_ABI,
        information_a_ABI,
        information_w_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt},
    };
    use adv_machine::{Clock, Machine};

    fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
        Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        }
    }

    #[test]
    fn interactive_user_objects_have_stable_typed_handles_and_checked_information() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let park = adv_machine::layout::PARK_ADDR;
        let cpu = JitCpu::new(park, false).unwrap();
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(".", "station.exe", "target/station-tests"),
        )
        .unwrap();
        machine.spawn("first", park, &[], 65536).unwrap();
        machine.spawn("second", park, &[], 65536).unwrap();
        let tids: Vec<_> = machine
            .sys
            .contexts
            .iter()
            .map(|context| context.tid)
            .collect();
        let memory = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 8192, Prot::RW)
            .unwrap();
        let needed = GuestPtr::new(memory + 128);
        let station = process_station(&mut env(&mut machine));
        let desktop = thread_desktop(&mut env(&mut machine), tids[0]);
        assert_ne!(station, Handle(0));
        assert_ne!(station, desktop);
        let object_count = machine.sys.objects.object_count();
        assert_eq!(process_station(&mut env(&mut machine)), station);
        assert_eq!(thread_desktop(&mut env(&mut machine), tids[1]), desktop);
        assert_eq!(machine.sys.objects.object_count(), object_count);
        assert!(machine.sys.objects.get(station.0).is_none());

        for (object, index, expected) in [
            (station, 1, vec![0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]),
            (desktop, 1, vec![0; 12]),
            (station, 2, b"WinSta0\0".to_vec()),
            (desktop, 2, b"Default\0".to_vec()),
            (station, 3, b"WindowStation\0".to_vec()),
            (desktop, 3, b"Desktop\0".to_vec()),
            (desktop, 6, 1u32.to_le_bytes().to_vec()),
        ] {
            for wide in [false, true] {
                let bytes: Vec<_> = if wide && matches!(index, 2 | 3) {
                    expected
                        .iter()
                        .flat_map(|&byte| u16::from(byte).to_le_bytes())
                        .collect()
                } else {
                    expected.clone()
                };
                machine.cpu.write(memory, &[0xa5; 64]).unwrap();
                assert_eq!(
                    information(
                        &mut env(&mut machine),
                        object,
                        index,
                        GuestPtr::new(memory),
                        bytes.len() as u32 - 1,
                        needed,
                        wide
                    ),
                    0
                );
                let short_needed = if matches!(index, 2 | 3) {
                    expected.len() * 2
                } else {
                    expected.len()
                };
                assert_eq!(
                    machine.cpu.read_u32(needed.addr()).unwrap(),
                    short_needed as u32
                );
                assert_eq!(crate::util::last_error(&env(&mut machine)), 122);
                let mut guard = [0; 64];
                machine.cpu.read(memory, &mut guard).unwrap();
                assert_eq!(guard, [0xa5; 64]);
                set_last_error(&mut env(&mut machine), 0x1234);
                assert_eq!(
                    information(
                        &mut env(&mut machine),
                        object,
                        index,
                        GuestPtr::new(memory),
                        bytes.len() as u32,
                        needed,
                        wide
                    ),
                    1
                );
                machine.cpu.read(memory, &mut guard).unwrap();
                assert_eq!(&guard[..bytes.len()], bytes.as_slice());
                assert!(guard[bytes.len()..].iter().all(|&byte| byte == 0xa5));
                assert_eq!(
                    machine.cpu.read_u32(needed.addr()).unwrap(),
                    bytes.len() as u32
                );
                assert_eq!(crate::util::last_error(&env(&mut machine)), 0x1234);
            }
        }
        for (object, index, error) in [(station, 6, 1), (station, 99, 87)] {
            assert_eq!(
                information_w(
                    &mut env(&mut machine),
                    object,
                    index,
                    GuestPtr::new(memory),
                    64,
                    needed
                ),
                0
            );
            assert_eq!(crate::util::last_error(&env(&mut machine)), error);
            assert_eq!(machine.cpu.read_u32(needed.addr()).unwrap(), 0);
        }
        // Preflight the complete output, including a nonwritable second page.
        let cross_page = memory + 4096 - 8;
        machine.cpu.write(cross_page, &[0xa5; 16]).unwrap();
        machine
            .cpu
            .protect(memory + 4096, 4096, Prot::READ)
            .unwrap();
        machine.cpu.write_u32(needed.addr(), 0x1234).unwrap();
        assert_eq!(
            information_w(
                &mut env(&mut machine),
                station,
                2,
                GuestPtr::new(cross_page),
                16,
                needed
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
        assert_eq!(machine.cpu.read_u32(needed.addr()).unwrap(), 0x1234);
        let mut guard = [0; 16];
        machine.cpu.read(cross_page, &mut guard).unwrap();
        assert_eq!(guard, [0xa5; 16]);
        set_last_error(&mut env(&mut machine), 0x1234);
        assert_eq!(thread_desktop(&mut env(&mut machine), u32::MAX), Handle(0));
        assert_eq!(
            information_a(
                &mut env(&mut machine),
                Handle(0),
                1,
                GuestPtr::new(memory),
                12,
                needed
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 0x1234);
        assert_eq!(machine.cpu.read_u32(needed.addr()).unwrap(), 0x1234);
    }
}
