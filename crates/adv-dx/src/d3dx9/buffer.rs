//! ID3DXBuffer owns an arbitrary guest byte buffer through the shared COM table.
use adv_abi::{Api, Conv, GuestPtr, ImplementationLevel, Registry};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};
use adv_os::{
    Heaps,
    com::{Com, Iid, QueryError},
};
const IID: Iid = [
    8, 0xfb, 0xa5, 0x8b, 0x95, 0x51, 0xe2, 0x40, 0xac, 0x58, 0x0d, 0x98, 0x9c, 0x3a, 1, 2,
];
const INVALID: u32 = 0x8876086c;
const OOM: u32 = 0x8007000e;
struct Store {
    com: Com,
    table: u32,
}
struct Buffer {
    data: u32,
    size: u32,
}
const METHODS: [(&str, u8, adv_machine::Handler); 5] = [
    ("ID3DXBuffer::QueryInterface", 3, query),
    ("ID3DXBuffer::AddRef", 1, add_ref),
    ("ID3DXBuffer::Release", 1, release),
    ("ID3DXBuffer::GetBufferPointer", 1, pointer),
    ("ID3DXBuffer::GetBufferSize", 1, size),
];
pub(super) fn register(registry: &mut Registry) {
    for (name, argc, handler) in METHODS {
        registry.register(Api {
            module: "d3dx9_43.dll",
            name,
            conv: Conv::Stdcall { argc },
            handler,
            level: ImplementationLevel::Implement,
        });
    }
}
fn alloc(env: &mut Env<'_>, bytes: &[u8]) -> Option<u32> {
    let h = env.ext.try_get_mut::<Heaps>()?;
    let p = h.alloc(
        env.cpu,
        &mut env.sys.vm,
        h.process_heap,
        bytes.len() as u32,
        false,
    )?;
    if env.cpu.write(p, bytes).is_err() {
        free(env, p);
        return None;
    }
    Some(p)
}
fn free(env: &mut Env<'_>, p: u32) {
    let h = env.ext.get_mut::<Heaps>();
    h.free(env.cpu, &mut env.sys.vm, h.process_heap, p);
}
fn initialize(env: &mut Env<'_>) -> Option<()> {
    if env.ext.try_get::<Store>().is_some() {
        return Some(());
    }
    let mut methods = [0u32; 5];
    for (slot, (name, _, _)) in METHODS.into_iter().enumerate() {
        methods[slot] = env.sys.thunks.publish(env.cpu, "d3dx9_43.dll", name).ok()?;
    }
    let table = alloc(env, bytemuck::cast_slice(&methods))?;
    let Some(com) = Com::new(&mut env.sys.objects) else {
        free(env, table);
        return None;
    };
    env.ext.insert(Store { com, table });
    Some(())
}
fn com(env: &Env<'_>) -> Option<Com> {
    Some(env.ext.try_get::<Store>()?.com)
}
pub(super) fn create(env: &mut Env<'_>, bytes: &[u8], output: GuestPtr<u32>) -> u32 {
    if bytes.len() > 64 * 1024 * 1024 || initialize(env).is_none() {
        return OOM;
    }
    let Some(data) = alloc(env, bytes) else {
        return OOM;
    };
    let table = env.ext.get::<Store>().table;
    let Some(pointer) = alloc(env, &table.to_le_bytes()) else {
        free(env, data);
        return OOM;
    };
    let owner = com(env).unwrap();
    if owner
        .create(
            &mut env.sys.objects,
            pointer,
            IID,
            Buffer {
                data,
                size: bytes.len() as u32,
            },
        )
        .is_none()
    {
        free(env, data);
        free(env, pointer);
        return OOM;
    }
    if output.write(env.cpu, pointer).is_err() {
        retire(env, pointer);
        return INVALID;
    }
    0
}
fn query(env: &mut Env<'_>) -> Outcome {
    let this = env.arg(0);
    let iid = GuestPtr::<Iid>::new(env.arg(1)).read(env.cpu);
    let out = GuestPtr::<u32>::new(env.arg(2));
    if adv_abi::guest::check_range(env.cpu, out.addr(), 4, Prot::WRITE).is_err()
        || out.write(env.cpu, 0).is_err()
    {
        return Outcome::Return(0x80004003);
    }
    let (Ok(iid), Some(owner)) = (iid, com(env)) else {
        return Outcome::Return(0x80004003);
    };
    let result = match owner.query::<Buffer>(&mut env.sys.objects, this, iid) {
        Ok(pointer) => {
            if out.write(env.cpu, pointer).is_ok() {
                0
            } else {
                retire(env, pointer);
                0x80004003
            }
        }
        Err(QueryError::NoInterface) => 0x80004002,
        Err(QueryError::ReferenceLimit) => OOM,
        Err(QueryError::InvalidObject) => 0x80004003,
    };
    Outcome::Return(result)
}
fn add_ref(env: &mut Env<'_>) -> Outcome {
    let this = env.arg(0);
    Outcome::Return(
        com(env)
            .and_then(|c| c.add_ref::<Buffer>(&mut env.sys.objects, this))
            .unwrap_or(0),
    )
}
fn retire(env: &mut Env<'_>, this: u32) -> u32 {
    let Some(dead) = com(env).and_then(|c| c.release::<Buffer>(&mut env.sys.objects, this)) else {
        return 0;
    };
    if let Some(value) = dead.destroyed {
        free(env, value.value.data);
        for p in value.interfaces {
            free(env, p);
        }
    }
    dead.references
}
fn release(env: &mut Env<'_>) -> Outcome {
    let this = env.arg(0);
    Outcome::Return(retire(env, this))
}
fn pointer(env: &mut Env<'_>) -> Outcome {
    let value = com(env)
        .and_then(|c| c.get::<Buffer>(&env.sys.objects, env.arg(0)))
        .map_or(0, |b| b.data);
    Outcome::Return(value)
}
fn size(env: &mut Env<'_>) -> Outcome {
    let value = com(env)
        .and_then(|c| c.get::<Buffer>(&env.sys.objects, env.arg(0)))
        .map_or(0, |b| b.size);
    Outcome::Return(value)
}
