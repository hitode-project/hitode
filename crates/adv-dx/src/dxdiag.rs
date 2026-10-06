//! A minimal DxDiag capability tree over the installed virtual DirectX fronts.
//! No host device inventory is projected. All interfaces and references belong
//! to the common COM/Objects owner, and Automation values use its shared BSTRs.
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::{Env, Machine, Outcome};
use adv_os::{
    Heaps,
    com::{Com, ComIdentity, Destroyed, Iid, QueryError, alloc_bstr, free_bstr},
};

pub const CLSID_PROVIDER: Iid = [
    0x71, 0x80, 0x5b, 0xa6, 0xfe, 0x3b, 0x13, 0x42, 0x9a, 0x5b, 0x49, 0x1d, 0xa4, 0x46, 0x1c, 0xa7,
];
pub const IID_PROVIDER: Iid = [
    0xb0, 0x4c, 0x6b, 0x9c, 0xf8, 0x23, 0xcc, 0x49, 0xa3, 0xed, 0x45, 0xa5, 0x50, 0, 0xa6, 0xd2,
];
pub const IID_CONTAINER: Iid = [
    0x2f, 0x46, 0x0f, 0x7d, 0x64, 0x40, 0x62, 0x48, 0xbc, 0x7f, 0x93, 0x3e, 0x50, 0x58, 0xc1, 0x0f,
];

const DLL: &str = "dxdiagn.dll";
const INVALID: u32 = 0x80070057;
const POINTER: u32 = 0x80004003;
const NO_INTERFACE: u32 = 0x80004002;
const OUT_OF_MEMORY: u32 = 0x8007000e;
const NOT_INITIALIZED: u32 = 0x800401f0;
const UNSUPPORTED: u32 = 0x80004001;
const NO_AGGREGATION: u32 = 0x80040110;
const INSUFFICIENT_BUFFER: u32 = 0x8007007a;
const SYSTEM: &str = "DxDiag_SystemInfo";
const PROPERTIES: [&str; 3] = [
    "dwDirectXVersionMajor",
    "dwDirectXVersionMinor",
    "szDirectXVersionLetter",
];

type Id = ComIdentity<Payload>;
#[derive(Clone, Copy)]
enum Node {
    Root,
    System,
}
enum Payload {
    Provider {
        initialized: bool,
        root: Option<Id>,
        version9c: bool,
    },
    Container {
        node: Node,
        child: Option<Id>,
        version9c: bool,
    },
}
struct State {
    com: Com,
    tables: [u32; 2],
}

fn installed_version(env: &Env<'_>) -> bool {
    #[cfg(feature = "gpu")]
    let graphics = env.ext.try_get::<crate::d3d9::State>().is_some();
    #[cfg(not(feature = "gpu"))]
    let graphics = false;
    graphics && env.ext.try_get::<crate::dsound::Sound>().is_some()
}

fn alloc(env: &mut Env<'_>, size: u32) -> Option<u32> {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, size, true)
}
fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}
fn initialize_tables(env: &mut Env<'_>) -> bool {
    if env.ext.try_get::<State>().is_some() {
        return true;
    }
    let mut tables = [0; 2];
    let result = (|| {
        for (index, (interface, slots)) in [("IDxDiagProvider", 5u32), ("IDxDiagContainer", 9)]
            .into_iter()
            .enumerate()
        {
            let signatures: Vec<_> = adv_abi::signature::com_signatures()
                .iter()
                .filter(|signature| signature.module == interface)
                .collect();
            if signatures.len() != slots as usize {
                return None;
            }
            let table = alloc(env, slots * 4)?;
            tables[index] = table;
            let mut occupied = 0u32;
            for signature in signatures {
                let slot = u32::from(signature.com_slot?);
                if slot >= slots || occupied & (1 << slot) != 0 {
                    return None;
                }
                occupied |= 1 << slot;
                let method = env
                    .sys
                    .thunks
                    .publish(env.cpu, DLL, &format!("{interface}::{}", signature.name))
                    .ok()?;
                env.cpu.write_u32(table + slot * 4, method).ok()?;
            }
        }
        let com = Com::new(&mut env.sys.objects)?;
        env.ext.insert(State { com, tables });
        Some(())
    })();
    if result.is_none() {
        for pointer in tables.into_iter().filter(|&pointer| pointer != 0) {
            free(env, pointer);
        }
    }
    result.is_some()
}

