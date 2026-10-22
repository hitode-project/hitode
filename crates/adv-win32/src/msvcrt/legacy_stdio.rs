//! Legacy FILE arrays are real PE data, with one shared x86 layout selected
//! by the runtime profile. No console objects or output handles are invented.
use super::*;
use adv_cpu::backend::MemFault;
use adv_os::{Loader, Modules};
use adv_pe::ExportTarget;

const INPUT_BUFFER: &str = "_iob_input_buffer";
fn initial(layout: versions::StdioLayout, buffer: u32) -> Vec<u8> {
    assert_eq!(layout.entry_bytes, 32);
    let mut bytes = vec![0; (layout.entry_bytes * layout.entries) as usize];
    let mut word =
        |offset: usize, value: u32| bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    word(0, buffer);
    word(8, buffer);
    word(12, 0x101); // _IOREAD | _IOYOURBUF, the native static stdin buffer.
    word(24, layout.stdin_buffer);
    for i in 0..3 {
        word(i * layout.entry_bytes as usize + 16, u32::MAX - 1); // No console: fd -2.
        if i != 0 {
            word(i * layout.entry_bytes as usize + 12, 2);
        }
    }
    bytes
}
pub(super) fn register(registry: &mut Registry) {
    for profile in versions::PROFILES {
        if let Some(layout) = profile.legacy_stdio {
            let bytes = initial(layout, 0);
            registry
                .register_data(profile.module, "_iob", &bytes)
                .expect("legacy FILE array declaration");
        }
    }
    versions::register(registry, &[("__iob_func", address), ("__p__iob", address)]);
}
pub(super) fn initialize(env: &mut Env<'_>) -> Result<(), MemFault> {
    let profiles: Vec<_> = (env.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .map(|registry| {
            versions::PROFILES
                .iter()
                .copied()
                .filter(|p| registry.data(p.module, "_iob").is_some())
                .collect()
        })
        .unwrap_or_default();
    for profile in profiles {
        let layout = profile
            .legacy_stdio
            .expect("only legacy FILE arrays are declared");
        let key = (profile.module.to_owned(), INPUT_BUFFER.to_owned());
        let buffer = match env.ext.get_or_default::<CrtState>().globals.get(&key) {
            Some(&buffer) => buffer,
            None => {
                // Guest heap storage backs the native static-buffer layout;
                // all aliases retain one allocation and process lifetime.
                let buffer = allocate(env, layout.stdin_buffer, true);
                if buffer == 0 {
                    return Err(MemFault {
                        addr: 0,
                        len: layout.stdin_buffer,
                    });
                }
                env.ext.get_mut::<CrtState>().globals.insert(key, buffer);
                buffer
            }
        };
        env.sys
            .thunks
            .resolver_mut::<Registry>()
            .expect("checked registry")
            .set_data(profile.module, "_iob", &initial(layout, buffer))
            .expect("same FILE array extent");
    }
    Ok(())
}
fn address(env: &mut Env<'_>) -> Outcome {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    let (win32, modules) = env.ext.get2_mut::<crate::Win32, Modules>();
    let id = match modules.by_name(&module) {
        Some(id) => id,
        None => match Loader::new(env.cpu, env.sys, modules, &win32.vfs).load(&module) {
            Ok(id) => id,
            Err(error) => return Outcome::Fail(format!("CRT FILE image: {error}")),
        },
    };
    let image = modules.get(id);
    // Native getters address their own static array, independently of later
    // EAT redirection by another module. Existing guest writes remain intact.
    match image.exports_by_name.get("_iob") {
        Some(ExportTarget::Rva(rva)) => Outcome::Return(image.base + rva),
        _ => Outcome::Fail("CRT FILE array has no data address".into()),
    }
}
pub(super) fn release(env: &mut Env<'_>) -> Result<(), String> {
    let buffers: Vec<_> = env
        .ext
        .try_get::<CrtState>()
        .map(|state| {
            state
                .globals
                .iter()
                .filter(|((_, name), _)| name == INPUT_BUFFER)
                .map(|(key, &p)| (key.clone(), p))
                .collect()
        })
        .unwrap_or_default();
    for (key, buffer) in buffers {
        let heaps = env.ext.get_mut::<Heaps>();
        if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, buffer) {
            return Err("CRT stdin backing buffer release failed".into());
        }
        env.ext.get_mut::<CrtState>().globals.remove(&key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    // spec: CRT-LIB-25
    #[test]
    fn input_backing_storage_has_one_process_lifetime_without_console_objects() {
        for jit in [false, true] {
            let mut registry = Registry::default();
            crate::register(&mut registry);
            super::super::register(&mut registry);
            let mut m = Machine::new(
                JitCpu::new(PARK_ADDR, jit).unwrap(),
                registry,
                Clock::virtual_time(),
            )
            .unwrap();
            crate::Win32::install(
                &mut m,
                crate::Win32Config::new(
                    "/nonexistent/crt-iob",
                    "fixture.exe",
                    "/nonexistent/crt-iob-data",
                ),
            )
            .unwrap();
            let buffers: Vec<_> = m
                .ext
                .get::<CrtState>()
                .globals
                .iter()
                .filter(|((_, name), _)| name == INPUT_BUFFER)
                .map(|(_, &p)| p)
                .collect();
            assert_eq!(buffers.len(), 10);
            for &p in &buffers {
                assert!(m.ext.get::<Heaps>().owner(&m.cpu, p).is_some());
            }
            let mut env = Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            };
            release(&mut env).unwrap();
            release(&mut env).unwrap();
            for p in buffers {
                assert!(env.ext.get::<Heaps>().owner(env.cpu, p).is_none());
            }
        }
    }
}
