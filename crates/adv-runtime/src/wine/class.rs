//! The fronts' class objects. Wine's ole32 serves `CoCreateInstance` from an
//! image's `DllGetClassObject`; the factory it returns activates the object
//! as the built-in stack's ole32 does: DirectDraw through `adv_dx::ddraw`,
//! other fronts through their registered class factories.
use std::collections::HashMap;

use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::layout::PAGE;
use adv_machine::{Env, Machine, Outcome};

/// The module whose thunks every factory's methods use.
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

/// What a factory activates.
#[derive(Clone, Copy)]
enum Activation {
    DirectDraw,
    Class([u8; 16]),
}

/// Static factories: a page holding the shared vtable, then one object per
/// class, each a pointer to that vtable.
struct Factories {
    page: u32,
    objects: HashMap<u32, Activation>,
}

pub(super) const EXPORTS: [&str; 2] = ["DllGetClassObject", "DllCanUnloadNow"];

pub(super) fn register(registry: &mut Registry) {
    for module in [MODULE, super::sound::MODULE] {
        registry.add(module, "DllCanUnloadNow", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::Return(1)
        });
    }
    registry.add(
        MODULE,
        "DllGetClassObject",
        Conv::Stdcall { argc: 3 },
        |env| {
            get_class_object(env, |class| {
                adv_dx::ddraw::front::CLASSES
                    .contains(&class)
                    .then_some(Activation::DirectDraw)
            })
        },
    );
    registry.add(
        super::sound::MODULE,
        "DllGetClassObject",
        Conv::Stdcall { argc: 3 },
        |env| {
            get_class_object(env, |class| {
                adv_dx::dsound::CLASSES
                    .contains(&class)
                    .then_some(Activation::Class(class))
            })
        },
    );
    registry.add(MODULE, METHODS[0].0, Conv::Stdcall { argc: 3 }, |env| {
        let (this, iid, out) = (env.arg(0), env.arg(1), env.arg(2));
        hresult(factory_interface(env, this, iid, out))
    });
    // The factories are static: their references never end them.
    for (name, argc) in [METHODS[1], METHODS[2]] {
        registry.add(MODULE, name, Conv::Stdcall { argc }, |_| Outcome::Return(1));
    }
    registry.add(MODULE, METHODS[3].0, Conv::Stdcall { argc: 4 }, |env| {
        let (this, outer, iid, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
        let iid = match read_guid(env, iid) {
            Ok(iid) => iid,
            Err(error) => return Outcome::Fail(error),
        };
        match env.ext.get::<Factories>().objects.get(&this).copied() {
            Some(Activation::DirectDraw) => adv_dx::ddraw::front::activate(env, iid, outer, out),
            Some(Activation::Class(class)) => {
                let factory = env
                    .ext
                    .get_or_default::<adv_os::com::ClassFactories>()
                    .get(&class);
                match factory {
                    Some(factory) => factory(env, iid, outer, out),
                    None => Outcome::Return(CLASS_E_CLASSNOTAVAILABLE),
                }
            }
            None => Outcome::Fail(format!("{MODULE}: unknown class factory {this:#x}")),
        }
    });
    registry.add(MODULE, METHODS[4].0, Conv::Stdcall { argc: 2 }, |_| {
        Outcome::Return(0)
    });
}

/// Write the factories' vtable into guest memory.
pub(super) fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    let page = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    for (index, (name, _)) in METHODS.into_iter().enumerate() {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        machine
            .cpu
            .write_u32(page + 4 * index as u32, entry)
            .map_err(|e| e.to_string())?;
    }
    machine.ext.insert(Factories {
        page,
        objects: HashMap::new(),
    });
    Ok(())
}

/// DllGetClassObject(class, iid, *out) for the classes `activation` knows.
fn get_class_object(
    env: &mut Env<'_>,
    activation: impl FnOnce([u8; 16]) -> Option<Activation>,
) -> Outcome {
    let (class, iid, out) = (env.arg(0), env.arg(1), env.arg(2));
    hresult((|| {
        let Some(activation) = activation(read_guid(env, class)?) else {
            write(env, out, 0)?;
            return Ok(CLASS_E_CLASSNOTAVAILABLE);
        };
        let factory = factory(env, activation)?;
        factory_interface(env, factory, iid, out)
    })())
}

/// The factory object for `activation`, made on first use.
fn factory(env: &mut Env<'_>, activation: Activation) -> Result<u32, String> {
    let factories = env.ext.get::<Factories>();
    let same = |a: &Activation| match (a, &activation) {
        (Activation::DirectDraw, Activation::DirectDraw) => true,
        (Activation::Class(a), Activation::Class(b)) => a == b,
        _ => false,
    };
    if let Some((&object, _)) = factories.objects.iter().find(|(_, a)| same(a)) {
        return Ok(object);
    }
    let object = factories.page + 4 * (METHODS.len() + factories.objects.len()) as u32;
    if object + 4 > factories.page + PAGE {
        return Err(format!("{MODULE}: too many class factories"));
    }
    let vtable = factories.page;
    write(env, object, vtable)?;
    env.ext
        .get_mut::<Factories>()
        .objects
        .insert(object, activation);
    Ok(object)
}

/// Hand out `factory` as `iid` through `out`.
fn factory_interface(env: &mut Env<'_>, factory: u32, iid: u32, out: u32) -> Result<u32, String> {
    let iid = read_guid(env, iid)?;
    if iid != IUNKNOWN && iid != ICLASSFACTORY {
        write(env, out, 0)?;
        return Ok(E_NOINTERFACE);
    }
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