fn object(env: &mut Env<'_>, payload: Payload) -> Option<(u32, Id)> {
    if !initialize_tables(env) {
        return None;
    }
    let provider = matches!(payload, Payload::Provider { .. });
    let pointer = alloc(env, 4)?;
    let state = env.ext.get::<State>();
    let (com, table) = (state.com, state.tables[usize::from(!provider)]);
    if env.cpu.write_u32(pointer, table).is_err() {
        free(env, pointer);
        return None;
    }
    let Some(identity) = com.create(
        &mut env.sys.objects,
        pointer,
        if provider {
            IID_PROVIDER
        } else {
            IID_CONTAINER
        },
        payload,
    ) else {
        free(env, pointer);
        return None;
    };
    Some((pointer, identity))
}

fn payload<'a>(env: &'a Env<'_>, this: u32) -> Option<&'a Payload> {
    env.ext.try_get::<State>()?.com.get(&env.sys.objects, this)
}
fn destroy(env: &mut Env<'_>, destroyed: Destroyed<Payload>) {
    let child = match destroyed.value {
        Payload::Provider { root, .. } => root,
        Payload::Container { child, .. } => child,
    };
    if let Some(child) = child {
        let com = env.ext.get::<State>().com;
        if let Some(Some(destroyed)) = com.release_internal(&mut env.sys.objects, child) {
            destroy(env, destroyed);
        }
    }
    for pointer in destroyed.interfaces {
        free(env, pointer);
    }
}
fn release(env: &mut Env<'_>, this: u32) -> u32 {
    let Some(state) = env.ext.try_get::<State>() else {
        return 0;
    };
    let Some(released) = state.com.release::<Payload>(&mut env.sys.objects, this) else {
        return 0;
    };
    if let Some(destroyed) = released.destroyed {
        destroy(env, destroyed);
    }
    released.references
}
fn add_ref(env: &mut Env<'_>, this: u32) -> u32 {
    env.ext
        .try_get::<State>()
        .and_then(|state| state.com.add_ref::<Payload>(&mut env.sys.objects, this))
        .unwrap_or(0)
}
fn query(env: &mut Env<'_>, this: u32, requested: GuestPtr<Iid>, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return POINTER;
    }
    let Ok(iid) = requested.read(env.cpu) else {
        return POINTER;
    };
    let Some(state) = env.ext.try_get::<State>() else {
        return INVALID;
    };
    match state.com.query::<Payload>(&mut env.sys.objects, this, iid) {
        Ok(pointer) => {
            if output.write(env.cpu, pointer).is_ok() {
                0
            } else {
                release(env, pointer);
                POINTER
            }
        }
        Err(QueryError::InvalidObject) => INVALID,
        Err(QueryError::NoInterface) => NO_INTERFACE,
        Err(QueryError::ReferenceLimit) => OUT_OF_MEMORY,
    }
}

fn create(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    let output = GuestPtr::<u32>::new(output);
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(POINTER);
    }
    if outer != 0 {
        return Outcome::Return(NO_AGGREGATION);
    }
    if iid != adv_os::com::IUNKNOWN && iid != IID_PROVIDER {
        return Outcome::Return(NO_INTERFACE);
    }
    let version9c = installed_version(env);
    let Some((pointer, _)) = object(
        env,
        Payload::Provider {
            initialized: false,
            root: None,
            version9c,
        },
    ) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return Outcome::Return(POINTER);
    }
    Outcome::Return(0)
}

/// Install the class constructor after assembling the virtual device fronts.
pub fn install<B: Backend>(machine: &mut Machine<B>) {
    machine
        .ext
        .get_or_default::<adv_os::com::ClassFactories>()
        .register(CLSID_PROVIDER, create);
}

