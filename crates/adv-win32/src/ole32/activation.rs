//! In-process activation shares installed constructors, registry data and the COM object owner.
use super::*;
mod multi;
use adv_abi::{
    WideStr,
    guest::check_range,
    types::{COSERVERINFO, MULTI_QI},
};
use adv_cpu::backend::{GuestExt, Prot};
use adv_os::{
    Heaps,
    com::{ClassFactories, ClassFactory, Com, IUNKNOWN, Iid, QueryError},
};
use std::collections::BTreeMap;
const CLASS_FACTORY: Iid = [1, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const POINTER: u32 = 0x80004003;
const INVALID: u32 = 0x80070057;
const MEMORY: u32 = 0x8007000e;
const UNREGISTERED: u32 = 0x80040154;
#[derive(Clone, Copy)]
struct Factory {
    class: Iid,
    construct: ClassFactory,
}
struct State {
    com: Com,
    table: u32,
    locks: BTreeMap<Iid, u32>,
}
fn allocate(env: &mut Env<'_>, bytes: u32) -> Option<u32> {
    env.ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, bytes, true)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    env.ext
        .get_mut::<Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, pointer);
}
fn initialize(env: &mut Env<'_>) -> Option<(Com, u32)> {
    if let Some(state) = env.ext.try_get::<State>() {
        return Some((state.com, state.table));
    }
    let table = allocate(env, 20)?;
    let result = (|| {
        for signature in adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| s.module == "IClassFactory")
        {
            let entry = env
                .sys
                .thunks
                .publish(
                    env.cpu,
                    "ole32.dll",
                    &format!("IClassFactory::{}", signature.name),
                )
                .ok()?;
            env.cpu
                .write_u32(table + u32::from(signature.com_slot?) * 4, entry)
                .ok()?;
        }
        let com = Com::new(&mut env.sys.objects)?;
        env.ext.insert(State {
            com,
            table,
            locks: BTreeMap::new(),
        });
        Some((com, table))
    })();
    if result.is_none() {
        free(env, table);
    }
    result
}
fn factory(env: &Env<'_>, pointer: u32) -> Option<Factory> {
    env.ext
        .try_get::<State>()?
        .com
        .get::<Factory>(&env.sys.objects, pointer)
        .copied()
}
fn constructor(env: &Env<'_>, class: Iid, context: u32) -> Option<ClassFactory> {
    (context & 1 != 0)
        .then(|| env.ext.try_get::<ClassFactories>()?.get(&class))
        .flatten()
}
fn guid(env: &Env<'_>, pointer: u32) -> Result<Iid, u32> {
    GuestPtr::<Iid>::new(pointer)
        .read(env.cpu)
        .map_err(|_| POINTER)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_ABI,
        multi::create_ex_ABI,
        progid_ABI,
        query_ABI,
        addref_ABI,
        release_ABI,
        create_ABI,
        lock_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(ole32,name="CoGetClassObject",level=implement)]
