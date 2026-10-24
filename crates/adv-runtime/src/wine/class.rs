//! DirectDraw's class object. Wine's ole32 serves `CoCreateInstance` from
//! the image's `DllGetClassObject`; the factory it returns activates the
//! object as the built-in stack's ole32 does, through `adv_dx::ddraw`.
use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::layout::PAGE;
use adv_machine::{Env, Machine, Outcome};

const MODULE: &str = "ddraw.dll";
const IUNKNOWN: [u8; 16] = adv_abi::com::IUNKNOWN;
const ICLASSFACTORY: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const E_NOINTERFACE: u32 = 0x8000_4002;
const CLASS_E_CLASSNOTAVAILABLE: u32 = 0x8004_0111;

/// IClassFactory's methods, in vtable order, with their argument counts.
const METHODS: [(&str, u8); 5] = [
    ("IClassFactory::QueryInterface", 3),
    ("IClassFactory::AddRef", 1),
    ("IClassFactory::Release", 1),
    ("IClassFactory::CreateInstance", 4),
    ("IClassFactory::LockServer", 2),
];

/// The static factory: its vtable pointer, then the vtable.
struct Factory(u32);

pub(super) const EXPORTS: [&str; 2] = ["DllGetClassObject", "DllCanUnloadNow"];

pub(super) fn register(registry: &mut Registry) {
    registry.add(
        MODULE,
        "DllGetClassObject",
        Conv::Stdcall { argc: 3 },
        |env| {
            let (class, iid, out) = (env.arg(0), env.arg(1), env.arg(2));
            hresult((|| {
                let class = read_guid(env, class)?;
                if !adv_dx::ddraw::front::CLASSES.contains(&class) {
                    write(env, out, 0)?;
                    return Ok(CLASS_E_CLASSNOTAVAILABLE);
                }
                factory_interface(env, iid, out)
            })())
        },
    );
    registry.add(MODULE, "DllCanUnloadNow", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::Return(1)
    });
    registry.add(MODULE, METHODS[0].0, Conv::Stdcall { argc: 3 }, |env| {
        let (iid, out) = (env.arg(1), env.arg(2));
        hresult(factory_interface(env, iid, out))
    });
    // The factory is static: its references never end it.
    for (name, argc) in [METHODS[1], METHODS[2]] {
        registry.add(MODULE, name, Conv::Stdcall { argc }, |_| Outcome::Return(1));
    }
    registry.add(MODULE, METHODS[3].0, Conv::Stdcall { argc: 4 }, |env| {
        let (outer, iid, out) = (env.arg(1), env.arg(2), env.arg(3));
        match read_guid(env, iid) {
            Ok(iid) => adv_dx::ddraw::front::activate(env, iid, outer, out),
            Err(error) => Outcome::Fail(error),
        }
    });
    registry.add(MODULE, METHODS[4].0, Conv::Stdcall { argc: 2 }, |_| {
        Outcome::Return(0)
    });
}

/// Write the factory into guest memory.
pub(super) fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    let factory = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    let mut words = vec![factory + 4];
    for (name, _) in METHODS {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        words.push(entry);
    }
    for (index, word) in words.into_iter().enumerate() {
        machine
            .cpu
            .write_u32(factory + 4 * index as u32, word)
            .map_err(|e| e.to_string())?;
    }
    machine.ext.insert(Factory(factory));
    Ok(())
}

/// Hand out the factory as `iid` through `out`.
fn factory_interface(env: &mut Env<'_>, iid: u32, out: u32) -> Result<u32, String> {
    let iid = read_guid(env, iid)?;
    if iid != IUNKNOWN && iid != ICLASSFACTORY {
        write(env, out, 0)?;
        return Ok(E_NOINTERFACE);
    }
    let factory = env.ext.get::<Factory>().0;
    write(env, out, factory)?;
    Ok(0)
}

fn hresult(result: Result<u32, String>) -> Outcome {
    match result {
        Ok(hr) => Outcome::Return(hr),
        Err(error) => Outcome::Fail(error),
    }
}

fn read_guid(env: &Env<'_>, at: u32) -> Result<[u8; 16], String> {
    let mut guid = [0; 16];
    env.cpu
        .read(at, &mut guid)
        .map_err(|e| format!("{MODULE}: {e}"))?;
    Ok(guid)
}

fn write(env: &mut Env<'_>, at: u32, value: u32) -> Result<(), String> {
    env.cpu
        .write_u32(at, value)
        .map_err(|e| format!("{MODULE}: {e}"))
}