macro_rules! unknown_methods {
    ($query:ident,$add:ident,$release:ident,$q:tt,$a:tt,$r:tt) => {
        #[win32(dxdiagn,name=$q,level=implement)]
        fn $query(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            iid: GuestPtr<Iid>,
            output: GuestPtr<u32>,
        ) -> u32 {
            query(env, this.addr(), iid, output)
        }
        #[win32(dxdiagn,name=$a,level=implement)]
        fn $add(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            add_ref(env, this.addr())
        }
        #[win32(dxdiagn,name=$r,level=implement)]
        fn $release(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
            release(env, this.addr())
        }
    };
}
unknown_methods!(
    provider_query,
    provider_add,
    provider_release,
    "IDxDiagProvider::QueryInterface",
    "IDxDiagProvider::AddRef",
    "IDxDiagProvider::Release"
);
unknown_methods!(
    container_query,
    container_add,
    container_release,
    "IDxDiagContainer::QueryInterface",
    "IDxDiagContainer::AddRef",
    "IDxDiagContainer::Release"
);

#[win32(dxdiagn,name="IDxDiagProvider::Initialize",level=implement)]
fn initialize(env: &mut Env<'_>, this: GuestPtr<()>, params: GuestPtr<u8>) -> u32 {
    if !matches!(payload(env, this.addr()), Some(Payload::Provider { .. })) {
        return INVALID;
    }
    if check_range(env.cpu, params.addr(), 16, Prot::READ).is_err() {
        return POINTER;
    }
    if env.cpu.read_u32(params.addr()).ok() != Some(16)
        || env.cpu.read_u32(params.addr() + 4).ok() != Some(111)
        || env.cpu.read_u32(params.addr() + 12).ok() != Some(0)
    {
        return INVALID;
    }
    let com = env.ext.get::<State>().com;
    let Some(Payload::Provider { initialized, .. }) =
        com.get_mut(&mut env.sys.objects, this.addr())
    else {
        return INVALID;
    };
    *initialized = true;
    0
}

fn child(env: &mut Env<'_>, owner: u32, root: bool, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return POINTER;
    }
    let (existing, version9c) = match payload(env, owner) {
        Some(Payload::Provider {
            initialized: true,
            root: child,
            version9c,
        }) if root => (*child, *version9c),
        Some(Payload::Provider {
            initialized: false, ..
        }) if root => return NOT_INITIALIZED,
        Some(Payload::Container {
            node: Node::Root,
            child,
            version9c,
        }) if !root => (*child, *version9c),
        _ => return INVALID,
    };
    let com = env.ext.get::<State>().com;
    let pointer = if let Some(identity) = existing {
        let Some(pointer) = com.canonical(&env.sys.objects, identity) else {
            return INVALID;
        };
        if com
            .add_ref_identity(&mut env.sys.objects, identity)
            .is_none()
        {
            return OUT_OF_MEMORY;
        }
        pointer
    } else {
        let Some((pointer, identity)) = object(
            env,
            Payload::Container {
                node: if root { Node::Root } else { Node::System },
                child: None,
                version9c,
            },
        ) else {
            return OUT_OF_MEMORY;
        };
        if !com.retain_internal(&mut env.sys.objects, identity) {
            release(env, pointer);
            return OUT_OF_MEMORY;
        }
        match com.get_mut::<Payload>(&mut env.sys.objects, owner) {
            Some(Payload::Provider { root, .. }) => *root = Some(identity),
            Some(Payload::Container { child, .. }) => *child = Some(identity),
            None => unreachable!("owner stayed alive during synchronous allocation"),
        }
        pointer
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return POINTER;
    }
    0
}

#[win32(dxdiagn,name="IDxDiagProvider::GetRootContainer",level=implement)]
fn root(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    child(env, this.addr(), true, output)
}