fn get(
    env: &mut Env<'_>,
    class: GuestPtr<Iid>,
    context: u32,
    reserved: GuestPtr<()>,
    interface: GuestPtr<Iid>,
    output: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return POINTER;
    }
    if !reserved.is_null() {
        return INVALID;
    }
    let (class, interface) = match (guid(env, class.addr()), guid(env, interface.addr())) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return POINTER,
    };
    let Some(construct) = constructor(env, class, context) else {
        return UNREGISTERED;
    };
    if interface != IUNKNOWN && interface != CLASS_FACTORY {
        return 0x80004002;
    }
    let Some((com, table)) = initialize(env) else {
        return MEMORY;
    };
    let Some(pointer) = allocate(env, 4) else {
        return MEMORY;
    };
    if env.cpu.write_u32(pointer, table).is_err()
        || com
            .create(
                &mut env.sys.objects,
                pointer,
                CLASS_FACTORY,
                Factory { class, construct },
            )
            .is_none()
    {
        free(env, pointer);
        return MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release_value(env, pointer);
        return POINTER;
    }
    0
}
#[win32(ole32,name="IClassFactory::QueryInterface",level=implement)]
fn query(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    interface: GuestPtr<Iid>,
    output: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return POINTER;
    }
    let interface = match interface.read(env.cpu) {
        Ok(value) => value,
        Err(_) => return POINTER,
    };
    let Some(com) = env.ext.try_get::<State>().map(|s| s.com) else {
        return INVALID;
    };
    match com.query::<Factory>(&mut env.sys.objects, this.addr(), interface) {
        Ok(pointer) => {
            if output.write(env.cpu, pointer).is_ok() {
                0
            } else {
                release_value(env, pointer);
                POINTER
            }
        }
        Err(QueryError::NoInterface) => 0x80004002,
        Err(QueryError::ReferenceLimit) => MEMORY,
        Err(QueryError::InvalidObject) => INVALID,
    }
}
#[win32(ole32,name="IClassFactory::AddRef",level=implement)]
fn addref(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    env.ext
        .try_get::<State>()
        .and_then(|s| s.com.add_ref::<Factory>(&mut env.sys.objects, this.addr()))
        .unwrap_or(0)
}
fn release_value(env: &mut Env<'_>, pointer: u32) -> u32 {
    let Some(com) = env.ext.try_get::<State>().map(|s| s.com) else {
        return 0;
    };
    let Some(released) = com.release::<Factory>(&mut env.sys.objects, pointer) else {
        return 0;
    };
    if let Some(dead) = released.destroyed {
        for pointer in dead.interfaces {
            free(env, pointer);
        }
    }
    released.references
}
#[win32(ole32,name="IClassFactory::Release",level=implement)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    release_value(env, this.addr())
}
#[win32(ole32,name="IClassFactory::CreateInstance",level=implement)]
fn create(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    outer: GuestPtr<()>,
    interface: GuestPtr<Iid>,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(POINTER);
    }
    let interface = match interface.read(env.cpu) {
        Ok(value) => value,
        Err(_) => return Outcome::Return(POINTER),
    };
    let Some(factory) = factory(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    (factory.construct)(env, interface, outer.addr(), output.addr())
}
#[win32(ole32,name="IClassFactory::LockServer",level=state)]
fn lock(env: &mut Env<'_>, this: GuestPtr<()>, lock: i32) -> u32 {
    let Some(factory) = factory(env, this.addr()) else {
        return INVALID;
    };
    let count = env
        .ext
        .get_mut::<State>()
        .locks
        .entry(factory.class)
        .or_default();
    if lock != 0 {
        let Some(next) = count.checked_add(1) else {
            return MEMORY;
        };
        *count = next;
    } else {
        *count = count.saturating_sub(1);
    }
    0
}
#[win32(ole32,name="CLSIDFromProgID",level=implement)]
fn progid(env: &mut Env<'_>, name: WideStr, output: GuestPtr<Iid>) -> u32 {
    if check_range(env.cpu, output.addr(), 16, Prot::WRITE).is_err() {
        return POINTER;
    }
    let name = match name.read(env.cpu, 32768) {
        Ok(name) if !name.is_empty() => name,
        _ => return INVALID,
    };
    let mut path = name;
    path.extend("\\CLSID".encode_utf16());
    let key = adv_os::registry::Key {
        root: 0x80000000,
        path: adv_os::registry::fold(&path),
    };
    let registry = crate::advapi32::state(env);
    if registry.load_error.is_some() {
        return 0x80040150;
    }
    let Some(value) = registry.keys.get(&key).and_then(|v| v.get(&Vec::new())) else {
        adv_abi::warn_unavailable(
            env,
            "ole32.dll!CLSIDFromProgID",
            "OLE 1 class generation and registry publication are unavailable",
        );
        return 0x80040151;
    };
    if value.kind != 1 || value.bytes.len() % 2 != 0 {
        return 0x800401f3;
    }
    let units = value
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .take_while(|u| *u != 0)
        .collect::<Vec<_>>();
    let class = String::from_utf16(&units)
        .ok()
        .and_then(|s| crate::misc::parse_guid(&s));
    match class {
        Some(class) => {
            if output.write(env.cpu, class).is_ok() {
                0
            } else {
                POINTER
            }
        }
        None => 0x800401f3,
    }
}
