//! File exports reuse the in-memory codec and ordinary Win32 file thunks.
//! Private output buffers and handles are retired if a continuation is cancelled.
use adv_abi::{Api, Conv, GuestPtr, ImplementationLevel, Registry, WideStr};
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_machine::{Env, Outcome};
use std::rc::{Rc, Weak};
const INVALID: u32 = 0x8876086c;
struct Pending {
    buffer: u32,
    handle: Option<u32>,
    guard: Weak<()>,
}
#[derive(Default)]
struct Writes(Vec<Pending>);
pub(super) fn register(registry: &mut Registry) {
    registry.register(Api {
        module: "d3dx9_43.dll",
        name: "D3DXSaveSurfaceToFileW",
        conv: Conv::Stdcall { argc: 5 },
        handler: save,
        level: ImplementationLevel::Implement,
    });
}
fn save(env: &mut Env<'_>) -> Outcome {
    let path = env.arg(0);
    if path == 0 || WideStr(GuestPtr::new(path)).read(env.cpu, 32768).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(scratch) = env.cpu.reg(Reg::Esp).checked_sub(16) else {
        return Outcome::Return(INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, scratch, 16, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let mut entries = [0; 3];
    for (slot, name) in ["CreateFileW", "WriteFile", "GetLastError"]
        .into_iter()
        .enumerate()
    {
        let Ok(entry) = env.sys.thunks.resolve(env.cpu, "kernel32.dll", name) else {
            return Outcome::Return(INVALID);
        };
        entries[slot] = entry;
    }
    let Ok(encode) =
        env.sys
            .thunks
            .resolve(env.cpu, "d3dx9_43.dll", "D3DXSaveSurfaceToFileInMemory")
    else {
        return Outcome::Return(INVALID);
    };
    Outcome::call_on_stack(
        encode,
        [scratch, env.arg(1), env.arg(2), env.arg(3), env.arg(4)],
        scratch,
        move |env, resumed| {
            if resumed.value() != 0 {
                return Outcome::Return(resumed.value());
            }
            let pointer = env.cpu.read_u32(scratch).unwrap_or(0);
            let Some((data, size)) = super::buffer::contents(env, pointer) else {
                return Outcome::Return(INVALID);
            };
            let guard = Rc::new(());
            env.ext.get_or_default::<Writes>().0.push(Pending {
                buffer: pointer,
                handle: None,
                guard: Rc::downgrade(&guard),
            });
            Outcome::call_on_stack(
                entries[0],
                [path, 0x40000000, 0, 0, 2, 0x80, 0],
                scratch,
                move |env, opened| {
                    let handle = opened.value();
                    if handle == u32::MAX {
                        return failure(env, entries[2], scratch, pointer, guard);
                    }
                    let Some(pending) = env
                        .ext
                        .get_mut::<Writes>()
                        .0
                        .iter_mut()
                        .find(|p| p.buffer == pointer)
                    else {
                        env.sys.objects.close(handle);
                        return Outcome::Return(INVALID);
                    };
                    pending.handle = Some(handle);
                    Outcome::call_on_stack(
                        entries[1],
                        [handle, data, size, scratch + 4, 0],
                        scratch,
                        move |env, wrote| {
                            if wrote.value() == 0 {
                                return failure(env, entries[2], scratch, pointer, guard);
                            }
                            let complete = env.cpu.read_u32(scratch + 4).ok() == Some(size);
                            finish(env, pointer);
                            drop(guard);
                            Outcome::Return(if complete { 0 } else { 0x8007001d })
                        },
                    )
                },
            )
        },
    )
}
fn failure(
    _env: &mut Env<'_>,
    get_error: u32,
    scratch: u32,
    pointer: u32,
    guard: Rc<()>,
) -> Outcome {
    Outcome::call_on_stack(get_error, [], scratch, move |env, error| {
        let code = error.value();
        finish(env, pointer);
        drop(guard);
        Outcome::Return(if code == 0 {
            INVALID
        } else {
            0x80070000 | (code & 0xffff)
        })
    })
}
fn finish(env: &mut Env<'_>, pointer: u32) {
    let pending = &mut env.ext.get_mut::<Writes>().0;
    if let Some(index) = pending.iter().position(|p| p.buffer == pointer) {
        let entry = pending.swap_remove(index);
        if let Some(handle) = entry.handle {
            env.sys.objects.close(handle);
        }
        super::buffer::retire(env, entry.buffer);
    }
}
pub(super) fn reap(env: &mut Env<'_>, all: bool) {
    let Some(writes) = env.ext.try_get::<Writes>() else {
        return;
    };
    let retired: Vec<_> = writes
        .0
        .iter()
        .filter(|p| all || p.guard.strong_count() == 0)
        .map(|p| p.buffer)
        .collect();
    for pointer in retired {
        finish(env, pointer);
    }
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    use adv_os::Heaps;

    #[test]
    fn cancelled_export_retires_unpublished_buffer_but_live_guard_keeps_it() {
        let mut registry = Registry::default();
        super::super::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        let mut heaps = Heaps::default();
        heaps.process_heap = heaps.create(&mut machine.cpu, &mut machine.sys.vm).unwrap();
        machine.ext.insert(heaps);
        let mut env = Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        assert_eq!(
            super::super::buffer::create(&mut env, b"authored", GuestPtr::new(0x410000)),
            0
        );
        let pointer = env.cpu.read_u32(0x410000).unwrap();
        let (data, _) = super::super::buffer::contents(&env, pointer).unwrap();
        let guard = Rc::new(());
        env.ext.get_or_default::<Writes>().0.push(Pending {
            buffer: pointer,
            handle: None,
            guard: Rc::downgrade(&guard),
        });
        reap(&mut env, false);
        assert!(super::super::buffer::contents(&env, pointer).is_some());
        drop(guard);
        reap(&mut env, false);
        assert!(super::super::buffer::contents(&env, pointer).is_none());
        assert!(env.ext.get::<Heaps>().owner(env.cpu, data).is_none());
        assert!(env.ext.get::<Writes>().0.is_empty());
    }
}