fn names(env: &Env<'_>, this: u32, children: bool) -> Option<&'static [&'static str]> {
    match payload(env, this)? {
        Payload::Container {
            node: Node::Root, ..
        } if children => Some(&[SYSTEM]),
        Payload::Container {
            node: Node::System,
            version9c: true,
            ..
        } if !children => Some(&PROPERTIES),
        Payload::Container { .. } => Some(&[]),
        _ => None,
    }
}
fn count(env: &mut Env<'_>, this: u32, output: GuestPtr<u32>, children: bool) -> u32 {
    let Some(names) = names(env, this, children) else {
        return INVALID;
    };
    if output.write(env.cpu, names.len() as u32).is_err() {
        POINTER
    } else {
        0
    }
}
fn enumerate(
    env: &mut Env<'_>,
    this: u32,
    index: u32,
    output: WideStr,
    capacity: u32,
    children: bool,
) -> u32 {
    let Some(names) = names(env, this, children) else {
        return INVALID;
    };
    let Some(name) = names.get(index as usize) else {
        return INVALID;
    };
    let units: Vec<_> = name.encode_utf16().chain([0]).collect();
    if capacity < units.len() as u32 {
        return INSUFFICIENT_BUFFER;
    }
    let bytes: Vec<_> = units.into_iter().flat_map(u16::to_le_bytes).collect();
    if check_range(env.cpu, output.0.addr(), bytes.len() as u32, Prot::WRITE).is_err() {
        return POINTER;
    }
    if env.cpu.write(output.0.addr(), &bytes).is_err() {
        POINTER
    } else {
        0
    }
}
#[win32(dxdiagn,name="IDxDiagContainer::GetNumberOfChildContainers",level=implement)]
fn child_count(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    count(env, this.addr(), output, true)
}
#[win32(dxdiagn,name="IDxDiagContainer::EnumChildContainerNames",level=implement)]
fn child_name(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: WideStr,
    capacity: u32,
) -> u32 {
    enumerate(env, this.addr(), index, output, capacity, true)
}
#[win32(dxdiagn,name="IDxDiagContainer::GetNumberOfProps",level=implement)]
fn prop_count(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    count(env, this.addr(), output, false)
}
#[win32(dxdiagn,name="IDxDiagContainer::EnumPropNames",level=implement)]
fn prop_name(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: WideStr,
    capacity: u32,
) -> u32 {
    enumerate(env, this.addr(), index, output, capacity, false)
}

#[win32(dxdiagn,name="IDxDiagContainer::GetChildContainer",level=implement)]
fn get_child(env: &mut Env<'_>, this: GuestPtr<()>, name: WideStr, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return POINTER;
    }
    let Ok(name) = name.read(env.cpu, 32768) else {
        return POINTER;
    };
    if name != SYSTEM.encode_utf16().collect::<Vec<_>>() {
        return INVALID;
    }
    child(env, this.addr(), false, output)
}
#[win32(dxdiagn,name="IDxDiagContainer::GetProp",level=implement)]
fn get_prop(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    name: WideStr,
    output: GuestPtr<[u8; 16]>,
) -> u32 {
    if output.write(env.cpu, [0; 16]).is_err() {
        return POINTER;
    }
    let Ok(name) = name.read(env.cpu, 32768) else {
        return POINTER;
    };
    let Some(Payload::Container {
        node: Node::System,
        version9c,
        ..
    }) = payload(env, this.addr())
    else {
        return INVALID;
    };
    if !version9c {
        return UNSUPPORTED;
    }
    let name = String::from_utf16_lossy(&name);
    let mut variant = [0; 16];
    let bstr = match name.as_str() {
        "dwDirectXVersionMajor" | "dwDirectXVersionMinor" => {
            variant[..2].copy_from_slice(&19u16.to_le_bytes()); // VT_UI4
            variant[8..12].copy_from_slice(
                &(if name == "dwDirectXVersionMajor" {
                    9u32
                } else {
                    0
                })
                .to_le_bytes(),
            );
            0
        }
        "szDirectXVersionLetter" => {
            let pointer = alloc_bstr(env, &[u16::from(b'c')]);
            if pointer == 0 {
                return OUT_OF_MEMORY;
            }
            variant[..2].copy_from_slice(&8u16.to_le_bytes()); // VT_BSTR
            variant[8..12].copy_from_slice(&pointer.to_le_bytes());
            pointer
        }
        _ => return INVALID,
    };
    if output.write(env.cpu, variant).is_err() {
        if bstr != 0 {
            free_bstr(env, bstr);
        }
        return POINTER;
    }
    0
}

pub fn register(registry: &mut Registry) {
    for api in [
        provider_query_ABI,
        provider_add_ABI,
        provider_release_ABI,
        initialize_ABI,
        root_ABI,
        container_query_ABI,
        container_add_ABI,
        container_release_ABI,
        child_count_ABI,
        child_name_ABI,
        get_child_ABI,
        prop_count_ABI,
        prop_name_ABI,
        get_prop_ABI,
    ] {
        registry.register(api);
    }
}
