//! DirectInput factory identity over common Objects. The current guest input
//! profile has keyboard/mouse channels and no game-controller device. This
//! front does not query host hardware or manufacture unavailable devices.
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_os::{
    Heaps,
    com::{Com, Iid, QueryError},
};
mod legacy;
pub use legacy::{InputProvider, InputSnapshot, post_input, release_input};

const INVALID: u32 = 0x80070057;
const UNSUPPORTED: u32 = 0x80004001;
const NO_INTERFACE: u32 = 0x80004002;
const OUT_OF_MEMORY: u32 = 0x8007000e;
const NO_AGGREGATION: u32 = 0x80040110;
const IID_A: Iid = [
    0x30, 0x80, 0x79, 0xbf, 0x3a, 0x48, 0xa2, 0x4d, 0xaa, 0x99, 0x5d, 0x64, 0xed, 0x36, 0x97, 0,
];
const IID_W: Iid = [
    0x31, 0x80, 0x79, 0xbf, 0x3a, 0x48, 0xa2, 0x4d, 0xaa, 0x99, 0x5d, 0x64, 0xed, 0x36, 0x97, 0,
];
struct Factory;
struct State {
    com: Com,
    tables: [u32; 2],
    legacy: Option<legacy::Tables>,
}
fn alloc(env: &mut Env<'_>, size: u32) -> Option<u32> {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, size, true)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}
/// Process-owned vtables are cached once; each factory payload remains in
/// Objects. Unregistered methods retain their generated failure thunks.
fn initialize(env: &mut Env<'_>) -> bool {
    if env.ext.try_get::<State>().is_some() {
        return true;
    }
    let mut tables = [0; 2];
    let result = (|| {
        for (index, interface) in ["IDirectInput8A", "IDirectInput8W"].iter().enumerate() {
            let signatures: Vec<_> = adv_abi::signature::com_signatures()
                .iter()
                .filter(|signature| signature.module == *interface)
                .collect();
            if signatures.len() != 11 {
                return None;
            }
            let table = alloc(env, 44)?;
            tables[index] = table;
            for signature in signatures {
                let slot = u32::from(signature.com_slot?);
                if slot >= 11 {
                    return None;
                }
                let name = format!("{interface}::{}", signature.name);
                let method = env.sys.thunks.publish(env.cpu, "dinput8.dll", &name).ok()?;
                env.cpu.write_u32(table + slot * 4, method).ok()?;
            }
        }
        let com = Com::new(&mut env.sys.objects)?;
        env.ext.insert(State {
            com,
            tables,
            legacy: None,
        });
        Some(())
    })();
    if result.is_none() {
        for pointer in tables.into_iter().filter(|&pointer| pointer != 0) {
            free(env, pointer);
        }
    }
    result.is_some()
}
fn release(env: &mut Env<'_>, pointer: u32) -> u32 {
    let Some(state) = env.ext.try_get::<State>() else {
        return 0;
    };
    let Some(released) = state.com.release::<Factory>(&mut env.sys.objects, pointer) else {
        return 0;
    };
    if let Some(destroyed) = released.destroyed {
        for interface in destroyed.interfaces {
            free(env, interface);
        }
    }
    released.references
}
fn object(env: &mut Env<'_>, wide: bool) -> Option<u32> {
    if !initialize(env) {
        return None;
    }
    let primary = alloc(env, 8)?;
    let Some(other) = alloc(env, 8) else {
        free(env, primary);
        return None;
    };
    let state = env.ext.get::<State>();
    let (com, tables) = (state.com, state.tables);
    let index = usize::from(wide);
    if env.cpu.write_u32(primary, tables[index]).is_err()
        || env.cpu.write_u32(other, tables[1 - index]).is_err()
    {
        free(env, other);
        free(env, primary);
        return None;
    }
    let Some(identity) = com.create(
        &mut env.sys.objects,
        primary,
        if wide { IID_W } else { IID_A },
        Factory,
    ) else {
        free(env, other);
        free(env, primary);
        return None;
    };
    if !com.register_interface(
        &mut env.sys.objects,
        identity,
        if wide { IID_A } else { IID_W },
        other,
    ) {
        free(env, other);
        release(env, primary);
        return None;
    }
    com.enable_guest(env, identity);
    Some(primary)
}
#[win32(dinput8, name = "DirectInput8Create", level = state)]
fn create(
    env: &mut Env<'_>,
    instance: Handle,
    version: u32,
    requested: GuestPtr<Iid>,
    output: GuestPtr<u32>,
    outer: GuestPtr<()>,
) -> u32 {
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let _ = output.write(env.cpu, 0);
    if instance.0 == 0
        || adv_abi::guest::check_range(env.cpu, requested.addr(), 16, Prot::READ).is_err()
    {
        return INVALID;
    }
    let Ok(iid) = requested.read(env.cpu) else {
        return INVALID;
    };
    if version < 0x800 {
        return 0x8007047e;
    } // DIERR_OLDDIRECTINPUTVERSION
    if version > 0x800 {
        return 0x80070481;
    } // DIERR_BETADIRECTINPUTVERSION
    if outer.addr() != 0 {
        return NO_AGGREGATION;
    }
    if iid != IID_A && iid != IID_W {
        return NO_INTERFACE;
    }
    let Some(pointer) = object(env, iid == IID_W) else {
        return OUT_OF_MEMORY;
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
fn query(env: &mut Env<'_>, this: u32, requested: GuestPtr<Iid>, output: GuestPtr<u32>) -> u32 {
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let _ = output.write(env.cpu, 0);
    if adv_abi::guest::check_range(env.cpu, requested.addr(), 16, Prot::READ).is_err() {
        return INVALID;
    }
    let Ok(iid) = requested.read(env.cpu) else {
        return INVALID;
    };
    let Some(state) = env.ext.try_get::<State>() else {
        return INVALID;
    };
    let result = state.com.query::<Factory>(&mut env.sys.objects, this, iid);
    match result {
        Ok(pointer) => {
            if output.write(env.cpu, pointer).is_ok() {
                0
            } else {
                release(env, pointer);
                INVALID
            }
        }
        Err(QueryError::NoInterface) => NO_INTERFACE,
        Err(QueryError::ReferenceLimit) => OUT_OF_MEMORY,
        Err(QueryError::InvalidObject) => INVALID,
    }
}
fn add_ref(env: &mut Env<'_>, this: u32) -> u32 {
    env.ext
        .try_get::<State>()
        .and_then(|state| state.com.add_ref::<Factory>(&mut env.sys.objects, this))
        .unwrap_or(0)
}
fn enumerate(env: &mut Env<'_>, this: u32, class: u32, callback: GuestPtr<()>, flags: u32) -> u32 {
    if !env
        .ext
        .try_get::<State>()
        .is_some_and(|state| state.com.get::<Factory>(&env.sys.objects, this).is_some())
        || flags & !0x70101 != 0
        || adv_abi::guest::check_range(env.cpu, callback.addr(), 1, Prot::EXEC).is_err()
    {
        return INVALID;
    }
    // Empty GAMECTRL enumeration is real: this profile has no controllers.
    // ALL/keyboard/pointer are deliberately unsupported, not reported empty.
    if class != 4 {
        return UNSUPPORTED;
    }
    0
}
macro_rules! factory_methods {
    ($query:ident,$add:ident,$release:ident,$enumerate:ident,$q:tt,$a:tt,$r:tt,$e:tt) => {
        #[win32(dinput8, name=$q, level=implement)]
        fn $query(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            iid: GuestPtr<Iid>,
            output: GuestPtr<u32>,
        ) -> u32 {
            query(env, this.addr(), iid, output)
        }
        #[win32(dinput8, name=$a, level=implement)]
        fn $add(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            add_ref(env, this.addr())
        }
        #[win32(dinput8, name=$r, level=implement)]
        fn $release(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            release(env, this.addr())
        }
        #[win32(dinput8, name=$e, level=state)]
        fn $enumerate(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            class: u32,
            callback: GuestPtr<()>,
            _context: GuestPtr<()>,
            flags: u32,
        ) -> u32 {
            enumerate(env, this.addr(), class, callback, flags)
        }
    };
}
factory_methods!(
    query_a,
    add_a,
    release_a,
    enum_a,
    "IDirectInput8A::QueryInterface",
    "IDirectInput8A::AddRef",
    "IDirectInput8A::Release",
    "IDirectInput8A::EnumDevices"
);
factory_methods!(
    query_w,
    add_w,
    release_w,
    enum_w,
    "IDirectInput8W::QueryInterface",
    "IDirectInput8W::AddRef",
    "IDirectInput8W::Release",
    "IDirectInput8W::EnumDevices"
);
pub fn register(registry: &mut Registry) {
    legacy::register(registry);
    for api in [
        create_ABI,
        query_a_ABI,
        add_a_ABI,
        release_a_ABI,
        enum_a_ABI,
        query_w_ABI,
        add_w_ABI,
        release_w_ABI,
        enum_w_ABI,
    ] {
        registry.register(api);
    }
    crate::refcounts::install(registry, "dinput8.dll");
    crate::refcounts::install(registry, "dinput.dll");
}
#[cfg(test)]
mod tests;
