//! Mutants and semaphores reuse the event transport and named/waiting owner.
use super::*;
use crate::kernel32::file_mapping;

const INVALID_PARAMETER: u32 = 0xc000000d;
const ACCESS_VIOLATION: u32 = 0xc0000005;
const INVALID_HANDLE: u32 = 0xc0000008;
const TYPE_MISMATCH: u32 = 0xc0000024;
const MUTANT_NOT_OWNED: u32 = 0xc0000046;
const SEMAPHORE_LIMIT: u32 = 0xc0000047;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Mutant,
    Semaphore,
}
impl Kind {
    fn id(self) -> u32 {
        match self {
            Self::Mutant => 2,
            Self::Semaphore => 3,
        }
    }
    fn full_access(self) -> u32 {
        match self {
            Self::Mutant => 0x1f0001,
            Self::Semaphore => 0x1f0003,
        }
    }
    fn create(self) -> &'static str {
        match self {
            Self::Mutant => "NtCreateMutant",
            Self::Semaphore => "NtCreateSemaphore",
        }
    }
    fn open(self) -> &'static str {
        match self {
            Self::Mutant => "NtOpenMutant",
            Self::Semaphore => "NtOpenSemaphore",
        }
    }
    fn release(self) -> &'static str {
        match self {
            Self::Mutant => "NtReleaseMutant",
            Self::Semaphore => "NtReleaseSemaphore",
        }
    }
    fn object(self, env: &Env<'_>, initial: u32, maximum: u32) -> Result<Object, u32> {
        match self {
            Self::Mutant => {
                let owned = initial as u8 != 0;
                Ok(Object::Mutex {
                    owner: owned.then_some(env.context),
                    recursion: u32::from(owned),
                })
            }
            Self::Semaphore if initial as i32 >= 0 && maximum as i32 > 0 && initial <= maximum => {
                Ok(Object::Semaphore {
                    count: initial,
                    max: maximum,
                })
            }
            Self::Semaphore => Err(INVALID_PARAMETER),
        }
    }
}
fn access(env: &mut Env<'_>, desired: u32, kind: Kind, api: &str) -> Result<(), u32> {
    if desired == kind.full_access() || desired == 0x10000000 {
        Ok(())
    } else {
        Err(event::unsupported(
            env,
            api,
            "restricted synchronization access is not modeled; use the object's ALL_ACCESS or GENERIC_ALL",
        ))
    }
}
fn create_named(
    env: &mut Env<'_>,
    kind: Kind,
    initial: u32,
    maximum: u32,
    name: Option<Vec<u16>>,
    open_if: bool,
) -> Result<(u32, bool), u32> {
    let object = kind.object(env, initial, maximum)?;
    file_mapping::sync_create_named(env, object, name, open_if).map_err(event::error)
}
pub(super) fn create_native(env: &mut Env<'_>, kind: Kind, args: [u32; 5]) -> u32 {
    let [output, desired, object, initial, maximum] = args;
    if let Err(status) = event::writable(env, output) {
        return status;
    }
    let attrs = match event::attributes(env, object, kind.create()) {
        Ok(attrs) => attrs,
        Err(status) => return status,
    };
    if let Err(status) = access(env, desired, kind, kind.create()) {
        return status;
    }
    if matches!(kind, Kind::Semaphore) && kind.object(env, initial, maximum).is_err() {
        // The authored native control confirms invalid signed counts clear the
        // creation output. Capability rejection above still publishes no handle.
        if env.cpu.write_u32(output, 0).is_err() {
            return ACCESS_VIOLATION;
        }
        return INVALID_PARAMETER;
    }
    match create_named(env, kind, initial, maximum, attrs.name, attrs.open_if) {
        Ok((handle, existed)) => {
            if env.cpu.write_u32(output, handle).is_err() {
                env.sys.objects.close(handle);
                file_mapping::prune(env);
                return ACCESS_VIOLATION;
            }
            super::super::handle::set_inherit(env, handle, attrs.inherit);
            if existed { 0x40000000 } else { 0 }
        }
        Err(status) => status,
    }
}
pub(super) fn open_native(env: &mut Env<'_>, kind: Kind, args: [u32; 3]) -> u32 {
    let [output, desired, object] = args;
    if let Err(status) = event::writable(env, output) {
        return status;
    }
    let attrs = match event::attributes(env, object, kind.open()) {
        Ok(attrs) => attrs,
        Err(status) => return status,
    };
    let Some(name) = attrs.name else {
        return 0xc0000033;
    };
    let identity = match file_mapping::sync_lookup_named(env, &name, kind.id()) {
        Ok(identity) => identity,
        Err(code) => return event::error(code),
    };
    if let Err(status) = access(env, desired, kind, kind.open()) {
        return status;
    }
    let Some(handle) = env.sys.objects.alias_identity(identity) else {
        return 0xc0000017;
    };
    if env.cpu.write_u32(output, handle).is_err() {
        env.sys.objects.close(handle);
        return ACCESS_VIOLATION;
    }
    super::super::handle::set_inherit(env, handle, attrs.inherit);
    0
}
pub(super) fn release_native(
    env: &mut Env<'_>,
    kind: Kind,
    handle: u32,
    count: u32,
    previous: u32,
) -> u32 {
    if matches!(kind, Kind::Semaphore) && count as i32 <= 0 {
        return INVALID_PARAMETER;
    }
    // The authored WoW64 control accepts writable unaligned LONG outputs for
    // both releases. Probe the whole range; creation-handle alignment is a
    // different contract and keeps the existing event transport's validator.
    if previous != 0
        && adv_abi::guest::check_range(env.cpu, previous, 4, adv_cpu::backend::Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let current = env.context;
    let old = match (kind, env.sys.objects.get(handle)) {
        (Kind::Mutant, Some(Object::Mutex { owner, recursion })) => {
            if *owner != Some(current) || *recursion == 0 {
                return MUTANT_NOT_OWNED;
            }
            1u32.wrapping_sub(*recursion)
        }
        (Kind::Semaphore, Some(Object::Semaphore { count: old, max })) => {
            if old.checked_add(count).is_none_or(|total| total > *max) {
                return SEMAPHORE_LIMIT;
            }
            *old
        }
        (_, Some(_)) => return TYPE_MISMATCH,
        (_, None) => return INVALID_HANDLE,
    };
    // Validate and publish optional output before committing any shared state.
    if previous != 0 && env.cpu.write_u32(previous, old).is_err() {
        return ACCESS_VIOLATION;
    }
    match kind {
        Kind::Mutant => env.sys.release_mutex(handle),
        Kind::Semaphore => env.sys.release_semaphore(handle, count),
    }
    0
}
pub(super) fn create_front(env: &mut Env<'_>, kind: Kind) -> Outcome {
    let (security, initial) = (env.arg(0), env.arg(1));
    let (maximum, name) = match kind {
        Kind::Mutant => (0, env.arg(2)),
        Kind::Semaphore => (env.arg(2), env.arg(3)),
    };
    let initial = if matches!(kind, Kind::Mutant) {
        u32::from(initial != 0)
    } else {
        initial
    };
    let wide = env.sys.thunks.get(env.thunk).name.ends_with('W');
    let inherit = match super::super::handle::security_inherit(env, security) {
        Ok(inherit) => inherit,
        Err(code) => {
            set_last_error(env, code);
            return Outcome::Return(0);
        }
    };
    let key = match file_mapping::sync_name(env, name, wide) {
        Ok(key) => key,
        Err(code) => {
            set_last_error(env, code);
            return Outcome::Return(0);
        }
    };
    match crate::native::overridden(env, kind.create()) {
        Ok(Some(target)) => {
            let buffer = match event::marshal(env, key.as_deref(), inherit, true) {
                Ok(buffer) => buffer,
                Err(outcome) => return outcome,
            };
            let mut args = vec![buffer, kind.full_access(), buffer + 8, initial];
            if matches!(kind, Kind::Semaphore) {
                args.push(maximum);
            }
            Outcome::call_on_stack(target, args, buffer, move |env, value| {
                event::published(env, buffer, value.value(), true)
            })
        }
        Ok(None) => match create_named(env, kind, initial, maximum, key, true) {
            Ok((handle, existed)) => {
                super::super::handle::set_inherit(env, handle, inherit);
                set_last_error(env, if existed { 183 } else { 0 });
                Outcome::Return(handle)
            }
            Err(status) => event::front_status(env, status, 0),
        },
        Err(_) => crate::native::unavailable(env, kind.create()),
    }
}
pub(super) fn open_front(env: &mut Env<'_>, kind: Kind) -> Outcome {
    let (desired, inherit, pointer) = (env.arg(0), env.arg(1) != 0, env.arg(2));
    let wide = env.sys.thunks.get(env.thunk).name.ends_with('W');
    if pointer == 0 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let key = match file_mapping::sync_name(env, pointer, wide) {
        Ok(Some(key)) => key,
        Ok(None) => {
            set_last_error(env, 6);
            return Outcome::Return(0);
        }
        Err(code) => {
            set_last_error(env, if code == 123 { 3 } else { code });
            return Outcome::Return(0);
        }
    };
    match crate::native::overridden(env, kind.open()) {
        Ok(Some(target)) => {
            let buffer = match event::marshal(env, Some(&key), inherit, false) {
                Ok(buffer) => buffer,
                Err(outcome) => return outcome,
            };
            Outcome::call_on_stack(
                target,
                [buffer, desired, buffer + 8],
                buffer,
                move |env, value| event::published(env, buffer, value.value(), false),
            )
        }
        Ok(None) => {
            let identity = match file_mapping::sync_lookup_named(env, &key, kind.id()) {
                Ok(identity) => identity,
                Err(code) => return event::front_status(env, event::error(code), 0),
            };
            if let Err(status) = access(env, desired, kind, kind.open()) {
                return event::front_status(env, status, 0);
            }
            match env.sys.objects.alias_identity(identity) {
                Some(handle) => {
                    super::super::handle::set_inherit(env, handle, inherit);
                    set_last_error(env, 0);
                    Outcome::Return(handle)
                }
                None => event::front_status(env, 0xc0000017, 0),
            }
        }
        Err(_) => crate::native::unavailable(env, kind.open()),
    }
}
pub(super) fn release_front(env: &mut Env<'_>, kind: Kind) -> Outcome {
    let handle = env.arg(0);
    let (count, previous) = if matches!(kind, Kind::Semaphore) {
        (env.arg(1), env.arg(2))
    } else {
        (0, 0)
    };
    match crate::native::overridden(env, kind.release()) {
        Ok(Some(target)) => {
            let args = if matches!(kind, Kind::Semaphore) {
                vec![handle, count, previous]
            } else {
                vec![handle, previous]
            };
            Outcome::call(target, args, |env, value| {
                event::front_status(env, value.value(), 1)
            })
        }
        Ok(None) => {
            let status = release_native(env, kind, handle, count, previous);
            event::front_status(env, status, 1)
        }
        Err(_) => crate::native::unavailable(env, kind.release()),
    }
}
pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, "ntdll.dll", "NtCreateMutant", 4, |env| {
        let args = [env.arg(0), env.arg(1), env.arg(2), env.arg(3), 0];
        Outcome::Return(create_native(env, Kind::Mutant, args))
    });
    stdcall(registry, "ntdll.dll", "NtCreateSemaphore", 5, |env| {
        let args = std::array::from_fn(|i| env.arg(i as u32));
        Outcome::Return(create_native(env, Kind::Semaphore, args))
    });
    for (kind, name) in [
        (Kind::Mutant, "NtOpenMutant"),
        (Kind::Semaphore, "NtOpenSemaphore"),
    ] {
        // A plain handler per known ABI avoids copying a routed Win32 frontend.
        let handler: adv_machine::Handler = match kind {
            Kind::Mutant => |env| {
                let args = std::array::from_fn(|i| env.arg(i as u32));
                Outcome::Return(open_native(env, Kind::Mutant, args))
            },
            Kind::Semaphore => |env| {
                let args = std::array::from_fn(|i| env.arg(i as u32));
                Outcome::Return(open_native(env, Kind::Semaphore, args))
            },
        };
        stdcall(registry, "ntdll.dll", name, 3, handler);
    }
    stdcall(registry, "ntdll.dll", "NtReleaseMutant", 2, |env| {
        let (handle, previous) = (env.arg(0), env.arg(1));
        Outcome::Return(release_native(env, Kind::Mutant, handle, 0, previous))
    });
    stdcall(registry, "ntdll.dll", "NtReleaseSemaphore", 3, |env| {
        let (handle, count, previous) = (env.arg(0), env.arg(1), env.arg(2));
        Outcome::Return(release_native(
            env,
            Kind::Semaphore,
            handle,
            count,
            previous,
        ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Prot},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    #[test]
    fn native_releases_probe_invalid_outputs_and_accept_writable_unaligned_longs() {
        for (kind, object) in [
            (
                Kind::Mutant,
                Object::Mutex {
                    owner: Some(0),
                    recursion: 1,
                },
            ),
            (Kind::Semaphore, Object::Semaphore { count: 0, max: 1 }),
        ] {
            let mut machine = Machine::new(
                JitCpu::new(PARK_ADDR, false).unwrap(),
                Registry::default(),
                Clock::virtual_time(),
            )
            .unwrap();
            machine
                .spawn("release caller", PARK_ADDR, &[], 65536)
                .unwrap();
            machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
            machine.cpu.write(0x410000, &[0xa5; 8]).unwrap();
            let handle = machine.sys.objects.insert(object);
            let mut env = Env {
                context: 0,
                cpu: &mut machine.cpu,
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                thunk: 0,
            };
            assert_eq!(
                release_native(&mut env, kind, handle, 1, 1),
                ACCESS_VIOLATION
            );
            assert!(matches!(
                env.sys.objects.get(handle),
                Some(
                    Object::Mutex {
                        owner: Some(0),
                        recursion: 1
                    } | Object::Semaphore { count: 0, max: 1 }
                )
            ));
            assert_eq!(release_native(&mut env, kind, handle, 1, 0x410001), 0);
            assert_eq!(env.cpu.read_u32(0x410001).unwrap(), 0);
            let mut bytes = [0; 8];
            env.cpu.read(0x410000, &mut bytes).unwrap();
            assert_eq!(bytes, [0xa5, 0, 0, 0, 0, 0xa5, 0xa5, 0xa5]);
            assert!(matches!(
                env.sys.objects.get(handle),
                Some(
                    Object::Mutex {
                        owner: None,
                        recursion: 0
                    } | Object::Semaphore { count: 1, max: 1 }
                )
            ));
        }
    }
}
