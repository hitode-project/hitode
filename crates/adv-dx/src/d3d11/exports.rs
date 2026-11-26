//! Private runtime entry points shared by the D3D10 and D3D11 libraries.
use super::glue::{State, array, read, write};
use crate::interop::ddi::register_proc;
use adv_abi::{Env, Handler, Outcome, Registry};
use adv_cpu::backend::{Fault, FaultKind, Reg};

const E_NOTIMPL: u32 = 0x80004001;
const E_INVALIDARG: u32 = 0x80070057;
const E_UNEXPECTED: u32 = 0x8000ffff;

pub(super) fn register(registry: &mut Registry, create_core: Handler) {
    crate::interop::ddi::register(registry, "d3d11.dll");
    for (name, argc, handler) in [
        ("D3D11CoreCreateDevice", 10, create_core),
        ("D3D11CoreRegisterLayers", 2, register_layers),
        ("D3D11CoreGetLayeredDeviceSize", 2, layered_size),
        ("D3D11CoreCreateLayeredDevice", 5, layered_create),
    ] {
        register_proc(registry, "d3d11.dll", name, argc, handler);
    }
    for (module, create, argc) in [
        ("d3d10core.dll", "D3D10CoreCreateDevice", 5),
        ("d3d10_1core.dll", "D3D10CoreCreateDevice1", 7),
    ] {
        crate::interop::ddi::register(registry, module);
        register_proc(registry, module, create, argc, not_implemented);
        register_proc(registry, module, "D3D10CoreGetVersion", 0, version);
        register_proc(
            registry,
            module,
            "D3D10CoreGetSupportedVersions",
            3,
            not_implemented,
        );
        register_proc(
            registry,
            module,
            "D3D10CoreRegisterLayers",
            0,
            not_implemented,
        );
    }
}

fn not_implemented(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(E_NOTIMPL)
}

fn version(_: &mut Env<'_>) -> Outcome {
    Outcome::Return64(0x000a_0001_0004_1770)
}

pub(super) fn access_violation(env: &Env<'_>) -> Outcome {
    Outcome::Raise(Fault {
        kind: FaultKind::Read(0),
        pc: env.cpu.reg(Reg::Eip),
    })
}

fn register_layers(env: &mut Env<'_>) -> Outcome {
    let Ok(layers) = array::<[u32; 4]>(env, env.arg(0), env.arg(1)) else {
        return Outcome::Return(E_INVALIDARG);
    };
    let registered = &mut env.ext.get_mut::<State>().layers;
    for layer in layers {
        if !registered.contains(&layer) {
            registered.push(layer);
        }
    }
    Outcome::Return(0)
}

fn layer_arguments(env: &Env<'_>) -> Option<([u32; 10], [u32; 4], u32)> {
    if env.arg(1) != 40 {
        return None;
    }
    let mut args = read::<[u32; 10]>(env, env.arg(0)).ok()?;
    let first = read::<u32>(env, args[2]).ok()?;
    let index = read::<u32>(env, args[3]).ok()?;
    let layer = *env.ext.get::<State>().layers.get(index as usize)?;
    args[2] += 4;
    args[3] += 4;
    Some((args, layer, first))
}

fn layered_size(env: &mut Env<'_>) -> Outcome {
    let Some((args, layer, first)) = layer_arguments(env) else {
        return Outcome::Return(u32::MAX);
    };
    let scratch = env.cpu.reg(Reg::Esp).wrapping_sub(40);
    if write(env, scratch, args).is_err() {
        return Outcome::Return(u32::MAX);
    }
    Outcome::call_on_stack(layer[2], [first, scratch, 40], scratch, |_, value| {
        Outcome::Return(value.value())
    })
}

fn layered_create(env: &mut Env<'_>) -> Outcome {
    let (opaque, iid, out) = (env.arg(2), env.arg(3), env.arg(4));
    if out == 0 {
        return Outcome::Return(E_UNEXPECTED);
    }
    if write(env, out, 0u32).is_err() || iid == 0 {
        return Outcome::Return(E_INVALIDARG);
    }
    let Some((args, layer, first)) = layer_arguments(env) else {
        return Outcome::Return(E_INVALIDARG);
    };
    let scratch = env.cpu.reg(Reg::Esp).wrapping_sub(40);
    if write(env, scratch, args).is_err() {
        return Outcome::Return(E_INVALIDARG);
    }
    Outcome::call_on_stack(
        layer[3],
        [first, scratch, 40, opaque, iid, out],
        scratch,
        |_, value| Outcome::Return(value.value()),
    )
}
